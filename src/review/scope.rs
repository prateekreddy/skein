//! Which pull requests skein reads on its own, and in what order it spends the day on them.
//!
//! The scope is the first budget, and it is the owner's answer rather than skein's guess: pull
//! requests somebody asked you to review, and the ones you opened yourself. Being mentioned is not
//! a way in. [`worth_a_visit`] is the one doorway all three callers ask, so the lane rule cannot
//! drift between the background pass, the reading predicate and the merged call.
//!
//! [`unasked_scope`] is where that rule is ENFORCED, at the model call, next to the budget — a
//! scope the client holds is a scope any client can widen. And [`the_engine_is_still_watching`] is
//! the exception `docs/pr-review.md` §7d calls the design's own worst bug: the first verdict an
//! engine posts would otherwise take the pull request out of the engine's own reading scope for
//! ever.

use super::budget::Trigger;
use super::cache::{cached, note_tried, read_tried};
use super::summary::{summaries_enabled, Depth};
use super::visit::summarise;
use crate::prq::Pr;
use crate::repos::Repo;

/// How many pull requests one background pass reads.
///
/// Burst control, not a budget: the budgets are the SCOPE — pull requests somebody asked you to
/// review or that you opened yourself, in a repo you switched reading on for (the owner's own
/// answers, SKEIN-185 and SKEIN-265) — and the day's spend ceiling
/// (`Config::review_reads_per_day`). This only stops a queue that has been quiet all week from
/// firing thirty model calls in one minute when it finally settles.
///
/// Counted in LINES REPORTED, not in pull requests, so a row that produced a summary and a review
/// spends two of it: a pass works through two merged visits, three summary-only ones. That is the
/// behaviour that was already here — the merged call landed before this constant was re-read — and
/// it is left alone deliberately, because the thing it bounds is a burst and a stack of ten
/// therefore takes five passes rather than four.
pub(super) const READ_PER_PASS: usize = 3;

/// Read the pull requests waiting on you, in the repos you asked skein to read, with nobody
/// watching.
///
/// **Three things bound this, and two of them are the owner's answers rather than skein's own
/// guesses:**
///
/// * a repo reads nothing until `read_prs` is switched on for it, so the feature costs exactly
///   nothing on a fleet nobody has opted in;
/// * only pull requests somebody asked you to review, **and the ones you opened yourself** — the
///   owner's decision of 2026-08-25 (SKEIN-265), on a fleet where every open pull request was
///   theirs and the surface therefore did nothing at all. Being mentioned is still not a request to
///   read. The scope is the first budget; the day's spend ceiling (`Config::review_reads_per_day`,
///   enforced inside [`summarise`] and the critique drafter) is the second, and this pass obeys the
///   same counter as every button press — one budget, not two. An authored row costs ONE unit for
///   summary and review together, because [`summarise`] merges them into a single call;
/// * not a draft, and not already read at this head. **Not settled** — the hour a branch had to
///   stand still before skein would read it was removed on the owner's instruction (2026-08-24)
///   once the budget became the money guard and re-anchoring made a draft against a moving head
///   postable; `worth_reading` no longer asks, and `read_waiting`'s own test reads an unsettled
///   branch on purpose.
///
/// Returns what it read, for the server's log.
pub fn read_waiting() -> Vec<String> {
    if !summaries_enabled() {
        return Vec::new();
    }
    let mut read = Vec::new();
    // Every switched-on repo's queue, read BEFORE anything is spent. The ordering below is
    // fleet-wide and cannot be applied to a repo whose queue has not been fetched yet, so the
    // gathering and the spending are two passes rather than one (SKEIN-276).
    let mut queues = Vec::new();
    for repo in crate::repos::load_repos() {
        if !repo.read_prs {
            continue;
        }
        let Ok(queue) = crate::prq::queue(&repo, false) else {
            continue;
        };
        if !queue.ai {
            continue;
        }
        queues.push((repo, queue));
    }
    // The budget is spent from the TOP of the pane, lane by lane, in the pane's own order —
    // not in the queue's transport order (`prq::newest_first`, number descending). Two lanes
    // are read now (see [`worth_a_visit`]) and each keeps ITS own sort, mirrored field for
    // field from `src/web/index.html`:
    //
    //   * **your move** first, oldest-waiting first ([`waited_since`], from `revWaitedSince`
    //     and the needs-you lane sort). Somebody else is blocked on these, so they take the
    //     day's money before anything of yours does — widening the scope must not push a
    //     colleague's review request behind a stack you opened this morning.
    //   * **their move** second, most-recently-updated first — `index.html`'s own rule for
    //     that lane, and its reason: "for your own PRs, what moved most recently is the right
    //     question".
    //
    // What the reader spends the day's money on must be the rows the person will read first,
    // or the ceiling starves exactly the pull requests at the top of the pane.
    //
    // **Across every repo at once, not repo by repo** (SKEIN-276). The sort used to sit inside
    // the `for repo in load_repos()` loop, so the order it produced was only the order *within*
    // one repo and the outer order was the order of `repos.json`. Every pass restarts at the
    // first repo in that file, so a repo with more unread rows than the day's ceiling meant the
    // repos below it were never reached at all that day — including a colleague's review
    // request, which is the exact row this ordering exists to protect. One list, one sort.
    let mut waiting: Vec<(&Repo, &crate::prq::Queue, &Pr)> = queues
        .iter()
        .flat_map(|(repo, queue)| queue.prs.iter().map(move |pr| (repo, queue, pr)))
        .collect();
    waiting.sort_by(|(_, _, a), (_, _, b)| {
        let rank = |pr: &Pr| u8::from(!matches!(pr.lane, crate::prq::Lane::NeedsYou));
        rank(a).cmp(&rank(b)).then_with(|| {
            if matches!(a.lane, crate::prq::Lane::NeedsYou) {
                waited_since(a).cmp(waited_since(b))
            } else {
                b.updated_at.cmp(&a.updated_at)
            }
        })
    });
    for (repo, queue, pr) in waiting {
        let identities = std::iter::once(queue.viewer.clone()).collect::<Vec<_>>();
        // The one doorway both halves share. Checked first so rows in lanes nobody reads cost
        // no disk scans at all.
        if !worth_a_visit(pr) {
            continue;
        }
        let read_it = worth_reading(&repo.id, pr);
        if read_it {
            if read.len() >= READ_PER_PASS {
                return read;
            }
            // Never `force`: a reading already on disk for this head is the answer, and asking
            // again would spend a model call to be told what skein already knows. Where the
            // review is yours to give, `summarise` asks for it INSIDE this same visit, off the
            // one diff download — `spend_a_visit` decides that as `draft_due` and sends the whole
            // row to `summarise_and_draft`.
            let summary = summarise(repo, &queue.slug, pr, &identities, false, Trigger::Unasked);
            // A reading that could not be made is written down as tried, or the next pass picks
            // it straight back up — see `tried_path`. The row still says "not summarised", and
            // the button still reads it on request.
            //
            // Only when a model call was SPENT, though (`computed`) — that is the cost the note
            // exists to stop repeating. A failure before the model — the diff would not
            // download, GitHub was slow — costs one HTTP call to retry, and writing it down
            // here pinned a bad network minute to the head sha as a permanent error row. Left
            // unnoted, the next pass simply tries again. A PR that could not even be
            // summarised earns no draft either: the same diff feeds both.
            if matches!(summary.depth, Depth::Unread) {
                if summary.computed {
                    note_tried(&repo.id, pr.number, &pr.head_sha, &summary.unread_because);
                }
                continue;
            }
            // Read, but BLIND: ownership could not be consulted, so `summarise` served this
            // one without caching it (SKEIN-117 — a stored blind summary would freeze
            // "yours: none" for the life of the head). Left unnoted, this pass would re-buy
            // the same degraded answer every ten minutes for ever; noted, only the pass
            // stands down. The tried-notes gate nothing but this loop — the pane's read
            // button and per-row requests go nowhere near them — so a recovered mirror is
            // consulted the next time anyone asks, and a new commit is a new key that reads
            // afresh either way.
            if summary.computed && !summary.ownership_unknown.is_empty() {
                note_tried(
                    &repo.id,
                    pr.number,
                    &pr.head_sha,
                    &format!("read blind — {}", summary.ownership_unknown),
                );
            }
            // Only what actually cost something is reported. A cache hit is not news, and a
            // line per cache hit would bury the ones that are.
            if summary.computed {
                read.push(format!("{}: read #{}", repo.id, pr.number));
            }
        }
        // **The draft-only door is gone with the draft.** It existed for a row whose summary was
        // on disk at this head while its REVIEW was not — a state that could arise when the two
        // were separate artefacts from separate calls. They are one call now, and its review does
        // not come back to skein at all: the session posts it to GitHub. A reading that happened
        // is a review that happened, so there is no second half to open a second door for.
    }
    read
}

