//! One reading, from the trigger to the summary on disk.
//!
//! Every path that spends a model call on a pull request comes through [`visit`], and the doors it
//! passes are in one order for one reason — everything above the purchase costs nothing. Cache hit,
//! switched off, no head commit, out of scope, already tried, out of budget: each returns a
//! [`Summary`] carrying its own sentence, and none of them charges the day.
//!
//! Below that line the reading is a purchase, and there are two ladders. Where the review is yours
//! to give it is ONE model call over one downloaded diff doing the summary and the review together
//! ([`summarise_and_draft`], the owner's decision of 2026-08-24); otherwise the cheap two-stage
//! path ([`summarise_in_stages`]), which is also where the merged call falls back to when it runs
//! out of time. Either is one unit of the day's budget: the unit is the pull request analysed, not
//! the number of things the analysis produced.
//!
//! [`readings`] and [`ReadingDone`] are here rather than at the route because this is the one door
//! every model-spending path already passes — registered at the route it would have seen presses
//! and missed the pump.

use super::asking::*;
use super::budget::*;
use super::cache::*;
use super::checkout::*;
use super::scope::{unasked_scope, worth_a_visit};
use super::summary::*;
use crate::ai::claude_oneshot_with;
use crate::prq::Pr;
use crate::repos::Repo;
use serde::Serialize;
use std::time::Duration;

/// Read this pull request, and review it too **where skein would have reviewed it anyway**.
///
/// Read at the depth it earns, and cached against `(number, head_sha)`; `force` re-reads. Never
/// returns an error: a PR that could not be read is a [`Depth::Unread`] summary carrying the
/// reason, because the caller's only sane response to a failure here is to show you the PR anyway.
///
/// The conservative half of the pair. [`Review::IfYours`] means the review half runs only where it
/// is yours to give — `spend_a_visit`'s `draft_due`, which asks the lane and then whether you
/// wrote this or somebody asked you for it. That is why this is the default and
/// [`re_read_and_review`] is not: the background may read anything in scope, and may not
/// put a review on a pull request nobody involved you in.
pub fn summarise(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
    force: bool,
    trigger: Trigger,
) -> Summary {
    visit(repo, slug, pr, identities, force, trigger, Review::IfYours)
}

/// Read this pull request again and ask for a review **whether or not it is yours to give**.
///
/// The other half of the pair. That is the finding SKEIN-293 is about: since the drafter was merged
/// (SKEIN-263) "re-read" and "review the code" both come here, force a reading, download the diff
/// once and spend one model call — and the two differ in whether a review is asked for at all.
/// Nothing in either name said so. The pair is named for the difference now, so the call site has
/// to choose it deliberately.
///
/// **Never reached except by a person who asked for it.** Nothing in the background comes here —
/// [`read_waiting`] and the pane's pump both go through [`summarise`].
///
/// `force` and [`Trigger::Asked`] are not choices the caller gets, because neither is meaningful
/// here. A cached reading returns from [`visit`] before a model is asked anything, so a redraft
/// that honoured the cache would be a press that did nothing; and the day's ceiling is on skein's
/// own initiative, which this is by definition not.
pub fn re_read_and_review(repo: &Repo, slug: &str, pr: &Pr, identities: &[String]) -> Summary {
    visit(
        repo,
        slug,
        pr,
        identities,
        true,
        Trigger::Asked,
        Review::Always,
    )
}

/// Whether this visit must come back having reviewed as well as summarised.
///
/// The distinction exists because a review can be **asked for directly** — the panel's redraft —
/// on a pull request the yours-to-give test would say no to: one in a lane the pass does not read
/// unasked, or one nobody involved you in. Asking is its own authority, exactly as
/// [`Trigger::Asked`] is for the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Review {
    /// Review it where the review is yours to give — [`spend_a_visit`]'s `draft_due`. The
    /// background pass and the read button.
    IfYours,
    /// Review it whatever `draft_due` would have answered. Somebody pressed for one.
    Always,
}

/// **One reading, whichever half you asked for.** The summary and the review are different tasks
/// needing different mindsets, but they are never separate surfaces and they are always wanted
/// together — so they are never two analyses (the owner's framing, 2026-08-25, SKEIN-263).
///
/// Every path that spends a model call on a pull request comes through here, and the ones that
/// want a review come through with [`Review::Always`]. There is no second drafter: there used to
/// be — `draft_critique`, its own prompt, its own diff download — reached by the panel's "draft
/// again" and by the pass's draft-only door, and it is what let the summary and the review of one
/// commit come from two different readings that never had to agree about what they saw.
pub(super) fn visit(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
    force: bool,
    trigger: Trigger,
    review: Review,
) -> Summary {
    let said = spend_a_visit(repo, slug, pr, identities, force, trigger, review);
    // **A reading skein bought and could not make is written down HERE**, not only in
    // [`read_waiting`] (SKEIN-253).
    //
    // The tried-note is what stops a failure being re-bought, and it used to be written by the
    // background pass alone — so the pane's own pump, which is `Trigger::Unasked` and IS charged,
    // spent a budget unit per row per reload against a `claude` that fails instantly and for free.
    // Thirty rows and three reloads is the day's ceiling gone on zero summaries, and then every row
    // reads "today's automatic reading budget is spent (100/100)".
    //
    // Two conditions, and they are the same ones the pass already used. **`computed`**: only a
    // spent model call is worth not repeating — a diff that would not download costs one HTTP call
    // to retry, and noting it pins a bad network minute to the head sha as a permanent error row.
    // **`Unasked`**: a person pressing "read it" is saying they think it will work now, and what
    // they are told must never become a standing state. A new commit is a new key either way.
    if trigger == Trigger::Unasked && said.computed && matches!(said.depth, Depth::Unread) {
        note_tried(&repo.id, pr.number, &pr.head_sha, &said.unread_because);
    }
    said
}

/// **Every pull request skein is reading right now, and since when.**
///
/// It exists because a reading is the one thing skein does that takes most of a minute and shows
/// nothing while it runs. Reported live, twice, and the second time as the diagnosis rather than
/// the symptom: "click on reread or redraft doesn't really produce new review and summary", then
/// "even if it is doing work, I am unable to see, the fact that I am feeling that means the UX is
/// not good enough" (SKEIN-333).
///
/// **Why the server holds this and not the page.** A reading outlives the browser that asked for
/// it: it runs in a blocking task and finishes whether or not anybody is still looking. A marker
/// kept in the page is lost to a reload, a repo switch, or a second tab — so the reader who
/// reloads at 20 seconds is shown a calm row for the remaining 15 and concludes nothing happened,
/// which is the whole complaint. Held here, any page that asks is told what is in flight and when
/// it started.
///
/// **Every reading, not only pressed ones.** The owner's answer when asked whether skein's own
/// background reads should show too: yes, any read in flight shows. So this is registered inside
/// [`spend_a_visit`], the one door every model-spending path already comes through, rather than at
/// the route — which would have seen presses and missed the pump.
///
/// Keyed `repo_id#number`, the same key the page keys its rows on.
pub(super) fn reading_now(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ReadingNow>> {
    static READING: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ReadingNow>>,
    > = std::sync::OnceLock::new();
    READING.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// One reading in flight. Named for the question it answers, because [`Reading`] in this module
/// is already the diff a visit read.
#[derive(Debug, Clone, Serialize)]
pub struct ReadingNow {
    pub repo_id: String,
    pub number: u64,
    /// Wall clock at the moment the purchase began, so a page that arrives late can still say how
    /// long it has been running. Milliseconds since the epoch, which is what the page's own clock
    /// speaks (`ageNow`).
    pub started_ms: i64,
    /// Did a person press for this, or is it skein's own initiative? The page draws the two
    /// differently — a press gets the full counter, the pump a quieter marker — and neither is
    /// inferable from the row itself.
    pub asked: bool,
}

/// Registered while a reading runs, removed however it ends.
///
/// A guard rather than a pair of calls: [`spend_a_visit`] returns from a dozen places and may
/// panic inside the model call, and an entry that outlives its reading is a row that says
/// "reading…" for ever with nothing able to clear it. `Drop` is the only thing that covers every
/// exit, including the ones added later.
pub(super) struct ReadingGuard(String);

impl Drop for ReadingGuard {
    fn drop(&mut self) {
        reading_now()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

impl ReadingGuard {
    fn begin(repo_id: &str, number: u64, trigger: Trigger) -> ReadingGuard {
        let key = format!("{repo_id}#{number}");
        reading_now()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.clone(),
                ReadingNow {
                    repo_id: repo_id.to_string(),
                    number,
                    started_ms: now_ms(),
                    asked: trigger == Trigger::Asked,
                },
            );
        ReadingGuard(key)
    }
}

/// What is being read right now, for the route that answers the page.
pub fn readings() -> Vec<ReadingNow> {
    let mut out: Vec<ReadingNow> = reading_now()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect();
    // Oldest first: the one that has been running longest is the one somebody is waiting on.
    out.sort_by_key(|r| (r.started_ms, r.number));
    out
}

/// One reading that has FINISHED, as the cockpit's live stream carries it back.
///
/// **Why a reading travels on the stream rather than on the reply to the request that asked for
/// it** (SKEIN-366). The cockpit is served over HTTP/1.1, browsers cap that at six connections per
/// origin, and a reading is a model call that takes tens of seconds — so a request left open until
/// its answer is ready is a connection held for tens of seconds. `REV_ASKED_PARALLEL = 10` in
/// `src/web/index.html` means one pressed stack read alone exceeds the cap, and everything else the
/// page does — the upload, the health tick, a second stack's progress — then queues in the BROWSER
/// behind it. Measured under Playwright against a build of `358d97f`: an unrelated
/// `GET /api/health` took 12 ms with three reads in flight, 12,814 ms with six, and 34,438 ms with
/// ten.
///
/// The fix is not a smaller width — the owner's rule is that a read he asks for is not rationed,
/// and a smaller number moves the cliff rather than removing it. It is to stop spending a
/// connection per reading: the page starts one with a request that returns in milliseconds and
/// picks the answer up here, on the `EventSource` it already holds. N concurrent readings then cost
/// one connection instead of N.
///
/// Carries the whole answer, not a nudge to go and fetch it: a "reading #12 is done" event would
/// put the N requests back, one per reading, which is the thing being removed.
#[derive(Debug, Clone, Serialize)]
pub struct ReadingDone {
    pub repo_id: String,
    pub number: u64,
    /// The same body `GET /review/:n/summary` answers with, serialised once here so this module
    /// does not have to name the route's response type. `None` when the read could not be made at
    /// all, in which case `error` says why.
    pub summary: Option<serde_json::Value>,
    /// Empty on success. The sentence the failed request would have carried in its body.
    pub error: String,
    /// Which queue the answer was built from — `fresh` or `remembered`, the same distinction the
    /// `x-skein-queue` header draws on the request-shaped route. A reading delivered over the
    /// stream has no headers, so the fact travels in the payload or not at all.
    pub queue: String,
    /// When that queue was taken, RFC 3339 — `x-skein-queue-as-of`'s value.
    pub as_of: String,
}

/// Where finished readings are announced. One sender, every open board subscribed.
///
/// Separate from `crate::stream`'s fleet channel on purpose: a reading is not a fact about the
/// fleet, it does not belong in a `Tick`, and the fleet producer's own capacity is sized for a
/// board that falls behind on box rows. The route merges the two streams onto one `EventSource`.
pub(super) fn readings_said() -> &'static tokio::sync::broadcast::Sender<ReadingDone> {
    static SAID: std::sync::OnceLock<tokio::sync::broadcast::Sender<ReadingDone>> =
        std::sync::OnceLock::new();
    // Sixty-four, against a browser that reads its `EventSource` on every frame: a reading takes
    // tens of seconds to produce, so this is deep enough for every read a fleet could finish while
    // one tab was descheduled. A subscriber that still falls behind is told (`Lagged`), and the
    // page's own reconciler — an in-flight poll that finds a read no longer running — picks up
    // whatever the gap swallowed.
    SAID.get_or_init(|| tokio::sync::broadcast::channel(64).0)
}

/// Listen for readings as they finish.
pub fn subscribe_readings() -> tokio::sync::broadcast::Receiver<ReadingDone> {
    readings_said().subscribe()
}

/// Say that a reading finished. Dropped when nobody is listening, which is not an error: a reading
/// runs to completion and is written to disk whether or not a board is open to hear about it.
pub fn announce_reading(done: ReadingDone) {
    let _ = readings_said().send(done);
}

/// Milliseconds since the epoch. The page's clock speaks the same unit.
pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The visit itself. Split from [`visit`] so that every way it can come back Unread passes the one
/// place that writes the tried-note, rather than each of the dozen returns below remembering to.
pub(super) fn spend_a_visit(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
    force: bool,
    trigger: Trigger,
    review: Review,
) -> Summary {
    // Somebody pressed "read it". Whatever the model refused with last time, they are entitled to
    // find out whether it still refuses — a standing refusal must never make a button do nothing.
    //
    // A PERSON pressed, though: the background pass forces a re-read of its own accord now (the
    // draft-only door, `read_waiting`), and an unattended pass has no standing to clear a refusal
    // nobody has seen. That rule was already written down beside the old standalone drafter; this
    // is where it belongs, because this is the only door left.
    if force && trigger == Trigger::Asked {
        crate::ai::forget_refusal();
    }
    if !force {
        if let Some(hit) = cached(&repo.id, pr.number, &pr.head_sha) {
            return hit;
        }
    }
    if !summaries_enabled() {
        return Summary::unread(
            pr.number,
            &pr.head_sha,
            "summaries are switched off — turn \"Read pull requests\" back on in Settings → Boxes. Until then every PR stays at full attention.",
        );
    }
    if pr.head_sha.is_empty() {
        // Without a head commit there is nothing to key a cache on, so a summary written now could
        // outlive the code it describes. Refusing is cheaper than being subtly wrong later.
        return Summary::unread(
            pr.number,
            "",
            "GitHub did not report a head commit for this PR.",
        );
    }
    // MAY skein read this at all, unasked, before asking whether it can AFFORD to. Scope first:
    // a repo the owner excluded must not be told what the fleet's budget is doing, and a row out
    // of scope is refused on a spent day and on a fresh one alike. `computed` stays false and the
    // ledger is untouched — nothing was read, so nothing is charged. See [`unasked_scope`].
    if let Some(because) = unasked_scope(repo, pr, trigger) {
        return Summary::unread(pr.number, &pr.head_sha, &because);
    }
    // **Tried at this commit already, and it cost a model call.** Free, and BEFORE the budget
    // check, because the honest answer to "why is this row not summarised" is what the model said —
    // reporting the day's ceiling instead would hide the thing that is actually broken behind a
    // number the person cannot act on.
    //
    // Read on `Unasked` only. That is the boundary this note has always had and the reason it is
    // safe to widen it from the pass to every unattended path: "read it" goes nowhere near it, so
    // a standing failure can never make a button do nothing (`ai::forget_refusal`'s rule). And
    // `computed` stays false, so saying this costs the day nothing.
    if trigger == Trigger::Unasked {
        if let Some(why) = read_tried(&repo.id).get(&format!("{}-{}", pr.number, pr.head_sha)) {
            return Summary::unread(
                pr.number,
                &pr.head_sha,
                &format!(
                    "{why} — skein already spent a reading on this commit and will not buy \
                     another by itself. Press \"read it\" to try again."
                ),
            );
        }
    }
    // **A ROUND RUNS WHEN SOMEBODY ASKS FOR ONE, and GitHub already has a way to ask** (SKEIN-444).
    //
    // This replaces the round gate, which asked the model to judge whether re-reading was worth it.
    // That gate worked and was still the wrong shape: it spent a turn per commit to find out, its
    // answer was a judgement nobody could predict or audit, and the owner's verdict on it was
    // *"I think we were complicating what the trigger for new round should be. Let's just reuse
    // github request review thing."* A re-request is an explicit act by a person who has decided
    // they are ready — which is precisely what the gate was trying to infer, available for free and
    // never wrong.
    //
    // Only re-reads are gated. A pull request skein has never read has no earlier reading to keep,
    // so `previous` answers `None` and the round runs — the board would otherwise have nothing on
    // it. And only `Unasked`: the owner has said twice that what he asks for is not rationed, so
    // the re-read press comes through `Trigger::Asked` and never reaches this.
    //
    // Deliberately NOT cached under the new head. The trigger can flip from false to true without
    // a single byte of the diff changing — that is what re-requesting a review IS — and a cache
    // entry keyed by the commit would swallow the request that arrives after it.
    if trigger == Trigger::Unasked && !pr.my_review_requested {
        if let Some(mut kept) =
            previous(&repo.id, pr.number, &pr.head_sha).filter(|s| s.depth != Depth::Unread)
        {
            // `head_sha` goes on naming the commit this reading actually describes, so `Known::stale`
            // stays true without anyone having to remember to compare — and `computed` stays false,
            // because nothing was bought.
            kept.not_reread = format!(
                "skein has not re-read {} — nobody has asked it to. It reads again when the author \
                 re-requests your review, or when you press re-read.",
                short(&pr.head_sha)
            );
            return kept;
        }
    }
    // Enforced where the money is spent, and only there. Everything above cost nothing — a cache
    // hit was already served, a switched-off repo asked for nothing — and everything below leads
    // to a model call. The check sits before the GitHub reads too: refusing after fetching the
    // diff would spend HTTP on an answer already known. `computed` stays false — nothing was
    // asked, so the day's budget is not charged for saying so, and the button can ask tomorrow.
    let day = utc_day();
    if let Some(because) = over_budget(trigger, &day) {
        return budget_stopped_row(pr, &because);
    }
    // **From here down this reading is a purchase**, and every return above cost nothing: a cache
    // hit was served, a switch was off, the scope or the day's ceiling refused. So this is where
    // the row starts saying "reading…" and, when the guard drops, stops (SKEIN-333).
    //
    // Registered here rather than at the route so that skein's own background reads are announced
    // on the same terms as a press — the owner's answer when asked: any read in flight shows.
    let _reading = ReadingGuard::begin(&repo.id, pr.number, trigger);
    let paths = changed_paths(slug, pr.number);
    let owned = ownership(repo, identities, &paths);
    // Fetched once beside the paths and the diff, and handed to whichever ladder runs — the same
    // rule the diff follows one comment down. It is one request on a path that is about to spend a
    // model call, and it is the only statement of INTENT that exists anywhere: without it every
    // reading skein has done answered "what does this change mean" while never being told what the
    // author said it was for.
    let described = described_by_the_author(slug, pr.number);

    // ONE download, every reader. The RAW diff is fetched once and every consumer truncates its
    // own view of it: the scanner and stage 2 at [`STAGE2_BYTES`], stage 1 at [`STAGE1_BYTES`],
    // and the merged summary-and-review call at [`CRITIQUE_BYTES`]. Fetching per consumer would
    // cost up to three round trips to be told the same bytes.
    let raw = match crate::prq::pr_diff_text(slug, pr.number) {
        Ok(raw) => raw,
        Err(e) => {
            return Summary::unread(
                pr.number,
                &pr.head_sha,
                &format!("its diff could not be read: {e}"),
            )
        }
    };
    if raw.trim().is_empty() {
        return Summary::unread(
            pr.number,
            &pr.head_sha,
            "GitHub returned an empty diff for this PR.",
        );
    }
    let (full, deep_cut) = truncate_diff(&raw, STAGE2_BYTES);
    let signals = crate::contracts::scan(&full);
    // **From `raw`, not from `full`** — the whole download rather than the truncated prompt. A
    // removal past the byte budget is still a removal, and an owed check that stopped firing
    // because the diff was long would go quiet on exactly the changes that most need auditing. The
    // audit itself runs in the review box against the checkout, so it is not limited by what fits
    // in a prompt either (`docs/pr-review.md` §8, §11).
    let fired: Vec<String> = crate::owed::triggered(&raw)
        .iter()
        .map(|c| c.spelled().to_string())
        .collect();

    // Where the review is yours to give, summary and review are ONE model call over the one
    // download — the owner's decision (2026-08-24): "combine summary with critique review …
    // merging summary and critique into one model call since they are always done together". The
    // merged call runs on the critique's (stronger) model because it is writing review comments;
    // rows needing a summary only — involved, but the review is not yours — keep the cheap
    // two-stage path below. Either visit is ONE unit of the day's budget: the unit is the pull
    // request analysed, not the number of things the analysis produced.
    //
    // The lane test is [`worth_a_visit`]'s, shared with the reader — which is what puts a pull
    // request you OPENED down this path rather than the cheap summary-only one (SKEIN-265, the
    // owner's "both, in waiting, on the same call"). Split, the two halves would cost two model
    // calls and two budget units for the one row: the reader's summary here, and the second door
    // in `read_waiting` drafting the review afterwards.
    // **Is the review this reading's to give?** `Review::Always` is somebody pressing, and a
    // person may always ask. Unasked, it is the same lane test the reader uses, plus the one thing
    // that made a review skein's business in the first place: somebody asked YOU, or you wrote it.
    //
    // What is NOT asked here any more is "has one already been drafted at this head". There is no
    // draft on disk to find — the session posts its review to GitHub — and the model is told to
    // read what is already on the pull request before it says anything, which is a better answer
    // to the same question than a file skein kept beside it.
    let draft_due = match review {
        Review::Always => true,
        Review::IfYours => identities.first().is_some_and(|viewer| {
            worth_a_visit(pr)
                && (pr.author == *viewer
                    || pr.reasons.iter().any(|r| {
                        matches!(
                            r,
                            crate::prq::Reason::Reviewer
                                | crate::prq::Reason::Reviewed
                                | crate::prq::Reason::Team(_)
                        )
                    }))
        }),
    };
    let what = Visit {
        repo,
        pr,
        owned: &owned,
        signals: &signals,
        fired: &fired,
        described: &described,
    };
    // **The purchase.** One analysed pull request = one unit, taken the moment before the model is
    // asked (a call that then fails still spent — the same boundary `computed` draws below), and
    // after everything that returned above cost nothing. Both lanes take the one unit here, which
    // is why the reservation sits above the branch rather than inside each arm: stage 2, when
    // stage 1 earns it, is the second half of the SAME unit, and a merged summary-and-review is
    // one analysis with two halves. A pull request that needed explaining must not cost double
    // what a boring one did.
    //
    // This is also where the ceiling is DECIDED. `over_budget` above turned away the rows already
    // at it before any HTTP was spent on them; this takes the unit and the decision together, so
    // three visits that all passed that earlier look cannot all take the last one. A row that
    // loses that race carries the same sentence and the same marker it would have carried up
    // there (REV-6).
    if let Some(because) = reserve_a_read(trigger, &repo.id, &day) {
        return budget_stopped_row(pr, &because);
    }
    if draft_due {
        return summarise_and_draft(what, slug, &raw);
    }
    // Stage 1, and stage 2 when stage 1 earns it. Extracted because this is now reached from TWO
    // places: here, and from the merged call when it runs out of time (`summarise_and_draft`) —
    // and both must be the same reading, not two ladders that drift apart.
    summarise_in_stages(what, &full, deep_cut)
}

/// One pull request being read, and everything established about it before a model is asked.
///
/// Six values that always travel together: [`summarise_in_stages`] and [`summarise_and_draft`] are
/// two readings of the SAME thing — one calls the other when the merged call runs out of time — so
/// a parameter either of them takes alone is a chance for the two to disagree about what was read.
/// Grouping them also takes both signatures under `clippy::too_many_arguments`, which they were
/// over for the same reason: the eight arguments were six facts and two knobs.
///
/// `Copy` because every field is a shared reference; passing it on costs nothing and reads as
/// handing over the same reading rather than a copy of it.
#[derive(Clone, Copy)]
pub(super) struct Visit<'a> {
    repo: &'a Repo,
    pr: &'a Pr,
    owned: &'a Ownership,
    signals: &'a [crate::contracts::Signal],
    /// The contract triggers this change fired, already matched.
    fired: &'a [String],
    /// What the author said the change is for — the one statement of intent that exists.
    described: &'a str,
}

/// The summary-only ladder: one cheap call over the first [`STAGE1_BYTES`], and a second, longer
/// one over [`STAGE2_BYTES`] when the first says this pull request needs explaining.
///
/// **Two callers, deliberately.** [`visit`] takes this path for a row whose review is not yours to
/// give. [`summarise_and_draft`] falls back to it when the merged summary-and-review call runs out
/// of time (SKEIN-392): the merged call is the largest thing this module asks for — the whole
/// [`CRITIQUE_BYTES`] diff, a summary AND a review with line comments in one answer — and the
/// reader who was told it was too big has nothing to do with a button that makes exactly that call
/// again. This is the reading skein gave before the two were merged, so its rules are already
/// written and already hold; what it does not produce is the review, and the tried-note the caller
/// leaves is what puts "no review — read again" on the row.
///
/// It never counts a budget unit of its own. The unit is the pull request analysed and the caller
/// counted it before the first call; a narrower second attempt at the same pull request is the
/// same unit, on the same rule that makes stage 2 free after stage 1.
pub(super) fn summarise_in_stages(what: Visit<'_>, full: &str, deep_cut: bool) -> Summary {
    let Visit {
        repo,
        pr,
        owned,
        signals,
        fired,
        described,
    } = what;
    let (diff, cut) = truncate(full, STAGE1_BYTES);
    let raw = match crate::ai::claude_oneshot_telling(
        &stage1_prompt(pr, owned, described, &diff, cut),
        review_model(None).as_deref(),
        Duration::from_secs(60),
    ) {
        Ok(raw) => raw,
        // The reason, not a disjunction. "the model call failed or timed out" was the whole of what
        // this said, for four different problems with four different fixes — and it named the
        // timeout first for a failure that came back in two seconds.
        // Asked, and could not answer. That counts as spent — a `claude` that is not logged in
        // fails instantly and free, and a budget that did not count it would ask it once per row on
        // every reload for ever.
        Err(unread) => {
            let mut said = Summary::unread(pr.number, &pr.head_sha, &unread.say());
            said.computed = true;
            return said;
        }
    };
    let Some(verdict) = parse_stage1(&raw) else {
        let mut said = Summary::unread(pr.number, &pr.head_sha, "skein read it but could not make sense of its own answer, so it is not vouching for one.");
        said.computed = true;
        return said;
    };

    // The scanner escalates and never clears. A model that read a moved default as routine is
    // overruled by the diff itself; a model that flagged something the scanner has no rule for
    // keeps its flag. There is no path here where mechanical evidence *lowers* the depth, which is
    // what makes shipping imperfect rules safe — see [`crate::contracts`].
    let mut flags = verdict.flags.clone();
    for s in signals {
        if !flags.contains(&s.kind) {
            flags.push(s.kind.clone());
        }
    }
    let expand = verdict.expand || !signals.is_empty();

    let (yours, others) = owned.split();
    let mut summary = Summary {
        number: pr.number,
        head_sha: pr.head_sha.clone(),
        depth: if expand { Depth::Expanded } else { Depth::Line },
        // Reached only by having run the model.
        computed: true,
        budget_stopped: false,
        // The two-stage path has no second turn — `sweep` is called from the merged path alone —
        // so nothing has accounted for what this pass covered and it says so.
        swept: false,
        // And nothing asked whether the findings block, for the same reason. `None` is "nobody
        // looked", which is what the engine must read here rather than "nothing blocks".
        findings_block: None,
        line: verdict.line.clone(),
        detail: String::new(),
        flags,
        signals: signals.to_vec(),
        // `Some`, always, on a reading that ran: this build computed the answer, and an
        // empty list means "nothing fired" rather than "nobody looked". See the field.
        owed_triggered: Some(fired.to_vec()),
        yours,
        others,
        ownership_unknown: owned.unread_why().unwrap_or_default().to_string(),
        unread_because: String::new(),
        // The two-stage path is `Machine::Wherever` throughout (`claude_oneshot_telling`), so
        // there is no box for it to have lost. See `Summary::read_outside_box`.
        read_outside_box: String::new(),
        not_reread: String::new(),
    };

    if expand {
        // The whole diff and a longer budget, because this is the pass whose output you will
        // actually decide from. The stronger model is named here rather than in the env so a pinned
        // `$SKEIN_AI_MODEL` still overrides both stages together.
        match claude_oneshot_with(
            &stage2_prompt(
                pr,
                &verdict,
                &summary.yours,
                signals,
                described,
                full,
                deep_cut,
            ),
            review_model(Some("claude-sonnet-5")).as_deref(),
            Duration::from_secs(180),
        ) {
            Some(detail) => summary.detail = detail,
            // Stage 1 said this one deserves explaining and stage 2 could not. Falling back to the
            // one-liner would be the exact inversion of this module's rule: it would present a PR
            // flagged as needing your judgement as though it had been summarised.
            None => {
                return Summary::unread(
                    pr.number,
                    &pr.head_sha,
                    &format!(
                    "this one needs explaining ({}) and skein could not do it — read it yourself.",
                    verdict.line
                ),
                )
            }
        }
    }
    // Cached ONLY when everything that would narrow it was actually consulted. A summary
    // computed while the repo was unreadable answered "yours: none" for lack of sight, not as a
    // fact — stored, it carried that blindness for the life of this head, and a mirror that
    // recovered a minute later could never correct it (SKEIN-117, the durable half of the bug).
    // So it is served — the person still gets their summary now — and NOT written down: the next
    // computation consults whatever can be read then. That recomputation is a second unit of the
    // day's budget, and that is correct — the first unit bought a degraded answer, not this one.
    // It does not loop the background pass either: `read_waiting` writes a tried-note at this
    // head for a blind reading, and the tried-notes gate only the pass — the pane's requests go
    // nowhere near them, which is exactly the door a recovered mirror is consulted through.
    if summary.ownership_unknown.is_empty() {
        let _ = store(&repo.id, &summary);
    }
    summary
}

/// The merged visit: ONE model call over the one downloaded diff, doing the summary AND the review
/// — for rows whose review is yours to give (the owner's decision, 2026-08-24; [`spend_a_visit`]
/// has already gated on that as `draft_due` and counted the budget unit).
///
/// Only the summary comes back here. The review is posted by the session itself, from its own
/// checkout, where it has a credential; where it has none the prompt sends what it found back in
/// the brief instead, so findings never simply evaporate.
///
/// Runs on the critique's (stronger) model, because it is writing review comments that land on a
/// pull request under a person's name; the cheap model keeps the summary-only rows. Strict in the
/// module's usual direction: an answer that did not follow the format is [`Depth::Unread`] — a
/// pull request skein could not summarise vouches for nothing, the same rule as the two-stage path
/// — and every failure from the model call onwards is still counted as spent, or the next pass
/// re-buys the whole visit.
pub(super) fn summarise_and_draft(what: Visit<'_>, slug: &str, raw_diff: &str) -> Summary {
    let Visit {
        repo,
        pr,
        owned,
        signals,
        fired,
        described,
    } = what;
    let spent_unread = |why: &str| {
        let mut said = Summary::unread(pr.number, &pr.head_sha, why);
        said.computed = true;
        said
    };
    // **This reading is a conversation, not a question** (SKEIN-393), and it is THE PULL REQUEST'S
    // conversation rather than this call's (SKEIN-376). The review is asked to account for its own
    // coverage on a second turn, and a second turn needs the first one to have been named; naming
    // it after the pull request instead of after the moment means the next round resumes what this
    // one left rather than paying to be told the same change again.
    let bench = conversation_of(repo, pr.number, &pr.head_sha, &pr.base_ref);
    let (talk, at, standing) = (&bench.talk, &bench.at, &bench.standing);
    // The review's byte budget, not the summary's: the review is the reader that cannot say
    // anything about a file it never saw, so the merged call gets the most diff either consumer
    // would have been given — **and only when it is being handed one at all.** With the change
    // standing in the cwd there is nothing to truncate and no cut to disclose, because nothing is
    // sent.
    let (diff, cut) = match standing {
        Standing::Change { .. } => (String::new(), false),
        _ => truncate_diff(raw_diff, CRITIQUE_BYTES),
    };
    // Whether this round should run at all was decided in `spend_a_visit`, before the diff was
    // downloaded — by GitHub's review request, not by asking a model to judge its own worth
    // (SKEIN-444). By here, a round is happening.
    // **Whether this reading can post what it finds**, decided once and used twice — the prompt
    // is written from it, and the call is given the credential the prompt promises. Two answers
    // here would be a prompt telling a model to run `gh` in a session that has no token.
    let credential = acting_credential();
    let answered = match crate::ai::claude_in_conversation(
        &merged_prompt(MergedPrompt {
            pr,
            slug,
            owned,
            signals,
            described,
            standing,
            posting: credential.is_some(),
            diff: &diff,
            cut,
        }),
        review_model(Some("claude-sonnet-5")).as_deref(),
        // Sized by the SIZE OF THE CHANGE, not the size of the prompt. A reading that goes and
        // gets the diff itself needs at least the time a reading handed one did — more of it goes
        // on tool calls — so the budget cannot be allowed to collapse to the floor just because
        // the bytes moved out of the message. `merged_budget` clamps at [`CRITIQUE_BYTES`], which
        // is what the truncated length used to be worth, so the handed-a-diff path is unchanged.
        merged_budget(raw_diff.len()),
        talk,
        at,
        credential.as_ref(),
        bench.machine(),
    ) {
        Ok(answered) => answered,
        // **Out of time is not the end of the reading** (SKEIN-392). This call carries the whole
        // [`CRITIQUE_BYTES`] diff and is asked for a summary AND a review with line comments over
        // it; when it does not come back, the thing to do is the reading skein gave before the two
        // were merged — smaller diff, cheaper model, and no review. The reader gets a line.
        //
        // Not a second budget unit: the unit is the pull request analysed, and the caller counted
        // it before the first call. Same rule that makes stage 2 free after stage 1.
        Err(unread) if after_merged(&unread) == AfterMerged::Narrow => {
            let (full, deep_cut) = truncate_diff(raw_diff, STAGE2_BYTES);
            let mut narrower = summarise_in_stages(what, &full, deep_cut);
            if narrower.depth == Depth::Unread {
                // BOTH attempts are the answer. The shorter one's own sentence alone would send the
                // reader to look at a 60-second call, which was never the thing that was slow.
                narrower.unread_because = format!(
                    "{} A shorter read of the first {}KB did not get there either: {}",
                    unread.say(),
                    STAGE1_BYTES / 1000,
                    narrower.unread_because,
                );
            }
            return narrower;
        }
        Err(unread) => return spent_unread(&unread.say()),
    };
    // **Taken off the answer before anything else makes a model call** (SKEIN-799). `sweep` below
    // is one, and it answers for itself; this is the reading's own reason and there is exactly one
    // point at which it is in hand.
    let outside_box = answered.outside_box;
    let answer = answered.said;
    let Some((verdict, detail)) = parse_merged(&answer) else {
        return spent_unread(
            "skein read it but could not make sense of its own answer, so it is not vouching for one.",
        );
    };
    // The second turn. Only ever adds; see [`sweep`]. Still ONE budget unit — the unit is the pull
    // request analysed, the same rule that makes stage 2 free after stage 1 — so nothing is counted
    // here.
    let sweep_said = sweep(talk, at, credential.as_ref(), bench.machine());
    let swept = sweep_said.is_some();
    let findings_block = sweep_said.as_deref().and_then(findings_block);
    // The scanner escalates and never clears — same rule as the two-stage path, see there.
    let mut flags = verdict.flags.clone();
    for s in signals {
        if !flags.contains(&s.kind) {
            flags.push(s.kind.clone());
        }
    }
    let expand = verdict.expand || !signals.is_empty();
    if expand && detail.trim().is_empty() {
        // Flagged as needing your judgement and not explained: unread, never presented as
        // summarised — the same inversion the two-stage path refuses when stage 2 fails.
        return spent_unread(&format!(
            "this one needs explaining ({}) and skein could not do it — read it yourself.",
            verdict.line
        ));
    }
    let (yours, others) = owned.split();
    let summary = Summary {
        number: pr.number,
        head_sha: pr.head_sha.clone(),
        depth: if expand { Depth::Expanded } else { Depth::Line },
        computed: true,
        budget_stopped: false,
        // The second turn's outcome, carried rather than dropped: this is the one reading in the
        // tree a sweep speaks for, and `crate::prwork::facts_of_in` reads it back off this file.
        swept,
        line: verdict.line.clone(),
        detail,
        flags,
        signals: signals.to_vec(),
        // The sweep's second answer, carried rather than dropped: `None` when the sweep did not
        // run or did not say, which the engine reads as unknown and never as "nothing blocks".
        findings_block,
        // **The downgrade, said where the reader is** (SKEIN-799). Composed here because this is
        // the one place both halves are in hand: the reason came off the reading's own answer, and
        // `swept` is decided one line up. Empty whenever the reading ran where it was addressed,
        // which is the ordinary case.
        read_outside_box: outside_box
            .map(|why| crate::review::summary::outside_box_notice(&why, swept))
            .unwrap_or_default(),
        // `Some`, always, on a reading that ran: this build computed the answer, and an
        // empty list means "nothing fired" rather than "nobody looked". See the field.
        owed_triggered: Some(fired.to_vec()),
        yours,
        others,
        ownership_unknown: owned.unread_why().unwrap_or_default().to_string(),
        unread_because: String::new(),
        not_reread: String::new(),
    };
    // Same rule and same reason as the two-stage path (see the comment there): a summary whose
    // ownership could not be consulted is served but never cached, so a recovered mirror is
    // consulted on the next computation instead of being outvoted by a blind file (SKEIN-117).
    if summary.ownership_unknown.is_empty() {
        let _ = store(&repo.id, &summary);
    }
    summary
}

// ───────────────────────────── asking, and drafting ─────────────────────────────

/// Answer a question about a PR, privately. Nothing here is posted anywhere.
///
/// The separation from [`draft_comment`] is the point: most questions are for your own
/// understanding, and an answer that might be published is a different, more careful, less useful
/// answer. Posting is a second, deliberate act.
pub fn ask(repo: &Repo, slug: &str, pr: &Pr, question: &str) -> Result<String, String> {
    if !summaries_enabled() {
        return Err(
            "reading PRs is switched off — turn \"Read pull requests\" back on in Settings → Boxes."
                .into(),
        );
    }
    let question = question.trim();
    if question.is_empty() {
        return Err("ask something".into());
    }
    // **Asked in the pull request's own conversation** (SKEIN-450), not as a call of its own.
    //
    // This used to build a `context()` — a fresh 140KB diff download, the changed paths, the module
    // notes and the prior summary — on every question typed into the box. All four are already in
    // the session that read this pull request, along with the reasoning behind what skein concluded
    // about it, which is the thing a follow-up question is usually about. The owner's words:
    // "you can use the same session for that as well btw".
    //
    // Cold, the ladder opens a new conversation instead, and the model is standing in a checkout of
    // the head commit either way — so it can go and look rather than be handed a diff. The prompt
    // says so, because a model that does not know it has the code will answer from the question
    // alone.
    let bench = conversation_of(repo, pr.number, &pr.head_sha, &pr.base_ref);
    let (talk, at, standing) = (&bench.talk, &bench.at, &bench.standing);
    let prompt = format!(
        r#"A senior engineer is reviewing {slug}#{number} to understand the system, not to check the code. Answer their question at mechanism, product, architecture and user level. Do not walk through functions or lines unless they ask for that specifically.
{checkout}
This answer is PRIVATE — it goes to them, not onto the pull request. Be direct, be brief, and say plainly when you cannot tell rather than inferring.

Their question: {question}"#,
        slug = slug,
        number = pr.number,
        checkout = standing_line(standing, "answering"),
        question = question,
    );
    crate::ai::claude_in_conversation(
        &prompt,
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(180),
        talk,
        at,
        acting_credential().as_ref(),
        bench.machine(),
    )
    // The answer only. A question answered outside its box is a downgrade nobody has decided how
    // to say yet — SKEIN-799 settled the wording for the review row, and this is a different
    // surface with a different reader. Captured rather than silently dropped: see the item.
    .map(|a| a.said)
    .map_err(|unread| unread.say())
}

/// Draft a comment for a PR from your rough intent. Returns text to **edit**, never to post.
///
/// The posting is a separate call for the reason you gave: the agent drafts, you correct it, then it
/// goes. A draft that could post itself would be a different feature with a different risk.
pub fn draft_comment(repo: &Repo, slug: &str, pr: &Pr, intent: &str) -> Result<String, String> {
    if !summaries_enabled() {
        return Err("reading PRs is switched off — turn \"Read pull requests\" back on in Settings → Boxes.".into());
    }
    let intent = intent.trim();
    if intent.is_empty() {
        return Err("say roughly what you want to tell them".into());
    }
    // Same conversation, same reason as [`ask`] (SKEIN-450): the change and what skein already
    // concluded about it are in the session, and a comment drafted from rough notes is nearly
    // always about one of them. Fetched before the prompt because the prompt says whether the code
    // is there, and only this call knows.
    let bench = conversation_of(repo, pr.number, &pr.head_sha, &pr.base_ref);
    let (talk, at, standing) = (&bench.talk, &bench.at, &bench.standing);
    let prompt = format!(
        r#"Write a comment on {slug}#{number} from a reviewer's rough notes. This WILL be posted publicly on GitHub under their name once they have edited it, so write what they would write.
{checkout}
Rules:
- Say only what the notes say. Do not add praise, caveats, or requests they did not make.
- Be specific about code where being specific helps the author act; reference paths, not line numbers.
- Plain, direct, collegial. No preamble, no sign-off, no "great work overall".
- Markdown is fine. Keep it as short as the point allows.
- Output a line reading exactly COMMENT: and then the comment body, and NOTHING else — no
  explanation of what you wrote or changed, no notes to the reviewer, before or after.

Their notes: {intent}"#,
        slug = slug,
        number = pr.number,
        checkout = standing_line(standing, "writing"),
        intent = intent,
    );
    crate::ai::claude_in_conversation(
        &prompt,
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(180),
        talk,
        at,
        acting_credential().as_ref(),
        bench.machine(),
    )
    .map(|a| drafted_body(&a.said))
    .map_err(|unread| unread.say())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::testkit::*;

    // ── what skein is reading right now (SKEIN-333) ────────────────────────────────────────────
    //
    // The pane draws a row's "⟳ reading again… 22s" from this registry, so an entry that outlives
    // its reading is a row that says "reading" for ever with nothing able to clear it.

    /// Every case here keys on its OWN pull request number and never on the size of the registry:
    /// it is process-wide by design — one skein, one set of readings in flight — and `cargo test`
    /// runs these on several threads at once, so a length is a number another test is changing
    /// underneath you. Measured: this suite passed alone and failed in the full run before the
    /// assertions were keyed this way.
    #[test]
    fn a_reading_is_announced_while_it_runs_and_forgotten_however_it_ends() {
        {
            let _g = super::ReadingGuard::begin("acme", 684, super::Trigger::Asked);
            let now = super::readings();
            let mine = now
                .iter()
                .find(|r| r.repo_id == "acme" && r.number == 684)
                .expect("a reading in flight is visible while its guard lives");
            assert!(mine.asked, "a pressed read reports itself as asked");
            assert!(
                mine.started_ms > 0,
                "and says when it started, so the page can count locally"
            );
        }
        assert!(
            !super::readings().iter().any(|r| r.number == 684),
            "and is gone the moment the reading ends"
        );
    }

    /// **A reading that PANICS still clears.** This is why the registry is a `Drop` guard and not a
    /// pair of insert/remove calls: [`spend_a_visit`] returns from a dozen places and the model call
    /// inside it can unwind, and every one of those exits has to leave the row clearable.
    #[test]
    fn a_reading_that_panics_does_not_leave_the_row_reading_for_ever() {
        let fell_over = std::panic::catch_unwind(|| {
            let _g = super::ReadingGuard::begin("acme", 999, super::Trigger::Unasked);
            assert!(
                super::readings().iter().any(|r| r.number == 999),
                "in flight before the panic"
            );
            panic!("the model call fell over");
        });
        assert!(fell_over.is_err(), "the panic is not swallowed");
        assert!(
            !super::readings().iter().any(|r| r.number == 999),
            "and the row is not left saying it is being read"
        );
    }

    /// **Skein's own reads are announced on the same terms as a press** — the owner's answer when
    /// asked whether background reads should show: yes, any read in flight shows.
    #[test]
    fn a_read_skein_started_itself_is_announced_and_says_so() {
        let _g = super::ReadingGuard::begin("acme", 715, super::Trigger::Unasked);
        let mine = super::readings()
            .into_iter()
            .find(|r| r.number == 715)
            .expect("the pump's own read is in flight too");
        assert!(
            !mine.asked,
            "and is distinguishable from one somebody pressed, which the row cannot infer"
        );
    }

    /// **Where the guard is begun is the whole of its coverage**, and it cannot be reached from a
    /// test: everything past it needs GitHub and a model. So this reads the source.
    ///
    /// The placement is load-bearing twice over. Registered inside `spend_a_visit` it covers EVERY
    /// model-spending path — the pump's reads as well as the route's — where the same two lines at
    /// the route would have seen presses and missed the pump. And registered after the cheap
    /// refusals it announces only readings that are actually being bought: a cache hit, a repo out
    /// of scope or a spent day all return above it, and a spinner over one of those would be the
    /// page claiming skein was working when it had already declined.
    #[test]
    fn the_guard_is_begun_where_the_reading_becomes_a_purchase() {
        let src = include_str!("visit.rs");
        let body = src
            .split_once("fn spend_a_visit(")
            .expect("spend_a_visit is still called that")
            .1;
        let begin = body
            .find("ReadingGuard::begin")
            .expect("the visit registers itself");
        let diff = body
            .find("pr_diff_text")
            .expect("the visit still downloads the diff");
        let budget = body
            .find("over_budget")
            .expect("the visit still checks the day's budget");
        let charged = body
            .find("reserve_a_read")
            .expect("the visit still takes a unit of the day's budget");
        let cached = body
            .find("if let Some(hit) = cached(")
            .expect("the visit still serves the cache");
        assert!(
            begin < diff,
            "announced BEFORE the download, or the row is silent for the slowest part"
        );
        assert!(
            budget < begin,
            "and after the budget refusal, which costs nothing and reads nothing"
        );
        assert!(
            cached < begin,
            "and after the cache hit, which is not a reading at all"
        );
        // **Where the unit is taken is the money boundary** (REV-6). The cheap look
        // (`over_budget`) comes first so a spent day costs no HTTP; the RESERVATION comes after
        // the download, because a reading that never reached a model was never a purchase — move
        // it above `pr_diff_text` and a PR whose diff GitHub refuses starts costing a unit.
        assert!(
            diff < charged,
            "the day was charged before the diff was even fetched"
        );
    }

    /// **A model that fails every time is bought once per commit, not once per row per reload**
    /// (SKEIN-253).
    ///
    /// The tried-note is what stops a failure being re-bought, and it used to gate the background
    /// pass alone. The pane's own pump sends no `asked` marker, so its requests are
    /// `Trigger::Unasked` and ARE charged — and a `claude` that is not logged in fails instantly
    /// and for free, so thirty rows over three reloads spent the day's ceiling on zero summaries,
    /// after which every row read "today's automatic reading budget is spent".
    ///
    /// Three asks, and each one is a different rule:
    ///
    ///   * the first spends a unit and asks the model, which is right — nothing knew yet;
    ///   * the second spends NOTHING, asks nothing, and comes back carrying what the model said;
    ///   * the third is a person pressing "read it", which goes nowhere near the note. A standing
    ///     failure must never make a button do nothing (`ai::forget_refusal`'s rule), and an asked
    ///     read is un-budgeted besides.
    #[cfg(unix)]
    #[test]
    fn a_model_that_always_fails_is_not_re_bought_on_every_reload() {
        let _g = crate::testutil::env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        // A fleet root of this test's own, for the reason given in
        // `a_change_nobody_asked_you_to_look_at_again_is_not_re_read`: the model is reached through
        // a review box, and opening one makes directories under whatever root it resolves.
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));
        env.set("SKEIN_REVIEW_AI", "on");
        env.set("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        // A GitHub that serves a diff, so the visit reaches the model rather than stopping at the
        // download — the failure this is about is the model's, and a transport failure is
        // deliberately NOT noted (`computed` is the boundary).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let mut stream = stream;
                let _ = read_request(&stream);
                let body = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-fn a() {}\n+fn a() { b() }\n";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // A model that runs, answers, and answers nothing the parser knows — the shape of a
        // `claude` that is not logged in: instant, free, and a failure every single time. Counted,
        // because "it returned nothing" looks identical whether or not it was asked.
        let asked = home.join("asked");
        let claude = home.join("claude-broken.sh");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\necho x >> {}\nprintf 'no format here'\n",
                asked.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &claude,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        env.set("SKEIN_CLAUDE_BIN", &claude);

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "burn", "source": "https://github.com/acme/thing.git",
            "source_tree": "", "store": "", "read_prs": true,
        }))
        .unwrap();
        let pr = budget_pr(4, "abc");
        let calls = || {
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count()
        };

        let first = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert!(
            matches!(first.depth, Depth::Unread) && first.computed,
            "{first:?}"
        );
        assert_eq!(
            calls(),
            1,
            "the first ask must reach the model — nothing knew anything yet"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "a spent model call must be counted, however fast it failed"
        );

        // The reload. Same row, same head, and this is the one that used to cost a unit per row.
        let again = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(
            calls(),
            1,
            "the model was asked again about a commit it had already refused — one budget unit per \
             row per reload, until the day's ceiling is gone and no row has a summary"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "the reload charged the day for a reading nobody bought"
        );
        assert!(
            !again.computed,
            "a refusal served from a note must not be reported as a reading that cost something"
        );
        assert!(
            again.unread_because.contains("could not make sense")
                && again.unread_because.contains("read it"),
            "the row must carry what the MODEL said, and how to try again — not a budget number \
             that hides it: {}",
            again.unread_because
        );
        assert!(
            !again.budget_stopped,
            "a standing failure was reported as the day's budget running out"
        );

        // And a person pressing the button goes nowhere near any of it.
        let pressed = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_eq!(
            calls(),
            2,
            "\"read it\" did nothing — a standing failure must never make a button dead"
        );
        assert!(matches!(pressed.depth, Depth::Unread));
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "an asked read was charged to the automatic allowance"
        );

        crate::prq::forget_host_token();
    }

    /// The durable half of SKEIN-117: a summary computed while the repo could not be read is
    /// SERVED — the person still gets their summary now — but never cached, so a mirror that
    /// recovers is consulted on the next computation. Under the old unconditional cache write,
    /// the blind answer ("yours: none", marked with nothing) froze for the life of the head:
    /// restore `let _ = store(&repo.id, &summary);` without the `ownership_unknown` guard and
    /// two assertions below fail — `cached(..).is_none()` after the blind visit, and the
    /// recovered visit's `yours`, which the stale cache answers with the blind emptiness.
    ///
    /// The visit is a PERSON pressing "read it" (`Trigger::Asked`). It used to be `Unasked`, on a
    /// pull request whose only reason is that somebody mentioned you, in a repo with read-ahead
    /// off — which is precisely the visit SKEIN-242 says skein must refuse, and `unasked_scope`
    /// now does. What the test is about is blindness, not scope, so it presses the button.
    ///
    /// The ledger is asserted at zero for the same reason: an asked read is never counted. That
    /// not caching has a PRICE — a second visit recomputes rather than serving the blind answer —
    /// is what the recovered visit's own `yours` proves, and what an unasked visit costs for it is
    /// `one_analysed_pull_request_is_one_unit_whatever_it_produced`'s assertion, not this one's.
    #[cfg(unix)]
    #[test]
    fn a_summary_computed_blind_is_served_but_never_cached() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_REVIEW_AI", "on");
        let claude = home.join("claude-stage1.sh");
        std::fs::write(
            &claude,
            "#!/bin/sh\nprintf 'KIND: fix\\nLINE: it changes a thing.\\nEXPAND: no\\nFLAGS: none\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &claude,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        env.set("SKEIN_CLAUDE_BIN", &claude);
        env.set("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        // A GitHub that answers: the diff for #5, and its changed files — one the CODEOWNERS
        // below will attribute, one it will not.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let mut stream = stream;
                let (head, _) = read_request(&stream);
                let answer = if head.contains("/pulls/5/files") {
                    r#"[{"filename":"src/a.rs"},{"filename":"web/b.js"}]"#.to_string()
                } else if head.contains("/pulls/5 ") {
                    "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string()
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // The repo's checkout does not exist yet: the mirror cannot be made, so skein is blind.
        let checkout = home.join("checkout");
        let repo = repo_at("heals", &checkout);
        let mut pr = budget_pr(5, "sha5");
        // Mentioned, not a reviewer: the review is not yours to give, so the visit takes the
        // two-stage path and the stage-1 stub above answers it.
        pr.reasons = vec![crate::prq::Reason::Mentioned];

        let blind = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            blind.depth,
            Depth::Unread,
            "a readable diff must still be summarised while the repo is not: {}",
            blind.unread_because
        );
        assert!(
            !blind.ownership_unknown.is_empty(),
            "the summary must say ownership was not consulted, not draw nothing owned"
        );
        assert!(
            cached("heals", 5, "sha5").is_none(),
            "the blind summary was written down — a recovered mirror can never correct it"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            0,
            "a read a person pressed for was counted against the day's automatic allowance"
        );

        // The mirror recovers: the checkout appears, CODEOWNERS and all.
        checkout_fixture(&checkout);
        std::fs::create_dir_all(checkout.join(".github")).unwrap();
        std::fs::write(checkout.join(".github").join("CODEOWNERS"), "src/ @me\n").unwrap();
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-q", "-m", "owners"]);

        let healed = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert!(
            healed.ownership_unknown.is_empty(),
            "the recovered mirror was not consulted: {}",
            healed.ownership_unknown
        );
        assert_eq!(
            healed.yours,
            vec!["src/a.rs".to_string()],
            "the recomputed summary must carry what CODEOWNERS actually attributes"
        );
        assert_eq!(healed.others, 1);
        assert!(
            cached("heals", 5, "sha5").is_some(),
            "a summary computed with everything consulted must be cached as ever"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            0,
            "two reads a person pressed for ate into the automatic allowance"
        );

        crate::prq::forget_host_token();
    }

    /// **What skein reads on its own is skein's answer, not the caller's** (SKEIN-242).
    ///
    /// `read_prs` and [`worth_reading`] used to live in [`read_waiting`] alone, which made them a
    /// rule the BACKGROUND obeyed rather than a rule about the repo. `GET /review/:n/summary`
    /// without an `asked` marker went straight to [`summarise`], which consulted neither: the
    /// pane's pump therefore read pull requests in repos where read-ahead was switched off, and
    /// pull requests whose only reason is that somebody mentioned you, and CHARGED the day's
    /// ledger for both. The budget check was moved server-side because a budget the client holds
    /// is a budget any client action can refill; the scope is here for the same reason.
    ///
    /// Both refusals are asserted on the WIRE as well as in the answer — a refused visit must not
    /// even download the diff — and both are followed by the same visit with the button pressed,
    /// because a scope that also gated a person pressing "read it" would be the opposite bug.
    ///
    /// Sabotage: delete the `!repo.read_prs` arm of `unasked_scope` and
    /// "read-ahead off: an unasked visit is refused" fails; delete the `worth_reading` arm and
    /// "a mention is not a reason to read on skein's initiative" fails; return `None` for
    /// `Trigger::Asked` unconditionally-in-reverse (drop the early return) and both
    /// "…and the same visit, pressed, reads it" assertions fail.
    #[cfg(unix)]
    #[test]
    fn skein_reads_on_its_own_only_where_you_switched_reading_on() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let _asked = drafting_fixture(home);
        let day = utc_day();
        let load = || {
            crate::repos::load_repos()
                .into_iter()
                .find(|r| r.id == "crit")
                .expect("the fixture repo")
        };
        let diff_reads = |n: u64| {
            std::fs::read_to_string(home.join("hits"))
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("GET") && l.contains(&format!("/pulls/{n} ")))
                .count()
        };

        // ---- read-ahead OFF, on a pull request squarely inside the scope ----
        crate::repos::set_read_prs("crit", false).unwrap();
        let pr = budget_pr(21, "sha21");
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(
            s.depth,
            Depth::Unread,
            "read-ahead off: an unasked visit is refused"
        );
        assert!(
            s.unread_because.contains("read ahead"),
            "the refusal must name the switch that would change it: {}",
            s.unread_because
        );
        assert!(
            !s.budget_stopped,
            "a scope refusal wore the budget marker — the pane would offer tomorrow to somebody \
             whose repo will still be switched off tomorrow"
        );
        assert!(!s.computed, "saying no must not count as reading");
        assert_eq!(reads_spent(&day), 0, "a refusal moved the ledger");
        assert!(cached("crit", 21, "sha21").is_none());
        assert_eq!(
            diff_reads(21),
            0,
            "a refused visit still downloaded the diff"
        );

        // ...and the same visit, pressed, reads it — and is still free.
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            s.depth,
            Depth::Unread,
            "read-ahead off refused a person pressing read it: {}",
            s.unread_because
        );
        assert_eq!(
            reads_spent(&day),
            0,
            "a read a person pressed for ate the automatic allowance"
        );

        // ---- read-ahead ON, but the only reason is that somebody mentioned you ----
        crate::repos::set_read_prs("crit", true).unwrap();
        let mut pr = budget_pr(22, "sha22");
        pr.reasons = vec![crate::prq::Reason::Mentioned];
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(
            s.depth,
            Depth::Unread,
            "a mention is not a reason to read on skein's initiative"
        );
        assert!(
            s.unread_because.contains("not one skein reads on its own"),
            "the refusal must say whose scope this is: {}",
            s.unread_because
        );
        assert_eq!(reads_spent(&day), 0, "a mention was charged to the day");
        assert_eq!(
            diff_reads(22),
            0,
            "a refused visit still downloaded the diff"
        );

        // ...and the same visit, pressed, reads it.
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            s.depth,
            Depth::Unread,
            "a mention could not be read even by hand: {}",
            s.unread_because
        );
        assert_eq!(reads_spent(&day), 0);

        // `_asked` (a `DraftingFixture`) restores the environment from `Drop`, here at the end of
        // scope — including on a panic, which the trailing `drafting_teardown()` this replaced
        // did not survive (SKEIN-703).
    }

    // ── what makes a round run (SKEIN-444) ───────────────────────────────────────────────────
    //
    // Rounds run unasked and nothing counts them, so something has to decide when. That used to be
    // a gate: the first turn of the round asked the model whether re-reading was worth it. It
    // worked, and it was still the wrong shape — a turn spent per commit to find out, and an answer
    // nobody could predict or audit. The owner's verdict: *"I think we were complicating what the
    // trigger for new round should be. Let's just reuse github request review thing."*
    //
    // So the trigger is GitHub's review request. Driven through a real spawn, because the property
    // that matters is that NO MODEL CALL HAPPENS — which a test of the decision alone cannot see.

    /// Nobody asked, so nothing is read: the reading skein had stands, it says why, and the model
    /// is never reached. The stub records every invocation, so "no round ran" is proven by the
    /// absence of a spawn rather than by the shape of the answer.
    #[test]
    fn a_change_nobody_asked_you_to_look_at_again_is_not_re_read() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        crate::ai::forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        // **A reading that is allowed through opens a review box**, and it is a real one:
        // `visit` → `checkout::conversation_of` → `reviewbox::open_at` → `fleet::start_box` →
        // `fleet::ensure_fleet_root`, which `mkdir -p`s the fleet root and then clones into
        // `<root>/<box>/tree`. Unpinned that root is `/boxes` — the live fleet — where this
        // suite's fixtures have left `acme-pr-1`, `acme-pr-3`, `acme-pr-4` and `acme-pr-6` sitting
        // among the owner's real boxes, one of them holding a git clone. A root of this test's own
        // is what makes the box it opens go away with the test.
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));
        env.set("SKEIN_REVIEW_AI", "on");
        env.set("HOME", home);
        // **A GitHub that is not there, pinned, because the depth this test reaches must not be
        // decided by its neighbours** (SKEIN-693). The requested visit at the bottom goes as far
        // as the diff download and stops, which is the boundary the comment down there argues
        // for — but nothing made it stop: unpinned, the download went to the real api.github.com
        // and was refused by a 404, and in a `--lib review::` run it succeeded against a stub
        // `scope`'s authored test had left listening, read a whole diff and spent a model call.
        // Same assertions, both times, over two different halves of this function.
        //
        // Port 1 rather than a port this test binds and drops: nothing in the fleet runs as root,
        // so nothing can be listening there, and a connection is refused at once rather than
        // hanging. A borrowed-and-released high port is a port something else may take.
        env.set("SKEIN_GITHUB_API", "http://127.0.0.1:1");

        // What skein said last time, at the commit the branch has since moved off.
        let mut before = super::Summary::unread(7, "9c1de07abc", "");
        before.depth = super::Depth::Line;
        before.line = "adds a bounds check the caller already makes.".into();
        super::store("acme", &before).unwrap();

        // A stub that TOUCHES A FILE before it answers. The file is the assertion that matters
        // here: a trigger's job is to stop a call being made, and "no call was made" is invisible
        // in the answer — every refusal further down produces an unread summary too.
        let bin = home.join("claude");
        let ran = home.join("the-model-was-called");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\ntouch {}\nfor a in \"$@\"; do p=\"$a\"; done\ncase \"$p\" in\n\
                 \x20 *\"account for what it actually covered\"*) printf 'OVERALL: nothing new\\n';;\n\
                 \x20 *) printf 'KIND: fix\\nLINE: a fresh reading.\\nEXPAND: no\\nFLAGS: \
                 none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: nothing to flag\\n';;\n\
                 esac\n",
                ran.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        env.set("SKEIN_CLAUDE_BIN", &bin);

        // A repo skein DOES read on its own, and a pull request in scope because you reviewed it
        // once — which is exactly the row this trigger is about. `Reason::Reviewed` keeps a pull
        // request in `in_reading_scope` for ever, so before SKEIN-444 every push to it bought a
        // round.
        let mut repo = repo_at("acme", home);
        repo.read_prs = true;
        let mut pr = crate::prq::blank_pr(7, "4f2ab1cdef");
        pr.reasons = vec![crate::prq::Reason::Reviewed];
        pr.my_review_requested = false;

        let out = super::visit(
            &repo,
            "acme/x",
            &pr,
            &[],
            false,
            super::Trigger::Unasked,
            super::Review::Always,
        );
        assert!(
            !ran.exists(),
            "nobody asked for a round and skein bought one anyway — this is the entire spend the \
             trigger exists to stop, and it now happens on every commit of every pull request"
        );
        assert_eq!(
            out.line, before.line,
            "no round ran and skein threw the reading away anyway ({:?})",
            out.unread_because
        );
        assert_eq!(
            out.head_sha, "9c1de07abc",
            "the kept reading was relabelled as describing the commit it never read, so the row \
             stops reporting itself as stale and the reader cannot tell"
        );
        assert!(
            out.not_reread.contains("4f2ab1c"),
            "the row does not say WHICH commit went unread, so a deliberate choice reads as \
             neglect: {:?}",
            out.not_reread
        );
        assert!(
            !out.computed,
            "the day was charged for a reading that never happened"
        );
        // NOT filed under the unread commit. The trigger can flip from false to true without the
        // diff changing by a byte — that is what re-requesting a review IS — and a cache entry
        // keyed by the commit would swallow the request that arrives after it.
        assert!(
            super::cached("acme", 7, "4f2ab1cdef").is_none(),
            "the un-read commit was cached, so a review request arriving later is answered from \
             disk and the round the author asked for never runs"
        );

        // And the other direction, same commit, one field changed: GitHub asks, and the trigger
        // lets it through. Proven up to the DECISION and no further — what lies past it is the
        // diff download, which needs a GitHub, so this asserts that the round was not turned down
        // rather than that it completed. That boundary is the honest one: turning a requested
        // round down is this function's failure, and failing to reach github.com is not.
        pr.my_review_requested = true;
        let asked = super::visit(
            &repo,
            "acme/x",
            &pr,
            &[],
            false,
            super::Trigger::Unasked,
            super::Review::Always,
        );
        assert!(
            asked.not_reread.is_empty(),
            "the author re-requested your review and skein still refused to re-read, so the one \
             trigger there is does not work: {:?}",
            asked.not_reread
        );
        assert_ne!(
            asked.line, before.line,
            "a requested round handed back the reading it already had, so the request changed \
             nothing"
        );
        // **How far it got, asserted rather than assumed** (SKEIN-693). The two sentences above
        // are true at either depth — an unread row's line differs from the stored one exactly as a
        // fresh reading's does — so on their own they cannot tell a round that reached the wire
        // from one that read a diff and called a model. This one can: the download is refused by
        // the pin at the top, and `spend_a_visit` says so in its own words.
        assert!(
            asked
                .unread_because
                .starts_with("its diff could not be read"),
            "the requested round did not stop where this test proves things up to — it either \
             never reached the diff download or found a GitHub this test did not put there: {:?}",
            asked.unread_because
        );
        assert!(
            !ran.exists(),
            "a model call was spent past the download this test pins shut, so the round ran on \
             somebody else's GitHub and this test is measuring a neighbour"
        );

        crate::ai::forget_refusal();
    }
}