/// When this pull request started waiting on YOU — the your-move lane's sort key, ascending.
///
/// Mirrored, on purpose and field for field, from the pane's own `revWaitedSince` and the
/// needs-you lane sort in `src/web/index.html`: the head moving after your review is the moment
/// the PR came back to you (so `committed_at` when your review exists and is no longer current),
/// and `updated_at` is the honest fallback. Oldest first is the order on screen. Do not invent a
/// second order here — the background reader spends the day's budget by this key precisely so
/// that what gets read unasked is what sits at the top of the pane.
pub(super) fn waited_since(pr: &Pr) -> &str {
    let moved = !pr.my_review.is_empty() && pr.my_review != "none" && !pr.review_is_current;
    if moved && !pr.committed_at.is_empty() {
        &pr.committed_at
    } else {
        &pr.updated_at
    }
}

/// Did YOU open this pull request? The queue's own answer: `Reason::Author` is the row that came
/// back from the `author:<you>` search that `queue_within` runs, so this needs no viewer to ask.
///
/// [`spend_a_visit`] asks the same question from the other end, as `draft_due`
/// (`pr.author == *viewer`), because there the viewer is already in hand.
pub(super) fn yours(pr: &Pr) -> bool {
    pr.reasons.contains(&crate::prq::Reason::Author)
}

/// Is this pull request in a lane skein reads at all, unasked? **The one doorway** — the background
/// pass, the reading predicate and the merged summary-and-review call all ask it, so the lane rule
/// cannot drift between them.
///
/// Two lanes, and the second is the owner's decision of 2026-08-25 (SKEIN-265), asked as "should an
/// authored pull request be read, drafted, or both, and in which lane" and answered **"both, in
/// waiting, on the same call"**:
///
///   * [`Lane::NeedsYou`] — somebody is waiting on your review. The lane this pass was built for.
///   * [`Lane::Waiting`] — **only when you opened it**. Not the whole lane: a PR you already
///     decided on sits here too, and it has had your attention already. `build_pr` files every
///     pull request you authored here and nowhere else, which is why the fleet this was reported
///     on — ten open PRs, every one the owner's — got nothing at all from a reader that only read
///     `NeedsYou`.
///
/// [`Lane::NotReady`] and [`Lane::Archived`] stay out: not-ready is its author still changing the
/// answer, archived is you saying it will not move.
///
/// A **draft** is refused whichever lane it is in, and that is not about authorship: it is the
/// author saying the change is not finished. It has to be checked here rather than left to the
/// lane, because a draft you opened yourself is `Lane::Waiting`, not `Lane::NotReady` — the
/// yours-or-decided arm in `build_pr` outranks the draft arm below it.
///
/// [`Lane::NeedsYou`]: crate::prq::Lane::NeedsYou
/// [`Lane::Waiting`]: crate::prq::Lane::Waiting
/// [`Lane::NotReady`]: crate::prq::Lane::NotReady
/// [`Lane::Archived`]: crate::prq::Lane::Archived
pub(super) fn worth_a_visit(pr: &Pr) -> bool {
    !pr.draft
        && match pr.lane {
            crate::prq::Lane::NeedsYou => true,
            crate::prq::Lane::Waiting => yours(pr),
            crate::prq::Lane::NotReady | crate::prq::Lane::Archived => false,
        }
}

/// **The scope**: is this a pull request skein may read on its own at all?
///
/// Split out of [`worth_reading`] so the two questions it used to answer together can be asked
/// apart. This one is about the pull request and nothing else — who asked, and which lane — and
/// its answer does not change when skein reads or fails to read something. [`worth_reading`] adds
/// the thrift on top: do not buy a reading you already have, do not re-buy one that just failed.
///
/// [`unasked_scope`] needs exactly this half. Asking it the whole question refused the forced
/// re-read [`read_waiting`]'s draft door makes — the summary being on disk is the REASON that door
/// opens, so `cached(..).is_none()` there is a no rather than a yes.
///
/// Two ways in, and being **mentioned** is neither: somebody talking about you is not a request to
/// read. A team request IS a review request; the queue's own filter says so, and dropping those
/// would silently exclude exactly the pull requests the team query was added to find.
pub(super) fn in_reading_scope(pr: &Pr) -> bool {
    let asked = pr.reasons.iter().any(|r| {
        matches!(
            r,
            crate::prq::Reason::Reviewer
                | crate::prq::Reason::Reviewed
                | crate::prq::Reason::Team(_)
        )
    });
    // The lane, and the draft rule, in one place for every caller.
    (asked || yours(pr)) && worth_a_visit(pr)
}

/// Is this a pull request skein should read for you, unasked?
///
/// Two ways in, and the lane each arrives in is [`worth_a_visit`]'s question, not this one's:
///
///   * somebody **asked** you to review it, and it is your move;
///   * **you opened it**, and it is therefore in the waiting lane. The owner's decision of
///     2026-08-25 (SKEIN-265): a summary of your own pull request is what the agent that wrote it
///     actually changed, which is exactly what its author does not know. It arrives on the SAME
///     model call as the drafted review — see `summarise`'s `draft_due` — so widening the scope
///     buys both halves for one unit of the day's budget, not two.
///
/// Being **mentioned** is still not a way in: somebody talking about you is not a request to read.
pub(super) fn worth_reading(repo_id: &str, pr: &Pr) -> bool {
    in_reading_scope(pr)
        // NO settle gate, any more. It required an hour of quiet before reading — the owner's
        // decision (2026-08-24) removed it: the daily spend ceiling (`Config::review_reads_per_day`,
        // enforced in `summarise` and the critique drafter) is now THE money guard, and re-anchoring
        // by line text (SKEIN-214/215) made drafts against a moving head postable, which is what
        // made the hour obsolete. A head that moves again simply becomes a new cache key.
        // A reading of THIS head. One of an earlier commit is kept and shown as such (SKEIN-184),
        // but it is not a reason to leave the current one unread.
        && cached(repo_id, pr.number, &pr.head_sha).is_none()
        // Tried at this head and could not be read. Not for ever: a new commit is a new key, and
        // asking for it by hand goes nowhere near this.
        && !read_tried(repo_id).contains_key(&format!("{}-{}", pr.number, pr.head_sha))
}

/// May skein read this pull request **on its own**? The scope guard, enforced at the model call —
/// the same place, and for the same reason, as [`over_budget`].
///
/// The whole rule, in one sentence: **if you pressed it, it is free and unconditional; if skein
/// decided to read it, that happens only in a repo you switched read-ahead on for, only within
/// [`worth_reading`]'s scope, and it is counted against the day.** There is no third case — the
/// pane's own pump and the ten-minute pass are both "skein decided", and both arrive here as
/// [`Trigger::Unasked`].
///
/// It used to live in [`read_waiting`] alone (SKEIN-242), which made it a rule the *background*
/// obeyed rather than a rule about the repo: `GET /review/:n/summary` with no `asked` marker read
/// anything the caller named — a repo with read-ahead switched off, a pull request whose only
/// reason is that somebody mentioned you — and charged the day's ledger for it. The budget check
/// was moved server-side because "a budget the client holds is a budget any client action can
/// refill" — the module note in `src/review/budget.rs`, above `reserve_a_read`. The same
/// argument is what puts the SCOPE here, because a scope the client holds is a scope any client
/// can widen.
///
/// [`read_waiting`] still asks [`worth_reading`] itself, before spending a GitHub call on a row
/// this would refuse. That is a shortcut, not a second rule: this is the doorway.
pub(super) fn unasked_scope(repo: &Repo, pr: &Pr, trigger: Trigger) -> Option<String> {
    // A person pressing a button is not skein's initiative, so no scope applies — the same
    // boundary `over_budget` draws, and the owner's own rule for it: "Limit is only for automatic
    // stuff, manually I can invoke as many as I want."
    if trigger == Trigger::Asked {
        return None;
    }
    if !repo.read_prs {
        return Some(
            "skein does not read this repo on its own — press \"read it\" to read this one now, \
             or turn on \"read ahead\" for the repo to have skein read what is waiting on you and \
             what you opened."
                .into(),
        );
    }
    // The SCOPE half of `worth_reading` and not its thrift half: "skein already has this reading"
    // is a reason not to buy another, not a reason this pull request is out of bounds — and the
    // forced re-read behind `read_waiting`'s draft door exists precisely because the summary is
    // already on disk.
    // **The engine's scope is not the lane** — `docs/pr-review.md` §7d, and this is the whole of
    // the fix. `in_reading_scope` is right for a person and wrong for an engine that has
    // undertaken to keep watching; both are asked, and either one is enough.
    if !in_reading_scope(pr) && !the_engine_is_still_watching(repo, pr) {
        return Some(
            "this is not one skein reads on its own — that is the pull requests somebody asked \
             you to review, and the ones you opened. Press \"read it\" to read this one now."
                .into(),
        );
    }
    None
}

/// **Has this repo's engine undertaken to keep watching this pull request?** — `docs/pr-review.md`
/// §7d, which that section calls the design's own worst bug.
///
/// `prq` files a pull request in `Lane::Waiting` the moment your review is a decision and nothing
/// has re-requested you, and [`worth_a_visit`] keeps a `Waiting` row in scope **only where you
/// authored it**. That rule is right for a person: you decided, it is somebody else's move. It is
/// wrong for an engine, and wrong in the one direction that matters — on a pull request somebody
/// else wrote, the first verdict the engine posts takes it out of the engine's own reading scope,
/// **permanently**. §9's *"the head moves on one you approved → re-check"* could then never fire,
/// which recreates the stale-approval hole this whole design exists to close, at the instant it
/// acts.
///
/// So the engine's scope is the lane **or an unfinished trigger this engine owns** — and "owns" is
/// the repo's own trigger set, not every event GitHub can produce. A repo with `auto_review` off
/// widens nothing, which is every repo by default.
///
/// **It costs less than it looks, and least where it is needed most.** A reading is keyed on
/// `(number, head_sha)`, so a pull request kept in scope whose head has not moved is answered from
/// the cache for nothing. That is exactly the stacked-workflow case §7d records: the fix lands on a
/// descendant branch, the head never moves, and this widening buys the pull request back into scope
/// at no cost at all.
pub(super) fn the_engine_is_still_watching(repo: &Repo, pr: &Pr) -> bool {
    if crate::repos::auto_review_stands(repo).is_some() {
        return false;
    }
    let fired = crate::workflow::woke(&triggers_read_from(pr));
    // The set governing THIS pull request (§10, "overridable per pull request"). Asked of `repos`
    // rather than read off `repo.auto_review_on` directly, so the engine's scope and the engine's
    // refusal cannot come to disagree about which words apply — `prwork` asks the same function.
    crate::repos::triggers_for(repo, pr.number)
        .iter()
        .filter_map(|word| crate::workflow::read_wake(word))
        .any(|wanted| fired.contains(&wanted))
}

/// The five facts a trigger reads, taken off a [`Pr`] — and **only those five**.
///
/// `workflow::woke` takes a whole `workflow::Facts`, which is `prwork`'s to build: it is the
/// adapter, it holds the corrections, and nothing may depend on it because it is the module that
/// merges pull requests. This module cannot reach it and must not, so it fills the fields the
/// triggers read and leaves the rest at their fail-closed defaults.
///
/// **That coupling cannot be expressed in the type**, so it is pinned by a test next door:
/// `workflow`'s `only_the_five_facts_a_trigger_reads_can_change_what_woke_says`. The day an arm of
/// `woke` reads a sixth field, that test fails and this function is what has to grow — rather than
/// this quietly answering from a default nobody looked anything up for, which is the hazard
/// `Facts::default()`'s own note is about.
pub(super) fn triggers_read_from(pr: &Pr) -> crate::workflow::Facts {
    crate::workflow::Facts {
        review_requested: pr.my_review_requested,
        reviews_whole: pr.reviews_whole(),
        my_review: pr.my_review.clone(),
        my_review_current: pr.review_is_current,
        checks: pr.checks.clone(),
        replied_to_me: pr.replied_to_me,
        ..Default::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::testkit::*;
    use crate::review::{budget::*, cache::*, checkout::*, summary::*};

    /// What skein reads with nobody watching, and — mostly — what it does not.
    ///
    /// The two limits are the owner's own, and they are limits of SCOPE rather than of number:
    ///
    /// * "only ones where I mark the automatic reading enabled" — a repo reads nothing until it is
    ///   switched on, so the ordinary state of this feature on a fleet is that it costs nothing;
    /// * "read only PRs where I am reviewer" — your own pull requests do not need summarising for
    ///   you, and being mentioned is not a request to review.
    ///
    /// Which is why there is no daily quota. A number bounds the damage of reading the wrong things;
    /// a scope stops reading them. This asserts the scope, one exclusion at a time, because every
    /// one of them is a model call that would otherwise be spent while nobody is looking.
    #[cfg(unix)]
    #[test]
    fn what_it_reads_unwatched_is_what_you_were_asked_to_review() {
        let _g = crate::testutil::env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        env.set("SKEIN_FLEET_ROOT", home);
        env.set("SKEIN_REVIEW_AI", "on");
        // A `claude` that answers stage one in the format the prompt demands. The shared stub does
        // not, and an answer that does not parse is an UNREAD summary — which would make this test
        // assert the failure path while looking like it asserted the happy one.
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

        let pr = |number: u64, reason: crate::prq::Reason, lane: crate::prq::Lane| crate::prq::Pr {
            reasons: vec![reason],
            lane,
            ..crate::prq::blank_pr(number, &format!("sha{number}"))
        };
        use crate::prq::{Lane, Reason};

        // Asked to review it, personally or through a team: both are review requests, and dropping
        // the team one here would silently exclude the pull requests the team query exists to find.
        assert!(worth_reading(
            "demo",
            &pr(1, Reason::Reviewer, Lane::NeedsYou)
        ));
        assert!(worth_reading(
            "demo",
            &pr(2, Reason::Team("infra".into()), Lane::NeedsYou)
        ));

        // **Yours, in the waiting lane** — the owner's decision, SKEIN-265. `Lane::Waiting` is the
        // only lane a pull request you opened is ever in (`build_pr`), so refusing it was refusing
        // every pull request on a fleet where the owner writes them all.
        assert!(
            worth_reading("demo", &pr(3, Reason::Author, Lane::Waiting)),
            "a pull request you opened yourself is not read — on a fleet where every PR is yours, \
             that is the whole surface doing nothing"
        );
        // Mentioned in a comment is not a request to review.
        assert!(!worth_reading(
            "demo",
            &pr(4, Reason::Mentioned, Lane::NeedsYou)
        ));
        // The rest of the waiting lane stays out: somebody ELSE's pull request you have already
        // decided on has had your attention, and the widening is about authorship, not the lane.
        assert!(
            !worth_reading("demo", &pr(5, Reason::Reviewer, Lane::Waiting)),
            "a PR you already decided on was read again — the waiting lane was widened wholesale \
             instead of for the ones you wrote"
        );
        assert!(!worth_reading(
            "demo",
            &pr(6, Reason::Reviewer, Lane::Archived)
        ));
        // Set aside by hand, and yours: still out. Archived is you saying it will not move.
        assert!(
            !worth_reading("demo", &pr(10, Reason::Author, Lane::Archived)),
            "a pull request you set aside was read anyway"
        );

        // A draft is the author saying it is not finished.
        let mut draft = pr(7, Reason::Reviewer, Lane::NeedsYou);
        draft.draft = true;
        assert!(!worth_reading("demo", &draft));
        // And your OWN draft, which is the case authorship could have swallowed: `build_pr` files
        // it in `Lane::Waiting` rather than `Lane::NotReady`, so the lane alone would have let it
        // through and only the draft test keeps it out.
        let mut mine_draft = pr(11, Reason::Author, Lane::Waiting);
        mine_draft.draft = true;
        assert!(
            !worth_reading("demo", &mine_draft),
            "a draft you opened was read — a draft is you saying it is not finished, whoever wrote it"
        );

        // And one already read AT THIS HEAD is not read again — the single most expensive mistake
        // available here, since it would spend a model call every pass, for ever, on every row.
        let already = pr(9, Reason::Reviewer, Lane::NeedsYou);
        assert!(worth_reading("demo", &already));
        store(
            "demo",
            &Summary {
                // A fixture, and this is the honest value for one: nobody scanned a diff.
                owed_triggered: None,
                findings_block: None,

                swept: false,
                number: 9,
                head_sha: already.head_sha.clone(),
                depth: Depth::Line,
                line: "read".into(),
                detail: String::new(),
                flags: Vec::new(),
                signals: Vec::new(),
                yours: Vec::new(),
                others: 0,
                ownership_unknown: String::new(),
                unread_because: String::new(),
                read_outside_box: String::new(),
                not_reread: String::new(),
                computed: true,
                budget_stopped: false,
                stopped_at_box: false,
            },
        )
        .unwrap();
        assert!(
            !worth_reading("demo", &already),
            "a pull request already read at this commit would be read again, every pass"
        );

        // A repo nobody switched on reads nothing, whatever is in its queue.
        //
        // Asserted against a GitHub that ANSWERS, and both ways round. The first version of this
        // checked only that the pass read nothing with consent off — and passed with the consent
        // check deleted, because the queue could not be read at all in the fixture. A test that
        // cannot tell "declined" from "failed" is not testing consent.
        env.set("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        // While this file exists, the stub fails the diff request — a GitHub having a bad minute.
        let diff_broken = home.join("diff-broken");
        let diff_broken_flag = diff_broken.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let mut stream = stream;
                let (head, body) = read_request(&stream);
                let (status, answer) = if head.contains("/pulls/11 HTTP")
                    && diff_broken_flag.exists()
                {
                    (500u16, r#"{"message":"transient"}"#.to_string())
                } else if head.contains("/user/teams") {
                    (200, "[]".to_string())
                } else if head.contains("/user") {
                    (200, r#"{"login":"me"}"#.to_string())
                } else if body.contains("review-requested") {
                    // One pull request, waiting on your review, unread. The batched wire
                    // (SKEIN-209): q0 review-requested, q1 reviewed-by, q2 author, q3 mentions.
                    (
                        200,
                        r#"{"data":{"q0":{"nodes":[{"number":11,"title":"t","url":"u",
                       "isDraft":false,"author":{"login":"someone"},"headRefName":"feat",
                       "headRefOid":"sha11","baseRefName":"main",
                       "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                       "latestReviews":{"nodes":[]},
                       "commits":{"nodes":[{"commit":{"committedDate":"2020-01-01T00:00:00Z"}}]}}]},
                       "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                            .to_string(),
                    )
                } else if head.contains("/graphql") {
                    (200, r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string())
                } else {
                    (200, "{}".to_string())
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // A real checkout behind the fixture repo, so its mirror is readable and summaries are
        // not computed blind — a blind summary is deliberately never cached (SKEIN-117), and this
        // test's dedupe steps are about the cache and the tried-notes, not about blindness. The
        // slug still comes from `source` (`gitgate::repo_slug` reads the URL first), so the queue
        // stub is untouched by the local tree.
        let checkout = home.join("checkout");
        checkout_fixture(&checkout);
        crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/thing.git",
            "store": "",
            "read_prs": false,
        }))
        .unwrap()])
        .unwrap();
        mirror_from("demo", &checkout);
        assert!(
            read_waiting().is_empty(),
            "a repo nobody switched reading on for was read anyway"
        );

        // And with consent given, the same queue IS read — which is what makes the assertion above
        // mean "it declined" rather than "it could not".
        crate::repos::set_read_prs("demo", true).unwrap();
        let read = read_waiting();
        assert_eq!(
            read.len(),
            1,
            "the repo was switched on and nothing was read: {read:?}"
        );
        assert!(read[0].contains("#11"), "{read:?}");

        // Twice does not read twice: the second pass finds the reading already on disk for this
        // head, which is the difference between a background reader and a standing order.
        // Twice does not read twice. Whether the reading succeeded or failed, the next pass leaves
        // it alone: a success is cached at this head, and a failure is written down as tried —
        // without which a pull request whose model call fails costs one every pass, for ever, while
        // the pane shows the same "not summarised" row throughout.
        assert!(
            read_waiting().is_empty(),
            "the same pull request was read again on the next pass"
        );

        // **And a reading that FAILS is not tried again either**, which is the expensive half. A
        // failed reading is deliberately not cached — caching it would make a failure read as a
        // reading — so without a note of the attempt the pass picks it straight back up: one model
        // call every ten minutes, for ever, while the pane shows the same "not summarised" row.
        //
        // Observed by counting how often the CLI is actually run, because "it returned nothing"
        // looks identical whether or not it asked.
        let asked = home.join("asked");
        let broken = home.join("claude-broken.sh");
        std::fs::write(
            &broken,
            format!(
                "#!/bin/sh\necho x >> {}\nprintf 'no format here'\n",
                asked.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &broken,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        env.set("SKEIN_CLAUDE_BIN", &broken);
        // Forget the good reading, so #11 is due again.
        std::fs::remove_dir_all(crate::prq::review_dir("demo").join("summaries")).unwrap();

        assert!(
            read_waiting().is_empty(),
            "an unreadable answer was reported as a reading"
        );
        let after_one = std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(after_one, 1, "the reader did not ask once: {after_one}");

        read_waiting();
        let after_two = std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(
            after_two, 1,
            "a pull request that could not be read was asked about again — one model call per pass, \
             for ever"
        );

        // **A failure that never reached the model is NOT written down** — the opposite rule from
        // the one just proved, and they share a boundary: `computed`. A diff that would not
        // download costs one HTTP call to retry; noting it pinned a bad network minute to the head
        // sha as a permanent error row (found live, as every big diff "timing out" once and
        // sticking). So: break the diff endpoint, watch the pass fail WITHOUT asking the model or
        // writing a note — then heal the endpoint and watch the same head get read, no new commit
        // and no button pressed.
        let _ = std::fs::remove_dir_all(crate::prq::review_dir("demo").join("summaries"));
        let _ = std::fs::remove_file(crate::prq::review_dir("demo").join("read-tried.json"));
        std::fs::write(&diff_broken, "").unwrap();
        assert!(
            read_waiting().is_empty(),
            "a pull request whose diff would not download was reported as read"
        );
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "the model was asked about a diff that never arrived"
        );
        // The raw file, not `read_tried` — that loader also filters legacy transport notes out,
        // and asserting through it let a wrongly-written note pass as an unwritten one.
        let raw = std::fs::read_to_string(crate::prq::review_dir("demo").join("read-tried.json"))
            .unwrap_or_default();
        assert!(
            !raw.contains("11-sha11"),
            "a transport failure was pinned to the head sha — it would never retry: {raw}"
        );
        std::fs::remove_file(&diff_broken).unwrap();
        env.set("SKEIN_CLAUDE_BIN", &claude);
        let healed = read_waiting();
        assert_eq!(
            healed.len(),
            1,
            "GitHub came back and the reader did not: {healed:?}"
        );
        assert!(healed[0].contains("#11"), "{healed:?}");

        crate::prq::forget_host_token();
    }

    /// The reader opens summary and drafted review in one go: a pull request waiting on YOUR
    /// review comes out of the background pass with both stored, so expanding the row costs
    /// nothing and asks nothing. No HTTP pane involved — the pass alone must do it.
    ///
    /// And "in one go" is asserted on the wire, not on the disk: ONE diff download and ONE model
    /// call for the whole visit (the merged summary-and-review call, the owner's decision
    /// 2026-08-24) — the fixture records every request head and every `claude` invocation
    /// precisely so this cannot regress into fetch-twice or ask-twice. The fixture's commit
    /// dates are NOW, so this also proves an unsettled branch is read: the settle hour is gone.
    #[cfg(unix)]
    #[test]
    fn the_reader_drafts_the_review_where_it_is_yours_to_give() {
        let _g = crate::testutil::env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        std::env::set_var("SKEIN_FLEET_ROOT", home);
        let asked = drafting_fixture(home);

        let read = read_waiting();
        assert!(
            cached("crit", 21, "sha21").is_some(),
            "the pass did not summarise the PR waiting on you: {read:?}"
        );
        // That the review itself happened is asserted through the CALL rather than through a file:
        // it is not written down here any more, it is posted to GitHub by the session that read the
        // change. What skein can still prove is that the merged prompt is the one that ran.

        // One model call made both. The stub tags each invocation with which prompt it saw.
        let calls = std::fs::read_to_string(&asked).unwrap_or_default();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec!["merged"],
            "summary and review must be ONE merged model call, and only one"
        );
        // **And the reading swept itself** (SKEIN-393). Counted apart from the line above, because
        // these are two different facts: how many times the diff was READ, and how many times the
        // review was checked against what it covered. The first is the money; the second is the
        // recall, and it rides on the first one's context rather than buying its own.
        let swept = std::fs::read_to_string(home.join("sweeps")).unwrap_or_default();
        assert_eq!(
            swept.lines().count(),
            1,
            "the review was shown to the reader without ever being asked what it did not open — \
             which is the whole of SKEIN-393, and it fails silently: a review that skimmed three \
             of eleven files looks exactly like one that read them all"
        );
        // **And the sweep's answer survives the pass** (`docs/pr-review.md` §7c). It used to be
        // dropped on the floor — `let _ =` — so a reading that had accounted for every changed
        // file was indistinguishable on disk from one that had not, and `Cond::ReadingWhole` had
        // nothing to read. This is what `crate::prwork::facts_of_in` reads back.
        assert!(
            cached("crit", 21, "sha21").is_some_and(|s| s.swept),
            "the sweep ran and its answer was not written down, so no approval can ever be \
             reached from this reading"
        );
        // One download fed it. The REST diff endpoint for #21 is `GET …/pulls/21` — the files
        // listing (`/pulls/21/files`) is a different, cheaper question and not counted.
        let hits = std::fs::read_to_string(home.join("hits")).unwrap_or_default();
        let diff_fetches = hits
            .lines()
            .filter(|l| l.starts_with("GET") && l.contains("/pulls/21 "))
            .count();
        assert_eq!(
            diff_fetches, 1,
            "one head must cost one diff download for both outputs: {hits}"
        );

        // `asked` (a `DraftingFixture`) restores $SKEIN_HOME et al. from `Drop`, at the end of
        // this scope — including on a panic, which the trailing `drafting_teardown()` this
        // replaced did not survive.
        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **Only a sweep that answered may say the change was wholly read** (`docs/pr-review.md` §7c).
    ///
    /// The other end of `crate::prwork::facts_of_in`'s rule, and the one that decides the
    /// direction: `Summary::swept` is the only evidence of coverage in the tree, so every way the
    /// second turn can fail to happen has to land on `false`. A sweep that refuses, times out, or
    /// exits fine having printed nothing did not get to the end of a prompt that asks it to name
    /// every touched file and go back and read the ones it skimmed.
    ///
    /// This is `review.rs`'s own rule at the seam: **AI may only add scrutiny, never remove it.**
    ///
    /// **What would make this fail:** `sweep` returning `true` on `Err` — a `.is_ok()` that
    /// ignores what came back, or an `unwrap_or(true)`, or dropping the empty-answer check. Any of
    /// those turns a model that never ran into an approval nobody read for.
    #[cfg(unix)]
    #[test]
    fn only_a_sweep_that_answered_may_say_the_change_was_wholly_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let at = home.join("tree");
        std::fs::create_dir_all(&at).unwrap();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_REVIEW_AI", "on");

        let stub = |name: &str, body: &str| -> std::path::PathBuf {
            let path = home.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(
                &path,
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
            )
            .unwrap();
            path
        };

        // The counter-case first: a sweep that answers. Without it every assertion below would
        // pass against a `sweep` that has been hard-wired to `false`, which reports as safe and
        // makes an approval permanently unreachable.
        env.set(
            "SKEIN_CLAUDE_BIN",
            stub("answers.sh", "printf 'nothing new\\n'"),
        );
        assert!(
            sweep("talk", &at, None, crate::ai::Machine::Wherever).is_some(),
            "a sweep that ran and answered did not count, so no reading can ever be whole"
        );

        // Refused, crashed, out of time — everything `Unread` is made of.
        env.set("SKEIN_CLAUDE_BIN", stub("refuses.sh", "exit 1"));
        assert!(
            sweep("talk", &at, None, crate::ai::Machine::Wherever).is_none(),
            "a sweep that failed was recorded as having accounted for the change"
        );

        // Exited fine and said nothing. The prompt asks for one line either way, so this turn did
        // not reach the end of it — and an empty answer is the shape a truncated or killed turn
        // arrives in.
        env.set("SKEIN_CLAUDE_BIN", stub("silent.sh", "printf ' \\n'"));
        assert!(
            sweep("talk", &at, None, crate::ai::Machine::Wherever).is_none(),
            "a sweep that answered nothing was read as an answer"
        );
    }

    /// **The pull requests you wrote yourself are read and reviewed, on one call.** The shape this
    /// whole feature was reported on: a fleet whose only open pull requests are the owner's, where
    /// every row answered "not summarised" for ever because the reader worked `Lane::NeedsYou` and
    /// a PR you opened is never in it (`build_pr`).
    ///
    /// The owner's decision, asked as read/draft/both and in which lane: **"both, in waiting, on
    /// the same call"** (SKEIN-265). So all three halves are asserted at once, and the third is the
    /// one that costs money if it is wrong:
    ///
    ///   * the summary is on disk for the authored PR;
    ///   * the drafted review is too, at the same head;
    ///   * and it was **ONE** model call and **ONE** diff download for both — not a summary now and
    ///     a review from the second door afterwards, which is the same row for two budget units.
    ///
    /// The draft #32 is the control: yours as well, in the same lane, and refused — because a draft
    /// is the author saying it is not finished, and that has nothing to do with who wrote it.
    #[cfg(unix)]
    #[test]
    fn your_own_pull_requests_are_read_and_reviewed_in_one_call() {
        let _g = crate::testutil::env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        std::env::set_var("SKEIN_FLEET_ROOT", home);
        let asked = authored_fixture(home);

        let read = read_waiting();
        assert!(
            cached("mine", 31, "sha31").is_some(),
            "the pass read nothing on a queue of pull requests you opened — the surface that \
             reported this bug does nothing at all: {read:?}"
        );

        // Your own DRAFT is still refused, and refused before the wire: no summary, no review, and
        // no diff downloaded to decide it with. `build_pr` puts it in `Lane::Waiting` beside #31,
        // so nothing but the draft test itself is keeping it out. Asserted BEFORE the call
        // count below, because a doorway that lost its draft rule shows up there as a second model
        // call, and "two calls" is the wrong sentence for it.
        assert!(
            cached("mine", 32, "sha32").is_none(),
            "a draft you opened was read — a draft is the author saying it is not finished"
        );

        // One call, and it was the MERGED one: the stub tags each invocation with the prompt shape
        // it saw, so "merged" twice or "merged" then "critique" both fail here rather than being
        // invisible in a passing disk assertion.
        let calls = std::fs::read_to_string(&asked).unwrap_or_default();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec!["merged"],
            "summary and review of your own PR must be ONE merged model call — one budget unit, \
             not two"
        );
        let hits = std::fs::read_to_string(home.join("hits")).unwrap_or_default();
        let diff_fetches = hits
            .lines()
            .filter(|l| l.starts_with("GET") && l.contains("/pulls/31 "))
            .count();
        assert_eq!(
            diff_fetches, 1,
            "one head must cost one diff download for both outputs: {hits}"
        );

        // And nothing was downloaded to decide it with either.
        assert_eq!(
            hits.lines()
                .filter(|l| l.starts_with("GET") && l.contains("/pulls/32 "))
                .count(),
            0,
            "a draft's diff was downloaded before it was refused: {hits}"
        );

        // And not again on the next pass: the reading and the draft on disk at this head are the
        // answer. Same money rule as every other door here.
        let _ = read_waiting();
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "your own pull request was re-read on the next pass — one model call every ten \
             minutes, for ever"
        );

        // The block that stood here drove SKEIN-265's "second door" — the pass re-opening a row
        // that had a summary and no review. Both come out of one call now and the review does not
        // come back to skein at all, so there is no half to be missing and no door to open.

        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
        // **And everything `authored_fixture` set**, which this test did not put back until
        // SKEIN-693. Its `$SKEIN_GITHUB_API` outlived it, pointing at a stub thread that goes on
        // listening for the life of the process, and the next test to reach a diff download got a
        // GitHub it had never registered: `visit`'s
        // `a_change_nobody_asked_you_to_look_at_again_is_not_re_read` stopped at the wire when run
        // alone and read a whole diff through this stub in a `--lib review::` run, so which half
        // of that test's subject it covered was decided by which of the two ran first. The
        // variable is the only handle anyone has on the stub — its port is written down nowhere
        // else. It is now put out of reach by `asked`'s `Drop` (SKEIN-703), which fires here at
        // the end of scope and carries the repo id ("mine") the fixture itself was built with —
        // rather than by a hand-typed `drafting_teardown_for("mine")`, which is exactly the line
        // the SIBLING test below got wrong, tearing down "busy" for a fixture built with a
        // different repo id, because nothing tied the two calls together.
    }

    /// Two repos, a budget that reaches neither the end of the first — and the row somebody else
    /// is blocked on lives in the SECOND (SKEIN-276).
    ///
    /// The lane ordering landed with SKEIN-265 and landed *inside* the `for repo in load_repos()`
    /// loop, which made it an ordering within one repository and left the outer order as the order
    /// of `repos.json`. Every pass restarts at the top of that file, so a repo whose unread rows
    /// outlast one pass — a stack you rebased this morning does it easily — means the repos below
    /// it are never reached at all. The row that never gets read is a colleague's review request,
    /// which is the exact row the ordering exists to put first.
    ///
    /// So: `busy` is registered first and holds four pull requests you opened; `quiet` is
    /// registered second and holds one waiting on your review. `READ_PER_PASS` is 3 lines and an
    /// authored visit reports two, so the pass stops inside `busy` — and the assertion is that
    /// `quiet`'s #51 was read anyway, and read FIRST.
    #[cfg(unix)]
    #[test]
    fn a_review_request_in_the_last_repo_is_read_before_a_stack_you_opened_in_the_first() {
        let _g = crate::testutil::env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        std::env::set_var("SKEIN_FLEET_ROOT", home);
        let _fixture = two_repo_fixture(home);

        let read = read_waiting();
        assert!(
            read.iter().any(|l| l == "quiet: read #51"),
            "the review request in the last repo of the registry was never reached — the busy \
             repo above it spent the whole pass: {read:?}"
        );
        assert_eq!(
            read.first().map(String::as_str),
            Some("quiet: read #51"),
            "the pass spent its first unit on a repo's registry position rather than on the row \
             somebody is blocked on: {read:?}"
        );
        // And the ordering did not simply invert into "last repo first": the same pass goes on to
        // spend what is left on `busy`, which is where the rest of the budget belongs.
        assert!(
            read.iter().any(|l| l.starts_with("busy: read #")),
            "nothing in the busy repo was read at all — the fleet-wide order dropped a repo \
             instead of ranking it: {read:?}"
        );

        // `_fixture` restores $SKEIN_HOME et al. and invalidates BOTH "busy" and "quiet" from
        // `Drop`, at the end of this scope (SKEIN-703). This used to be a hand-typed
        // `drafting_teardown_for("busy")` that named only one of the two repo ids the fixture
        // above actually registered — "quiet" was invalidated only because a second, separate
        // line happened to do it by hand.
        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **What makes it fail:** removing `impl Drop for DraftingFixture` (or emptying its body) in
    /// `src/review/testkit.rs`. `$SKEIN_GITHUB_API` would then still answer after the panic below,
    /// and the final `assert!` here would fail instead of the deliberate one inside the closure.
    ///
    /// This is the proof SKEIN-703 exists for: a fixture torn down by a trailing
    /// `drafting_teardown()` call restores the environment when a test PASSES and leaks it when a
    /// test PANICS, because a failing `assert!` unwinds straight past the last line of the
    /// function. `DraftingFixture::drop` runs on every way out of scope, unwind included, so a
    /// panic while holding one must leave the same five variables unset as a clean return does.
    #[cfg(unix)]
    #[test]
    fn a_panic_holding_the_drafting_fixture_still_leaves_skein_github_api_unset() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;

        // The panic is caught rather than allowed to fail the test, and the hook is silenced so
        // the deliberate one does not read as a failure in the output — same shape as
        // `src/testutil.rs::a_test_that_panics_still_puts_the_environment_back`, one level up
        // from `EnvPins` to the fixture built on top of it.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _fixture = drafting_fixture(home);
            assert!(
                std::env::var("SKEIN_GITHUB_API").is_ok(),
                "the fixture never set the variable this test is about to prove Drop unsets"
            );
            panic!("deliberate: proving DraftingFixture::drop runs on unwind, not only on return");
        }));
        std::panic::set_hook(hook);
        assert!(outcome.is_err(), "the inner closure was supposed to panic");

        assert!(
            std::env::var("SKEIN_GITHUB_API").is_err(),
            "a panic while holding a DraftingFixture leaked $SKEIN_GITHUB_API into whatever test \
             this process runs next — Drop did not run on unwind"
        );
    }

    /// **§7d**: a pull request the LANE has released, and the engine has not.
    ///
    /// Somebody else's, decided by you, and the head has moved since — which is `Lane::Waiting`
    /// with `reasons: []`, so `worth_a_visit` says no and `in_reading_scope` says no. That is
    /// correct for a person: you decided, it is their move. For an engine that has undertaken to
    /// keep watching it is the design's own worst bug, because the first verdict it posts takes the
    /// pull request out of its own scope permanently and §9's "the head moves on one you approved →
    /// re-check" can never fire.
    ///
    /// **What would make this fail:** deleting the `the_engine_is_still_watching` arm from
    /// `unasked_scope` — row three below then refuses a pull request the engine is watching, which
    /// is the bug. Or dropping the `auto_review_on` filter from that function, so it widens on any
    /// event rather than the ones this repo asked for — row four catches that, and it is the half
    /// that keeps the widening bounded.
    #[test]
    fn a_verdict_the_engine_posted_does_not_take_the_pull_request_out_of_its_own_scope() {
        // `unasked_scope` asks `repos::triggers_for`, which reads the PR trigger file under
        // `config::skein_home` — refused rather than answered in a test since SKEIN-626, and
        // unpinned it read the owner's live `~/.skein`. It only passed because a neighbour in this
        // process had left `$SKEIN_HOME` set (SKEIN-646). An empty home is the fixture: the
        // triggers this test is about are the ones on the `Repo` rows below, not any on disk.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // The head moved after you approved it: `approved-commits` fired, and nothing else can —
        // nobody re-requested you, and you have decided.
        let released: Pr = serde_json::from_value(serde_json::json!({
            "number": 41, "title": "t", "author": "someone-else", "url": "u",
            "head_ref": "feat", "head_sha": "def5678", "base_ref": "main",
            "draft": false, "updated_at": "", "committed_at": "",
            "labels": [], "labels_total": 0,
            "review_decision": "", "standing_approvals": 1,
            "mergeable": true, "merge_state": "CLEAN", "checks": "passing",
            "my_review": "approved", "review_is_current": false,
            // Empty: not yours, and nobody asked you again. This is what puts it in the lane the
            // reader has released.
            "reasons": [], "lane": "waiting", "box_name": "",
        }))
        .unwrap();
        assert!(
            !super::in_reading_scope(&released),
            "the fixture must be a pull request the LANE has released, or this proves nothing"
        );

        let reading_on = crate::repos::Repo {
            id: "demo".into(),
            read_prs: true,
            ..Default::default()
        };
        let engine_on = crate::repos::Repo {
            auto_review: true,
            auto_review_on: vec!["approved-commits".into()],
            ..reading_on.clone()
        };
        let watching_something_else = crate::repos::Repo {
            auto_review_on: vec!["requested".into()],
            ..engine_on.clone()
        };

        // The switch alone, with a trigger set that WOULD match — so nothing but `auto_review`
        // can exclude it. Without this row the first one below passes for the wrong reason: its
        // default trigger set is `requested`, which has not fired here, so the trigger filter
        // excludes it whether or not the switch is asked at all. The sabotage found that.
        let set_but_never_switched_on = crate::repos::Repo {
            auto_review: false,
            auto_review_on: vec!["approved-commits".into()],
            ..reading_on.clone()
        };

        for (what, repo, in_scope) in [
            (
                "the engine is off, which is every repo by default",
                &reading_on,
                false,
            ),
            (
                "the trigger set matches and the engine was never switched on",
                &set_but_never_switched_on,
                false,
            ),
            (
                "the engine is on and this is a trigger it asked for",
                &engine_on,
                true,
            ),
            (
                "the engine is on and this is not a trigger it asked for",
                &watching_something_else,
                false,
            ),
        ] {
            let refused = super::unasked_scope(repo, &released, Trigger::Unasked);
            assert_eq!(
                refused.is_none(),
                in_scope,
                "{what}: the pull request was {}",
                match in_scope {
                    true => "refused",
                    false => "read",
                }
            );
        }

        // And a person pressing "read it" is never gated by any of this, in either direction.
        assert_eq!(
            super::unasked_scope(&reading_on, &released, Trigger::Asked),
            None
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// The budget is spent from the TOP of the PANE, lane by lane. Within **your move**, oldest-
    /// waiting first (`revWaitedSince` in index.html, mirrored by `waited_since`) — never
    /// newest-number and never most-recently-touched. Three rows there, budget for two: the two
    /// that have waited longest are read, the most recently touched is not.
    ///
    /// And a fourth row that is **yours** — read now (SKEIN-265), sitting in the waiting lane, and
    /// older than every other row — comes AFTER all of them, however long it has been there. That
    /// is the ordering half of widening the scope: a colleague blocked on your review must not end
    /// up behind a stack you opened this morning, and `waited_since` alone would have put your own
    /// pull request first.
    #[cfg(unix)]
    #[test]
    fn the_budget_is_spent_on_the_lanes_oldest_waiting_rows_first() {
        let _g = crate::testutil::env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        env.set("SKEIN_FLEET_ROOT", home);
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

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let (head, body) = read_request(&stream);
                // Three PRs waiting on your review. updatedAt is the waited-since key here (no
                // review of yours on any): #31 has waited longest, then #33, then #32 — while
                // the NUMBER order and the most-recently-touched order both put #32/#33 first.
                let node = |number: u64, updated: &str, author: &str| {
                    format!(
                        r#"{{"number":{number},"title":"t","url":"u",
                           "isDraft":false,"author":{{"login":"{author}"}},"headRefName":"feat",
                           "headRefOid":"sha{number}","baseRefName":"main",
                           "updatedAt":"{updated}","reviewDecision":"REVIEW_REQUIRED",
                           "latestReviews":{{"nodes":[]}},
                           "commits":{{"nodes":[{{"commit":{{"committedDate":"2020-01-01T00:00:00Z"}}}}]}}}}"#
                    )
                };
                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if body.contains("review-requested:") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{}]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(31, "2020-01-01T00:00:00Z", "someone"),
                        node(32, "2020-01-03T00:00:00Z", "someone"),
                        node(33, "2020-01-02T00:00:00Z", "someone"),
                        // Yours, and the oldest row in the queue by a year.
                        node(34, "2019-01-01T00:00:00Z", "me"),
                    )
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
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
        // A readable checkout behind the repo, as in `drafting_fixture` and for the same reason:
        // this test asserts WHICH rows landed in the cache, and a repo whose mirror cannot be
        // read has its summaries served without being cached (SKEIN-117).
        let checkout = home.join("checkout");
        checkout_fixture(&checkout);
        crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
            "id": "ord", "source": "https://github.com/acme/thing.git", "store": "", "read_prs": true,
        }))
        .unwrap()])
        .unwrap();
        mirror_from("ord", &checkout);
        crate::prq::invalidate("ord");
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"review_reads_per_day":2}"#,
        )
        .unwrap();

        let _ = read_waiting();
        assert!(
            cached("ord", 31, "sha31").is_some(),
            "the row that waited longest was skipped"
        );
        // Before the rest: the oldest row in the whole queue is the one YOU opened, and it is not
        // what the budget bought. Asserted here so a lost lane rank says whose row took the money.
        assert!(
            cached("ord", 34, "sha34").is_none(),
            "a pull request YOU opened took the day's budget from a colleague's review request — \
             your own rows are read with what is left, not first"
        );
        assert!(
            cached("ord", 33, "sha33").is_some(),
            "the second-longest waiting row was skipped"
        );
        assert!(
            cached("ord", 32, "sha32").is_none(),
            "the most recently touched row was read ahead of the queue's top — the budget is \
             being spent from the wrong end"
        );

        crate::prq::invalidate("ord");
        crate::prq::forget_host_token();
    }
}
