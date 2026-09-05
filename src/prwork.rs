//! Doing what a workflow decided — the half with consequences.
//!
//! [`crate::workflow`] is a vocabulary and a decision and has no way to touch anything. This is the
//! part that merges pull requests and deletes branches on its own, which is what the owner asked
//! for: *"merge automatically and delete the branch"*, reaffirmed after the risk was put to them.
//!
//! Three things make that recoverable rather than a decision nobody can see, and they exist BEFORE
//! any action can fire rather than after the first surprise.
//!
//! # The switch
//!
//! [`enabled`] is off by default and one key turns every workflow in the fleet off, reachable
//! without the cockpit. Not a per-workflow flag: the moment somebody wants this stopped, they want
//! it stopped, and hunting through several places for the one that is still on is not a thing to
//! ask of a person who has just watched something merge.
//!
//! # The record
//!
//! Every action reaches the host's audit log — the one skein does not own, through the warden, the
//! same sink a box's own lifecycle events use. With the authority on it: which workflow, which
//! step. "Skein merged #41" is a fact; "*ship-mine*, step 4, merged #41" is one somebody can act
//! on, because it says which line to change so it does not happen again.
//!
//! # No blind retries
//!
//! An action that fails because the world moved is not a transient error. A merge that 409s because
//! somebody pushed while skein was deciding must not be attempted again on the next poll: the same
//! decision was made from facts that are now provably stale, and a loop that re-tries it is a loop
//! that eventually wins the race. So a failure STOPS that pull request's workflow, in writing, with
//! the reason — and it stays stopped until a person clears it.
//!
//! That is deliberately stronger than "retry a few times". The actions here are outward-facing and
//! most are hard to undo; the cost of stopping too eagerly is that somebody presses a button, and
//! the cost of retrying too eagerly is a merge nobody asked for.

use crate::workflow::{Act, Chosen, MergeAs, Update, Workflow};
use std::path::PathBuf;

/// What a workflow sees, built from what GitHub said.
///
/// The one place the translation happens. Three things here are easy to get wrong, and the first
/// one was wrong for the whole life of the feature:
///
/// * **"has anybody approved this" and "does this satisfy the repository" are two questions, and
///   `reviewDecision` only answers the second.** They are `approved` and `review_requirement_met`,
///   and this comment used to say the opposite in as many words — *"`approved` is the REPOSITORY's
///   verdict, not yours"* — which is how one field came to stand in for both. See
///   [`the_repository_has_a_verdict_only_when_it_asks_for_one`] for what that cost.
/// * **`mergeable` stays three-valued.** GitHub says UNKNOWN for a while after every push, and
///   `workflow::holds` turns on unknown being neither mergeable nor not-mergeable — so flattening
///   it here would undo that one layer below the test that protects it.
/// * **`review_requirement_met` is three-valued too, and its third value is the opposite kind.**
///   `mergeable`'s `None` is skein not knowing yet; this one's is GitHub saying there is nothing
///   to know, because the repository requires no review. `workflow::holds` reads them
///   differently on purpose — see [`crate::workflow::Facts::review_requirement_met`].
///
/// `trunk` is [`crate::prq::Queue::trunk`] — the repository's default branch, `""` when not
/// known. `behind` keeps `mergeable`'s three values: `BEHIND` is yes, `""` and `UNKNOWN` are
/// *no answer* rather than no, and everything else GitHub says (`CLEAN`, `BLOCKED`, `DIRTY`,
/// `UNSTABLE`, …) is a head GitHub has compared with its base and not found behind.
///
/// **An unknown trunk is `None`, not `false`.** Both keep a train from shipping into what it only
/// believes is the trunk, but they are different situations and only one of them is the pull
/// request's fault: a base that is not the trunk is a stacked child and stops, a trunk skein
/// cannot see is skein's own blindness and waits.
/// [`crate::workflow::instead_of_merging_off_the_trunk`] is where that difference is spent.
///
/// **Without a repository, skein cannot see its own reading**, so this answers the reviewer's
/// reading facts as unknown. It is not a shorthand for [`facts_of_in`] with a blank: a reading is
/// filed under `(repo, number, head_sha)` and a caller that cannot name the repo genuinely does
/// not know, which is what `None` is spelled as here. A caller that CAN name it should, and every
/// caller that acts does.
/// **Test-only.** [`facts_of_in`] with no repository, which means no reading can be looked up and
/// `reading_whole` is `None` however well the pull request was actually read.
///
/// `#[cfg(test)]` rather than merely discouraged, and that is the whole point of this being its own
/// item: it was public and had exactly one production caller — the cockpit's train panel — which
/// was therefore drawing a preview from strictly weaker facts than the tick acts on. The comment at
/// that call site already forbids that class of thing in as many words, about a different field, so
/// the answer is to make the weaker call unreachable from production rather than to add a rule
/// nobody can see. A `bin` is a separate crate and cannot link a `cfg(test)` item, so this is
/// enforced by the compiler and not by review.
#[cfg(test)]
fn facts_of(pr: &crate::prq::Pr, viewer: &str, trunk: &str) -> crate::workflow::Facts {
    facts_of_in("", pr, viewer, trunk)
}

/// The same facts, for a pull request in a **named repository** — so the reviewer's reading facts
/// can be answered instead of confessed (`docs/pr-review.md` §7c, §15 step 3).
///
/// **Why the repository is a parameter and not something this looks up.** A reading lives under
/// `crate::prq::review_dir(repo_id)`, keyed by `(number, head_sha)`, and a `prq::Pr` carries no
/// repository at all — so the only two honest choices were to be handed one or to guess one from
/// `Pr::url` against the registry, and a fact guessed from a URL is the shape of mistake §7 is a
/// list of. Both call sites already hold `repo.id` one line away.
///
/// **Where the reading comes from.** [`crate::review::cached`], through
/// [`the_reading_skein_holds_at`]. That used to be a hand-copy of `review::cache_path` and a walk
/// over the JSON by field name, because `docs/modules.toml` did not allow `prwork -> review` — a
/// boundary that was worth its cost while this module only merged pull requests. `Act::Read` ended
/// it: the reviewer's first act IS a reading, so the edge exists now and the copy was left standing
/// for no reason but history. It is still pinned by a test that writes a `review::Summary` at the
/// path and requires this lookup to find it, so the day the filename moves the gate says so rather
/// than the engine going quietly blind.
pub fn facts_of_in(
    repo_id: &str,
    pr: &crate::prq::Pr,
    viewer: &str,
    trunk: &str,
) -> crate::workflow::Facts {
    let refused = pr.review_decision == "CHANGES_REQUESTED";
    // **Has anybody approved it?** (SKEIN-356) `prq::Pr::standing_approvals` counts the approvals
    // GitHub still holds against the head that is there now, from any reviewer — which is the
    // question, and which neither field this used to read can answer. `review_decision` is the
    // REPOSITORY's verdict and is empty wherever review is social; `my_review` is yours alone, so
    // a third party's approval on a repository that requires no review was invisible and the merge
    // train sat on approved work. On a repo where the owner is the author and somebody else
    // reviews, that is the ordinary case rather than an edge.
    //
    // `None` is a queue remembered by a skein from before the count existed. It falls back to the
    // one approval that queue could name — yours, against the head it was left on — so an old
    // queue on disk decides exactly what it decided before, and nothing acts on a field that was
    // never written.
    let somebody_approved = match pr.standing_approvals {
        Some(n) => n > 0,
        None => pr.my_review == "approved" && pr.review_is_current,
    };
    // One lookup, used by two fields below, so they cannot disagree about whether a reading
    // exists — `reading_whole: Some(true)` beside `reading_sha: None` would be skein saying a
    // reading it does not have covered the whole change.
    let read = the_reading_skein_holds_at(repo_id, pr.number, &pr.head_sha);
    crate::workflow::Facts {
        // Two sources, because GitHub gives no single field for "has anybody approved this".
        // `APPROVED` proves an approval exists even where skein cannot see whose — branch
        // protection folds in CODEOWNERS and a required-approvals count, and the reviewers behind
        // it may be outside anything the queue lists; the count beside it is the approvals skein
        // can see for itself, which is what a repository asking for no review leaves. A refusal
        // outranks both — see `workflow::Facts::approved`.
        approved: !refused && (pr.review_decision == "APPROVED" || somebody_approved),
        changes_requested: refused,
        review_requirement_met: the_repository_has_a_verdict_only_when_it_asks_for_one(
            &pr.review_decision,
        ),
        labels: pr.labels.clone(),
        // Carried beside the names, never inferred from them: `labels` being shorter than
        // `LABELS_FETCHED` is not evidence of completeness on its own, and `prq::Pr::labels_whole`
        // is the one place that rule lives — the same one the queue's blind spot is written from,
        // so what a person is told and what the train acts on cannot drift (SKEIN-373).
        labels_whole: pr.labels_whole(),
        checks: pr.checks.clone(),
        mergeable: pr.mergeable,
        draft: pr.draft,
        mine: !viewer.is_empty() && pr.author.eq_ignore_ascii_case(viewer),
        behind: match pr.merge_state.as_str() {
            "BEHIND" => Some(true),
            "" | "UNKNOWN" => None,
            _ => Some(false),
        },
        base_is_trunk: match trunk.is_empty() {
            true => None,
            false => Some(pr.base_ref == trunk),
        },
        // ---- the reviewer's facts (`docs/pr-review.md` §7) ----------------------------------
        //
        // **This is where a lying source is corrected**, and the reason all four of §7's rules are
        // rules about fields: a step vocabulary cannot fix a field that answers the wrong
        // question, and re-deriving one every poll gives the wrong answer more often and with more
        // confidence. Each field's own doc on `crate::workflow::Facts` carries the argument.
        //
        // **§7a.** The reviewer's question is `my_review` with `review_is_current`, never
        // `approved` above. That field is the pull request's answer — `reviewDecision` plus the
        // standing approvals of any reviewer — and reading it for "what did I say" is SKEIN-339
        // under a new word.
        review_requested: pr.my_review_requested,
        my_review: pr.my_review.clone(),
        my_review_current: pr.review_is_current,
        // **§7b.** Carried beside the verdict and never inferred from it, exactly as
        // `labels_whole` is: `prq::Pr::reviews_whole` is the one place "the review list is short"
        // is decided, so what the queue says out loud and what the engine acts on cannot drift.
        reviews_whole: pr.reviews_whole(),
        // **§4**, the half of the sha guard a queue row can answer.
        head_sha: pr.head_sha.clone(),
        // **§7c and §4, from the one store that can answer them.** A reading is not on a queue
        // row — `review.rs` keys every reading on `(number, head_sha)` under a repository — so
        // this is a lookup at THIS commit and nowhere else. A reading of an older head is not
        // found by it, which is §4 got for free: the sha is in the filename, so the question
        // "does a reading of the code that is there now exist" is asked by opening a file.
        //
        // `swept` is the only evidence of coverage the tree holds (see `review::Summary::swept`),
        // and it is read under §7's rule that unknown is not false: this can produce `Some(true)`
        // and `None`, and nothing else. A reading that failed, timed out, would not parse, or ran
        // before there was a sweep to run is `None` — blindness, which `Act::PostApproval` waits
        // on rather than refuses. There is deliberately no path from here to `Some(false)`: that
        // would be skein stating that a pass was partial, and nothing in the store says that.
        reading_sha: read.is_some().then(|| pr.head_sha.clone()),
        reading_whole: read.unwrap_or(false).then_some(true),
        // **Answered now, and by the reading rather than by GitHub.** This was unconditionally
        // `None`, for a reason that was true: the findings are on GitHub, skein keeps no copy
        // (`docs/pr-review.md` §5), and nothing here could read them. What changed is not the
        // access — it is that the sweep is asked, in the turn that already accounts for coverage,
        // whether what it raised must block, and the answer is recorded against the sha
        // (`review::Summary::findings_block`). The same shape as `owed_triggered`: computed once
        // where the whole change was in hand, stored, read back here.
        //
        // Still `None` far more often than not, and that is correct: the two-stage path runs no
        // sweep, a sweep that did not finish said nothing, and an answer that would not parse is
        // not an answer. Every one of those is "nobody looked", which is the value that keeps
        // `Cond::FindingsBlocking` unsatisfied.
        findings_blocking: the_reading_at_that_head_blocks(repo_id, pr.number, &pr.head_sha),
        // §8. Its own lookup rather than a field off `read` above, because the two ask different
        // questions of the same file: that one is "was this commit read, and did a sweep speak for
        // it", this one is "what did its diff fire, and has anybody answered".
        checks_owed: what_this_change_still_owes(repo_id, pr.number, &pr.head_sha),
        // §10's `reply`. Carried off the row rather than recomputed: `prq` answers it where the
        // viewer's login is in scope, and a second implementation here is how the queue a person
        // reads and the engine that acts come to disagree about what a reply is.
        replied_to_me: pr.replied_to_me,
    }
}

/// **Is a check this repository owes still outstanding at this commit?** — `docs/pr-review.md` §8.
///
/// Three sets meet here and each can empty the answer: what the diff FIRED, read off the reading
/// skein holds at this head; what the repository ASKS FOR, which is `Repo::owed_checks`; and what
/// has been ANSWERED, which `owed::record` writes when an `audit` step completes.
///
/// **`None` is unknown and it is the common case at the start of a round.** No reading at this
/// head, a reading that predates the field, a reading of another commit, a repo skein cannot find
/// — every one of them lands here, and `Cond::ChecksOwed` and `Cond::ChecksSettled` both fail on
/// it, so a verdict waits. That is the direction §8 asks for: the guard exists to withhold a post
/// until a check has been made, and an unknown that let posts through would be the guard switched
/// off by an empty file.
fn what_this_change_still_owes(repo_id: &str, number: u64, head_sha: &str) -> Option<bool> {
    if repo_id.is_empty() || head_sha.is_empty() {
        return None;
    }
    let said = crate::review::cached(repo_id, number, head_sha)?;
    // The same three refusals `the_reading_skein_holds_at` makes, and for the same reasons: a
    // summary filed under this head that describes another one is not a reading of this code, and
    // `Depth::Unread` is a record of a reading that did not happen.
    if said.head_sha != head_sha || said.depth == crate::review::Depth::Unread {
        return None;
    }
    // `None` here is a reading made before this field existed. Unknown, deliberately — see the
    // field's own doc for why an empty list would have read every older reading as settled.
    let fired = crate::owed::read(&said.owed_triggered?).0;
    let repos = crate::repos::load_repos();
    let repo = repos.iter().find(|r| r.id == repo_id)?;
    let (set, _refused) = crate::owed::for_repo(repo.owed_checks.as_ref());
    let done = crate::owed::answered(repo_id, number, head_sha);
    Some(!crate::owed::outstanding(&set, &fired, &done).is_empty())
}

/// Does skein hold a reading of this pull request **at this commit**, and did a sweep speak for it?
///
/// `None` — no reading skein can see at this head. `Some(false)` — a reading, with no sweep behind
/// it. `Some(true)` — a reading whose sweep ran and accounted for every changed file.
///
/// Four things are refused, and each one is a way this could have lied:
///
/// * **another commit.** The head is in the filename AND checked inside the file, so a summary
///   copied, renamed or written by an older skein against a different head cannot answer for the
///   one that is there now (`docs/pr-review.md` §4).
/// * **a reading that failed.** `Depth::Unread` is skein saying it did not read this — "never a
///   judgement about the PR, always about skein" — so it is not a reading, whatever it is filed
///   as. It answers `None` rather than `Some(false)`, because a failure is blindness and blindness
///   is not a verdict.
/// * **a file that will not parse.** Same answer as no file: `read_json_or_why` is not used here on
///   purpose, because there is no reader to tell — a workflow that cannot see a reading waits, and
///   the review pane is where a corrupt summary is a person's problem.
/// * **no repository named.** `""` is [`facts_of`]'s caller admitting it cannot say, and a lookup
///   in `review/` itself would find whatever a repo called nothing had.
///
/// It does not scan for readings of OTHER heads. `Cond::ReadingStale` therefore still never holds,
/// which is honest: this knows whether the current commit was read, not what came before it.
/// Did the reading skein holds at exactly this commit account for the whole change?
///
/// `None` in all three ways there is nothing to answer with: no reading at this head, a reading
/// that is [`crate::review::Depth::Unread`] — which is a record of a reading that did NOT happen —
/// or a file that will not parse. Never `Some(false)` for any of them: absence is unknown, and
/// [`crate::workflow::Cond::ReadingWhole`] is what an approval hangs on.
///
/// **`Some(false)` means one thing only**: a reading exists, and no sweep spoke for it. That is
/// still not an approval, by [`crate::review::Summary::swept`]'s own rule — a sweep that refused,
/// timed out or answered nothing lands on the same `false` — so the caller widens it back to
/// `None`. The distinction is kept here anyway because this function answers "what is on disk"
/// and the widening is a policy, and the two drift when one function does both.
/// **Did the reading at this head say its findings must block?** — the other half of §7b.
///
/// Its own lookup rather than a second return from [`the_reading_skein_holds_at`], because the two
/// answer different questions of the same file and one is allowed to be `None` while the other is
/// not: a reading can be complete and have said nothing about blocking (the two-stage path has no
/// sweep at all), and collapsing them would make coverage depend on a verdict that has nothing to
/// do with it.
///
/// Every way this is unknown returns `None`, and `None` is what
/// [`crate::workflow::Facts::findings_blocking`] is documented to mean: nobody looked.
fn the_reading_at_that_head_blocks(repo_id: &str, number: u64, head_sha: &str) -> Option<bool> {
    if repo_id.is_empty() || head_sha.is_empty() {
        return None;
    }
    let said = crate::review::cached(repo_id, number, head_sha)?;
    if matches!(said.depth, crate::review::Depth::Unread) || said.head_sha != head_sha {
        return None;
    }
    said.findings_block
}

fn the_reading_skein_holds_at(repo_id: &str, number: u64, head_sha: &str) -> Option<bool> {
    if repo_id.is_empty() || head_sha.is_empty() {
        return None;
    }
    // `review`'s own reader, since `Act::Read` made this module a caller of it (docs/modules.toml).
    // This used to be a hand-copy of `review::cache_path` plus a `serde_json::Value` walk that
    // named `depth`, `head_sha` and `swept` as strings — three field names owned by another module,
    // and drift there is silent in the worst direction: the engine simply stops finding readings
    // and every approval waits for ever with nothing to say why.
    let said = crate::review::cached(repo_id, number, head_sha)?;
    if matches!(said.depth, crate::review::Depth::Unread) {
        return None;
    }
    // The filename carries the head, so this can only fail on a file somebody moved by hand — and
    // a reading that names another commit is not a reading of this one whatever it is called.
    if said.head_sha != head_sha {
        return None;
    }
    Some(said.swept)
}

/// GitHub's `reviewDecision`, read as the answer to the question it is actually asked.
///
/// **The question is "is this branch's review requirement satisfied", not "has anybody approved
/// this".** The two read identically on a protected repository and are unrelated everywhere else,
/// and skein spent the whole life of the merge train assuming the first was the second
/// (SKEIN-339).
///
/// What that cost, measured rather than argued. On the owner's own live queue —
/// `GET /api/repos/gadget-demo/review`, twenty-one open pull requests, 2026-08-26 —
/// `review_decision` was `""` on twenty and `CHANGES_REQUESTED` on one. `APPROVED` on **none**,
/// including the two the owner had personally approved (`my_review` was `approved` on two,
/// `commented` on seven, `none` on twelve). The repository asks for no review, so GitHub has no
/// verdict to give and says nothing — which the old `== "APPROVED"` read as *not approved*.
///
/// The documented merge train's `matches` asks for `approved`, so on that repository it claimed
/// nothing, ever. Not a failure anybody could see: `matches` gates before `steps`, so even the
/// train's own catch-all `{"when": [], "do": "wait:…"}` — the step whose whole job is to make
/// silence audible, via [`a_wait_that_will_not_end_on_its_own`] — never ran. The feature was off,
/// and the only evidence was that nothing happened.
///
/// So the mapping keeps the field's real meaning and gives it its own fact:
///
/// * `APPROVED` — a requirement exists and is met.
/// * `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, anything else GitHub coins later — a requirement
///   exists and is not met. Unknown words fail closed, which for this fact means "in the way":
///   a word skein does not recognise is not a word to merge on.
/// * `""` — **no requirement**, and therefore `None` rather than `Some(false)`. GitHub has
///   answered; the answer is that there is nothing here to satisfy.
///   [`crate::workflow::Cond::ReviewSatisfied`] holds on it for that reason, and carries the
///   argument for why in full.
fn the_repository_has_a_verdict_only_when_it_asks_for_one(review_decision: &str) -> Option<bool> {
    match review_decision {
        "APPROVED" => Some(true),
        "" => None,
        _ => Some(false),
    }
}

/// May skein act on pull requests at all?
///
/// **Off unless it is switched on.** Every other default in skein leans toward showing you more;
/// this one leans the other way, because the thing being defaulted is not a reading but a merge.
/// `$SKEIN_PR_WORKFLOWS=on|off` overrides, so it can be turned off from the command line that
/// starts the server — no cockpit, no config edit, on a fleet that is doing something you want
/// stopped now.
pub fn enabled() -> bool {
    match std::env::var("SKEIN_PR_WORKFLOWS").ok().as_deref() {
        Some("on" | "1" | "true" | "yes") => true,
        Some("off" | "0" | "false" | "no") => false,
        _ => crate::config::load_config().pr_workflows,
    }
}

/// What happened when skein tried to take a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It happened. The string is what the audit and the row say.
    Did(String),
    /// Nothing to do — the step was `wait`, which is an answer and not an absence.
    Waited(String),
    /// The workflow has stopped on this pull request, and will not act again until a person clears
    /// it. Either because a step said `flag`, or because an action failed.
    Stopped(String),
}

/// Which workflow a pull request carries, and how it came to carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carries {
    /// Somebody chose this one on the row. Outranks every rule, in both directions.
    ///
    /// Outranks every rule about *which* workflow — never a workflow's own conditions. Where those
    /// do not hold, this is [`Carries::Holding`] instead (SKEIN-279).
    Assigned(String),
    /// A workflow's own `matches` claimed it.
    Matched(String),
    /// Assigned by hand, and **holding**: the workflow governs this pull request and its own
    /// `matches` do not hold yet, so it may not act (SKEIN-279).
    ///
    /// A third answer rather than a flavour of [`Carries::Assigned`], because the two are read for
    /// different things and used to be the same word. The row must still say the workflow is on it
    /// — somebody chose it, and it has not been forgotten — while nothing may act, which is
    /// [`Carries::name`] against [`Carries::acting`].
    ///
    /// **This is not a stop and not a wait.** Nothing is written down, no clock runs, and there is
    /// nothing for anybody to clear: `unmet` is recomputed from the queue every pass, so the pull
    /// request starts acting on the pass after the condition becomes true. It is also kept out of
    /// a serial train's line, which is the part that matters most — a pull request that cannot act
    /// standing at the front of a train would hold up everything behind it, which is the failure
    /// this whole design is written against.
    Holding {
        name: String,
        /// The conditions that do not hold, spelled as they are written in the file, so the
        /// sentence on the row is checkable against it. From [`crate::workflow::unmet`].
        unmet: Vec<String>,
    },
    /// Excluded by hand — the row said "no workflow", and no rule may override that.
    ///
    /// A distinct answer from [`Carries::Nothing`] and the whole reason assignment is a
    /// three-valued thing: with the tick sweeping every repo in the registry, "not this one" has to
    /// be sayable about a single pull request. Otherwise the only way to exclude one is to edit the
    /// rule for everybody.
    Excluded,
    /// No rule claims it and nobody assigned one.
    Nothing,
}

impl Carries {
    /// The workflow's name, where there is one. **What governs it**, which is what a row shows —
    /// including one that is holding, because an assignment nobody can see is one that looks lost.
    pub fn name(&self) -> Option<&str> {
        match self {
            Carries::Assigned(name) | Carries::Matched(name) | Carries::Holding { name, .. } => {
                Some(name)
            }
            Carries::Excluded | Carries::Nothing => None,
        }
    }

    /// The workflow that may act on this pull request **now**, which is a different question from
    /// [`Carries::name`] and the whole of SKEIN-279.
    ///
    /// Everything that acts, or that decides who acts next, reads this one: the sweep, and the
    /// serial line it builds. Everything that draws reads [`Carries::name`].
    pub fn acting(&self) -> Option<&str> {
        match self {
            Carries::Assigned(name) | Carries::Matched(name) => Some(name),
            Carries::Holding { .. } | Carries::Excluded | Carries::Nothing => None,
        }
    }
}

fn assign_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-assigned.json")
}

/// Read one of this module's three per-repo files, change it, and write it back — **with one
/// exclusive lock held across all three, and the write itself atomic** (SKEIN-414).
///
/// Every one of the three is a read-modify-write over a whole map: `assign` inserts one choice,
/// `stop` inserts one stop, `record` appends one line to one pull request's timeline. Two of those
/// interleaving is last-write-wins, and what the loser loses is a whole stop or a whole choice
/// rather than a field. The writers are not hypothetical and never were: the tick sweeps every repo
/// on its own thread while the cockpit's routes call `assign`, `stop` and `clear` from request
/// threads. A bare `std::fs::write` also truncates before it writes, so a crash or a kill mid-write
/// left a half-written file — the *manufacturing* end of SKEIN-359, in the module whose reading end
/// it had already fixed.
///
/// **Not [`crate::util::update_json`], which is otherwise exactly this.** That one words the
/// refusal itself, and these three files each say something different about what would be lost —
/// and the journal is the one file in the fleet that is deliberately written over when it cannot be
/// read (argued at [`record`]). So the recovery is a parameter: `unreadable` is handed the reason
/// [`crate::util::read_json_or_why`] gives and decides, in the caller's own words, whether this
/// write may go ahead at all. The lock file is [`crate::util::lock_beside`]'s, so a file guarded
/// here and a file guarded by `update_json` can never be guarded by two different locks.
///
/// `change` answers whether it changed anything: `false` writes nothing and is `Ok`, because a
/// clear on a pull request with no stop must not rewrite the file — nor fail because it could not.
fn update_file<T>(
    path: &std::path::Path,
    unreadable: impl FnOnce(String) -> Result<T, String>,
    change: impl FnOnce(&mut T) -> bool,
) -> Result<(), String>
where
    T: serde::de::DeserializeOwned + serde::Serialize + Default,
{
    let dir = path
        .parent()
        .ok_or("no directory to write into")?
        .to_path_buf();
    crate::util::with_lock(&crate::util::lock_beside(path)?, || {
        let mut current: T = match crate::util::read_json_or_why(path) {
            Ok(found) => found.unwrap_or_default(),
            Err(why) => unreadable(why)?,
        };
        if !change(&mut current) {
            return Ok(());
        }
        let body = serde_json::to_vec_pretty(&current).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // Named here rather than left to `write_atomic`, whose failures are "writing temp" and
        // "renaming into place" — true of any file in skein, and the one thing a person reading a
        // row needs is WHICH file could not be written.
        crate::util::write_atomic(path, &dir, &body).map_err(|e| format!("{}: {e}", path.display()))
    })
}

/// What skein says instead of writing over the workflow choices it could not read.
///
/// A parse error on its own says what broke and not what skein declined to do, which is the half a
/// person acts on. Same shape as `repos::unreadable_refusal` and `tracking::unreadable_refusal`, in
/// this file's own words.
fn assignment_refusal(why: String) -> String {
    format!(
        "not saving over the workflow choices skein cannot read ({why}). Saving now would replace \
         every choice made in this repo — including the pull requests somebody excluded by hand — \
         with a default nobody chose. Fix or move the file, then try again."
    )
}

/// Change the assignments, refusing over a file skein could not read.
///
/// A file nobody has written yet is no assignments, which is what every repo starts as. A file that
/// is *there* and will not parse still holds every choice somebody made, and [`assign`] says why
/// replacing those is not a thing one assignment gets to do.
fn update_assigned(
    repo_id: &str,
    change: impl FnOnce(&mut std::collections::BTreeMap<String, String>) -> bool,
) -> Result<(), String> {
    update_file(
        &assign_path(repo_id),
        |why| Err(assignment_refusal(why)),
        change,
    )
}

/// The assignments, with an unreadable file read as none.
///
/// For the readers only — the row, and the sweep deciding what governs a pull request. Both of them
/// fail toward *no workflow acting*, which is the direction a person can see and correct. The
/// writers go through [`update_assigned`], where the same misreading is what destroys the choices.
fn read_assigned(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    crate::util::read_json_or_why(&assign_path(repo_id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Put a workflow on one pull request, or take it off.
///
/// `name` empty means **excluded** — not "no opinion". Clearing the choice entirely is
/// [`unassign`], which lets the rules speak again. Three states, because with a rule sweeping every
/// repo in the fleet, "leave this one alone" is a thing somebody has to be able to say.
///
/// **Refuses over a file it could not read** (SKEIN-359). This is a read-modify-write over every
/// assignment in the repo, so an unparseable file read as empty turned assigning one pull request
/// into forgetting the choice made on all the others — and the choice that matters most is
/// `Excluded`, which is a person saying "no rule may touch this one". Losing it does not leave a
/// pull request idle; it hands it back to the sweep, which then acts on the pull request somebody
/// took out of its reach.
pub fn assign(repo_id: &str, number: u64, name: &str) -> Result<(), String> {
    update_assigned(repo_id, |all| {
        all.insert(number.to_string(), name.to_string());
        true
    })
}

/// **Did somebody choose this pull request's workflow by hand?** — §10's layer 7.
///
/// The one thing that overrides `Repo::auto_review`, and it is asked of the same file
/// [`carries`] reads so the two cannot disagree about what an assignment is.
///
/// **An empty name is not an assignment.** That is [`Carries::Excluded`] — a person saying "no rule
/// may touch this one" — and reading it as "somebody switched this on" would turn the row that
/// means *leave it alone* into the row that means *act on it whatever the repo says*, which is the
/// per-PR flag inverted on exactly the pull request somebody took out of reach.
fn chosen_by_hand(repo_id: &str, number: u64) -> bool {
    read_assigned(repo_id)
        .get(&number.to_string())
        .is_some_and(|name| !name.is_empty())
}

/// Forget any choice made on this pull request, and let the rules decide again.
///
/// Refuses on an unreadable file, for [`assign`]'s reason: forgetting one choice is not how the
/// rest are forgotten.
pub fn unassign(repo_id: &str, number: u64) -> Result<(), String> {
    // Written back even when the choice was not there, as it always was: the answer this owes its
    // caller is "there is no choice on this pull request", and that is true either way.
    update_assigned(repo_id, |all| {
        all.remove(&number.to_string());
        true
    })
}

/// What a choice made on the row means, in one place.
///
/// The mapping lived in the route and grew a bug there within an hour of being written: "let it run
/// again" sent no name, "no name" meant *forget the choice*, and so clearing a stop quietly took the
/// workflow off the pull request as well — one button doing a second thing nobody asked for. It is
/// here now because it is a rule about what an assignment IS, and a route is where rules go to be
/// untested.
///
/// - `name: Some("x")` — put x on it.
/// - `name: Some("")` — leave this one out, and let no rule claim it.
/// - `unassign` — forget the choice; the rules speak for it again.
/// - neither — **change nothing**. `clear_stop` alone is not a statement about what governs it.
///
/// A `clear_stop` that could not be written is an `Err` and stops the call there — see [`clear`].
pub fn apply(
    repo_id: &str,
    number: u64,
    name: Option<&str>,
    unassign_it: bool,
    clear_stop: bool,
) -> Result<(), String> {
    // Propagated, not swallowed: this is the half the person actually pressed, and a clear that
    // did not land leaves the pull request stopped. Before the assignment, so a failure stops here
    // rather than reporting an error over a change that did go through.
    if clear_stop {
        clear(repo_id, number)?;
    }
    match (name, unassign_it) {
        (Some(name), _) => assign(repo_id, number, name),
        (None, true) => unassign(repo_id, number),
        (None, false) => Ok(()),
    }
}

/// Which workflow governs this pull request.
///
/// The choice on the row wins over every rule. Where there is none, the first workflow whose
/// `matches` claims it does — and a workflow with no `matches` claims nothing, ever
/// ([`crate::workflow::claims`]).
///
/// **An assignment says which workflow, not that its conditions are met** (SKEIN-279). So an
/// assigned workflow whose own `matches` do not hold comes back [`Carries::Holding`]: it governs
/// the pull request and may not act on it, and both halves are said rather than one silently
/// winning. Until this, `matches` was evaluated on exactly one of the two roads to acting — assign
/// the documented merge train to a draft, or to an unapproved pull request, and it would label,
/// rebase and wait its way through the steps with every guard written in `matches` switched off.
/// The consequence that could not be undone was fixed in SKEIN-237 by moving that one guard into
/// the act; this is the general hole it left behind.
///
/// A **matched** workflow can never be holding: its `matches` were just evaluated to get here.
/// A workflow with no `matches` states no conditions, so assigning it is unconditional — which is
/// what "it only ever runs where somebody assigned it" already meant.
///
/// An assignment naming a workflow that no longer exists is [`Carries::Nothing`] rather than an
/// error: the file it named was edited, and the honest thing is to act on nothing rather than to
/// guess which of the remaining ones was meant. The row says so.
pub fn carries(
    repo_id: &str,
    number: u64,
    facts: &crate::workflow::Facts,
    flows: &[Workflow],
) -> Carries {
    match read_assigned(repo_id).get(&number.to_string()) {
        Some(name) if name.is_empty() => return Carries::Excluded,
        Some(name) => {
            return match flows.iter().find(|f| &f.name == name) {
                Some(flow) => match crate::workflow::unmet(flow, facts) {
                    unmet if unmet.is_empty() => Carries::Assigned(name.clone()),
                    unmet => Carries::Holding {
                        name: name.clone(),
                        unmet,
                    },
                },
                None => Carries::Nothing,
            }
        }
        None => {}
    }
    flows
        .iter()
        .find(|flow| crate::workflow::claims(flow, facts))
        .map(|flow| Carries::Matched(flow.name.clone()))
        .unwrap_or(Carries::Nothing)
}

/// Where a repo's stopped pull requests are written down.
///
/// Beside the review state for that repo, and on the host: this is skein's own memory of a decision
/// it made, not something a box should be able to edit.
fn stops_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-stops.json")
}

/// Why this pull request's workflow is stopped, if it is.
pub fn stopped(repo_id: &str, number: u64) -> Option<String> {
    read_stops(repo_id).remove(&number.to_string())
}

/// Every stopped pull request in this repo somebody can still act on, in numeric order, in the
/// shape the counts payload carries ([`crate::prq::StoppedPr`] — the type is the payload's, the
/// file is this module's).
///
/// Numeric rather than the file's own: the stops are keyed by strings, and `"10"` sorting before
/// `"9"` is not an order anybody asked to read a banner in.
///
/// # A stop is only worth saying where there is a row to clear it from
///
/// This is what the cockpit's banner is built from, and a banner is a demand for somebody's
/// attention. The stop FILE is a different thing: it is skein's memory of a refusal, and its whole
/// job is to outlive the pass that wrote it. Reading the file straight out onto the banner
/// conflated the two, and the difference showed up as the one failure a banner cannot survive
/// (SKEIN-241): the train stops on #123, a person merges #123 on GitHub, #123 leaves the queue —
/// and an orange row sits above the whole application naming a pull request with no row, for ever.
/// Both "let it run again" buttons are built from the live queue (`src/web/index.html`), so there
/// was no way to dismiss it at all. `crate::prq::queue_within` already makes the argument this
/// rests on: *a banner that is always there stops being read.*
///
/// So the file keeps everything and this answers about what is in front of a person. That also
/// closes the second half of SKEIN-241, without either surface having to know about the other:
/// the panel's set ([`trains`]) is drawn from the queue too, so the banner can no longer name a
/// pull request the panel has never heard of.
///
/// **The queue is read from what is already on this machine — never over the network.** Same rule
/// and same two roads as [`crate::prq::remembered_head`]: this runs on the badge poll, for every
/// repo in the fleet, and a filter that cost a GitHub round trip would be paid for by the one
/// thing the ten-minute badge budget exists to protect (SKEIN-208).
///
/// **Blindness shows everything, rather than nothing.** No queue on this machine yet, or one whose
/// searches did not see every open pull request ([`crate::prq::Queue::whole`]), and the file is
/// answered unfiltered. A refresh that went dark has an empty `prs` list for the same reason a
/// repo with nothing open does, and reading that as "every stop is dismissible" would silence
/// every stop in the fleet during one rate-limit window — which is SKEIN-229's failure, arriving
/// through a different file. Erring toward a banner that is too loud is recoverable; erring toward
/// one that is silent is the failure this whole feature exists to prevent.
pub fn stops(repo_id: &str) -> Vec<crate::prq::StoppedPr> {
    let open = open_pull_requests(repo_id);
    let mut out: Vec<crate::prq::StoppedPr> = read_stops(repo_id)
        .into_iter()
        .filter_map(|(number, why)| {
            number
                .parse::<u64>()
                .ok()
                .map(|number| crate::prq::StoppedPr { number, why })
        })
        .filter(|s| match &open {
            Some(open) => open.contains(&s.number),
            None => true,
        })
        .collect();
    out.sort_by_key(|s| s.number);
    out
}

/// Which pull requests this repo has open, from what is already on this machine — and `None` when
/// nothing here can say.
///
/// `None` is the answer for two different situations and deliberately the same one: no queue has
/// been read for this repo yet, and a queue whose searches were cut off or failed. Both mean a
/// pull request's ABSENCE from the list is evidence about the searches rather than about the pull
/// request, which is exactly what [`crate::prq::Queue::whole`] was added to say — the archive and
/// snooze prunes in `prq::queue_within` read it for the same reason.
fn open_pull_requests(repo_id: &str) -> Option<Vec<u64>> {
    let known = crate::prq::unexpired(repo_id).or_else(|| crate::prq::remembered(repo_id))?;
    known
        .whole
        .then(|| known.prs.iter().map(|pr| pr.number).collect())
}

/// Change the stops, with the refusal worded by whoever is writing.
///
/// A file nobody has written yet is no stops — a repo where nothing has ever gone wrong. A file
/// that is there and will not parse holds every stop in the repo, and the two writers below say
/// different things about that: [`stop`] has nobody to tell but stderr, and [`clear`] is answering
/// a press. Under one lock and written atomically, like the other two files (SKEIN-414).
fn update_stops(
    repo_id: &str,
    unreadable: impl FnOnce(String) -> Result<std::collections::BTreeMap<String, String>, String>,
    change: impl FnOnce(&mut std::collections::BTreeMap<String, String>) -> bool,
) -> Result<(), String> {
    update_file(&stops_path(repo_id), unreadable, change)
}

/// The stops, with an unreadable file read as none.
///
/// For the readers, where "no stop" is the loud answer rather than the quiet one: a stop that
/// cannot be read means the banner and the train view show a pull request as free to move, which is
/// wrong in the direction somebody notices. The writers go through [`update_stops`], because for
/// them the same misreading is what *destroys* the stops.
fn read_stops(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    crate::util::read_json_or_why(&stops_path(repo_id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Stop this pull request's workflow, and say why.
///
/// **Never over a stop file skein could not read** (SKEIN-359). Every stop in the repo is in that
/// file, and writing this one over a read that answered "no stops" would let every other stopped
/// pull request move again — each of them stopped because acting on it went wrong once, which is
/// the loop this whole rule exists to prevent, arriving all at once and for every pull request
/// rather than for one.
pub fn stop(repo_id: &str, number: u64, why: &str) {
    let wrote = update_stops(
        repo_id,
        |unreadable| {
            Err(format!(
                "the stop file will not parse ({unreadable}), and replacing it would let every \
                 other stopped pull request in {repo_id} move again. Fix or move that file"
            ))
        },
        |stops| {
            stops.insert(number.to_string(), why.to_string());
            true
        },
    );
    // Loudest of the three, because nothing else will say it: this one is not on a person's button,
    // so the sentence on stderr is the only place the failure exists. Both ways of failing are said
    // here rather than one each side of the read, because what a person does about them is the
    // same, and the consequence certainly is: the next poll re-attempts an action that has already
    // failed once, which is the loop this whole rule exists to prevent.
    if let Err(e) = wrote {
        eprintln!(
            "skein: #{number}'s workflow stopped ({why}) and skein could not write it down — {e}. \
             Until that is fixed this pull request may be attempted again."
        );
    }
}

/// Let it run again. What a person does after fixing whatever the reason was.
///
/// **The write is the act, and it is reported.** This used to discard it — `let _ = …` on the write
/// — and then journal the clear unconditionally, so a stops file that could not be written (a
/// read-only host state directory, a full disk) left the stop exactly where it was, put
/// a line in the timeline saying a person had lifted it, and answered the button "done". The
/// person is told it worked, shown a record saying it worked, and the train never moves
/// (SKEIN-249). Of everything skein writes, this was the only place a journal entry could describe
/// an act that had not happened — its sibling [`stop`] already says a failed write out loud, and
/// says it for the same reason in the opposite direction.
///
/// So the journal entry is written only after the file is, and the error travels back through
/// [`apply`] to the row. The sentence names the consequence rather than the syscall, because what
/// somebody needs to know is not that a write failed but that the pull request is still stopped.
///
/// **A pull request with no stop is `Ok`, not an error.** Nothing needed doing and nothing was
/// written, which is the same rule [`crate::prq::set_archived`] keeps and for the same reason: a
/// retried request must not report a failure for having arrived twice.
pub fn clear(repo_id: &str, number: u64) -> Result<(), String> {
    // Both ways this can fail end the same sentence, and the sentence is the point: what somebody
    // needs to know is not that a read or a write failed but that the pull request is still
    // stopped. An unreadable file is refused for the reason one paragraph up — read as "no stops"
    // this would answer the button Ok for a stop it never saw, and then write an empty file over
    // every other stop in the repo (SKEIN-359).
    let mut removed = false;
    update_stops(
        repo_id,
        |unreadable| {
            Err(format!(
                "skein cannot read the stop file, and will not replace it with one holding no \
                 stops at all: {unreadable}"
            ))
        },
        |stops| {
            removed = stops.remove(&number.to_string()).is_some();
            // Nothing to clear is not a failure and is not a write: a retried press must not report
            // one, and must not depend on a file it has no reason to touch.
            removed
        },
    )
    .map_err(|e| format!("#{number} is still stopped — {e}"))?;
    if !removed {
        return Ok(());
    }
    // A person clearing a stop is an event the timeline must show — without it, a journal
    // reads "stopped … did …" with no sign of the hand that let it move again. AFTER the write,
    // so the timeline can only ever describe something that happened.
    record(
        repo_id,
        number,
        "",
        0,
        "cleared",
        "the stop was cleared — the workflow may act again",
    );
    Ok(())
}

/// One line of a pull request's workflow history — the durable answer to "what happened to this
/// one, and in what order".
///
/// The stop file says only the *latest* reason; the audit log belongs to the host and mixes every
/// box's events. This is skein's own per-PR timeline, written the moment something happens, read
/// oldest-first.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JournalEntry {
    /// When, as epoch milliseconds.
    pub at_ms: i64,
    /// Which workflow acted. Empty for events no workflow owns — a person clearing a stop.
    pub flow: String,
    /// Which step, 1-based. `0` means "not a step": a clear, or a stop written by hand.
    pub step: usize,
    /// `"did"` | `"stopped"` | `"cleared"`.
    pub kind: String,
    /// The human sentence — the same one the audit and the row carry.
    pub what: String,
}

/// Where a repo's workflow journal lives: beside the stops, keyed the same way.
fn journal_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-journal.json")
}

fn read_journal(repo_id: &str) -> std::collections::BTreeMap<String, Vec<JournalEntry>> {
    // A corrupt or absent file reads as empty, never as an error: the journal is a record of what
    // happened, and losing it must not stop anything from happening.
    crate::util::read_json_or_why(&journal_path(repo_id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Write one journal entry down, now.
///
/// Write-through like the stops file: every event lands on disk before the function returns, so a
/// server that dies mid-pass has still said what it did. Each pull request keeps its newest 50
/// entries — a train PR sees a handful of acts on its way to merged, so 50 covers weeks of
/// stop/clear churn without the file growing without bound.
///
/// **This is the one file in SKEIN-359's list that is deliberately written over when it cannot be
/// read, and the argument is not "best effort".** It is that both answers lose the same thing —
/// history — and only one of them ever gets it back. Refusing would keep the unreadable bytes and
/// end journalling for this repo permanently: nothing repairs the file, `record` is best-effort so
/// no caller is stopped by the refusal, and the sweep would go on acting with no record that it
/// did. Writing over it loses what was there and journalling resumes on the next event. Nothing
/// reads this file to decide anything — [`stops`] is what decides, and it refuses — so what is lost
/// is a timeline somebody reads a week later, not a stop somebody's pull request depends on. It is
/// said out loud each time rather than once per process, because the line names the repo whose
/// timeline was discarded and there is more than one repo.
fn record(repo_id: &str, number: u64, flow: &str, step: usize, kind: &str, what: &str) {
    let at_ms = now_ms();
    // Under the same lock and the same atomic write as the other two (SKEIN-414). The sweep
    // journals on its own thread while a person's clear journals from a request thread, and two
    // appends interleaving lose a whole entry — which for a timeline is the one kind of loss that
    // cannot be noticed, because what is missing is the line that would have said so.
    let written = update_file(
        &journal_path(repo_id),
        |unreadable| {
            eprintln!(
                "skein: {repo_id}'s workflow journal will not parse ({unreadable}) — the timeline \
                 it held is being written over so that journalling can carry on. Nothing acts on \
                 this file; the stops it sits beside are refused instead."
            );
            Ok(Default::default())
        },
        |all: &mut std::collections::BTreeMap<String, Vec<JournalEntry>>| {
            let entries = all.entry(number.to_string()).or_default();
            entries.push(JournalEntry {
                at_ms,
                flow: flow.to_string(),
                step,
                kind: kind.to_string(),
                what: what.to_string(),
            });
            if entries.len() > 50 {
                let drop = entries.len() - 50;
                entries.drain(..drop);
            }
            true
        },
    );
    if let Err(e) = written {
        // Best-effort, said out loud: a journal that could not be written loses history, not
        // safety — the stop file is the one whose loss re-attempts an action.
        eprintln!("skein: could not journal #{number}'s workflow event ({e})");
    }
}

/// One pull request's workflow history, oldest first.
pub fn journal(repo_id: &str, number: u64) -> Vec<JournalEntry> {
    read_journal(repo_id)
        .remove(&number.to_string())
        .unwrap_or_default()
}

/// Every journaled pull request in this repo, in numeric order, each timeline oldest first.
pub fn journals(repo_id: &str) -> std::collections::BTreeMap<u64, Vec<JournalEntry>> {
    read_journal(repo_id)
        .into_iter()
        .filter_map(|(number, entries)| number.parse::<u64>().ok().map(|n| (n, entries)))
        .collect()
}

/// What a workflow would do to one pull request, and why — without doing any of it.
///
/// The dry run the owner asked to see before trusting this, and the same [`crate::workflow::next`]
/// the tick uses. Deliberately the same function: a preview computed a second way is a preview that
/// can disagree with what happens, and the whole point of showing it is that it cannot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Standing {
    /// The workflow's name, or empty.
    pub workflow: String,
    /// `assigned` | `matched` | `excluded` | `none` — how it came to carry that workflow, because
    /// "you chose this" and "a rule chose this" are different things to see on a row.
    pub how: String,
    /// The step it would take next, spelled as it is written in the file. Empty when nothing
    /// applies, which is what a healthy workflow says most of the time.
    pub next: String,
    /// Which step that is, 1-based, for a row that wants to say "waiting on step 2".
    pub step: usize,
    /// Why it is stopped, if it is. A stopped workflow does nothing until this is cleared.
    pub stopped: String,
    /// Why the workflow on this pull request is not acting, though nothing is wrong (SKEIN-279):
    /// somebody assigned it and its own `matches` do not hold yet. Empty when it is acting
    /// normally.
    ///
    /// Not a stop and not a wait, and it says so in those words — there is nothing to clear and no
    /// clock running. The conditions are spelled as the file spells them, so the sentence is
    /// checkable against the workflow somebody is reading.
    ///
    /// A separate field rather than a sentence in [`Standing::next`], because `next` is a step
    /// spelled as it is written in the file and this is the reason there is no step. An older
    /// cockpit that does not read this still says "nothing to do right now", which is true; a
    /// cockpit that does says which condition.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub holding: String,
}

/// Everything the cockpit needs to draw one pull request's workflow state.
pub fn standing(
    repo_id: &str,
    number: u64,
    facts: &crate::workflow::Facts,
    flows: &[Workflow],
) -> Standing {
    let carried = carries(repo_id, number, facts, flows);
    // A holding workflow is still an ASSIGNED one — that is how it came to carry the pull request,
    // and it is what the row's chooser has to show as chosen. What holds it back is `holding`, so
    // the two facts are separate rather than one overwriting the other.
    let how = match &carried {
        Carries::Assigned(_) | Carries::Holding { .. } => "assigned",
        Carries::Matched(_) => "matched",
        Carries::Excluded => "excluded",
        Carries::Nothing => "none",
    };
    let holding = match &carried {
        Carries::Holding { name, unmet } => format!(
            "{name} is on this pull request and is not acting yet: its own {} {} not true — \
             {}. Nothing is stopped and nothing is waiting on a clock; it joins in on the next \
             pass after that changes.",
            match unmet.len() {
                1 => "condition",
                _ => "conditions",
            },
            match unmet.len() {
                1 => "is",
                _ => "are",
            },
            unmet.join(", "),
        ),
        _ => String::new(),
    };
    // `acting`, not `name`: a holding workflow has no next step, and asking for one would spell
    // out a step it is not going to take.
    let flow = carried
        .acting()
        .and_then(|name| flows.iter().find(|f| f.name == name));
    let chosen = flow.and_then(|flow| crate::workflow::next(flow, facts));
    Standing {
        workflow: carried.name().unwrap_or_default().to_string(),
        how: how.to_string(),
        holding,
        next: chosen
            .as_ref()
            .map(|c| crate::workflow::spell_act(&c.act))
            .unwrap_or_default(),
        step: chosen.as_ref().map(|c| c.step + 1).unwrap_or(0),
        stopped: stopped(repo_id, number).unwrap_or_default(),
    }
}

/// Take one step, or say why not.
///
/// `head_sha` anchors the two acts that can carry it: `update_branch` sends it as `expectedHeadOid`
/// and `merge_pr` sends it as `sha`, so GitHub refuses rather than acts if somebody pushed between
/// skein deciding and skein acting. That is the same rule as the anchor on a box: prove the thing
/// is what you think before touching it.
///
/// `add_label` and `remove_label` carry no head, and cannot: GitHub's issue-labels API accepts no
/// head parameter on either verb. That matters because `add-label:ci-queue` is the step that starts
/// CI, so a push landing mid-decision starts a run against a head skein never saw. The cost is a
/// wasted CI run and a front that goes round again — not a bad merge, because `merge_pr` re-checks
/// with `sha` and GitHub answers 409. The anchor is on the acts where being wrong ships something.
/// The table is in `docs/pr-workflow.md`, "Which acts carry a head anchor, and which two cannot".
pub struct Subject<'a> {
    /// The repo as skein knows it, which is where the stop is written down.
    pub repo_id: &'a str,
    /// `owner/name` as GitHub knows it.
    pub slug: &'a str,
    pub number: u64,
    /// The commit the decision was made about. The two acts that can carry it do — see the note on
    /// this struct for the two that cannot, and why it is the labels API rather than an oversight.
    pub head_sha: &'a str,
    pub head_ref: &'a str,
    /// What [`Act::Read`] needs, and what no other act does — see [`Reading`].
    pub reading: Option<Reading<'a>>,
}

/// The three things a reading needs that the five fields above cannot supply.
///
/// Carried as an `Option` rather than folded into [`Subject`] because it is honestly optional: the
/// five fields above are what GitHub's write APIs take, and every one of them is a `&str` a caller
/// can hold without having looked anything up. A reading needs the whole [`crate::repos::Repo`] —
/// its flags decide whether it may happen at all — and the whole [`crate::prq::Pr`], which is what
/// the reading path takes; the one production caller has both in hand already.
///
/// **`None` is not "read it anyway with defaults".** It is a caller that cannot read, and
/// [`Act::Read`] refuses out loud rather than inventing a `Repo` — which, with `auto_review`
/// defaulting off, would refuse for the wrong reason and read as a flag problem.
pub struct Reading<'a> {
    /// Whose flags decide whether skein may read this at all — `repos::auto_review_stands`.
    pub repo: &'a crate::repos::Repo,
    /// The pull request as the queue has it. The reading path anchors on `pr.head_sha`, which is
    /// the same commit [`Subject::head_sha`] carries.
    pub pr: &'a crate::prq::Pr,
    /// Who skein is acting as, for CODEOWNERS. One identity, the same one `read_waiting` uses.
    pub viewer: &'a str,
    /// What the step was decided from. `Act::Read` asks it a second question the evaluator does
    /// not: **which of §10's triggers fired**, which is a per-repo gate rather than a step
    /// condition and therefore cannot live in a workflow file.
    pub facts: &'a crate::workflow::Facts,
}

/// Answer one check this repository owes, at the commit the step was decided about —
/// `docs/pr-review.md` §8 and §15 step 5.
///
/// **One check per evaluation**, which is the engine's own rule rather than a throttle: a step is
/// chosen from the state that is there now, and a pass that answered three checks would be three
/// decisions made from one reading of the world. The next evaluation sees one fewer outstanding and
/// picks the next, and `Cond::ChecksSettled` starts holding when the last one is answered.
///
/// # Why the same guards as a reading, in the same order
///
/// An audit spends a model call and can post to the pull request, so every door [`read_now`] opens
/// this one opens too: [`crate::repos::auto_review_stands`] for the money, §10's trigger set and
/// author filter for whether this repository reviews this pull request at all, and the sha anchor
/// so an answer is never filed against a commit the pass did not evaluate. Sharing the shape rather
/// than the code is deliberate — they differ in what they spend it on, and a helper that took a
/// closure would hide which of the two a failure came from.
///
/// # What is recorded, and when
///
/// [`crate::owed::record`] runs **only after** [`crate::review::audit_owed`] returns an answer. A
/// check recorded on a turn that timed out would satisfy §8's condition with nothing behind it,
/// which is a verdict released by a failed model call — the exact shape of the failure the sha
/// guard and the sweep both exist to prevent.
fn audit_now(pr: &Subject) -> ReadStep {
    let Some(reading) = &pr.reading else {
        return ReadStep::Failed(format!(
            "this caller cannot audit #{} — it passed no repo, pull request or viewer (an `audit` \
             step is only takeable from the workflow pass)",
            pr.number
        ));
    };
    if let Some(why) = crate::repos::auto_review_stands_for(
        reading.repo,
        chosen_by_hand(&reading.repo.id, pr.number),
    ) {
        return ReadStep::Failed(why);
    }
    if let Some(why) = no_trigger_of_this_repos_fired(reading.repo, pr.number, reading.facts) {
        return ReadStep::Waited(why);
    }
    if let Some(why) = not_an_author_this_repo_reviews(reading.repo, reading.facts) {
        return ReadStep::Waited(why);
    }
    if reading.pr.head_sha != pr.head_sha {
        return ReadStep::Failed(format!(
            "the step was decided about {} and the audit would be recorded against {} — refusing \
             to audit #{} at a commit this pass did not evaluate",
            pr.head_sha, reading.pr.head_sha, pr.number
        ));
    }
    let Some(check) = the_first_check_still_owed(reading.repo, pr.number, pr.head_sha) else {
        // Not a fault: `Cond::ChecksOwed` and this lookup read the same three sets, and the pass
        // between the two is where the last one can be answered by another tick.
        return ReadStep::Waited(format!(
            "nothing #{} owes is outstanding at {}",
            pr.number, pr.head_sha
        ));
    };
    if reading.repo.auto_review_dry_run {
        return ReadStep::Waited(format!(
            "dry run: would audit #{} at {} for {}",
            pr.number,
            pr.head_sha,
            check.spelled()
        ));
    }
    let said = match crate::review::audit_owed(
        reading.repo,
        pr.number,
        pr.head_sha,
        &reading.pr.base_ref,
        check.owed(),
    ) {
        Ok(said) => said,
        // A wait rather than a stop, for `read_now`'s reason: an audit changes nothing outside
        // skein, its failures are the transient kind, and nothing downstream can act on one that
        // did not happen — `checks_owed` stays `Some(true)` and the verdict stays out of reach.
        Err(why) => {
            return ReadStep::Waited(format!(
                "#{} was not audited at {}: {why}",
                pr.number, pr.head_sha
            ))
        }
    };
    if let Err(why) = crate::owed::record(&reading.repo.id, pr.number, pr.head_sha, check) {
        // The turn HAPPENED — it may have posted a finding — so this is not a failure of the
        // audit. It is a failure to remember it, and the consequence is one repeated audit rather
        // than a verdict let through, so it says so and waits.
        return ReadStep::Waited(format!(
            "#{} was audited for {} at {} but the answer could not be written down ({why}), so it \
             will be asked again",
            pr.number,
            check.spelled(),
            pr.head_sha
        ));
    }
    ReadStep::Did(format!(
        "audited #{} at {} for {} — {said}",
        pr.number,
        pr.head_sha,
        check.spelled()
    ))
}

/// The next check this repository owes that nobody has answered at this commit.
///
/// The same three sets [`what_this_change_still_owes`] intersects, returning the check rather than
/// whether there is one — two readers of one rule, which is why the intersection itself lives in
/// [`crate::owed::outstanding`] and neither of these implements it.
fn the_first_check_still_owed(
    repo: &crate::repos::Repo,
    number: u64,
    head_sha: &str,
) -> Option<crate::owed::Check> {
    let said = crate::review::cached(&repo.id, number, head_sha)?;
    if said.head_sha != head_sha || said.depth == crate::review::Depth::Unread {
        return None;
    }
    let fired = crate::owed::read(&said.owed_triggered?).0;
    let (set, _refused) = crate::owed::for_repo(repo.owed_checks.as_ref());
    let done = crate::owed::answered(&repo.id, number, head_sha);
    crate::owed::outstanding(&set, &fired, &done)
        .first()
        .copied()
}

/// What one verdict step came to. [`ReadStep`]'s shape and the same three answers, because a
/// verdict has the same third case: **drafted, and waiting for a person.** That is what a ceiling
/// below the verdict means, and folding it into `Err` would stop a workflow that is behaving
/// exactly as it was configured to.
enum VerdictStep {
    Did(String),
    Waited(String),
    Failed(String),
}

/// What one `read` step came to. Its own type because a reading has a third answer the other acts
/// do not: *nothing to do, and that is fine* — the reading is already on disk at this head, or the
/// repo is in dry run. Folding that into `Err` would stop the workflow on a pull request nothing
/// is wrong with.
enum ReadStep {
    /// A model call was spent and a reading now exists at this head.
    Did(String),
    /// Nothing was spent, and nothing is wrong. Says why.
    Waited(String),
    /// A step a person wrote that skein may not take. Stops, loudly, like any other failure.
    Failed(String),
}

pub fn perform(
    pr: &Subject,
    flow: &Workflow,
    chosen: &Chosen,
    token: &crate::secret::Secret,
) -> Outcome {
    let (repo_id, slug, number, head_sha, head_ref) =
        (pr.repo_id, pr.slug, pr.number, pr.head_sha, pr.head_ref);
    // The switch is read here rather than only by the caller, because this is the function with the
    // consequences. A caller that forgot to check would be a bug that merges pull requests.
    if !enabled() {
        return Outcome::Stopped(
            "workflows are switched off for this fleet (Settings, or $SKEIN_PR_WORKFLOWS=on)"
                .into(),
        );
    }
    if let Some(why) = stopped(repo_id, number) {
        return Outcome::Stopped(why);
    }
    // The authority for everything below: which workflow, which step. It goes in the audit and on
    // the row, so a person can find the line that decided this.
    let by = format!("{} step {}", flow.name, chosen.step + 1);

    let done = match &chosen.act {
        Act::Wait(why) => return Outcome::Waited(why.clone()),
        Act::Flag(why) => {
            // A flag is the workflow saying it has gone as far as it can. Written down like any
            // other stop so the next poll does not simply say it again.
            //
            // The journal write sits HERE, beside the stop write, not inside `stop()`: `stop` is
            // also called by hands other than a workflow's, and those stops are not this flow's
            // step doing something — journaling them here keeps the flow and step honest.
            stop(repo_id, number, why);
            record(repo_id, number, &flow.name, chosen.step + 1, "stopped", why);
            return Outcome::Stopped(why.clone());
        }
        Act::AddLabel(label) => add_label(slug, number, label, token)
            .map(|_| format!("added the label {label:?} to #{number}")),
        Act::RemoveLabel(label) => remove_label(slug, number, label, token)
            .map(|_| format!("removed the label {label:?} from #{number}")),
        Act::UpdateBranch(how) => update_branch(slug, number, head_sha, *how, token).map(|_| {
            let how = match how {
                Update::Rebase => "rebase",
                Update::Merge => "merge",
            };
            // Said plainly, because the owner asked for a rebase that keeps approvals and GitHub
            // does not offer one. Whether the approval survived is the repository's setting, not
            // skein's doing — see docs/pr-workflow.md.
            format!(
                "updated #{number} with its base by {how} — if this repository dismisses stale \
                 approvals, that approval is now gone and it needs approving again"
            )
        }),
        // §15 step 3: the one reviewer action that is wired. It spends a model call and writes a
        // reading to the cache; it posts nothing, which is step 4 and the four arms below.
        Act::Read => match read_now(pr) {
            ReadStep::Did(what) => Ok(what),
            ReadStep::Failed(why) => Err(why),
            // Returned rather than folded into `done`, because a wait is not an outcome the
            // journal wants a line for on every pass: `Read` is chosen again on the next one, and
            // "already read #41 at abc1234" written every tick would bury the actions.
            ReadStep::Waited(why) => return Outcome::Waited(why),
        },
        // §15 step 4: the verdicts. `Read` already posts the findings — the reading session does
        // it from inside the box, which is what `docs/pr-review.md` §6 means by "it reads, and
        // posts" — so these two are the half nothing did.
        Act::PostChanges | Act::PostApproval => {
            let verdict = match chosen.act {
                Act::PostApproval => crate::prq::Verdict::Approve,
                _ => crate::prq::Verdict::RequestChanges,
            };
            match post_verdict(pr, flow, chosen, verdict, token) {
                VerdictStep::Did(what) => Ok(what),
                VerdictStep::Failed(why) => Err(why),
                // Below the ceiling is not a fault: §10 says anything past it "is drafted and
                // waits for you", and a stop would need clearing for a repo behaving as set up.
                VerdictStep::Waited(why) => return Outcome::Waited(why),
            }
        }
        // **`post-findings` is a vestige, and saying so is better than wiring it.** §9's table gave
        // findings their own row when the design assumed skein would post them; since `8c49c34` the
        // reading session posts its own comment review with `gh` from inside its checkout, and
        // skein keeps no copy of it. So a step here would post the SUMMARY — a different artefact —
        // beside a review that is already on the pull request, and a reader would get the same
        // reading twice in two voices. The act stays in the vocabulary because §9's table is a
        // person's mental model, and removing a row from it silently is worse than refusing one
        // out loud.
        Act::PostFindings => Err("post-findings is not wired, and deliberately: the reading \
             session posts its own comment review from inside its checkout, so a step here would \
             post the summary beside a review that is already there. Use `read`, which reads and \
             posts (docs/pr-review.md §6)"
            .into()),
        // §15 step 5: the scar, as a step. One owed check per evaluation, asked of the reading's
        // own session and recorded against the commit it was asked about.
        Act::Audit => match audit_now(pr) {
            ReadStep::Did(what) => Ok(what),
            ReadStep::Failed(why) => Err(why),
            ReadStep::Waited(why) => return Outcome::Waited(why),
        },
        Act::Merge(merge) => merge_pr(slug, number, head_sha, merge.how, token).and_then(|_| {
            match merge.delete_branch {
                false => Ok(format!("merged #{number}")),
                // Only after the merge landed. A branch deleted before it is merged closes the pull
                // request instead of shipping it.
                true => delete_branch(slug, head_ref, token)
                    .map(|_| format!("merged #{number} and deleted {head_ref}"))
                    // The merge DID happen. Reporting the whole step as failed would be a lie, and
                    // a retry would try to merge an already-merged pull request.
                    .or_else(|e| {
                        Ok(format!(
                            "merged #{number}, but {head_ref} is still there: {e}"
                        ))
                    }),
            }
        }),
    };

    match done {
        Ok(what) => {
            crate::warden_client::reported(&format!("pr-workflow:{}", flow.name), &what, &by);
            record(repo_id, number, &flow.name, chosen.step + 1, "did", &what);
            Outcome::Did(what)
        }
        Err(why) => {
            // Not a retry. See the module note: the decision was made from facts this failure has
            // just proved stale, and the next poll would make the same one.
            let why = format!("{by} could not be done: {why}");
            stop(repo_id, number, &why);
            record(
                repo_id,
                number,
                &flow.name,
                chosen.step + 1,
                "stopped",
                &why,
            );
            crate::warden_client::reported(
                &format!("pr-workflow:{}", flow.name),
                &format!("stopped on #{number}"),
                &why,
            );
            Outcome::Stopped(why)
        }
    }
}

/// **Did any trigger this repo asked for actually fire?** `None` when one did — `docs/pr-review.md`
/// §10's trigger set, which was a stored field deciding nothing until this.
///
/// Two different silences, said differently, because they need different actions from a person.
/// A set whose triggers are real and none fired is the ordinary state of a queue: this pull request
/// is simply not one of the events this repo asked to be woken by, and there is nothing to fix. A
/// set with **no trigger this build can compute** is a repo switched on and inert — §10 says that
/// state must *say* it is off rather than present as on — and the only way out is to change the
/// set, so the sentence names the words that cannot fire.
///
/// An unrecognised word is treated exactly as an uncomputable one: a trigger from a newer skein is
/// one this build cannot tell has fired. Both fail towards not reading. See `workflow::read_wake`.
fn no_trigger_of_this_repos_fired(
    repo: &crate::repos::Repo,
    number: u64,
    facts: &crate::workflow::Facts,
) -> Option<String> {
    // **The set this pull request is governed by, not the repo's** — §10's "overridable per pull
    // request". `repos::triggers_for` answers the repo's own words unless somebody has said
    // otherwise about this one, so the ordinary case is unchanged and the sentences below go on
    // naming the words that actually apply.
    let words = crate::repos::triggers_for(repo, number);
    let wanted: Vec<crate::workflow::Wake> = words
        .iter()
        .filter_map(|word| crate::workflow::read_wake(word))
        .collect();
    if wanted.is_empty() {
        // Every word in the set is one this build cannot answer — or the set is empty, which
        // `auto_review_stands` has already refused, so reaching here means the first.
        return Some(format!(
            "automatic review is on for {} with a trigger set this build cannot act on ({}) — no \
             reading can ever be woken by it, so change the set or switch the repo off",
            repo.id,
            match words.is_empty() {
                true => "it is empty".to_string(),
                false => words.join(", "),
            }
        ));
    }
    let fired = crate::workflow::woke(facts);
    if fired.iter().any(|w| wanted.contains(w)) {
        return None;
    }
    Some(format!(
        "no trigger {} asks for has fired on this one — it wakes on {}{}",
        repo.id,
        wanted
            .iter()
            .map(|w| w.spelled())
            .collect::<Vec<_>>()
            .join(", "),
        match fired.is_empty() {
            // Naming what DID fire is the difference between "nothing is happening" and "the set
            // is the wrong shape": a person who sees `approved-commits fired` beside a set of
            // `requested` knows immediately which line to change.
            true => String::new(),
            false => format!(
                ", and what fired here is {}",
                fired
                    .iter()
                    .map(|w| w.spelled())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    ))
}

/// **Whose pull requests may this repo's engine read?** `mine` or `all` — §10's `auto_review_authors`.
///
/// `mine` is the default and the intended use: reviewing what your own boxes open. An outside
/// contributor's pull request is a different risk with a different audience, and it is the first
/// place a wrong verdict is seen by somebody who did not opt into any of this.
///
/// **A word this build does not know reads as `mine`**, the narrow one. Same rule as
/// `repos::Ceiling` and `workflow::read_wake`: this is a permission, and a value skein cannot
/// understand must never widen what it does unattended. That is the opposite of `place::Purpose`'s
/// lenient reader, and deliberately — there an unknown value costs a box nobody can reach.
fn not_an_author_this_repo_reviews(
    repo: &crate::repos::Repo,
    facts: &crate::workflow::Facts,
) -> Option<String> {
    if repo.auto_review_authors.trim() == "all" || facts.mine {
        return None;
    }
    Some(format!(
        "{} reviews only pull requests you opened, and this is somebody else's (its \
         auto_review_authors is {:?})",
        repo.id,
        repo.auto_review_authors.trim()
    ))
}

/// **Has this repo been configured into the loop?** — `docs/pr-review.md` §13, the one obligation
/// the owner's decision came with.
///
/// > engine reviews → engine approves → `Facts::approved` → label, await CI, merge, delete branch
///
/// skein approving its own work and merging it, with nobody in it. The owner's answer was *keep
/// them apart, per repo* — *"If needed, we can just chain them by saying merge all approved ones;
/// how it reached approved is not needed by the merge train."* That reading is right, and it is why
/// this costs nothing to build: the train reads `Facts::approved` and has no interest in
/// **provenance**, so keeping the two apart is a *configuration* and chaining them is the same
/// configuration switched on deliberately. No mechanism has to know the difference, and none
/// should — a train that asked who approved would be a second place where "does this count" is
/// decided, which is how `Facts::approved` came to be wrong in the first place.
///
/// **What is owed is therefore a sentence, not a guard.** A person who switches auto-review on for
/// a repo that already has a train has just built the loop, and nothing would say so. This is the
/// house rule applied to a configuration rather than to a failure: say it, rather than let it be
/// discovered.
///
/// # Four conditions, and the fourth is why this is not noisy
///
/// §13 sketched this as "both on". Built, it is narrower, because `auto_review_ceiling` was
/// designed after that paragraph was written:
///
/// 1. **workflows can act at all** — [`enabled`], the fleet's one kill switch;
/// 2. **some workflow merges** — a `Merge` act in a step somewhere in the file;
/// 3. **the engine may act on this repo** — `repos::auto_review_stands`;
/// 4. **the ceiling reaches an approval.** A `comment` or `changes` ceiling never posts one, so
///    there is no approval for a train to read and no loop to warn about. That is the default a
///    repo is switched on at, so the ordinary way of turning auto-review on does not trip this.
///
/// Deliberately **not** asked: whether a train's `matches` claims any particular pull request.
/// That is a per-pull-request question needing `Facts`, and this is a question about a repo's
/// settings. Being early is the right direction for a warning about self-approving merges.
pub fn the_loop_this_repo_has_built(repo: &crate::repos::Repo) -> Option<String> {
    if !enabled() {
        return None;
    }
    if crate::repos::auto_review_stands(repo).is_some() {
        return None;
    }
    if repo.auto_review_ceiling < crate::repos::Ceiling::Approve {
        return None;
    }
    let trains: Vec<String> = crate::workflow::load()
        .unwrap_or_default()
        .into_iter()
        .filter(|flow| {
            flow.steps
                .iter()
                .any(|step| matches!(step.act, Act::Merge(_)))
        })
        .map(|flow| flow.name)
        .collect();
    if trains.is_empty() {
        return None;
    }
    Some(format!(
        "an approval this engine posts on {} will merge it — automatic review is on with a \
         ceiling of `approve`, and {} {} in this fleet. That composition was chosen rather than \
         prevented (docs/pr-review.md §13); lower `auto_review_ceiling` to keep verdicts waiting \
         for a person, or take {} off this repo.",
        repo.id,
        match trains.len() {
            1 => "the workflow",
            _ => "the workflows",
        },
        trains.join(", "),
        match trains.len() {
            1 => "that workflow",
            _ => "those workflows",
        },
    ))
}

/// **Post a verdict under the reader's name** — `docs/pr-review.md` §15 step 4, and the only thing
/// skein does that a person cannot take back by pressing something.
///
/// # Why the reading session is still forbidden to do this
///
/// §13 records the owner's decision as *"lift the prohibition"*, and what they asked for is that
/// skein post verdicts unattended. This delivers that, and it does **not** lift the prohibition in
/// the prompt — the reading session still may not approve or request changes, in as many words.
/// The difference is mechanism, and it is the whole reason every guard in this design exists:
///
/// * the **ceiling** below is a value in a config file, and a session never sees it;
/// * the **sha guard** (§4) is `Cond::ReadingCurrent`, evaluated here from facts;
/// * **§7c** — a partial pass may never approve — is `instead_of_approving_what_was_not_wholly_read`,
///   an override on the evaluator that no workflow file can defeat;
/// * the **audit** is `record` and the warden, naming which workflow and which step.
///
/// A session that posted its own verdict would be outside all four. So the prohibition stays where
/// it is and the engine takes the verdict, which is the same outcome through the machine that can
/// be argued with. That is a deviation from §13's letter and it is recorded there.
///
/// # The attribution §13 said was missing
///
/// > Nothing records who posted — skein keeps no copy of a review any more, by design, so an engine
/// > verdict is indistinguishable from the owner's, on GitHub and in the queue.
///
/// The body carries it, which is the one place a person actually looks: the workflow, the step, and
/// the commit the reading was made against. The journal and the warden have it too, but those are
/// skein's own records, and a verdict that discharges somebody's review must say what left it on
/// the pull request itself.
fn post_verdict(
    pr: &Subject,
    flow: &Workflow,
    chosen: &Chosen,
    verdict: crate::prq::Verdict,
    token: &crate::secret::Secret,
) -> VerdictStep {
    let Some(reading) = &pr.reading else {
        return VerdictStep::Failed(format!(
            "this caller cannot post on #{} — it passed no repo, so there is no ceiling to check \\
             a verdict against, and an unattended post with no ceiling is the one thing this must \\
             never do",
            pr.number
        ));
    };
    let repo = reading.repo;
    if let Some(why) =
        crate::repos::auto_review_stands_for(repo, chosen_by_hand(&repo.id, pr.number))
    {
        return VerdictStep::Failed(why);
    }
    if let Some(why) = no_trigger_of_this_repos_fired(repo, pr.number, reading.facts) {
        return VerdictStep::Waited(why);
    }
    if let Some(why) = not_an_author_this_repo_reviews(repo, reading.facts) {
        return VerdictStep::Waited(why);
    }
    // **The ceiling**, and it is the last gate before something appears under somebody's name.
    // Ordered by consequence, so one comparison covers all three positions — which is the whole
    // argument for a ceiling over three checkboxes (§10).
    let wants = match verdict {
        crate::prq::Verdict::Approve => crate::repos::Ceiling::Approve,
        crate::prq::Verdict::RequestChanges => crate::repos::Ceiling::Changes,
        crate::prq::Verdict::Comment => crate::repos::Ceiling::Comment,
    };
    if wants > repo.auto_review_ceiling {
        return VerdictStep::Waited(format!(
            "#{} is ready for {}, and {} goes no further than {} on its own — so it waits for you",
            pr.number,
            wants.spelled(),
            repo.id,
            repo.auto_review_ceiling.spelled(),
        ));
    }
    if repo.auto_review_dry_run {
        return VerdictStep::Waited(format!(
            "dry run: would post {} on #{} at {}",
            wants.spelled(),
            pr.number,
            pr.head_sha
        ));
    }
    // The same anchor `read_now` refuses on, for a much sharper reason: a verdict filed against a
    // commit this pass did not evaluate is a review describing tree A anchored to tree B, which is
    // §3's own account of what a memoryless engine gets wrong.
    if reading.pr.head_sha != pr.head_sha {
        return VerdictStep::Failed(format!(
            "the step was decided about {} and the verdict would be filed against {} — refusing \\
             to post on #{} at a commit this pass did not evaluate",
            pr.head_sha, reading.pr.head_sha, pr.number
        ));
    }
    let by = format!("{} step {}", flow.name, chosen.step + 1);
    let body = format!(
        "skein posted this automatically — *{by}*, against `{}`.\\n\\nThe reading it is based on is \\
         the review already on this pull request. Turn it off for this repository with \\
         `auto_review`, or lower `auto_review_ceiling` to keep verdicts waiting for a person.",
        pr.head_sha
    );
    // `drafted_at` is the same head, which is what makes it "assume current": the reading is
    // current — `Cond::ReadingCurrent` is what let this step be chosen — so there is nothing to
    // re-anchor and no displaced comments to fold in.
    match crate::prq::submit_review_with_comments(crate::prq::ReviewPost {
        slug: pr.slug,
        number: pr.number,
        head_sha: pr.head_sha,
        verdict,
        body: &body,
        comments: &[],
        drafted_at: pr.head_sha,
        // `perform`'s own token, which is the one every other act here is given. It is
        // `prq::host_token` either way today — the tick sources it from the same function — and
        // that is the point of passing it rather than the reason not to: the credential a verdict
        // is posted under is now visible at the call site instead of reached for two modules away.
        token,
    }) {
        Ok(_) => VerdictStep::Did(format!(
            "posted {} on #{} at {} under your name",
            wants.spelled(),
            pr.number,
            pr.head_sha
        )),
        Err(why) => VerdictStep::Failed(why),
    }
}

/// Read this pull request at the head the step was decided about — `docs/pr-review.md` §15 step 3.
///
/// **Wired to the reading skein already has**, rather than to a second one beside it. Everything
/// this needs is in [`crate::review::summarise`]: it stands the change up in a checkout, runs the
/// sweep that accounts for what it covered, and writes a [`crate::review::Summary`] keyed on
/// `(number, head_sha)` — which is the same cache `facts_of_in` reads `reading_sha` and
/// `reading_whole` back out of. So one `read` step closes the engine's own loop: the next
/// evaluation of the same workflow sees `ReadingCurrent`, and where the sweep answered,
/// `ReadingWhole`.
///
/// **It posts nothing.** §15 step 4 is the posts, and the four acts beside this one still refuse.
///
/// # Why a failed reading waits rather than stops
///
/// Every other act in `perform` turns a failure into a stop, and the module note argues that hard:
/// a decision made from facts a failure has just proved stale must not be made again. A reading is
/// the one act that is not like that. It changes nothing outside skein, its failures are the
/// ordinary transient kind — the day's spend ceiling, a diff that would not download, a model call
/// that timed out — and `review.rs` already fixes the direction they fail in: an unread pull
/// request is [`crate::review::Depth::Unread`], `ReadingWhole` does not hold, and
/// [`Act::PostApproval`] is unreachable. Nothing downstream can act on a reading that did not
/// happen, so the fail-closed behaviour is in the type rather than in this stop.
///
/// Stopping here would instead demand a person clear a workflow because a budget rolled over at
/// midnight. And it would not even save the model call: `review::note_tried` already writes the
/// failure against the head, so the next pass is told rather than charged.
///
/// The wait is never silent — it carries `unread_because` verbatim, which is the sentence written
/// to be shown to a person.
fn read_now(pr: &Subject) -> ReadStep {
    let Some(reading) = &pr.reading else {
        // A caller that cannot read, reported as that. Never a default `Repo`: with `auto_review`
        // off by default it would refuse with "automatic review is switched off", and somebody
        // would go and turn on a flag that was never the problem.
        return ReadStep::Failed(format!(
            "this caller cannot read #{} — it passed no repo, pull request or viewer (a `read` \
             step is only takeable from the workflow pass)",
            pr.number
        ));
    };
    // The money door, and the one place it is asked on the acting path. `read_prs` first, then
    // `auto_review`, then a trigger set that could wake it — `repos::auto_review_stands` layers
    // them so the sentence names the OUTER switch that is shut.
    if let Some(why) = crate::repos::auto_review_stands_for(
        reading.repo,
        chosen_by_hand(&reading.repo.id, pr.number),
    ) {
        return ReadStep::Failed(why);
    }
    // §10's chain, in §10's order: the trigger set and the author filter are asked here, AFTER the
    // repo may act at all and BEFORE the step's own conditions have any consequence. Waits rather
    // than stops, because neither is a fault — they are the flags working. A pull request this
    // repo does not review is one that queues, which is what §9 says "off" means.
    if let Some(why) = no_trigger_of_this_repos_fired(reading.repo, pr.number, reading.facts) {
        return ReadStep::Waited(why);
    }
    if let Some(why) = not_an_author_this_repo_reviews(reading.repo, reading.facts) {
        return ReadStep::Waited(why);
    }
    // The anchor, the same rule `merge_pr` and `update_branch` obey: prove the thing is what you
    // think before touching it. `Subject::head_sha` is the commit the step was DECIDED about and
    // `reading.pr.head_sha` is the commit that would be READ, and a reading filed against a commit
    // the engine did not evaluate is the anchoring failure this whole design is about.
    if reading.pr.head_sha != pr.head_sha {
        return ReadStep::Failed(format!(
            "the step was decided about {} and the reading would be filed against {} — refusing \
             to read #{} at a commit this pass did not evaluate",
            pr.head_sha, reading.pr.head_sha, pr.number
        ));
    }
    // Before the model call and after the flags, so a dry run answers exactly what a live one
    // would have been asked and costs nothing. It is a wait rather than a `Did`, because nothing
    // was done — and because a `Did` every two minutes for as long as the dry run is on would fill
    // the journal with an action that never happened.
    if reading.repo.auto_review_dry_run {
        return ReadStep::Waited(format!(
            "dry run: would read #{} at {}",
            pr.number, pr.head_sha
        ));
    }
    // `Unasked`, deliberately: this fires without anybody present, on every push, which is exactly
    // the spend the day's ceiling exists to bound. `Trigger::Asked` would exempt an unattended
    // engine from the limit written for skein's own initiative.
    //
    // Never `force`: a reading already on disk at this head IS the answer, and re-buying it every
    // pass is the loop this reads the cache to avoid.
    let identities = [reading.viewer.to_string()];
    let said = crate::review::summarise(
        reading.repo,
        pr.slug,
        reading.pr,
        &identities,
        false,
        crate::review::Trigger::Unasked,
    );
    let number = pr.number;
    let head = pr.head_sha;
    match said.depth {
        crate::review::Depth::Unread => ReadStep::Waited(format!(
            "#{number} is not read at {head}: {}",
            said.unread_because
        )),
        // Not computed and not unread means the cache answered. Nothing was spent and nothing is
        // wrong: the step will be chosen again next pass and answer from the cache again, until a
        // condition that depends on the reading moves the workflow on.
        _ if !said.computed => ReadStep::Waited(format!("#{number} is already read at {head}")),
        _ => ReadStep::Did(format!(
            "read #{number} at {head} ({}): {}",
            // Said on the line because it is what decides whether an approval is reachable at all
            // (§7c), and "skein read it" without it is the claim that failed 53 seconds apart.
            match said.swept {
                true => "the sweep accounted for every changed file",
                false => "no sweep accounted for it, so an approval stays out of reach",
            },
            said.line.trim(),
        )),
    }
}

fn add_label(
    slug: &str,
    number: u64,
    label: &str,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    crate::github::send_json(
        "POST",
        &format!("/repos/{slug}/issues/{number}/labels"),
        token,
        &serde_json::json!({ "labels": [label] }),
    )
    .map(|_| ())
}

fn remove_label(
    slug: &str,
    number: u64,
    label: &str,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    // A label may contain a space or a slash. Encoded rather than interpolated raw: a label called
    // `needs review` would otherwise produce a path GitHub answers 404 for, and the workflow would
    // stop on a step that was perfectly well written. The encoder moved to `github::path_segment`
    // so `delete_branch` below can reach it too — it was written here and forgotten there.
    let label = crate::github::path_segment(label);
    crate::github::send_json(
        "DELETE",
        &format!("/repos/{slug}/issues/{number}/labels/{label}"),
        token,
        &serde_json::json!({}),
    )
    .map(|_| ())
}

/// Bring the branch up to date with its base.
///
/// GraphQL, because REST cannot rebase: `PUT …/update-branch` takes `expected_head_sha` and merges,
/// full stop. `updatePullRequestBranch` is what `gh pr update-branch --rebase` calls, and its
/// `updateMethod` is the only way to ask for a rebase over the API at all. Established in
/// `docs/pr-workflow.md`, against GitHub's live schema.
///
/// `expectedHeadOid` is not optional here even though it is in the schema: without it, a push that
/// landed while skein was deciding gets rebased sight unseen.
fn update_branch(
    slug: &str,
    number: u64,
    head_sha: &str,
    how: Update,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    let id = node_id(slug, number, token)?;
    let method = match how {
        Update::Rebase => "REBASE",
        Update::Merge => "MERGE",
    };
    let query = "mutation($id: ID!, $oid: GitObjectID!, $how: PullRequestBranchUpdateMethod!) {\n\
       \x20 updatePullRequestBranch(input: {pullRequestId: $id, expectedHeadOid: $oid, \
         updateMethod: $how}) { pullRequest { headRefOid } }\n\
     }";
    crate::github::graphql(
        query,
        serde_json::json!({ "id": id, "oid": head_sha, "how": method }),
        token,
    )
    .map(|_| ())
}

/// The pull request's GraphQL node id, which the mutation needs and the queue does not carry.
///
/// One extra read, and only on the rare step that rebases. GitHub reads are cheap here — the owner
/// said so explicitly — and adding a field to the queue's search for the sake of an action almost
/// no poll takes would make every poll pay for it.
fn node_id(slug: &str, number: u64, token: &crate::secret::Secret) -> Result<String, String> {
    let pr = crate::github::get_json(&format!("/repos/{slug}/pulls/{number}"), token)?;
    pr.get("node_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("GitHub did not say what #{number}'s node id is"))
}

fn merge_pr(
    slug: &str,
    number: u64,
    head_sha: &str,
    how: MergeAs,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    let method = match how {
        MergeAs::Squash => "squash",
        MergeAs::Merge => "merge",
        MergeAs::Rebase => "rebase",
    };
    crate::github::send_json(
        "PUT",
        &format!("/repos/{slug}/pulls/{number}/merge"),
        token,
        // `sha` is the head skein decided on. GitHub answers 409 if the branch has moved since,
        // which is exactly the answer wanted: somebody pushed, and the decision to merge was made
        // about code that is no longer what would be merged.
        &serde_json::json!({ "merge_method": method, "sha": head_sha }),
    )
    .map(|_| ())
    .map_err(|e| conflicts_stopped_the_train(number, e))
}

/// GitHub's 405 for a conflicted branch, said to somebody reading a stopped train (SKEIN-423).
///
/// This `Err` does not go back to a person who is standing there: [`perform`] turns it into
/// `"{flow} step {n} could not be done: {why}"`, writes it down with [`stop`], and the cockpit
/// draws it days later under **Stopped.** in `revFlowBox` (`src/web/index.html`). Untranslated,
/// what that read was `GitHub said 405: Pull Request has merge conflicts` — SKEIN-411 fixed exactly
/// that sentence for the merge a person presses ([`merge_by_hand`] below, via
/// `prq::it_conflicts_with_its_base`) and scoped itself to that one road; this is the other.
///
/// **The gate is `prq::refused_for_conflicts`, and it is shared on purpose.** Which answers are
/// this refusal is a fact about `crate::github`'s two wrappers, and the rule — match the status
/// skein itself formatted, never GitHub's prose — is written out in full at
/// `prq::it_conflicts_with_its_base`. A second copy here would be a second thing to miss.
///
/// **The words are not shared, because the reader is not the same reader.** The press says
/// "Resolve them on the branch, push, then merge", which is what to do next when your finger is on
/// the button. A stop is read by somebody who was not watching, so this says what happened (the
/// merge was refused, and nothing was merged), what follows from it (the train has stopped and will
/// not try again by itself — [`stop`] is durable and [`perform`] returns early on it), and what to
/// do (resolve, push, then the button that is actually there, which `revFlowBox` labels
/// "let it run again").
///
/// Every other status stops with GitHub's answer verbatim, as it did before. A 409 here is the
/// branch having moved under a decision this train made — real, and not this sentence.
fn conflicts_stopped_the_train(number: u64, said: String) -> String {
    match crate::prq::refused_for_conflicts(&said) {
        false => said,
        true => format!(
            "#{number} conflicts with its base, so GitHub refused the merge and nothing was \
             merged. The train has stopped here and will not try again by itself. Resolve the \
             conflicts on the branch and push, then press \"let it run again\"."
        ),
    }
}

/// The merge a PERSON presses, with the two guards the merge train has and this road did not.
/// (SKEIN-338)
///
/// **There were two merges and they were not equally safe.** The train's went out with `sha`
/// ([`merge_pr`] above) and passed [`crate::workflow::instead_of_merging_off_the_trunk`] on both
/// roads into [`crate::workflow::next`]; the cockpit's merge chip called `prq::merge`, which sent
/// `{"merge_method": …}` and nothing else — no expected head, no base check.
/// `grep -rn instead_of_merging_off_the_trunk src/` found the guard reachable from `workflow.rs`
/// and `prwork.rs` only, never from that route. And `$SKEIN_PR_WORKFLOWS` is **off** on the owner's
/// fleet, so the guarded road was the one nobody was driving: the only merge skein actually offered
/// was the unguarded one.
///
/// What that cost, on the owner's own data: opening step 7 of a stack (base
/// `ladder/tenants-07-auth-cutover`), reading it, and pressing merge would merge step 6 into step 7
/// — SKEIN-237 reproduced by hand, from the surface built for reading pull requests.
///
/// **Here rather than in `prq`, and it is the module graph that decides.** `docs/modules.toml` has
/// `prq.depends_on` without `workflow`, and `prwork` — "the half with consequences… nothing depends
/// on THIS except the tick and the routes" — already depends on both. So the guard composes from
/// where it can see both halves, `tools/module-check.py` needs no new edge, and the thing that
/// merges pull requests stays a leaf.
///
/// **It does not consult `enabled()`, and that is not the oversight it looks like.** `perform`
/// checks the switch because a caller that forgot would be a bug that merges pull requests by
/// itself. `$SKEIN_PR_WORKFLOWS` governs skein acting **unattended**; a person with their finger on
/// the button is not that, and the fleet where the switch is off is exactly the fleet where this
/// path is the only merge there is. Refusing here would remove the merge chip from every fleet that
/// has not opted into automation, which is every fleet the owner runs.
///
/// **Nor does it consult `stopped()`.** A workflow stop is a durable note that the TRAIN has gone
/// as far as it can and needs a person; a person then merging by hand is that note being answered,
/// not overridden. The guards below are the ones that survive a human being certain, because they
/// are about facts rather than about policy: what you are merging, and where it lands.
///
/// The order is deliberate. Base first, then head. A stacked child is wrong to merge at *any* head,
/// so "the branch moved" would be a distraction in front of it — and the reader who re-read and
/// pressed again would get the real refusal on the second press instead of the first.
pub fn merge_by_hand(slug: &str, number: u64, seen_head: &str) -> Result<String, String> {
    // Before any request, because there is no request worth making. An empty `seen_head` means the
    // caller cannot say which commit the person was looking at, and a merge that cannot name its
    // revision is the unguarded merge this function exists to replace — "assume current" is the
    // hole, not the fallback.
    if seen_head.trim().is_empty() {
        return Err(format!(
            "skein does not know which commit of #{number} you are looking at, and will not merge \
             a revision it cannot name. Refresh the queue and read the change again."
        ));
    }
    // One request for both facts, live. Not the queue: `base_ref` moves under a stacked child the
    // moment its parent lands, and `head_sha` moves on every push, so a merge decided from a
    // sixty-second cache is a merge decided from a photograph. An `Err` stops the merge — see
    // `prq::base_and_head` for why this is the one read whose failure must not fall back.
    let (base_ref, live_head) = crate::prq::base_and_head(slug, number)?;
    // The same memoised answer `prq::queue` uses, so the two roads to a merge cannot disagree about
    // what this repository's trunk is. `""` is "not known", which is `None` and not `false` — see
    // `crate::workflow::Facts::base_is_trunk`.
    let trunk = crate::prq::trunk_of(slug);
    let base_is_trunk = match trunk.is_empty() {
        true => None,
        false => Some(base_ref == trunk),
    };
    if let Some(instead) = crate::workflow::merging_off_the_trunk(base_is_trunk) {
        // `Flag` and `Wait` mean different things to a train — one is durable and one clears itself
        // — and exactly the same thing to a person standing at the button: not this, not now. The
        // sentence is the shared one so both roads refuse in the same words. Anything this rule
        // ever grows is ALSO a refusal here: a new answer that fell through to the merge below
        // would fail open on the one act that cannot be taken back.
        let why = match &instead {
            Act::Flag(why) | Act::Wait(why) => why.clone(),
            other => crate::workflow::spell_act(other),
        };
        return Err(format!("#{number} is based on {base_ref} — {why}"));
    }
    // Checked here as well as sent as `sha`, and both are wanted. This one can say what the head
    // moved TO, which GitHub's 409 cannot; the `sha` on the wire closes the window between this
    // check and the merge, which no check up here can. Neither is redundant — one is a better
    // sentence and the other is the guarantee.
    if live_head != seen_head {
        return Err(format!(
            "the branch moved since you read it — you read {}, #{number} is now at {}. Read the \
             new code, then merge.",
            short(seen_head),
            short(&live_head)
        ));
    }
    crate::prq::merge(slug, number, seen_head)
}

/// Enough of a sha to recognise, for a sentence a person reads. `get` rather than a slice so a
/// short or empty sha is returned whole instead of panicking on a merge refusal.
fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Delete the head branch of a pull request that has just been merged.
///
/// `head_ref` is `headRefName` as GitHub reported it — the *author's* string, not skein's — so it
/// is encoded a segment at a time rather than interpolated. A branch called `release#2` used to
/// issue `DELETE /repos/o/r/git/refs/heads/release`, because curl never puts a fragment on the
/// wire: the wrong branch deleted, and the train then reporting that it had deleted `release#2`.
///
/// Split on `/` and encoded per part, because a ref legitimately contains slashes (`feat/x`) and
/// GitHub's refs endpoint takes them as path separators — so `%2F` there would 404 every branch
/// anybody has ever named after a topic.
fn delete_branch(slug: &str, head_ref: &str, token: &crate::secret::Secret) -> Result<(), String> {
    let head_ref = head_ref
        .split('/')
        .map(crate::github::path_segment)
        .collect::<Vec<_>>()
        .join("/");
    crate::github::send_json(
        "DELETE",
        &format!("/repos/{slug}/git/refs/heads/{head_ref}"),
        token,
        &serde_json::json!({}),
    )
    .map(|_| ())
}

/// How long the front of a serial train may wait on something skein cannot see running.
///
/// Twenty minutes, and the number is chosen against the two things it must not get wrong. It has
/// to be long enough that a check which is merely slow to be QUEUED is never mistaken for one that
/// is not coming — GitHub Actions starts within seconds normally, and minutes on a busy runner
/// pool — and short enough that a person watching a train notices the same day. A wait on
/// something that IS running is bounded by [`WAITING_ON_A_CHECK_MS`] instead, which is more than
/// seventy times as long, so no CI run is ever cut short by this one.
pub const WAITING_ON_NOTHING_MS: i64 = 20 * 60 * 1000;

/// How long the front of a serial train may wait on a check that HAS started and has not finished.
///
/// **Twenty-four hours, and the number is GitHub's own** rather than a guess about how slow a
/// pipeline is allowed to be. GitHub cancels a job that has been running for six hours (the
/// default `timeout-minutes: 360`), and cancels one that has sat unassigned to a runner for
/// twenty-four. A check still reported as `pending` past the longer of those has outlived every
/// bound GitHub itself applies to one, so it is not a slow run: whatever was going to report it is
/// gone, and nothing that happens on GitHub will ever move it (SKEIN-283).
///
/// That this needs to exist at all is the point. `checks: pending` was treated as proof that
/// something was in flight and therefore not bounded at all — which is true of a check that is
/// running and false of a check that has *stopped* running without saying so, and those two look
/// identical from here. A required context whose run was deleted, a self-hosted runner that went
/// away mid-job, a check GitHub is waiting on that will never be posted: each parks the front of a
/// serial train for ever, and everything behind it with it, saying "CI is running" about nothing.
/// The twenty-minute rule below cannot reach any of them, because they all say `pending`.
///
/// **The cost of getting it wrong is deliberately lopsided.** Too long, and a broken pipeline
/// parks a train until tomorrow — bad, and exactly the state this leaves it in today, for ever.
/// Too short, and somebody presses "let it run again" and the wait restarts with a fresh clock,
/// having lost one pass. There is no honest CI run this can cut short: at twenty-four hours GitHub
/// has already cancelled it.
pub const WAITING_ON_A_CHECK_MS: i64 = 24 * 60 * 60 * 1000;

/// When this pull request started waiting on THIS step, if it is still waiting on it.
///
/// The NEWEST entry is the whole answer, and that is the point: anything at all having happened
/// since — an act, a flag, a person clearing a stop, a wait on a different step — means the wait
/// that was being timed ended, and whatever is being waited on now starts its own clock. So a
/// train that is making progress can never accumulate patience across the steps it walked through.
fn waiting_since(entries: &[JournalEntry], flow: &str, step: usize) -> Option<i64> {
    entries
        .last()
        .filter(|e| e.kind == "waiting" && e.flow == flow && e.step == step)
        .map(|e| e.at_ms)
}

/// The front of a serial train said `wait`. Start its clock, or stop it because the clock ran out.
///
/// **This is not a stale-state bug and the fix is not a re-read** (SKEIN-240). `prq::rollup`
/// answers `"none"` when nothing has ever run against a commit, and that reading is CORRECT and
/// CURRENT — it is the same answer whether CI is five seconds away or will never come, because
/// nothing GitHub sends distinguishes "no check yet" from "no check, ever, on this repository".
/// Asking again produces the same true answer for ever. The documented train has no step for
/// `checks:none` once its label is on, so the front falls to the catch-all `wait:` and holds the
/// line at one pass per two minutes, for ever, saying *"waiting for GitHub to catch up"* when
/// GitHub caught up long ago. `docs/pr-workflow.md` names exactly this: *"The failure mode to
/// avoid is not the stall. It is a **silent** stall."*
///
/// So the only thing that can tell those two apart is how long the waiting has gone on, and this
/// is where that is decided.
///
/// **Two ceilings, because there are two waits.** A wait with nothing behind it — no check
/// running, nothing in flight skein can point at, and a sentence promising something is going to
/// change — runs out after [`WAITING_ON_NOTHING_MS`]. A wait on a check that HAS started runs out
/// after [`WAITING_ON_A_CHECK_MS`], which is more than seventy times as long: the module note
/// above builds the whole guarded-step design around surviving *"a forty-minute CI run"*, and
/// cutting one short would be a worse bug than either of the ones this fixes.
///
/// It is one function and not two because it is one decision — *how long may this go on* — and the
/// only thing the evidence changes is the number. Written as a second mechanism beside the first,
/// the two would keep separate clocks over the same journal and disagree about when a wait began.
///
/// **`pending` earns patience, not immunity.** It used to end this function on the spot, on the
/// reading that a check which has started is something skein can expect to end. That is true of a
/// check that is running and false of one that has stopped running without saying so — a deleted
/// run, a self-hosted runner that went away mid-job, a required context nobody will ever post —
/// and from here the two are the same word. So `pending` was an unbounded park, which is the state
/// this rule exists to prevent, reachable by saying the one thing that switched the rule off
/// (SKEIN-283).
///
/// The clock does not restart when the evidence changes, and it does not need to: it can only ever
/// move the ceiling under a wait already in progress. Nothing running for nineteen minutes and
/// then a check starts, and the ceiling rises to a day — less eager, never a stop. A check pending
/// for twenty-three hours and then it vanishes, and the ceiling drops to twenty minutes it has
/// long since passed — a stop, on a pull request that has waited twenty-three hours with nothing
/// running. Neither direction can stop something that was going to resolve on its own.
///
/// **Only a serial train.** A stop is a demand for somebody's attention, and it is earned when the
/// alternative is a queue that has stopped moving. A pull request on a workflow that blocks nobody
/// is not costing anything by waiting, and stopping it would be manufacturing work.
///
/// Recoverable, in the two ways that matter: the stop names the elapsed time and the step so the
/// sentence is checkable, and clearing it puts the pull request back in line — where, if the wait
/// really was on something slow, it simply waits again with a fresh clock.
fn a_wait_that_will_not_end_on_its_own(
    repo_id: &str,
    number: u64,
    flow: &Workflow,
    chosen: &Chosen,
    facts: &crate::workflow::Facts,
    why: &str,
) -> Option<String> {
    if !flow.serial {
        return None;
    }
    // The one thing the evidence decides. `pending` is the only answer that means a check has
    // started; `passing`, `failing`, `none` and the empty string all mean nothing is in flight.
    let running = facts.checks == "pending";
    let ceiling = match running {
        true => WAITING_ON_A_CHECK_MS,
        false => WAITING_ON_NOTHING_MS,
    };
    let step = chosen.step + 1;
    let now_ms = now_ms();
    let Some(since) = waiting_since(&journal(repo_id, number), &flow.name, step) else {
        // The first pass on this step: put the clock down and say nothing. A wait is the ordinary
        // state of a train and this entry is what makes it a *timed* one.
        record(repo_id, number, &flow.name, step, "waiting", why);
        return None;
    };
    let waited = now_ms - since;
    if waited < ceiling {
        return None;
    }
    // The sentence says which of the two ceilings fell and what would have had to be true for it
    // to be wrong, because that is what makes it checkable by the person it interrupts.
    let reason = match running {
        true => format!(
            "step {step} has been waiting {hours} hours — {why:?} — and its checks have read \
             `pending` that whole time. GitHub cancels a job that has run for six hours and one \
             that has waited twenty-four for a runner, so a check still pending past both is not \
             a slow build: whatever was going to report it is gone. Look for a cancelled or \
             deleted run, or a required check nothing posts. Clearing this stop puts it back in \
             line with a fresh clock.",
            hours = waited / 3_600_000,
        ),
        false => format!(
            "step {step} has been waiting {minutes} minutes — {why:?} — and nothing is running \
             (checks: {}). Whatever was expected to start has not, so this wait will not end on \
             its own; if a label is meant to start CI here, check it is the one the repository's \
             workflow keys on. Clearing this stop puts it back in line.",
            match facts.checks.is_empty() {
                true => "none",
                false => facts.checks.as_str(),
            },
            minutes = waited / 60_000,
        ),
    };
    stop(repo_id, number, &reason);
    record(repo_id, number, &flow.name, step, "stopped", &reason);
    crate::warden_client::reported(
        &format!("pr-workflow:{}", flow.name),
        &format!("stopped waiting on #{number}"),
        &reason,
    );
    Some(reason)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One serial workflow's train in one repo: who is in line, who is at the front, who has been
/// passed over — the answer to "what is it working on, and which step is everyone else waiting
/// behind".
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrainView {
    /// The serial workflow's name.
    pub flow: String,
    /// The one pull request that may act this pass: the oldest carrying PR without a stop. `None`
    /// when the line is empty or everyone in it is stopped — a train with nobody to move.
    pub front: Option<u64>,
    /// Every carrying pull request in train order — oldest first, lowest number — front included.
    pub line: Vec<u64>,
    /// Only this flow's carrying pull requests that are stopped, with their reasons, in train
    /// order. The passed-over, not the whole repo's stop file.
    pub stopped: Vec<crate::prq::StoppedPr>,
}

/// Every serial workflow's train in this repo, from the same ordering-and-front rule the tick
/// acts on.
///
/// `prs` is (number, carried flow name) for the non-archived carrying pull requests — the caller
/// has already decided who carries what, because that needs facts this function should not
/// re-derive. This is the ONE place the train order and the front are computed: [`sweep`] calls
/// through it before acting, so a panel drawn from it cannot disagree with what the tick then
/// does. A non-serial workflow gets no view — a train is the serial thing.
pub fn trains(repo_id: &str, prs: &[(u64, String)], flows: &[Workflow]) -> Vec<TrainView> {
    let stops = read_stops(repo_id);
    flows
        .iter()
        .filter(|flow| flow.serial)
        .map(|flow| {
            // Oldest first — lowest number, the sort key the owner chose.
            let mut line: Vec<u64> = prs
                .iter()
                .filter(|(_, name)| *name == flow.name)
                .map(|(number, _)| *number)
                .collect();
            line.sort_unstable();
            // The first one without a stop is the front; a stopped PR is passed over — the "skip
            // failures and move ahead" (docs/pr-workflow.md, "The merge train").
            let front = line
                .iter()
                .copied()
                .find(|number| !stops.contains_key(&number.to_string()));
            let stopped = line
                .iter()
                .filter_map(|number| {
                    stops
                        .get(&number.to_string())
                        .map(|why| crate::prq::StoppedPr {
                            number: *number,
                            why: why.clone(),
                        })
                })
                .collect();
            TrainView {
                flow: flow.name.clone(),
                front,
                line,
                stopped,
            }
        })
        .collect()
}

/// One pass over the fleet: every repo skein manages, every pull request a workflow governs, one
/// step each.
///
/// **One step per pull request per pass, and the pass is the only thing that acts.** After an
/// action lands, what skein believes about that pull request is one action out of date — the label
/// is on but no check has been queued, so `checks:passing` is still true from the previous run. The
/// next pass re-reads GitHub, which is the only thing that can say what the action did.
///
/// **Every repo in the registry**, not only ones whose queue somebody has opened — the owner's
/// decision, and what makes this automation rather than a thing you have to remember to visit. A
/// repo with no workflow claiming anything costs one cached queue read.
///
/// Returns what it did, for the server's log. Every action is also in the host audit with its
/// authority; this is the line a person watching a terminal sees.
/// How many readings one pass may buy, across the whole fleet.
///
/// **One**, and the number comes from the tick rather than from a taste for caution. The pass runs
/// every 120 seconds and a reading is most of a minute, so one keeps a pass comfortably inside its
/// own interval; two could leave the next tick waiting on the last, with the merge train's
/// second-long steps queued behind a stack of model calls.
///
/// Burst control, not a budget. The budget is `Config::review_reads_per_day`, which this spends
/// from like every other reading — this only decides how fast. A queue where ten pull requests
/// come into scope at once therefore takes ten passes, twenty minutes, which for something nobody
/// is waiting at a keyboard for is the right trade.
const READINGS_PER_SWEEP: usize = 1;

pub fn sweep() -> Vec<String> {
    // Nothing at all when the switch is off — not even a queue read. A feature that is switched off
    // should be invisible in every way somebody might notice, including a rate limit.
    if !enabled() {
        return Vec::new();
    }
    let flows = match crate::workflow::load() {
        Ok(flows) => flows,
        // A file with one bad step loads none of them (`workflow::from_bytes`), which is the right
        // answer and a silent one — so it is said here, where somebody watching the server sees it.
        Err(why) => {
            eprintln!("skein: no workflow is running — {why}");
            return Vec::new();
        }
    };
    if flows.is_empty() {
        return Vec::new();
    }
    let token = match crate::prq::host_token() {
        Ok(token) => token,
        Err(why) => {
            eprintln!("skein: workflows are on, and there is no GitHub token to act with — {why}");
            return Vec::new();
        }
    };

    let mut did = Vec::new();
    // Across every repo, not per repo: the thing being protected is the pass, and a pass that
    // spent a minute on repo A's reading has that minute gone whether repo B reads anything.
    let mut spent_readings = 0usize;
    for repo in crate::repos::load_repos() {
        let Ok(queue) = crate::prq::queue(&repo, false) else {
            // A queue that cannot be read is not a reason to stop the fleet's other repos. The
            // review pane reports the failure with its reason; this pass simply has nothing to
            // decide from.
            continue;
        };
        // Who carries what, decided once for the whole repo before anyone may act: a serial
        // workflow's rule below is about the *whole* train, and a decision made one pull request
        // at a time could not see past the one in hand.
        let mut rows = Vec::new();
        for pr in &queue.prs {
            // A pull request you set aside is one you said "not now" about. A workflow acting on it
            // would be overruling that with a rule, which is the opposite of what setting aside is
            // for — and the row that says "archived" would be acting.
            if matches!(pr.lane, crate::prq::Lane::Archived) {
                continue;
            }
            // The repo-aware one: this is the pass that ACTS, so it is the one that must be
            // able to see skein's own reading rather than wait on a fact it declined to look up.
            let facts = facts_of_in(&repo.id, pr, &queue.viewer, &queue.trunk);
            // `acting`, not `name` (SKEIN-279): a workflow whose own `matches` do not hold is
            // shown on the row and takes no part in this pass — no step, no stop, no clock, and
            // no place in a serial train's line, where standing at the front unable to act would
            // hold up everything behind it.
            let Some(name) = carries(&repo.id, pr.number, &facts, &flows)
                .acting()
                .map(str::to_string)
            else {
                continue;
            };
            rows.push((pr, facts, name));
        }
        // The front of each serial train: carrying pull requests oldest-first (lowest number —
        // the sort key the owner chose), and the first one without a stop is the only one that
        // may act this pass. A stopped front is passed over rather than reported — that is the
        // "skip failures and move ahead" — and everyone behind the front is simply waiting, which
        // is the ordinary state of a train and not an event (`docs/pr-workflow.md`, "The merge
        // train"). A workflow whose every carrying PR is stopped has no front, and nobody acts.
        //
        // Computed by [`trains`] — the same function the cockpit's train panel reads — so what a
        // person is shown and what the tick then does cannot be two computations that drift apart.
        let carrying: Vec<(u64, String)> = rows
            .iter()
            .map(|(pr, _, name)| (pr.number, name.clone()))
            .collect();
        let fronts: std::collections::BTreeMap<String, u64> = trains(&repo.id, &carrying, &flows)
            .into_iter()
            .filter_map(|train| train.front.map(|front| (train.flow, front)))
            .collect();
        let mut acted_in_repo = false;
        for (pr, facts, name) in &rows {
            let Some(flow) = flows.iter().find(|f| &f.name == name) else {
                continue;
            };
            // Everyone but the front of a serial train is passed over: no action, and no stop —
            // being behind the front is where a train's pull requests live, not a fault.
            if flow.serial && fronts.get(name) != Some(&pr.number) {
                continue;
            }
            let Some(chosen) = crate::workflow::next(flow, facts) else {
                continue;
            };
            // Burst control, and the only act in this pass that needs any: every other one is an
            // HTTP call taking a second, and a reading is most of a minute. Left uncapped, a repo
            // where a dozen pull requests came into scope at once would spend the pass on model
            // calls while a green, approved pull request three repos along waited behind them.
            //
            // Counted in readings SPENT, below, not in `read` steps taken: a step that answers
            // from the cache costs nothing and must not use the allowance up. Nothing is lost when
            // it bites — the pull request is unread, so the same step is chosen next pass.
            if matches!(chosen.act, Act::Read) && spent_readings >= READINGS_PER_SWEEP {
                eprintln!(
                    "skein: {} #{} is due a reading, and this pass has already spent its {} — \
                     next pass",
                    repo.id, pr.number, READINGS_PER_SWEEP
                );
                continue;
            }
            let subject = Subject {
                repo_id: &repo.id,
                slug: &queue.slug,
                number: pr.number,
                head_sha: &pr.head_sha,
                head_ref: &pr.head_ref,
                // The reviewer's half. Everything it carries is already in hand here, which is
                // why `Act::Read` is takeable from this caller and from no other.
                reading: Some(Reading {
                    repo: &repo,
                    pr,
                    viewer: &queue.viewer,
                    facts,
                }),
            };
            match perform(&subject, flow, &chosen, &token) {
                Outcome::Did(what) => {
                    if matches!(chosen.act, Act::Read) {
                        spent_readings += 1;
                    }
                    did.push(format!("{}: {what}", repo.id));
                    acted_in_repo = true;
                }
                // Waiting is the ordinary state and says nothing — but the front of a serial
                // train waiting on nothing is the line not moving, so it is timed. See
                // [`a_wait_that_will_not_end_on_its_own`], which is the only thing standing between a
                // repo whose CI label starts nothing and a train parked for ever.
                Outcome::Waited(why) => {
                    a_wait_that_will_not_end_on_its_own(
                        &repo.id, pr.number, flow, &chosen, facts, &why,
                    );
                }
                // A stop has already been written down and audited by `perform`; repeating it here
                // every pass would bury the log.
                Outcome::Stopped(_) => {}
            }
        }
        // The queue is cached for a minute, and skein has just changed the thing it describes. Left
        // alone, the next pass would decide from facts it had itself made stale — which is the one
        // input a cascade needs to merge on a check that has not run.
        if acted_in_repo {
            crate::prq::invalidate(&repo.id);
        }
    }
    did
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The credential every stub GitHub below is called with.
    ///
    /// Prefixed `skein-test-` deliberately: a fixture that looked like a real token
    /// (`gho_…`, `ghp_…`) is indistinguishable from one in a grep, and this tree has already had
    /// to sweep a client's real strings out of its fixtures once.
    fn fixture_token() -> crate::secret::Secret {
        crate::secret::Secret::new("skein-test-github-token")
    }
    use crate::workflow::Merge;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// A GitHub that records what skein sent it, and answers however the test says.
    ///
    /// Every assertion here is about a request that CHANGES somebody's repository, so what is
    /// checked is the wire: the method, the path, and the body. A doer tested through its own
    /// return value would pass while merging with the wrong method, or without the head it decided
    /// on — which is the failure that matters, because that one merges a commit nobody looked at.
    fn github(status: u16) -> (String, Arc<Mutex<Vec<String>>>) {
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let body = said
                    .split("\r\n\r\n")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                seen.lock().unwrap().push(format!("{head} {body}"));
                // The node id lookup always works: it is not what any of these tests are about.
                // GraphQL answers in GraphQL's shape, because `github::graphql` reads `data` and
                // would report a perfectly good mutation as a failure otherwise.
                let (status, answer) = if head.starts_with("GET") && head.contains("/pulls/") {
                    (200, r#"{"node_id":"PR_node"}"#.to_string())
                } else if head.contains("/graphql") {
                    (
                        status,
                        r#"{"data":{"updatePullRequestBranch":{"pullRequest":{"headRefOid":"new"}}}}"#
                            .to_string(),
                    )
                } else {
                    (status, r#"{"merged":true}"#.to_string())
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
        (base, heard)
    }

    fn flow() -> Workflow {
        crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"ship-mine","steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap()
        .remove(0)
    }

    fn subject(head_sha: &str) -> Subject<'_> {
        Subject {
            repo_id: "demo",
            slug: "acme/thing",
            number: 41,
            head_sha,
            head_ref: "feat",
            // No reading: this helper stands in for every act but `read`, and a `read` step taken
            // from here must refuse rather than quietly invent a repo (see `Reading`).
            reading: None,
        }
    }

    fn chosen(act: Act) -> Chosen {
        Chosen { step: 3, act }
    }

    /// **A branch name reaches GitHub as one path segment, and a `#` in it does not truncate the
    /// URL into a different branch.**
    ///
    /// `head_ref` is `headRefName` as GitHub reports it, so its characters are the pull request
    /// author's choice, not skein's. Git forbids `~ ^ : ? * [ \` in a ref and allows `#` — and
    /// curl never puts a fragment on the wire, so `DELETE …/heads/release#2` used to arrive at
    /// GitHub as `DELETE …/heads/release`. That deletes a branch nobody asked about, on a
    /// repository where `release` exists, and the train then says it deleted `release#2`.
    ///
    /// The ordinary half is the half that makes the first one worth anything: a ref really does
    /// contain slashes, and those must stay separators or every topic branch 404s. The two
    /// together are why this is a *segment* encoder applied per part, and not `encode(head_ref)`.
    #[test]
    fn a_branch_name_reaches_github_as_one_path_segment() {
        let _env = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let deleted = |head_ref: &str| {
            heard.lock().unwrap().clear();
            let subject = Subject {
                repo_id: "demo",
                slug: "acme/thing",
                number: 41,
                head_sha: "abc123",
                head_ref,
                reading: None,
            };
            let out = perform(
                &subject,
                &flow(),
                &chosen(Act::Merge(Merge {
                    how: MergeAs::Squash,
                    delete_branch: true,
                })),
                &fixture_token(),
            );
            assert!(matches!(out, Outcome::Did(_)), "{out:?}");
            let said = heard.lock().unwrap().clone();
            said.iter()
                .find(|s| s.starts_with("DELETE /repos/acme/thing/git/refs/heads/"))
                .unwrap_or_else(|| panic!("no branch was deleted: {said:?}"))
                .clone()
        };

        let hostile = deleted("release#1");
        assert!(
            hostile.starts_with("DELETE /repos/acme/thing/git/refs/heads/release%231 "),
            "a `#` in a branch name still steers the request at another branch: {hostile}"
        );

        let ordinary = deleted("feat/nested/name");
        assert!(
            ordinary.starts_with("DELETE /repos/acme/thing/git/refs/heads/feat/nested/name "),
            "a topic branch's slashes were encoded, so every branch with one now 404s: {ordinary}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// The owner's example, walked to merged by the tick alone, with nothing open.
    ///
    /// The claim the whole feature makes: a pull request that is approved and green ends up merged
    /// without anybody pressing anything. Driven through `sweep` against a GitHub that answers from
    /// a fixture and CHANGES as skein acts on it — a label appears when skein adds one, checks go
    /// green once it is there — because a stub that answers the same thing every time cannot tell a
    /// workflow that advances from one that is stuck in a loop taking the same step.
    ///
    /// The other half of the claim is that it takes ONE step per pass. After an action lands, what
    /// skein believes is one action out of date, so a pass that kept going would decide the next
    /// step from facts it had just made stale — a label added, no check yet queued, `checks:passing`
    /// still true from the previous run, and it merges.
    #[test]
    fn the_tick_walks_a_pull_request_to_merged_one_step_per_pass() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The workflow, as the owner described it.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"ship-mine","matches":["mine"],"steps":[
              {"when":["approved","no-label:ci"],"do":"add-label:ci"},
              {"when":["checks:pending"],"do":"wait:CI is running"},
              {"when":["checks:failing"],"do":"flag:CI is red"},
              {"when":["approved","mergeable","checks:passing"],"do":"merge:squash+delete"},
              {"when":["approved","not-mergeable"],"do":"update-branch:rebase"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub whose answers move as skein acts on it.
        // **Green before the label goes on**, which is the state that makes "one step per pass" a
        // property with teeth. The branch passed CI on an earlier run, so `checks:passing` is true
        // AND the label is missing — both step 1 and step 4 apply at once. A pass that kept going
        // would add the label and then merge, in the same breath, on a check run that predates it.
        let state: Arc<Mutex<(bool, String)>> = Arc::new(Mutex::new((false, "passing".into())));
        let merged = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (world, seen) = (state.clone(), merged.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let (labelled, checks) = world.lock().unwrap().clone();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    // What the repository's default branch is. A real GitHub answers this and a
                    // stub that did not used to be harmless — until a merge started requiring
                    // skein to KNOW the base it is shipping into
                    // ([`crate::workflow::instead_of_merging_off_the_trunk`]), at which point a
                    // fixture with no trunk is a fixture where nothing may merge.
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/labels") {
                    // The label lands, and this repository's CI starts on it.
                    *world.lock().unwrap() = (true, "pending".into());
                    "[]".to_string()
                } else if head.contains("/merge") {
                    seen.lock().unwrap().push(head.clone());
                    r#"{"merged":true}"#.to_string()
                } else if head.starts_with("DELETE") {
                    seen.lock().unwrap().push(head.clone());
                    "{}".to_string()
                } else if head.contains("/graphql") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{{"number":7,"title":"t","url":"u",
                          "isDraft":false,"author":{{"login":"me"}},"headRefName":"feat",
                          "headRefOid":"abc","baseRefName":"main",
                          "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                          "mergeable":"MERGEABLE",
                          "labels":{{"nodes":[{}]}},
                          "latestReviews":{{"nodes":[]}},
                          "commits":{{"nodes":[{{"commit":{{
                             "committedDate":"2026-08-23T00:00:00Z",
                             "statusCheckRollup":{{"contexts":{{"nodes":[{}]}}}}}}}}]}}}}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        match labelled {
                            true => r#"{"name":"ci"}"#,
                            false => "",
                        },
                        match checks.as_str() {
                            "pending" => r#"{"status":"IN_PROGRESS"}"#,
                            "passing" => r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#,
                            _ => "",
                        },
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // **A pull request you set aside is left alone**, even by a rule that claims it. Setting
        // aside is a person saying "not now" about this one; a workflow acting on it would overrule
        // that with a rule, and the row would say "archived" while skein merged it.
        std::fs::create_dir_all(crate::prq::review_dir("demo")).unwrap();
        std::fs::write(crate::prq::review_dir("demo").join("archived.json"), b"[7]").unwrap();
        assert!(
            sweep().is_empty(),
            "a pull request that was set aside was acted on anyway"
        );
        std::fs::write(crate::prq::review_dir("demo").join("archived.json"), b"[]").unwrap();

        // Pass one: approved, unlabelled. The label that starts CI.
        let did = sweep();
        assert_eq!(did.len(), 1, "a pass took more than one step: {did:?}");
        assert!(did[0].contains("label"), "{did:?}");
        assert!(
            merged.lock().unwrap().is_empty(),
            "it merged in the same pass that started CI — on a check that had not run"
        );

        // Pass two: CI is running. Waiting is not an action, so nothing is reported and nothing is
        // done — and above all it does not merge.
        assert!(
            sweep().is_empty(),
            "waiting for CI was reported as doing something"
        );
        assert!(merged.lock().unwrap().is_empty());

        // CI goes green.
        state.lock().unwrap().1 = "passing".into();
        let did = sweep();
        assert_eq!(did.len(), 1, "{did:?}");
        assert!(did[0].contains("merged #7"), "{did:?}");
        let calls = merged.lock().unwrap().clone();
        assert!(
            calls.iter().any(|c| c.contains("/pulls/7/merge"))
                && calls.iter().any(|c| c.contains("git/refs/heads/feat")),
            "the branch did not go with the merge: {calls:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// Which workflow governs a pull request, and who gets the last word.
    ///
    /// The owner's answer was "rules, plus a per-PR override" — and the override matters more than
    /// it looks, because the tick sweeps every repo in the registry. Without a way to say "not this
    /// one" about a single pull request, the only way to exclude one is to edit the rule for
    /// everybody, which is how a rule stops being written honestly.
    ///
    /// So the choice on a row wins in BOTH directions: it can put a workflow on a pull request no
    /// rule claims, and it can keep every rule off one.
    #[test]
    fn the_row_has_the_last_word_over_a_rule() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[
              {"name":"ship-mine","matches":["mine"],"steps":[{"when":[],"do":"merge:squash"}]},
              {"name":"by-hand","steps":[{"when":[],"do":"flag:look at this"}]}]}"#,
        )
        .unwrap();
        let mine = crate::workflow::Facts {
            mine: true,
            ..Default::default()
        };
        let theirs = crate::workflow::Facts {
            mine: false,
            ..Default::default()
        };

        // A rule claims what it matches, and nothing else.
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Matched("ship-mine".into())
        );
        assert_eq!(carries("demo", 2, &theirs, &flows), Carries::Nothing);

        // A workflow with no rule of its own is never picked up by matching — it exists to be
        // chosen, and choosing it works on a pull request no rule would have claimed.
        assign("demo", 2, "by-hand").unwrap();
        assert_eq!(
            carries("demo", 2, &theirs, &flows),
            Carries::Assigned("by-hand".into())
        );

        // And the row overrules a rule that would otherwise have claimed it.
        assign("demo", 1, "by-hand").unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Assigned("by-hand".into())
        );

        // "No workflow" is a thing you can say, and it is not the same as saying nothing. This is
        // the one that keeps a fleet-wide rule usable.
        assign("demo", 1, "").unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Excluded,
            "a rule reclaimed a pull request that was excluded by hand"
        );

        // Clearing the choice is different again: the rules speak for it once more.
        unassign("demo", 1).unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Matched("ship-mine".into())
        );

        // An assignment naming a workflow that has since been deleted acts on nothing, rather than
        // guessing which of the survivors was meant.
        assign("demo", 3, "the-one-that-was-deleted").unwrap();
        assert_eq!(carries("demo", 3, &mine, &flows), Carries::Nothing);

        std::env::remove_var("SKEIN_HOME");
    }

    /// Letting a stopped workflow run again does not also take the workflow off.
    ///
    /// The bug this exists for was written and found within an hour: "let it run again" sends no
    /// workflow name, no name meant "forget the choice", and so one button quietly did two things —
    /// the second being to un-assign the workflow somebody had chosen. On a fleet where a rule
    /// would then re-claim the pull request, that is a change of behaviour nobody asked for,
    /// arriving through a button labelled something else.
    #[test]
    fn clearing_a_stop_does_not_change_what_governs_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"by-hand","steps":[{"when":[],"do":"flag:hi"}]}]}"#,
        )
        .unwrap();
        let facts = crate::workflow::Facts::default();

        apply("demo", 5, Some("by-hand"), false, false).unwrap();
        stop("demo", 5, "CI is red");
        assert!(stopped("demo", 5).is_some());

        // The button, and only the button.
        apply("demo", 5, None, false, true).unwrap();
        assert_eq!(stopped("demo", 5), None, "the stop was not cleared");
        assert_eq!(
            carries("demo", 5, &facts, &flows),
            Carries::Assigned("by-hand".into()),
            "letting it run again silently took the workflow off it"
        );

        // And forgetting the choice is its own request, which still works.
        apply("demo", 5, None, true, false).unwrap();
        assert_eq!(carries("demo", 5, &facts, &flows), Carries::Nothing);

        std::env::remove_var("SKEIN_HOME");
    }

    /// What GitHub said becomes what a workflow sees, and UNKNOWN survives the trip.
    ///
    /// The queue is the only source of facts, so anything lost here is lost to every decision. Two
    /// things are easy to get wrong and both are asserted:
    ///
    /// * `approved` and `review_requirement_met` are two facts and must stay two. This comment
    ///   used to say "approved is the REPOSITORY's verdict, not yours", and this test used to
    ///   assert exactly that — see
    ///   [`an_approval_is_still_an_approval_where_the_repository_asks_for_none`] for what it cost.
    /// * UNKNOWN is not "cannot be merged". GitHub says it for a while after every push; read as a
    ///   conflict it rebases on a guess, and that rebase costs the approval authorising the merge
    ///   on any repository that dismisses stale approvals.
    #[test]
    fn what_github_said_becomes_what_a_workflow_sees() {
        // Built from JSON rather than a struct literal: `Pr` gains fields regularly, and a literal
        // is the thing that stops compiling for a reason unrelated to what is being tested. How the
        // fields get there from GitHub's own answer is prq's to prove, and it does.
        let pr = |decision: &str, mergeable: Option<bool>| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "Me", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": ["ci", "needs docs"],
                "review_decision": decision,
                "mergeable": mergeable,
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        let approved = facts_of(&pr("APPROVED", Some(true)), "me", "main");
        assert!(
            approved.approved,
            "the repository approved it and skein did not see that"
        );
        assert!(!approved.changes_requested);
        assert_eq!(approved.mergeable, Some(true));
        // Labels come through by name, including one with a space in it — which also has to survive
        // being put back in a URL when a step removes it.
        assert_eq!(
            approved.labels,
            vec!["ci".to_string(), "needs docs".to_string()]
        );
        assert!(
            approved.mine,
            "the author is the viewer, in whatever case GitHub spells it"
        );

        let conflicting = facts_of(&pr("REVIEW_REQUIRED", Some(false)), "me", "main");
        assert!(!conflicting.approved);
        assert_eq!(conflicting.mergeable, Some(false));
        assert_eq!(
            conflicting.review_requirement_met,
            Some(false),
            "`REVIEW_REQUIRED` is a requirement that exists and is not met — the one review state \
             where `approved` and `review-satisfied` really do move together"
        );
        assert_eq!(
            approved.review_requirement_met,
            Some(true),
            "`APPROVED` is the repository's requirement being met, and that fact must survive the \
             trip whole: on a protected branch it is the only thing that knows about CODEOWNERS"
        );

        // The one that matters.
        let unknown = facts_of(&pr("APPROVED", None), "me", "main");
        assert_eq!(
            unknown.mergeable, None,
            "an unknown mergeable state was given an answer on the way to the workflow"
        );
        assert!(
            !crate::workflow::holds(&crate::workflow::Cond::NotMergeable, &unknown)
                && !crate::workflow::holds(&crate::workflow::Cond::Mergeable, &unknown),
            "unknown satisfied one of the two conditions it must satisfy neither of"
        );

        // Changes requested is its own state, not the absence of approval.
        let blocked = facts_of(&pr("CHANGES_REQUESTED", Some(true)), "me", "main");
        assert!(blocked.changes_requested && !blocked.approved);

        // And somebody else's pull request is not yours, however it is spelled.
        assert!(!facts_of(&pr("APPROVED", Some(true)), "someone-else", "main").mine);
    }

    /// **An approval is still an approval where the repository asks for no review** (SKEIN-339).
    ///
    /// The state the owner's whole fleet was in and no test described: `reviewDecision` is `""`,
    /// because there is no branch protection to satisfy, and a person has approved the pull
    /// request anyway. `facts_of` read `== "APPROVED"` and called that not-approved, so the
    /// documented train's `matches` claimed nothing on twenty-one open pull requests — including
    /// the two the owner had approved by hand. No error, no flag, no stop: `matches` gates before
    /// `steps`, so even the train's catch-all `wait:` never evaluated, and the only symptom was
    /// that nothing ever happened.
    ///
    /// Three assertions, and the middle one is the counter-case that keeps this from being a
    /// licence to merge anything:
    ///
    /// 1. an approved pull request on such a repo is claimed by the documented train;
    /// 2. the same pull request with nobody's approval on it is NOT — the train must not rebase
    ///    branches and start CI on work nobody has looked at;
    /// 3. a protected repository still gets the protection: an approval of mine does not override
    ///    a `REVIEW_REQUIRED` that CODEOWNERS or a required-approvals count is holding, because
    ///    `reviewDecision` is the only thing that can see those and GitHub would refuse the merge
    ///    — which under this module's no-blind-retries rule is a stop somebody has to clear.
    ///
    /// Through [`carries`] rather than `facts_of` alone, because the defect was not that a boolean
    /// was wrong. It was that a whole feature never started, and `carries` is where that is
    /// decided.
    #[test]
    fn an_approval_is_still_an_approval_where_the_repository_asks_for_none() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // The train from docs/pr-workflow.md, "The train, written down" — its `matches`, which is
        // the half that was dead.
        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,
                 "matches":["ready","approved","review-satisfied","base:trunk"],
                 "steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap();

        // `review_decision` and `my_review` move independently, which is the point: the first is
        // the repository's, the second is the viewer's.
        let pr = |decision: &str, my_review: &str, current: bool| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": decision, "mergeable": true,
                "merge_state": "CLEAN", "checks": "passing",
                "my_review": my_review, "review_is_current": current,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        // 1. No review requirement, and a standing approval against the head that is there now.
        let social = facts_of(&pr("", "approved", true), "me", "main");
        assert!(
            social.approved,
            "a repository that requires no review says nothing in `reviewDecision`, and skein read \
             that silence as \"not approved\" — measured on the owner's queue as APPROVED on 0 of \
             21 open pull requests, two of which he had approved himself"
        );
        assert_eq!(
            social.review_requirement_met, None,
            "\"there is no requirement\" was flattened into \"the requirement is not met\", which \
             is the same bug wearing the other word"
        );
        assert_eq!(
            carries("demo", 7, &social, &flows),
            Carries::Matched("merge-train".into()),
            "the documented merge train cannot claim an approved pull request on a repository \
             where review is social — so on such a repository the train does nothing, for ever, \
             and nothing anywhere says so"
        );

        // 2. The counter-case. Same repository, same silence, and nobody has approved it.
        let unreviewed = facts_of(&pr("", "none", false), "me", "main");
        assert!(
            !unreviewed.approved,
            "\"the repository requires no review\" was read as \"approved\" — which puts every \
             open pull request in the repository on the train"
        );
        assert_eq!(
            carries("demo", 7, &unreviewed, &flows),
            Carries::Nothing,
            "the train claimed a pull request nobody has approved"
        );

        // An approval that was left on an EARLIER head is not a standing one. This half of the
        // rule is real — `prq::my_review_state` compares the review's commit with the head — and
        // it is the half the old doc comment claimed for `reviewDecision`, which does not have it.
        let stale = facts_of(&pr("", "approved", false), "me", "main");
        assert!(
            !stale.approved,
            "an approval of a head that has been pushed over was counted as standing"
        );

        // A refusal outranks an approval, including one of yours. Before SKEIN-339 this was free —
        // both facts came off one field and could not disagree — and making approvals countable is
        // exactly what could have ended it: a workflow written as `matches: ["approved"]` with a
        // merge step would start merging over a reviewer who had said no, on files nobody edited.
        let over_a_refusal = facts_of(&pr("CHANGES_REQUESTED", "approved", true), "me", "main");
        assert!(
            !over_a_refusal.approved && over_a_refusal.changes_requested,
            "your own approval was allowed to outrank a reviewer's refusal — a workflow whose \
             `matches` says only `approved` would now merge past somebody who said no"
        );

        // 3. And branch protection is not overridden by an approval skein can count itself.
        let protected = facts_of(&pr("REVIEW_REQUIRED", "approved", true), "me", "main");
        assert!(
            protected.approved,
            "somebody HAS approved it, and that is what the word means now"
        );
        assert_eq!(
            protected.review_requirement_met,
            Some(false),
            "the repository is still asking for a review and skein cannot see what for — \
             CODEOWNERS and the required-approvals count live only in `reviewDecision`"
        );
        assert_eq!(
            carries("demo", 7, &protected, &flows),
            Carries::Nothing,
            "one approval satisfied a branch protection rule skein has no way to read. GitHub \
             would refuse the merge, and a refused act is a stop a person has to clear"
        );
        // And put on the train by hand it still may not act, naming the condition off the file —
        // the row says "review-satisfied" and not "approved", which is the difference a person
        // needs: this is not waiting for a reviewer, it is waiting for the reviewer the repository
        // named and skein cannot.
        assign("demo", 7, "merge-train").unwrap();
        assert_eq!(
            carries("demo", 7, &protected, &flows),
            Carries::Holding {
                name: "merge-train".into(),
                unmet: vec!["review-satisfied".into()]
            },
            "a hand-assigned pull request merged past branch protection, or the row cannot say \
             which half of the review question is unanswered"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **Somebody else's approval is an approval** (SKEIN-356).
    ///
    /// The half [`an_approval_is_still_an_approval_where_the_repository_asks_for_none`] left open.
    /// That item made an approval countable where the repository asks for no review, but the only
    /// approval `prq::Pr` could name was the VIEWER's — `review_decision` is `""` there and
    /// `my_review` is `"none"` — so on a repository where the owner opens the pull requests and
    /// somebody else reviews them, the merge train still sat on approved work. Not a rare shape:
    /// it is the ordinary one on a repo with two people on it.
    ///
    /// `prq::Pr::standing_approvals` is the count skein now carries, from the same
    /// `latestOpinionatedReviews` the viewer's own verdict is read from. Three things have to hold
    /// at once, and the last two are what keep this from being a licence to merge:
    ///
    /// 1. a third party's standing approval is one, and the documented train claims the row;
    /// 2. nobody's approval is not one — the count is `0`, and the train may not touch it;
    /// 3. a refusal still outranks any number of approvals standing behind it.
    #[test]
    fn an_approval_from_a_third_party_is_an_approval() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,
                 "matches":["ready","approved","review-satisfied","base:trunk"],
                 "steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap();

        // The viewer is `me`, and `me` has done nothing at all on any of these: `my_review` is
        // `"none"` throughout, which is exactly the state the old reading could not get past.
        let pr = |decision: &str, standing: serde_json::Value| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": decision, "mergeable": true,
                "merge_state": "CLEAN", "checks": "passing",
                "my_review": "none", "review_is_current": false,
                "standing_approvals": standing,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        // 1. Alice approved it. Nobody asked the repository's permission, because there is none to
        //    ask — `reviewDecision` is empty on every repo where review is social.
        let by_alice = facts_of(&pr("", serde_json::json!(1)), "me", "main");
        assert!(
            by_alice.approved,
            "an approval that is not yours was invisible — on a repository requiring no review \
             that is every approval anybody else ever gives, and the merge train holds the pull \
             request for ever with nothing anywhere saying why"
        );
        assert_eq!(
            by_alice.review_requirement_met, None,
            "counting somebody else's approval must not invent a requirement for it to satisfy"
        );
        assert_eq!(
            carries("demo", 7, &by_alice, &flows),
            Carries::Matched("merge-train".into()),
            "the documented train still cannot claim a pull request somebody has approved"
        );

        // 2. The counter-case: skein looked, and nothing is standing. `Some(0)` is an answer.
        let unreviewed = facts_of(&pr("", serde_json::json!(0)), "me", "main");
        assert!(
            !unreviewed.approved,
            "a count of nought standing approvals was read as an approval — that puts every open \
             pull request in the repository on the train"
        );
        assert_eq!(carries("demo", 7, &unreviewed, &flows), Carries::Nothing);

        // 3. And a refusal outranks them however many there are.
        let over_a_refusal = facts_of(&pr("CHANGES_REQUESTED", serde_json::json!(3)), "me", "main");
        assert!(
            !over_a_refusal.approved && over_a_refusal.changes_requested,
            "three approvals were allowed to outrank one reviewer's refusal"
        );

        // And the queue that has no such count — every one remembered on disk by an older skein.
        // It falls back to the one approval that queue could name, so it decides exactly what it
        // decided before: an absent field may not change an answer, in either direction.
        let remembered = |my_review: &str, current: bool| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": "", "mergeable": true,
                "merge_state": "CLEAN", "checks": "passing",
                "my_review": my_review, "review_is_current": current,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };
        assert!(
            remembered("approved", true).standing_approvals.is_none(),
            "the fixture is meant to be a queue from before the count existed"
        );
        assert!(
            facts_of(&remembered("approved", true), "me", "main").approved,
            "a queue remembered before the count existed stopped seeing the one approval it could \
             name, so an older queue on disk holds back work it was already letting through"
        );
        assert!(
            !facts_of(&remembered("approved", false), "me", "main").approved,
            "and the stale half of that fallback went with it: an approval left on a head that has \
             been pushed over is not standing"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A label list that was cut off reaches the workflow as cut off** (SKEIN-373).
    ///
    /// `facts_of` is the only bridge between the queue and the engine, so a fact that stops here
    /// is a fact no workflow can ever read. `prq::Pr::labels` is paged at `prq::LABELS_FETCHED`,
    /// and `workflow::Cond::NoLabel` is a claim about the labels that did NOT arrive — so the
    /// completeness of the list has to travel with it or the engine answers from a hole.
    ///
    /// The other half is asserted at `workflow::tests::
    /// a_condition_about_a_label_skein_never_saw_holds_neither_way`; this is the wire between them,
    /// and it is checked end to end here because a `labels_whole` hard-coded to `true` would pass
    /// that test and this module is where it would be hard-coded.
    #[test]
    fn a_label_list_that_was_cut_off_reaches_the_workflow_as_cut_off() {
        let pr = |names: &[&str], total: serde_json::Value| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 20, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": names, "labels_total": total,
                "review_decision": "", "mergeable": true,
                "merge_state": "CLEAN", "checks": "passing",
                "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };
        let hold = crate::workflow::Cond::NoLabel("hold".into());

        // Complete: two labels and GitHub says two. `no-label:hold` is answerable and true.
        let whole = facts_of(
            &pr(&["ci", "area/mod-01"], serde_json::json!(2)),
            "me",
            "main",
        );
        assert!(whole.labels_whole);
        assert!(
            crate::workflow::holds(&hold, &whole),
            "a pull request whose whole label set arrived cannot answer `no-label:` — the \
             counter-case, without which stopping on truncation is just stopping"
        );

        // Short: the shape measured on acme/testbed#20, two labels behind the page.
        let cut = facts_of(
            &pr(&["ci", "area/mod-01"], serde_json::json!(22)),
            "me",
            "main",
        );
        assert!(
            !cut.labels_whole,
            "the queue knew its label list was short and the workflow engine was told it was whole"
        );
        assert!(
            !crate::workflow::holds(&hold, &cut),
            "`hold` may be one of the twenty labels skein never received, and the engine answered \
             that it is not on the pull request"
        );
        assert_eq!(
            cut.labels,
            vec!["ci".to_string(), "area/mod-01".to_string()],
            "the labels that DID arrive must still reach the workflow — truncation may only take \
             an answer away, never a label"
        );

        // And a queue remembered before the count existed reads as whole, which is what that queue
        // was already deciding — see `prq::Pr::labels_whole`.
        let older = facts_of(&pr(&["ci"], serde_json::Value::Null), "me", "main");
        assert!(
            older.labels_whole && crate::workflow::holds(&hold, &older),
            "an older queue on disk started refusing every `no-label:` in the fleet, on the \
             strength of a field it never wrote"
        );
    }

    /// Nothing happens on a fleet that has not switched this on.
    ///
    /// The first assertion of the feature, and the one worth being unable to break: this merges
    /// pull requests. A default that acts because a config file was missing is not one anybody
    /// would trust twice — so the check is inside the function with the consequences, not only in
    /// whoever calls it.
    #[test]
    fn a_fleet_that_has_not_switched_this_on_does_nothing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            })),
            &fixture_token(),
        );
        assert!(
            matches!(out, Outcome::Stopped(_)),
            "it acted with the switch off: {out:?}"
        );
        assert!(
            heard.lock().unwrap().is_empty(),
            "a fleet with workflows off still reached GitHub: {:?}",
            heard.lock().unwrap()
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
    }

    /// A merge carries the head skein decided on, and the branch goes after the merge.
    #[test]
    fn a_merge_names_the_commit_it_was_decided_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc123"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            })),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Did(_)), "{out:?}");
        let said = heard.lock().unwrap().clone();
        let merge = said
            .iter()
            .find(|s| s.contains("/merge"))
            .unwrap_or_else(|| panic!("nothing was merged: {said:?}"));
        assert!(
            merge.starts_with("PUT /repos/acme/thing/pulls/41/merge"),
            "{merge}"
        );
        assert!(merge.contains("\"merge_method\":\"squash\""), "{merge}");
        // The head it decided about. Without it GitHub merges whatever is there now — which is the
        // one thing a workflow must never do, because the decision was made about something else.
        assert!(
            merge.contains("\"sha\":\"abc123\""),
            "the merge did not name the commit the decision was made about: {merge}"
        );
        // And the branch goes AFTER the merge, never before: deleting the head branch of a pull
        // request that is still open closes it instead of shipping it.
        let deleted = said.iter().position(|s| s.contains("git/refs/heads/feat"));
        let merged = said.iter().position(|s| s.contains("/merge"));
        assert!(
            deleted > merged,
            "the branch was deleted before the merge landed: {said:?}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// A GitHub whose pull request can be posed: a base, a head, and whether the repository will
    /// say what its default branch is. Records every request line with its body.
    ///
    /// Separate from [`github`] above because these tests are about what skein REFUSES, and a stub
    /// that answers everything the same way cannot tell a merge that was refused from one that was
    /// attempted and failed. The three answers here are the three facts a merge turns on.
    #[allow(clippy::type_complexity)]
    fn merge_world() -> (
        String,
        Arc<Mutex<(String, String, Option<String>, u16)>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let world: Arc<Mutex<(String, String, Option<String>, u16)>> = Arc::new(Mutex::new((
            "main".to_string(),
            "abc1234def".to_string(),
            Some("main".to_string()),
            200,
        )));
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (state, seen) = (world.clone(), heard.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let body = said
                    .split("\r\n\r\n")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                seen.lock().unwrap().push(format!("{head} {body}"));
                let (base_ref, head_sha, trunk, merge_status) = state.lock().unwrap().clone();
                let (status, answer) = if head.contains("/user") {
                    (200, r#"{"login":"me"}"#.to_string())
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    match trunk {
                        // The repository, and what it calls its trunk.
                        Some(t) => (
                            200,
                            format!(r#"{{"full_name":"acme/thing","default_branch":"{t}"}}"#),
                        ),
                        // A repository GitHub answers about without naming a default branch,
                        // which `trunk_of` reads as `""` — the blindness
                        // `Facts::base_is_trunk: None` stands for.
                        //
                        // **Deliberately not a 403.** The first version of this stub posed an
                        // unknown trunk as a rate limit, which is the commonest real cause — and
                        // `github::rate_limited` engages a PROCESS-GLOBAL hold that stops every
                        // GitHub call in the test binary for fifteen minutes. Two unrelated tests
                        // in this module failed on it, in another module's words, with nothing at
                        // their own failure site to say why. The hold has no reset, so a fixture
                        // must never trip it.
                        None => (200, r#"{"full_name":"acme/thing"}"#.to_string()),
                    }
                } else if head.starts_with("GET /repos/acme/thing/pulls/41") {
                    (
                        200,
                        format!(
                            r#"{{"number":41,"node_id":"PR_n","base":{{"ref":"{base_ref}"}},"head":{{"sha":"{head_sha}"}}}}"#
                        ),
                    )
                } else if head.contains("/merge") {
                    match merge_status {
                        // GitHub's own words for a conditional merge whose branch moved. Quoted
                        // here so the translation is tested against what GitHub sends, not against
                        // what skein hopes it sends.
                        409 => (
                            409,
                            r#"{"message":"Head branch was modified. Review and try the merge again."}"#.to_string(),
                        ),
                        // And its words for a merge it will not attempt because the branch
                        // conflicts with its base. Quoted from the same place: SKEIN-385's commit
                        // measured `GitHub said 405: Pull Request has merge conflicts` against
                        // `acme/testbed#20`.
                        405 => (
                            405,
                            r#"{"message":"Pull Request has merge conflicts"}"#.to_string(),
                        ),
                        s => (s, r#"{"merged":true,"message":"Pull Request successfully merged"}"#.to_string()),
                    }
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
        (base, world, heard)
    }

    /// **A merge a person presses reaches GitHub only when the base is the trunk and the head is
    /// still the one they read — and when it does, it names that head.**
    ///
    /// One assertion over a table rather than an outcome per pair, because the last regression in
    /// this area was exactly a per-pair test: `prq::merge` was tested for "it merges", which it did,
    /// and nobody asked what it merged. The claim here is a biconditional — the merge happens IF AND
    /// ONLY IF every guard is satisfied — so a guard that is deleted fails a row that expected a
    /// refusal, and a guard that is inverted fails the row that expected a merge. Neither can be
    /// made to pass by weakening the other.
    ///
    /// The two facts each row poses are the two the merge turns on and the two the queue is worst
    /// at: `base_ref` moves under a stacked child when its parent lands, `head_sha` moves on every
    /// push. See `crate::prq::base_and_head`.
    #[test]
    fn a_merge_by_hand_happens_only_on_the_trunk_at_the_head_you_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        // The switch is OFF for the whole table, on purpose: `$SKEIN_PR_WORKFLOWS` governs skein
        // acting unattended, and the fleet where it is off is precisely the fleet where this is the
        // only merge there is. A guard that only ran with automation on would guard nothing.
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);

        // (what it is named, base ref, live head, the head the reader says they saw, trunk)
        let table: &[(&str, &str, &str, &str, Option<&str>)] = &[
            ("clean", "main", "abc1234def", "abc1234def", Some("main")),
            ("no sha at all", "main", "abc1234def", "", Some("main")),
            ("blank sha", "main", "abc1234def", "   ", Some("main")),
            (
                "the branch moved",
                "main",
                "999999999",
                "abc1234def",
                Some("main"),
            ),
            (
                "a stacked child",
                "ladder/tenants-07",
                "abc1234def",
                "abc1234def",
                Some("main"),
            ),
            (
                "a stacked child whose head also moved",
                "ladder/tenants-07",
                "999999999",
                "abc1234def",
                Some("main"),
            ),
            (
                "the trunk is unknown",
                "main",
                "abc1234def",
                "abc1234def",
                None,
            ),
            (
                "the trunk is unknown and the base is odd",
                "ladder/tenants-07",
                "abc1234def",
                "abc1234def",
                None,
            ),
        ];

        for (name, base_ref, live, seen, trunk) in table {
            *world.lock().unwrap() = (
                base_ref.to_string(),
                live.to_string(),
                trunk.map(str::to_string),
                200,
            );
            // Both are memoised per process, and the trunk especially: without this every row after
            // the first would be answered from the first row's repository.
            crate::prq::forget_trunks();
            crate::prq::forget_host_token();
            heard.lock().unwrap().clear();

            let out = merge_by_hand("acme/thing", 41, seen);
            let calls = heard.lock().unwrap().clone();
            let merged: Vec<String> = calls
                .iter()
                .filter(|c| c.contains("/pulls/41/merge"))
                .cloned()
                .collect();

            // The rule, written once. Everything below compares against THIS rather than against a
            // literal per row, so a row cannot be made to pass by adjusting its own expectation.
            let should =
                trunk.is_some_and(|t| t == *base_ref) && !seen.trim().is_empty() && live == seen;

            assert_eq!(
                !merged.is_empty(),
                should,
                "{name}: a merge request {} GitHub when it should {} — base {base_ref:?}, trunk \
                 {trunk:?}, live head {live:?}, head read {seen:?}. Answer was {out:?}",
                match merged.is_empty() {
                    true => "never reached",
                    false => "reached",
                },
                match should {
                    true => "have",
                    false => "not have",
                },
            );
            assert_eq!(
                out.is_ok(),
                should,
                "{name}: merge_by_hand answered {out:?}, which disagrees with whether it merged"
            );
            // The whole point of the `sha`: whatever went out named the commit the person read, not
            // "whatever is there now".
            for call in &merged {
                assert!(
                    call.contains(&format!("\"sha\":\"{seen}\"")),
                    "{name}: the merge did not name the head the reader read ({seen:?}): {call}"
                );
            }
            // A refusal that still asked GitHub to merge and was turned down is not a guard — it is
            // GitHub guarding skein. Nothing may be attempted on a row that must not merge.
            if !should {
                assert!(
                    merged.is_empty(),
                    "{name}: the guard let the request out and relied on GitHub to refuse it: {merged:?}"
                );
            }
        }

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **Each refusal says which guard refused, and a merge with no sha costs no request at all.**
    ///
    /// The table above proves the guards fire; this proves they are distinguishable, which is what
    /// makes them actionable. A reader told only "not merged" cannot tell "read the new code" from
    /// "this is a stacked child and never will merge from here", and those two need opposite
    /// responses.
    ///
    /// The base check running BEFORE the head check is asserted here rather than left to reading
    /// order: a stacked child is wrong to merge at any head, so being told its branch moved would
    /// send the reader to re-read a change that still must not merge.
    #[test]
    fn a_refused_merge_says_which_guard_refused_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let pose = |base: &str, live: &str, trunk: Option<&str>| {
            *world.lock().unwrap() = (
                base.to_string(),
                live.to_string(),
                trunk.map(str::to_string),
                200,
            );
            crate::prq::forget_trunks();
            crate::prq::forget_host_token();
            heard.lock().unwrap().clear();
        };

        // No sha: refused before anything is asked. A merge with nothing to name is not a question
        // worth putting to GitHub, and a round trip here would be a round trip on every press.
        pose("main", "abc1234def", Some("main"));
        let out = merge_by_hand("acme/thing", 41, "");
        let why = out.unwrap_err();
        assert!(
            why.contains("which commit") && why.contains("#41"),
            "a merge with no head read did not say that is what was wrong: {why}"
        );
        assert!(
            heard.lock().unwrap().is_empty(),
            "a merge that could not name a commit still spent a request: {:?}",
            heard.lock().unwrap()
        );

        // Moved: names both commits, because "it moved" without saying where to is not actionable.
        pose("main", "999999999", Some("main"));
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("moved") && why.contains("abc1234") && why.contains("9999999"),
            "the refusal did not name both the head that was read and the head that is there: {why}"
        );

        // A stacked child: names its base, and says the thing that makes it recoverable — that it
        // rejoins when its parent lands. This is SKEIN-237's sentence, reached from the hand path.
        pose("ladder/tenants-07", "abc1234def", Some("main"));
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("ladder/tenants-07") && why.contains("not based on the trunk"),
            "a stacked child was refused without saying it is one: {why}"
        );
        assert!(
            !why.contains("moved"),
            "a stacked child was refused for the wrong reason — the head check ran first: {why}"
        );

        // And with a head that ALSO moved, the base is still what it is told about: a child must
        // not be sent away to re-read a change it may never merge from here.
        pose("ladder/tenants-07", "999999999", Some("main"));
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("not based on the trunk") && !why.contains("moved since you read it"),
            "the base check did not run before the head check: {why}"
        );

        // Blind, not wrong. An unknown trunk is skein's own failure to see and says so, rather than
        // accusing the pull request of being stacked — `Facts::base_is_trunk`'s `None` versus
        // `Some(false)`, spent here exactly as `instead_of_merging_off_the_trunk` spends it.
        pose("main", "abc1234def", None);
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("default branch") && !why.contains("not based on the trunk"),
            "an unknown trunk was reported as the pull request's fault: {why}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **A 409 from GitHub is reported as the branch having moved, not as GitHub's own prose.**
    ///
    /// The check inside `merge_by_hand` cannot close the window between reading the head and
    /// sending the merge — only the `sha` on the wire can — so this is the path where the guard
    /// actually holds, and its sentence has to be the same one the pre-check gives. Posed by moving
    /// the branch only in GitHub's ANSWER, which is what a push landing mid-request looks like.
    #[test]
    fn a_race_lost_to_a_push_reads_as_the_branch_moving() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
        // Everything checks out and the merge itself 409s — the branch moved between the check and
        // the PUT, which no amount of checking beforehand can prevent.
        *world.lock().unwrap() = (
            "main".to_string(),
            "abc1234def".to_string(),
            Some("main".to_string()),
            409,
        );
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();

        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("moved since you read it") && why.contains("abc1234"),
            "a 409 was passed through in GitHub's words instead of the reader's: {why}"
        );
        assert!(
            !why.contains("409"),
            "the raw status reached the reader: {why}"
        );
        // It got as far as trying, which is the difference between this and the pre-check.
        assert!(
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains("/pulls/41/merge")),
            "the 409 test never reached the merge, so it proves nothing about the 409"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **A conflicted pull request is refused in a sentence about the pull request.** (SKEIN-411)
    ///
    /// The end of the road SKEIN-385 opened: that item put a refusal on screen, and what it put
    /// there was `GitHub said 405: Pull Request has merge conflicts` — a status code and somebody
    /// else's noun phrase. Posed here through `merge_by_hand`, not against
    /// `prq::it_conflicts_with_its_base` directly, because the translation and the press are wired
    /// together by one `map_err` in `prq::merge` and a unit test on the function proves nothing
    /// about that wire. Every guard passes, so the only thing that can refuse this merge is GitHub.
    #[test]
    fn a_merge_refused_for_conflicts_says_so_in_skein_s_words() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
        // On the trunk, at the head the reader read, and GitHub answers the PUT with a 405.
        *world.lock().unwrap() = (
            "main".to_string(),
            "abc1234def".to_string(),
            Some("main".to_string()),
            405,
        );
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();

        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("conflicts with its base") && why.contains("#41"),
            "a 405 for conflicts was passed through in GitHub's words instead of the reader's: \
             {why}"
        );
        assert!(
            why.contains("Resolve them on the branch"),
            "the refusal named the problem without naming the way out of it: {why}"
        );
        assert!(
            !why.contains("405") && !why.contains("GitHub said"),
            "the raw status reached the reader: {why}"
        );
        // The guards passed and the merge was actually attempted — without this the test would
        // also pass if `merge_by_hand` had refused before ever asking GitHub, which is a different
        // sentence about a different problem.
        assert!(
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains("/pulls/41/merge")),
            "the conflict test never reached the merge, so it proves nothing about the 405"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **Only a 405 that names conflicts stops the train in skein's words, and it never says
    /// 405.** (SKEIN-423)
    ///
    /// The same two directions `prq`'s
    /// `only_a_405_naming_conflicts_is_reported_as_conflicts_with_the_base` pins for the press, on
    /// the train's own sentence: a 409, a 422 or a rate limit whose body happens to carry the word
    /// "conflict" must stop with GitHub's answer verbatim, and a 405 for a draft or a blocking rule
    /// must too — a stop that sends somebody to resolve conflicts that are not there is worse than
    /// one that quotes a status, because they will go and look.
    ///
    /// Written here rather than left to the shared gate: the gate says WHICH answers, and this test
    /// is about what a stopped train SAYS, which is the half that is this module's.
    #[test]
    fn only_a_405_naming_conflicts_stops_the_train_in_skein_s_words() {
        // GitHub's own words for a conflicted merge, in both shapes `crate::github` wraps a non-2xx
        // in, across the statuses a merge actually draws.
        for status in [401, 403, 404, 405, 409, 422, 500, 502] {
            for said in [
                format!("GitHub said {status}: Pull Request has merge conflicts"),
                format!("GitHub answered {status}: <html>merge conflicts</html>"),
            ] {
                let out = conflicts_stopped_the_train(41, said.clone());
                let translated = out != said;
                assert_eq!(
                    translated,
                    status == 405,
                    "status {status} was {} translated into a stop about conflicts: {out}",
                    match translated {
                        true => "wrongly",
                        false => "not",
                    }
                );
                if translated {
                    assert!(
                        out.contains("conflicts with its base") && out.contains("#41"),
                        "the stop lost the pull request or what is wrong with it: {out}"
                    );
                    // What a stop has to carry that a press's refusal does not: it is read by
                    // somebody who was not watching, so it has to say that the merge did not
                    // happen, that nothing is going to happen next, and what to press.
                    assert!(
                        out.contains("nothing was merged"),
                        "the reader was not told whether the merge landed: {out}"
                    );
                    assert!(
                        out.contains("will not try again"),
                        "the reader was not told the train has given up until they act: {out}"
                    );
                    assert!(
                        out.contains("let it run again"),
                        "the stop names no way out of itself — `revFlowBox`'s button is the one \
                         thing in front of this reader: {out}"
                    );
                    assert!(
                        !out.contains("405"),
                        "the raw status survived into the reader's sentence: {out}"
                    );
                }
            }
        }

        // A 405 that is not about conflicts. GitHub answers every unmergeable pull request with
        // this status, and only one of the reasons is fixed by resolving anything.
        for said in [
            "GitHub said 405: Pull Request is not mergeable".to_string(),
            "GitHub said 405: Base branch was modified".to_string(),
            "GitHub answered 405: <html>no</html>".to_string(),
            "GitHub answered 405 with an empty body".to_string(),
        ] {
            assert_eq!(
                conflicts_stopped_the_train(41, said.clone()),
                said,
                "a 405 that says nothing about conflicts stopped the train as a conflict"
            );
        }

        // Not a status at all, and the word appearing somewhere it is not one.
        for said in [
            "GitHub sent nothing at all".to_string(),
            "the 405 in this sentence is not a status, and neither is this conflict".to_string(),
        ] {
            assert_eq!(
                conflicts_stopped_the_train(41, said.clone()),
                said,
                "an answer that was not a 405 stopped the train as conflicts with the base"
            );
        }
    }

    /// **A train stopped by conflicts says so where the stop is read.** (SKEIN-423)
    ///
    /// Driven through [`perform`] rather than against `conflicts_stopped_the_train` directly,
    /// for the reason `a_merge_refused_for_conflicts_says_so_in_skein_s_words` gives about the
    /// press: the translation and the act are joined by one `map_err` in [`merge_pr`], and a unit
    /// test on the function proves nothing about that wire — delete the `map_err` and the test
    /// above stays green. What is asserted is the thing a person actually reads, which is not the
    /// return value but the stop `revFlowBox` draws, so [`stopped`] is read back from the file.
    #[test]
    fn a_train_stopped_by_conflicts_says_so_in_skein_s_words() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        // `merge_world` and not `github(405)`: this turns on GitHub's real 405 BODY, and the
        // blanket stub answers every path with `{"merged":true}`, which carries no `message` and
        // so cannot pose the answer under test.
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
        world.lock().unwrap().3 = 405;

        let out = perform(
            &subject("abc123"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            })),
            &fixture_token(),
        );
        let why = match &out {
            Outcome::Stopped(why) => why.clone(),
            other => panic!("a 405 for conflicts was not a stop: {other:?}"),
        };
        // The merge was actually attempted. Without this the test would also pass on a stop written
        // by a guard that refused before ever asking GitHub, which is a different sentence about a
        // different problem.
        assert!(
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains("/pulls/41/merge")),
            "the conflict test never reached the merge, so it proves nothing about the 405"
        );
        // What `revFlowBox` draws under **Stopped.** — read back from the file, not from `out`,
        // because the file is what outlives the poll and is what somebody reads later.
        let filed = stopped("demo", 41).expect("the stop was not written down");
        assert_eq!(filed, why, "the stop filed is not the stop reported");
        assert!(
            filed.contains("conflicts with its base") && filed.contains("#41"),
            "a 405 for conflicts was filed in GitHub's words instead of the reader's: {filed}"
        );
        assert!(
            filed.contains("nothing was merged") && filed.contains("let it run again"),
            "the stop named the problem without saying what did not happen or what to press: \
             {filed}"
        );
        assert!(
            !filed.contains("405") && !filed.contains("GitHub said"),
            "the raw status reached the reader: {filed}"
        );
        // And the step that decided it is still on the front of the sentence — the translation
        // replaces GitHub's words, not `perform`'s attribution.
        assert!(
            filed.contains("ship-mine") && filed.contains("step 4"),
            "the stop no longer names the step that decided it: {filed}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_trunks();
    }

    /// An action that failed is not tried again, and the reason is kept.
    ///
    /// A merge 409s when somebody pushed while skein was deciding. Retrying is not resilience: the
    /// decision was made from facts that failure has just proved stale, so the same decision would
    /// be made again, and a loop like that eventually wins the race.
    #[test]
    fn an_action_that_failed_stops_the_workflow_rather_than_looping() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(409);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let act = Act::Merge(Merge {
            how: MergeAs::Squash,
            delete_branch: false,
        });
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(act.clone()),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("ship-mine") && why.contains("step 4"),
                "the stop must name the step that decided it: {why}"
            ),
            other => panic!("a 409 was not treated as a stop: {other:?}"),
        }
        assert!(
            stopped("demo", 41).is_some(),
            "the stop was not written down"
        );

        // The next poll. It must not reach GitHub at all.
        let before = heard.lock().unwrap().len();
        let out = perform(&subject("abc"), &flow(), &chosen(act), &fixture_token());
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");
        assert_eq!(
            heard.lock().unwrap().len(),
            before,
            "a stopped workflow tried the same failing action again"
        );

        // And a person can let it run again.
        clear("demo", 41).expect("the stop must clear");
        assert_eq!(stopped("demo", 41), None);

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// A rebase goes through GraphQL, names the head, and says what it may have cost.
    #[test]
    fn a_rebase_asks_graphql_and_says_what_it_may_have_cost() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc123"),
            &flow(),
            &chosen(Act::UpdateBranch(Update::Rebase)),
            &fixture_token(),
        );
        let said = heard.lock().unwrap().clone();
        let call = said
            .iter()
            .find(|s| s.contains("/graphql"))
            .unwrap_or_else(|| panic!("nothing asked GraphQL: {said:?}"));
        // REST cannot rebase at all — it takes expected_head_sha and merges. This is the only way
        // to ask, and it is what `gh pr update-branch --rebase` does. See docs/pr-workflow.md.
        assert!(call.contains("updatePullRequestBranch"), "{call}");
        assert!(
            call.contains("REBASE"),
            "the rebase was sent as a merge: {call}"
        );
        assert!(
            call.contains("abc123"),
            "the rebase did not name the head it was deciding about: {call}"
        );
        // And the sentence tells the truth about the approval, which GitHub's own setting decides.
        match out {
            Outcome::Did(what) => assert!(
                what.contains("approving again"),
                "a rebase that may have dismissed the approval said nothing about it: {what}"
            ),
            other => panic!("{other:?}"),
        }

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// What GitHub's `mergeStateStatus` becomes on the way to a workflow, and what "trunk" means.
    ///
    /// `behind` keeps `mergeable`'s discipline — `""` and `UNKNOWN` are *no answer*, not "current"
    /// — because the merge step leans on `current`, and unknown read as current merges code CI
    /// never tested against the trunk (docs/pr-workflow.md, "The merge train"). And an unknown
    /// trunk claims nothing: `base_is_trunk` is `None` — no answer rather than "no", which is the
    /// direction that keeps a train parked rather than shipping into a branch it only believes is
    /// the trunk, without turning skein's own blindness into a stop somebody has to clear.
    #[test]
    fn the_train_facts_come_from_merge_state_and_the_trunk() {
        let pr = |merge_state: &str, base_ref: &str| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "me", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": base_ref,
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": "APPROVED", "mergeable": true,
                "merge_state": merge_state,
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        assert_eq!(
            facts_of(&pr("BEHIND", "main"), "me", "main").behind,
            Some(true)
        );
        assert_eq!(
            facts_of(&pr("CLEAN", "main"), "me", "main").behind,
            Some(false)
        );
        assert_eq!(
            facts_of(&pr("", "main"), "me", "main").behind,
            None,
            "a queue from before the field was given an answer it does not have"
        );
        assert_eq!(
            facts_of(&pr("UNKNOWN", "main"), "me", "main").behind,
            None,
            "UNKNOWN was flattened to an answer on the way to the workflow"
        );

        assert_eq!(
            facts_of(&pr("CLEAN", "main"), "me", "main").base_is_trunk,
            Some(true)
        );
        assert_eq!(
            facts_of(&pr("CLEAN", "feat-parent"), "me", "main").base_is_trunk,
            Some(false),
            "a stacked child's base was not recognised as one that is NOT the trunk"
        );
        // The third value, and the one that keeps a rate limit from becoming a stop: skein has not
        // learned this repository's default branch, which is not the same answer as "no".
        assert_eq!(
            facts_of(&pr("CLEAN", "main"), "me", "").base_is_trunk,
            None,
            "an unknown trunk was flattened to an answer on the way to the workflow"
        );
    }

    /// A stacked child never boards the train, and no stack model was needed.
    ///
    /// The one rule from docs/pr-workflow.md ("Stacks need no stack model"): the train only
    /// touches a PR whose base is the trunk. A child's base is its parent's *branch* — merging it
    /// would merge into the parent, not ship it — so `base:trunk` in `matches` keeps it out until
    /// GitHub retargets it onto the trunk after the parent merges.
    #[test]
    fn a_stacked_child_is_kept_out_by_its_matches() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,
                 "matches":["ready","approved","review-satisfied","base:trunk"],
                 "steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap();
        let pr = |base_ref: &str| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 12, "title": "t", "author": "me", "url": "u",
                "head_ref": "feat-child", "head_sha": "abc", "base_ref": base_ref,
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": "APPROVED", "mergeable": true,
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        let child = facts_of(&pr("feat-parent"), "me", "main");
        assert_eq!(
            carries("demo", 12, &child, &flows),
            Carries::Nothing,
            "a stacked child was claimed by the train — it would merge into its parent's branch"
        );
        // And the same pull request, retargeted onto the trunk after its parent merged, is an
        // ordinary trunk-based PR the train claims — that is the whole stack mechanism.
        let retargeted = facts_of(&pr("main"), "me", "main");
        assert_eq!(
            carries("demo", 12, &retargeted, &flows),
            Carries::Matched("merge-train".into())
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A workflow that acted drops the queue the next pass would have decided from**
    /// (SKEIN-314).
    ///
    /// [`sweep`] ends a pass that did anything with `crate::prq::invalidate`, and the reason is in
    /// its own comment: the queue is cached for a minute, skein has just changed the thing that
    /// queue describes, and left alone the next pass would decide from facts it made stale itself.
    /// That is the one input a cascade needs to merge on a check that has not run.
    ///
    /// **It had no test that could fail, and could not have had one**: `queue_within` skipped the
    /// cache outright in this crate's unit tests, so deleting the `invalidate` changed nothing any
    /// test could see. [`crate::prq::CachedQueues`] switches the cache on for the length of this
    /// test, which makes the sweep's second pass a real one.
    ///
    /// Two passes over a repository whose state changes in between, exactly as it would on GitHub
    /// after the first act: pass one puts `ci-queue` on, CI then goes green, and pass two merges.
    /// With the `invalidate` deleted, pass two reads the cached queue instead — no label, no green
    /// — and adds the label a second time rather than merging, which is what this fails on.
    #[test]
    fn acting_drops_the_queue_the_next_pass_would_have_decided_from() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // The same pull request twice: before its label and CI run, and after.
        let answer_for = |labels: &str, checks: &str| {
            format!(
                r#"{{"data":{{"q0":{{"nodes":[{{"number":12,"title":"t","url":"u","isDraft":false,
                  "author":{{"login":"me"}},"headRefName":"feat-12","headRefOid":"abc",
                  "baseRefName":"main","updatedAt":"2026-08-23T00:00:00Z",
                  "reviewDecision":"APPROVED","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                  "labels":{{"nodes":[{labels}]}},"latestReviews":{{"nodes":[]}},
                  "commits":{{"nodes":[{{"commit":{{
                    "committedDate":"2026-08-23T00:00:00Z",
                    "statusCheckRollup":{{"contexts":{{"nodes":[
                      {{"status":"COMPLETED","conclusion":"{checks}"}}]}}}}}}}}]}}}}]}},
                  "q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#
            )
        };
        let answer = Arc::new(Mutex::new(answer_for(r#"{"name":"ready"}"#, "SUCCESS")));
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (seen, queue) = (heard.clone(), answer.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(head.clone());
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing") && !head.contains("/pulls") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") {
                    queue.lock().unwrap().clone()
                } else {
                    r#"{"merged":true}"#.to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // The cache live, which is the whole point: without this the second sweep refetches
        // whether or not anything invalidated, and the assertion below cannot fail.
        let _cache = crate::prq::CachedQueues::live();

        let first = sweep();
        assert!(
            first.iter().any(|d| d.contains("ci-queue")),
            "the first pass did not act, so there is nothing for an invalidate to be about: \
             {first:?}"
        );

        // GitHub's state moves on, exactly as it would have: the label is on, and CI went green
        // against it. Nothing tells skein — the only thing that can is reading the queue again.
        *answer.lock().unwrap() = answer_for(r#"{"name":"ready"},{"name":"ci-queue"}"#, "SUCCESS");

        let second = sweep();
        let calls = heard.lock().unwrap().clone();
        assert!(
            calls.iter().filter(|c| c.contains("/graphql")).count() >= 2,
            "the second pass decided from the queue the first pass made stale — it never asked \
             GitHub again: {second:?} / {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.contains("/pulls/12/merge")),
            "the pull request was not merged on the second pass, so the pass acted on facts from \
             before its own act: {second:?} / {calls:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// **An assignment says WHICH workflow, not that its conditions are met** (SKEIN-279).
    ///
    /// The rule this pins, in one sentence: a workflow's `matches` are conditions on ACTING, read
    /// on both roads to acting, and an assignment overrides only which workflow is responsible.
    /// Until this, `matches` was evaluated by [`crate::workflow::claims`] and nowhere else, so a
    /// hand assignment switched off every guard written there — the documented merge train,
    /// assigned to a pull request nobody had approved, would label it, rebase it and merge it.
    ///
    /// Both candidate answers are asserted here, because the value of this test is that it fails
    /// under either of the other two:
    ///
    /// * **"this one, guards and all"** — the behaviour that was there. It fails on `#11`, which
    ///   would have had `ci-queue` put on it and started CI on an unapproved change.
    /// * **"the conditions hold, and holding is a stop or a wait"** — the objection that left this
    ///   item open, since "put this on the train, it will go when it is approved" is an ordinary
    ///   thing to want. It fails on the three assertions that nothing was written down, and on the
    ///   second sweep, where approval alone is enough to make it act: no stop to clear, no
    ///   re-assignment, and no clock that could have run out in between.
    ///
    /// And the assertion that is neither: `#11` is the LOWER number, so under the serial train's
    /// oldest-first rule it would be the front. A pull request that cannot act must not be able to
    /// stand at the front of a train — that would park everything behind it on a condition its own
    /// workflow stated — so `#12` merges in the same pass that `#11` is held.
    #[test]
    fn an_assigned_workflow_holds_for_its_own_conditions_without_blocking_the_train() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, "The train, written down".
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["changes-requested"],"do":"flag:changes were requested"},
                {"when":["not-mergeable"],"do":"flag:conflicts with the base"},
                {"when":["behind"],"do":"update-branch:rebase"},
                {"when":["checks:failing"],"do":"flag:CI failed"},
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #11 is `ready`, trunk-based, green — and NOT approved. #12 is all four, and the train's
        // own rule claims it. The review decision on #11 is what the second sweep changes.
        let pr = |number: u64, decision: &str, labels: &str| {
            format!(
                r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                  "author":{{"login":"me"}},"headRefName":"feat-{number}","headRefOid":"abc{number}",
                  "baseRefName":"main","updatedAt":"2026-08-23T00:00:00Z",
                  "reviewDecision":"{decision}","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                  "labels":{{"nodes":[{labels}]}},"latestReviews":{{"nodes":[]}},
                  "commits":{{"nodes":[{{"commit":{{
                    "committedDate":"2026-08-23T00:00:00Z",
                    "statusCheckRollup":{{"contexts":{{"nodes":[
                      {{"status":"COMPLETED","conclusion":"SUCCESS"}}]}}}}}}}}]}}}}"#
            )
        };
        let queue_answer = |eleven: &str| {
            format!(
                r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                pr(11, eleven, r#"{"name":"ready"}"#),
                pr(12, "APPROVED", r#"{"name":"ready"},{"name":"ci-queue"}"#),
            )
        };
        let answer = Arc::new(Mutex::new(queue_answer("REVIEW_REQUIRED")));
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (seen, queue) = (heard.clone(), answer.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(head.clone());
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing") && !head.contains("/pulls") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") {
                    queue.lock().unwrap().clone()
                } else {
                    r#"{"merged":true}"#.to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        assign("demo", 11, "merge-train").unwrap();
        let did = sweep();

        // **Nothing was done to #11**, on the wire, which is the only place it would show.
        let calls = heard.lock().unwrap().clone();
        let touched_11 = |c: &String| c.contains("/11/") || c.contains("/pulls/11");
        assert!(
            !calls.iter().any(touched_11),
            "the train acted on a pull request nobody has approved: {did:?} / {calls:?}"
        );
        // And nothing was written down about it either: no stop to clear, no timeline entry, and
        // therefore no clock that could later turn this into one.
        assert_eq!(stopped("demo", 11), None, "holding became a stop");
        assert!(
            journal("demo", 11).is_empty(),
            "holding was recorded as an event: {:?}",
            journal("demo", 11)
        );

        // **The train moved anyway.** #11 is the lower number and would have been the front.
        assert!(
            calls.iter().any(|c| c.contains("/pulls/12/merge")),
            "a held pull request blocked the train behind it: {did:?} / {calls:?}"
        );

        // **The row says both halves**: the train is on #11 — it must still show as chosen in the
        // pane's chooser — and it is not acting, naming the condition off the file.
        let flows = crate::workflow::load().unwrap();
        let held = crate::prq::queue(&crate::repos::load_repos()[0], false)
            .unwrap()
            .prs
            .into_iter()
            .find(|p| p.number == 11)
            .expect("#11 is in the queue");
        let facts = facts_of(&held, "me", "main");
        assert_eq!(
            carries("demo", 11, &facts, &flows),
            Carries::Holding {
                name: "merge-train".into(),
                // Both, and they are two different sentences to a reader: nobody has approved it,
                // AND this repository's protection is still asking for a review. #11 is
                // `REVIEW_REQUIRED`, which is the state where those really are the same event —
                // on a repo that requires no review they come apart, which is the whole of
                // SKEIN-339.
                unmet: vec!["approved".into(), "review-satisfied".into()]
            },
            "the condition it is holding for is not the one the file states"
        );
        let seen = standing("demo", 11, &facts, &flows);
        assert_eq!(
            (
                seen.how.as_str(),
                seen.workflow.as_str(),
                seen.next.as_str()
            ),
            ("assigned", "merge-train", ""),
            "the dry run lost the assignment, or promised a step: {seen:?}"
        );
        assert!(
            seen.holding.contains("approved"),
            "the row does not say which condition it is not moving on: {seen:?}"
        );
        assert_eq!(
            seen.stopped, "",
            "holding was reported to the pane as a stop, which is something to clear: {seen:?}"
        );

        // **And approval alone starts it.** Nothing is cleared, re-assigned or waited out: the
        // next pass reads the same assignment against new facts.
        *answer.lock().unwrap() = queue_answer("APPROVED");
        crate::prq::invalidate("demo");
        heard.lock().unwrap().clear();
        sweep();
        let calls = heard.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("POST") && c.contains("/issues/11/labels")),
            "an approved pull request did not rejoin the train it was assigned to: {calls:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// A stacked child somebody put the train on by hand is never merged into its parent, and no
    /// merge reaches the wire.
    ///
    /// The other half of the rule above, and the one that was missing (SKEIN-237). `matches` used
    /// to be read by [`crate::workflow::claims`] and by nothing else: [`carries`] returned
    /// `Carries::Assigned` straight from the assignment file, [`sweep`] took its name, and
    /// [`crate::workflow::next`] evaluates only `steps` — so on a documented merge train, whose
    /// `base:trunk` lives in `matches`, one hand assignment merged a child into its PARENT's
    /// branch and deleted the child's branch. Putting a workflow on a row by hand is an ordinary
    /// cockpit act; on an eighteen-deep stack it takes the rest of the stack with it, and there is
    /// no undo for a landed merge and a deleted branch.
    ///
    /// **What refuses it moved earlier, and the test says which** (SKEIN-279). SKEIN-237 could
    /// only refuse this at the act, because the train had already claimed the pull request and
    /// walked its steps: the merge became a `flag`, and a stop somebody had to clear. Now the
    /// train's own `base:trunk` holds on both roads, so the pull request is never carried for
    /// acting at all — no step, no stop, nothing written down, and it rejoins by itself when its
    /// parent merges and GitHub retargets it. The act-level guard has NOT gone anywhere and is
    /// still what catches a workflow whose `matches` never mentioned the base: it is pinned by
    /// `workflow::tests::a_merge_is_refused_on_a_base_that_is_not_known_to_be_the_trunk`, on
    /// [`crate::workflow::next`] directly.
    ///
    /// Driven through [`sweep`] against a GitHub that records every request, because the assertion
    /// that matters is about the wire: a doer tested through its return value would pass while
    /// merging. The workflow is the train exactly as `docs/pr-workflow.md` writes it down.
    #[test]
    fn a_hand_assigned_stacked_child_is_never_merged_into_its_parent() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, "The train, written down".
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["changes-requested"],"do":"flag:changes were requested"},
                {"when":["not-mergeable"],"do":"flag:conflicts with the base"},
                {"when":["behind"],"do":"update-branch:rebase"},
                {"when":["checks:failing"],"do":"flag:CI failed"},
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #12 is a stacked child: based on `feat-parent`, and otherwise in the exact state that
        // makes the train's last real step fire — approved, ready, labelled, green, CLEAN.
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(head.clone());
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing") && !head.contains("/pulls") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[{"number":12,"title":"t","url":"u",
                      "isDraft":false,"author":{"login":"me"},"headRefName":"feat-12",
                      "headRefOid":"abc","baseRefName":"feat-parent",
                      "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                      "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                      "labels":{"nodes":[{"name":"ci-queue"}]},
                      "latestReviews":{"nodes":[]},
                      "commits":{"nodes":[{"commit":{
                        "committedDate":"2026-08-23T00:00:00Z",
                        "statusCheckRollup":{"contexts":{"nodes":[
                          {"status":"COMPLETED","conclusion":"SUCCESS"}]}}}}]}}]},
                      "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else {
                    r#"{"merged":true}"#.to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // The rule refuses it — that is `a_stacked_child_is_kept_out_by_its_matches` above. A
        // person puts the train on it by hand, which is the road that skipped every guard.
        assign("demo", 12, "merge-train").unwrap();
        let did = sweep();

        let calls = heard.lock().unwrap().clone();
        assert!(
            !calls.iter().any(|c| c.contains("/pulls/12/merge")),
            "the train merged a stacked child into feat-parent: {did:?} / {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("DELETE")),
            "a branch was deleted on a pull request that was never merged: {calls:?}"
        );
        // And it was refused BEFORE the train took it up, so there is nothing to clear: no stop
        // written down, and no journal entry, because nothing happened to record. A stop the
        // owner has to clear on a pull request that will retarget itself is work manufactured out
        // of a condition the workflow already stated.
        assert_eq!(
            stopped("demo", 12),
            None,
            "a stop was written for a pull request the train never took up: {did:?}"
        );
        let entries = journal("demo", 12);
        assert!(
            entries.is_empty(),
            "nothing acted, so nothing may be in the timeline: {entries:?}"
        );
        // The dry run is where this has to be visible, and it says both halves: the train is on
        // this pull request (somebody chose it, and the chooser must show it as chosen) and it is
        // not acting, naming the condition off the file. This is what the owner reads with the
        // switch off, so silence here is the whole failure SKEIN-279 is about.
        let flows = crate::workflow::load().unwrap();
        let facts = facts_of(
            &crate::prq::queue(&crate::repos::load_repos()[0], false)
                .unwrap()
                .prs[0],
            "me",
            "main",
        );
        let seen = standing("demo", 12, &facts, &flows);
        assert_eq!(
            (seen.how.as_str(), seen.workflow.as_str(), seen.step),
            ("assigned", "merge-train", 0),
            "the row must still say the train is on it, with no step it is about to take: {seen:?}"
        );
        assert_eq!(
            seen.next, "",
            "the dry run promised a step on a pull request nothing will act on: {seen:?}"
        );
        assert!(
            seen.holding.contains("base:trunk"),
            "the dry run says nothing about why the train is not moving on it: {seen:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// One rate-limited refresh must not disable the merge train until somebody restarts skein.
    ///
    /// The shape (SKEIN-238), and it is the same shape twice over. `prq::queue_within` asks in the
    /// order `viewer()` [REST], `search_prs_all` [GraphQL], `trunk_of` [REST] — so a GraphQL-only
    /// limit passes the first, engages `crate::github`'s process-wide hold on the second, and the
    /// third is refused by that hold having asked GitHub nothing. `trunk_of` swallowed that
    /// refusal into `""` and REMEMBERED it, for the life of the process. Everything downstream
    /// then did exactly what it should with an unknown trunk: `base_is_trunk` none, `base:trunk`
    /// unsatisfied, `claims` false, `Carries::Nothing`, `sweep` moves on. A dead train, with no
    /// banner, no blind spot and no log line — nothing anybody could clear, because nothing said
    /// it was there.
    ///
    /// So what this asserts is RECOVERY, not correctness: skein is allowed to know nothing while
    /// GitHub is refusing it, and is not allowed to still know nothing one refresh after GitHub
    /// comes back. The fix is that only an ANSWER is remembered (`prq::trunk_of`); a failure is
    /// asked again, and during the hold that retry is refused before it is spent.
    #[test]
    fn a_rate_limited_refresh_does_not_disable_the_train_until_a_restart() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
        crate::prq::forget_renames();

        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[{"when":["no-label:ci-queue"],"do":"add-label:ci-queue"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub whose GRAPHQL quota alone is spent — REST is fine, which is the live shape:
        // skein's search is where nearly all of its quota goes. `/rate_limit` stays free and
        // answers, because that is where the hold learns how long to last.
        let spent = Arc::new(Mutex::new(true));
        let out = spent.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let answer = if head.starts_with("GET /rate_limit") {
                    format!(
                        r#"{{"resources":{{"core":{{"remaining":4000,"reset":{}}},
                          "search":{{"remaining":30,"reset":{}}},
                          "graphql":{{"remaining":0,"reset":{}}}}}}}"#,
                        now + 600,
                        now + 600,
                        now + 600
                    )
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") && *out.lock().unwrap() {
                    r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#
                        .to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[{"number":5,"title":"t","url":"u",
                      "isDraft":false,"author":{"login":"me"},"headRefName":"feat-5",
                      "headRefOid":"abc","baseRefName":"main",
                      "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                      "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                      "labels":{"nodes":[]},"latestReviews":{"nodes":[]},
                      "commits":{"nodes":[{"commit":{
                        "committedDate":"2026-08-23T00:00:00Z",
                        "statusCheckRollup":{"contexts":{"nodes":[
                          {"status":"COMPLETED","conclusion":"SUCCESS"}]}}}}]}}]},
                      "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else {
                    "[]".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // The refresh that lands inside the outage. Knowing nothing here is correct.
        let repo = crate::repos::load_repos().remove(0);
        let during = crate::prq::queue(&repo, true).expect("a blind queue still answers");
        assert_eq!(
            during.trunk, "",
            "skein claimed to know the trunk during an outage that refused the lookup"
        );

        // GitHub comes back: the quota returns and the hold is released.
        *spent.lock().unwrap() = false;
        let _cleared = crate::github::HoldClear::new();

        let after = crate::prq::queue(&repo, true).expect("a healthy GitHub answers");
        assert_eq!(
            after.prs.len(),
            1,
            "the recovered queue lost its pull request"
        );
        assert_eq!(
            after.trunk, "main",
            "one rate-limited refresh disabled the merge train until a restart: the failed trunk \
             lookup was remembered as an answer, so `base:trunk` can never hold again"
        );

        // And the train claims it again — the thing the memoised failure had silently switched
        // off. Asserted through `claims`, which is the gate the whole chain narrows to.
        let flows = crate::workflow::load().unwrap();
        let facts = facts_of(&after.prs[0], &after.viewer, &after.trunk);
        assert!(
            crate::workflow::claims(&flows[0], &facts),
            "the merge train still claims nothing after GitHub came back: {facts:?}"
        );
        // …and the tick acts on it, which is what "the train is running" means to a person.
        let did = sweep();
        assert!(
            did.iter().any(|line| line.contains("ci-queue")),
            "the train claimed #5 and still did nothing: {did:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
        crate::prq::forget_renames();
    }

    /// A serial workflow acts on the front of the train, and only the front.
    ///
    /// Oldest first — lowest number, the sort key the owner chose — and a stopped front is
    /// passed over so the train moves ahead of a failure rather than parking behind it
    /// (docs/pr-workflow.md, "The merge train"). The assertion is on the wire, the file's
    /// discipline: two pull requests both due the same action, and exactly one request leaves.
    #[test]
    fn a_serial_workflow_acts_on_the_front_of_the_train_only() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();

        let serial = br#"{"workflow":[{"name":"train","serial":true,"matches":["mine"],"steps":[
          {"when":["no-label:ci"],"do":"add-label:ci"}]}]}"#;
        std::fs::write(home.join("workflows.json"), serial).unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/serial.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub with two open pull requests, both mine, both unlabelled — and #9 listed FIRST,
        // so a sweep that took the queue's own order would act on the wrong one.
        let labelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = labelled.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.contains("/labels") {
                    seen.lock().unwrap().push(head.clone());
                    "[]".to_string()
                } else if head.contains("/graphql") {
                    let node = |number: u64| {
                        format!(
                            r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                              "author":{{"login":"me"}},"headRefName":"feat-{number}",
                              "headRefOid":"abc","baseRefName":"main",
                              "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                              "mergeable":"MERGEABLE","labels":{{"nodes":[]}},
                              "latestReviews":{{"nodes":[]}},
                              "commits":{{"nodes":[{{"commit":{{
                                "committedDate":"2026-08-23T00:00:00Z",
                                "statusCheckRollup":{{"contexts":{{"nodes":[]}}}}}}}}]}}}}"#
                        )
                    };
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(9),
                        node(5)
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // Pass one: both #5 and #9 are due the same step. Exactly one request leaves, and it is
        // for #5 — the oldest, not the first in the queue's own order.
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "a serial workflow acted past the front of the train: {calls:?} ({did:?})"
        );
        assert!(
            calls[0].contains("/issues/5/labels"),
            "the train did not act on its oldest pull request: {calls:?}"
        );

        // The front stops — CI failed, say. The next pass skips it and moves ahead: #9 is the
        // front now. That pass-over is the "skip failures and move ahead", and it is silent,
        // because a stopped PR's story is in the stops file, not re-announced every pass.
        stop("demo", 5, "CI is red");
        labelled.lock().unwrap().clear();
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "a stopped front did not yield to the next in line: {calls:?} ({did:?})"
        );
        assert!(
            calls[0].contains("/issues/9/labels"),
            "the train did not move ahead of its stopped front: {calls:?}"
        );

        // And the stops read back in numeric order, the shape the banner row carries.
        let stops = stops("demo");
        assert_eq!(stops.len(), 1);
        assert_eq!((stops[0].number, stops[0].why.as_str()), (5, "CI is red"));

        // The same two pull requests under a NON-serial workflow: everyone due a step acts, which
        // is today's behavior and must stay — serial is a property of a workflow, not of the sweep.
        clear("demo", 5).expect("the stop must clear");
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"train","matches":["mine"],"steps":[
              {"when":["no-label:ci"],"do":"add-label:ci"}]}]}"#,
        )
        .unwrap();
        labelled.lock().unwrap().clear();
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            2,
            "a workflow that never asked to be serial was serialized: {calls:?} ({did:?})"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
    }

    /// A pull request whose CI never starts stops the train's clock, not the train.
    ///
    /// The bug (SKEIN-240) and, more importantly, what KIND of bug it is. It looks like the
    /// rate-limit family — a temporary condition that became permanent — and it is not one.
    /// `prq::rollup` says `"none"` when nothing has ever run against a commit, and that answer is
    /// correct, current and unchanging: nothing GitHub sends tells "no check yet" apart from "no
    /// check, ever, on this repository". There is no staler cache to drop and no re-read that
    /// helps. The documented train has no step for `checks:none` once its label is on, so the
    /// front falls to the catch-all `wait:` and holds the line at one pass per two minutes, for
    /// ever, over a sentence that says GitHub has not caught up when GitHub caught up long ago.
    ///
    /// The only thing that can tell the two apart is elapsed time, so this asserts a clock: the
    /// first pass writes the wait down, passes inside the twenty minutes change nothing, and the
    /// pass after it stops the pull request with a sentence naming the wait — at which point the
    /// serial train's existing pass-over rule moves it aside and #9, which has been in line all
    /// along, gets its turn.
    ///
    /// The back-dated journal entry is the clock: `record` stamps `now`, so the only way to reach
    /// the far side of twenty minutes in a test is to write the timeline the way it would look
    /// twenty minutes later.
    #[test]
    fn a_front_waiting_on_a_check_that_never_starts_stops_and_lets_the_train_past() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, ending in the catch-all that has no clock of its own.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #5 is labelled and its label started nothing — `checks: none`, for ever. #9 is behind it
        // in the line, unlabelled, with a step of its own it has never been given a chance to take.
        let labelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = labelled.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/labels") {
                    seen.lock().unwrap().push(head.clone());
                    "[]".to_string()
                } else if head.contains("/graphql") {
                    // No `statusCheckRollup` contexts and none claimed on either: `checks: none`.
                    let node = |number: u64, labels: &str| {
                        format!(
                            r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                              "author":{{"login":"me"}},"headRefName":"feat-{number}",
                              "headRefOid":"abc","baseRefName":"main",
                              "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                              "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                              "labels":{{"nodes":[{labels}]}},"latestReviews":{{"nodes":[]}},
                              "commits":{{"nodes":[{{"commit":{{
                                "committedDate":"2026-08-23T00:00:00Z",
                                "statusCheckRollup":{{"contexts":{{"nodes":[]}}}}}}}}]}}}}"#
                        )
                    };
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(5, r#"{"name":"ci-queue"}"#),
                        node(9, "")
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // Pass one, and four more inside the twenty minutes. #5 is the front and waits; #9 is
        // behind it and is passed over. This is the reported failure, and up to here it is CORRECT
        // — a wait that has not gone on long enough to be suspicious.
        for _ in 0..5 {
            let _ = sweep();
        }
        assert!(
            labelled.lock().unwrap().is_empty(),
            "the train acted past a waiting front: {:?}",
            labelled.lock().unwrap()
        );
        assert_eq!(stopped("demo", 5), None, "a wait was stopped far too early");
        // …and the wait was written down once, not once per pass, with the step it is on.
        let waits: Vec<JournalEntry> = journal("demo", 5)
            .into_iter()
            .filter(|e| e.kind == "waiting")
            .collect();
        assert_eq!(
            waits.len(),
            1,
            "five passes wrote {} waiting entries: one is the clock being started, none is no \
             clock at all, and more than one is a journal turning into a log file",
            waits.len()
        );
        assert_eq!(waits[0].step, 4, "the wait must name the step it is on");

        // Twenty minutes pass. Written into the timeline, because `record` stamps `now`.
        let mut all = journal("demo", 5);
        let last = all.len() - 1;
        all[last].at_ms -= WAITING_ON_NOTHING_MS + 1;
        let mut file: std::collections::BTreeMap<String, Vec<JournalEntry>> =
            serde_json::from_str(&std::fs::read_to_string(journal_path("demo")).unwrap()).unwrap();
        file.insert("5".into(), all);
        std::fs::write(
            journal_path("demo"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();

        // The pass on the far side of the clock: #5 stops, and says what it waited for.
        let _ = sweep();
        let why = stopped("demo", 5).unwrap_or_else(|| {
            panic!("a front that has waited twenty minutes on a check that never started is still holding the line, silently")
        });
        assert!(
            why.contains("waiting for GitHub to catch up") && why.contains("checks: none"),
            "the stop does not say what it waited for or why the wait cannot end: {why}"
        );
        assert!(
            why.contains("minutes"),
            "the stop does not say how long it waited, so nobody can judge it: {why}"
        );

        // And the line moves: the serial pass-over rule now finds #9 at the front, and it acts.
        let _ = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert!(
            calls.iter().any(|c| c.contains("/issues/9/labels")),
            "#5 stopped and the train still did not move on to #9: {calls:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// A check that IS running is not cut short — and a check that says it is running for ever is.
    ///
    /// The other side of [`a_wait_that_will_not_end_on_its_own`], and the more dangerous one: this
    /// bound is the only thing in skein that can stop a pull request for taking too long, and a CI
    /// run is allowed to take as long as it takes. The module note above builds the whole
    /// guarded-step design around surviving *"a forty-minute CI run"*, so a train that stopped one
    /// at twenty minutes would have traded a parked train for a broken one.
    ///
    /// What `checks: pending` earns is [`WAITING_ON_A_CHECK_MS`] of patience — more than seventy
    /// times the other ceiling — and not immunity. It used to earn immunity, and the two things it
    /// cannot tell apart are a check that is running and a check that stopped running without
    /// saying so; the second parks the front of a serial train for ever, and everything behind it,
    /// saying "CI is running" about a run that no longer exists (SKEIN-283).
    ///
    /// So the assertions come in pairs. Six hours of `pending` is a long build and is left alone.
    /// Twenty-five hours of `pending` is past every bound GitHub applies to a check of its own,
    /// and stops — with a sentence that says which of the two ceilings fell.
    #[test]
    fn a_running_check_earns_patience_and_not_immunity() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flow = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,"steps":[
              {"when":[],"do":"wait:CI is running"}]}]}"#,
        )
        .unwrap()
        .remove(0);
        let chosen = Chosen {
            step: 0,
            act: Act::Wait("CI is running".into()),
        };
        let running = crate::workflow::Facts {
            checks: "pending".into(),
            ..Default::default()
        };

        // A wait of a chosen age, written into the timeline. Each pull request gets its own,
        // because a stop written by one assertion would otherwise reset the next one's clock and
        // it would pass without the rule ever being consulted.
        let waiting_for = |number: u64, flow: &str, ms: i64| {
            record("demo", number, flow, 1, "waiting", "CI is running");
            let mut all = journal("demo", number);
            let last = all.len() - 1;
            all[last].at_ms -= ms;
            let mut file: std::collections::BTreeMap<String, Vec<JournalEntry>> =
                serde_json::from_str(&std::fs::read_to_string(journal_path("demo")).unwrap())
                    .unwrap();
            file.insert(number.to_string(), all);
            std::fs::write(
                journal_path("demo"),
                serde_json::to_vec_pretty(&file).unwrap(),
            )
            .unwrap();
        };

        // Six hours of a build that is genuinely running. Eighteen times past the ceiling a wait
        // with nothing behind it gets, and it must not be touched.
        waiting_for(7, "merge-train", 6 * 60 * 60 * 1000);
        assert_eq!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                7,
                &flow,
                &chosen,
                &running,
                "CI is running"
            ),
            None,
            "a check that is still running was stopped six hours into a build that is allowed to \
             take as long as it takes"
        );
        assert_eq!(stopped("demo", 7), None, "and it wrote the stop down too");

        // And the same wait with nothing running IS bounded, from the same timeline — so what
        // separates them is the evidence and not the clock.
        let nothing = crate::workflow::Facts {
            checks: "none".into(),
            ..Default::default()
        };
        assert!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                7,
                &flow,
                &chosen,
                &nothing,
                "CI is running"
            )
            .is_some(),
            "the bound never falls at all, on any wait"
        );

        // A check that has said `pending` for twenty-five hours. GitHub cancels a job at six hours
        // of running and at twenty-four of queueing, so nothing is coming — and until SKEIN-283
        // this parked the front of the train, and everything behind it, with no bound at all.
        waiting_for(11, "merge-train", WAITING_ON_A_CHECK_MS + 60 * 60 * 1000);
        let why = a_wait_that_will_not_end_on_its_own(
            "demo",
            11,
            &flow,
            &chosen,
            &running,
            "CI is running",
        )
        .unwrap_or_else(|| {
            panic!(
                "a front whose check has read `pending` for twenty-five hours is still holding \
                 the line, silently, and nothing in skein can ever stop it"
            )
        });
        assert!(
            why.contains("25 hours") && why.contains("pending"),
            "the stop does not say which ceiling fell or for how long: {why}"
        );
        assert_eq!(
            stopped("demo", 11).as_deref(),
            Some(why.as_str()),
            "the stop was returned but never written down, so the next pass parks again"
        );

        // The line between them is the ceiling and nothing else: one hour SHORT of it, the same
        // pending check is left alone.
        waiting_for(12, "merge-train", WAITING_ON_A_CHECK_MS - 60 * 60 * 1000);
        assert_eq!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                12,
                &flow,
                &chosen,
                &running,
                "CI is running"
            ),
            None,
            "a check pending for twenty-three hours was stopped — the ceiling is not where it says"
        );

        // And a workflow that is not a train is left alone even then — on its OWN expired clock,
        // so the only thing that can spare it is being non-serial. A stop is a demand for
        // somebody's attention, earned when the alternative is a queue that has stopped moving; a
        // pull request blocking nobody is not costing anything by waiting, and stopping it would
        // be manufacturing work.
        let loose = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"loose","steps":[{"when":[],"do":"wait:CI is running"}]}]}"#,
        )
        .unwrap()
        .remove(0);
        waiting_for(8, "loose", WAITING_ON_A_CHECK_MS + 60 * 60 * 1000);
        assert_eq!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                8,
                &loose,
                &chosen,
                &nothing,
                "CI is running"
            ),
            None,
            "a workflow with no train behind it stopped a pull request for waiting, which costs a \
             person an interruption and nobody a queue"
        );
        assert_eq!(stopped("demo", 8), None, "and it wrote the stop down too");

        std::env::remove_var("SKEIN_HOME");
    }

    /// The journal keeps the timeline: an act, a flag, and the hand that cleared it, in order.
    ///
    /// The owner's words: *"I need to know exactly what is it working on, which step is it on,
    /// status of previous steps and so on."* The stops file answers only "why is it stopped now";
    /// this is the record of what already happened — written where the events happen, so a
    /// timeline read tomorrow says what the audit said today.
    #[test]
    fn the_journal_keeps_the_timeline_of_did_stopped_and_cleared() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, _heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // An action that lands is a "did", carrying the flow, the 1-based step, and the sentence.
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false,
            })),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Did(_)), "{out:?}");
        let entries = journal("demo", 41);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(
            (
                entries[0].kind.as_str(),
                entries[0].flow.as_str(),
                entries[0].step
            ),
            ("did", "ship-mine", 4),
            "the entry must name the flow and the 1-based step: {entries:?}"
        );
        assert!(entries[0].what.contains("merged #41"), "{entries:?}");
        assert!(entries[0].at_ms > 0, "no timestamp: {entries:?}");

        // A flag is a "stopped", with the workflow's own reason.
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Flag("CI is red".into())),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");

        // And a person clearing the stop is an event too — flow-less, step-less, but on record.
        clear("demo", 41).expect("the stop must clear");

        let entries = journal("demo", 41);
        let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["did", "stopped", "cleared"],
            "the timeline is not in the order things happened: {entries:?}"
        );
        assert_eq!(entries[1].what, "CI is red");
        assert_eq!(entries[1].step, 4, "a flag is a step and must say which");
        assert_eq!(
            (entries[2].flow.as_str(), entries[2].step),
            ("", 0),
            "a clear is nobody's step: {entries:?}"
        );

        // The other reader carries the same timelines, keyed numerically.
        let all = journals("demo");
        assert_eq!(all.keys().copied().collect::<Vec<_>>(), vec![41]);
        assert_eq!(all[&41].len(), 3);

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// A failed action journals the same "stopped" it writes to the stops file.
    #[test]
    fn a_failed_action_reaches_the_journal_as_stopped() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, _heard) = github(409);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false,
            })),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");
        let entries = journal("demo", 41);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].kind, "stopped");
        assert!(
            entries[0].what.contains("ship-mine") && entries[0].what.contains("could not be done"),
            "the journal must keep the failure's own sentence: {entries:?}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// Each pull request keeps its newest fifty entries, and the oldest fall off.
    #[test]
    fn the_journal_caps_each_pull_request_at_its_newest_fifty() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        for i in 1..=55 {
            record("demo", 7, "train", 1, "did", &format!("event {i}"));
        }
        let entries = journal("demo", 7);
        assert_eq!(entries.len(), 50, "the cap did not hold");
        assert_eq!(
            entries[0].what, "event 6",
            "the OLDEST must fall off, not the newest"
        );
        assert_eq!(entries[49].what, "event 55");

        // Another pull request's timeline is untouched by #7's churn.
        record("demo", 9, "train", 1, "did", "only one");
        assert_eq!(journal("demo", 9).len(), 1);
        assert_eq!(journal("demo", 7).len(), 50);

        std::env::remove_var("SKEIN_HOME");
    }

    /// A corrupt journal reads as empty, never as an error — history lost is not action stopped.
    #[test]
    fn a_corrupt_journal_reads_as_empty() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        std::fs::create_dir_all(crate::prq::review_dir("demo")).unwrap();
        std::fs::write(journal_path("demo"), b"{ this is not json").unwrap();
        assert!(journal("demo", 7).is_empty());
        assert!(journals("demo").is_empty());
        // And the next write recovers rather than failing forever on the bad file. This is the
        // deliberate half of SKEIN-359: every other unreadable file in skein is refused, and this
        // one is written over, because refusing here would end journalling for the repo for good —
        // nothing repairs the file, and `record` is best-effort, so no caller would ever be told.
        record("demo", 7, "train", 1, "did", "back on the rails");
        assert_eq!(
            journal("demo", 7).len(),
            1,
            "the journal stopped recording after one unreadable file — a timeline that gives up \
             permanently is a worse answer than one that starts again, which is the whole reason \
             this file is written over rather than refused"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// The train view names the front, the whole line, and the passed-over — and only for a
    /// workflow that is serial, because a train is the serial thing.
    ///
    /// This is the panel's read of the same rule the tick acts on ([`sweep`] calls [`trains`]
    /// too), so what it asserts is the rule itself: oldest first, the first unstopped one is the
    /// front, a stopped PR is in the line AND named with its reason.
    #[test]
    fn a_train_view_names_the_front_the_line_and_the_passed_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[
              {"name":"train","serial":true,"steps":[{"when":[],"do":"merge:squash"}]},
              {"name":"loose","steps":[{"when":[],"do":"add-label:ci"}]}]}"#,
        )
        .unwrap();
        stop("demo", 5, "CI is red");

        // Handed over scrambled, so the order below is the function's own and not the caller's.
        let prs = vec![
            (9, "train".to_string()),
            (3, "loose".to_string()),
            (5, "train".to_string()),
            (7, "train".to_string()),
        ];
        let views = trains("demo", &prs, &flows);
        assert_eq!(
            views.len(),
            1,
            "a non-serial workflow got a train view: {views:?}"
        );
        let view = &views[0];
        assert_eq!(view.flow, "train");
        assert_eq!(
            view.front,
            Some(7),
            "the front must be the oldest UNSTOPPED pull request"
        );
        assert_eq!(
            view.line,
            vec![5, 7, 9],
            "train order is oldest first, front included"
        );
        assert_eq!(view.stopped.len(), 1);
        assert_eq!(
            (view.stopped[0].number, view.stopped[0].why.as_str()),
            (5, "CI is red"),
            "the passed-over must be named with its reason"
        );

        // Everyone stopped: a train with nobody to move has no front, and still shows its line.
        stop("demo", 7, "conflicts");
        stop("demo", 9, "checks");
        let views = trains("demo", &prs, &flows);
        assert_eq!(views[0].front, None);
        assert_eq!(views[0].line, vec![5, 7, 9]);
        assert_eq!(views[0].stopped.len(), 3);

        std::env::remove_var("SKEIN_HOME");
    }

    /// A remembered queue for `repo_id` listing exactly these open pull requests, put where
    /// `prq::remembered` reads it.
    ///
    /// That is the state a real counts poll runs in and not a convenience: a stop can only be
    /// written by a pass that read this repo's queue, so "a repo with stops and no remembered
    /// queue" is the cold-start case, never the steady one. `queue_within` deliberately neither
    /// caches nor remembers under `cfg!(test)`, so nothing is here unless a test says so.
    fn remember_open(repo_id: &str, numbers: &[u64], whole: bool) {
        let prs: Vec<serde_json::Value> = numbers
            .iter()
            .map(|n| {
                serde_json::json!({
                    "number": n,
                    "title": format!("pull request {n}"),
                    "author": "me",
                    "url": format!("https://github.com/acme/thing/pull/{n}"),
                    "head_ref": format!("feat-{n}"),
                    "head_sha": "deadbeef",
                    "base_ref": "main",
                    "draft": false,
                    "updated_at": "",
                    "committed_at": "",
                    "checks": "passing",
                    "my_review": "none",
                    "review_is_current": true,
                    "reasons": ["author"],
                    "lane": "needs-you",
                    "box_name": "",
                })
            })
            .collect();
        let queue: crate::prq::Queue = serde_json::from_value(serde_json::json!({
            "repo_id": repo_id,
            "slug": "acme/thing",
            "viewer": "me",
            "ai": false,
            "prs": prs,
            "blind_spots": [],
            "whole": whole,
        }))
        .expect("a queue in the shape prq writes one");
        crate::prq::remember_for_test(&queue);
    }

    /// The banner names only stops a person can reach — and the panel cannot name one the banner
    /// has never heard of (SKEIN-241).
    ///
    /// The reported failure, in order: the train stops on a pull request, a human merges it on
    /// GitHub, it leaves the queue, and the orange block row above the whole application goes on
    /// naming it. Clicking it opens a queue without it; both "let it run again" buttons are built
    /// from the live queue, so there is no way to dismiss it at all.
    ///
    /// The second half of the same defect is asserted here rather than in a second test on
    /// purpose: the two surfaces were two computations over two different sets, and what makes
    /// that a defect is only visible by comparing them. `trains` is the panel's set and `stops` is
    /// the banner's, and the departed pull request must be in neither.
    #[test]
    fn a_stop_on_a_pull_request_that_has_left_the_queue_is_not_shouted_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"train","serial":true,"steps":[{"when":[],"do":"merge:squash"}]}]}"#,
        )
        .unwrap();
        stop("gone", 123, "CI is red");
        stop("gone", 124, "conflicts");
        // #123 was merged by hand and has left the queue. #124 is still there.
        remember_open("gone", &[124], true);

        let banner = stops("gone");
        assert_eq!(
            banner.iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![124],
            "the banner still names a pull request that is not in the queue, and no row in the \
             cockpit can clear it: {banner:?}"
        );
        // The file keeps it. The banner is a demand for attention; the file is skein's memory of a
        // refusal, and forgetting that is how an action that failed gets attempted again.
        assert_eq!(
            stopped("gone", 123).as_deref(),
            Some("CI is red"),
            "the stop itself was deleted — the workflow may now re-attempt what it refused"
        );

        // The panel, from the same queue. Nothing it names may be missing from the banner, which
        // is the drift the two-computations defect was.
        let carrying = vec![(124u64, "train".to_string())];
        let panel = trains("gone", &carrying, &flows);
        assert_eq!(panel.len(), 1);
        for skipped in &panel[0].stopped {
            assert!(
                banner.iter().any(|s| s.number == skipped.number),
                "the panel names #{} as stopped and the banner does not — the two surfaces are \
                 reading different sets again",
                skipped.number
            );
        }
        assert!(
            !panel[0].line.contains(&123) && !banner.iter().any(|s| s.number == 123),
            "the merged pull request survives on one surface or the other"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// The whole chain, on the one event the train had no way to say anything about (SKEIN-247):
    /// a reviewer requests changes, and it becomes a stop on the row and a line on the banner.
    ///
    /// Four links, and every one of them was broken by the first: `facts_of` reads GitHub's
    /// verdict, `carries` decides the pull request is still the train's, `perform` runs the step
    /// written for it, and `stops` puts it where somebody sees it. This is asserted here rather
    /// than only in `workflow` because the defect was that the chain never STARTED — a unit test
    /// of `claims` alone would have passed against a train nothing ever reached.
    ///
    /// No network: `Act::Flag` is answered before `perform` touches the wire, which is what lets
    /// the whole path be walked without a GitHub.
    #[test]
    fn a_reviewer_requesting_changes_becomes_a_stop_and_a_banner_line() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");

        // The documented train's first two steps, and its `matches` — docs/pr-workflow.md, "The
        // train, written down".
        let flows = crate::workflow::from_bytes(
            r#"{"workflow":[{"name":"merge-train","serial":true,
                 "matches":["ready","approved","review-satisfied","base:trunk"],
                 "steps":[
                   {"when":["changes-requested"],"do":"flag:changes were requested - resolve them to rejoin the train"},
                   {"when":["label:ci-queue","checks:passing","mergeable","current"],"do":"merge:squash+delete"},
                   {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#
                .as_bytes(),
        )
        .unwrap();
        let pr = |decision: &str| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": decision, "mergeable": true,
                "merge_state": "CLEAN", "checks": "passing", "my_review": "none",
                "review_is_current": false, "reasons": [], "lane": "needs-you",
                "box_name": "demo-feat",
            }))
            .unwrap()
        };

        // Approved, and on the train. This is the state it is in the pass BEFORE the review lands.
        let approved = facts_of(&pr("APPROVED"), "me", "main");
        assert_eq!(
            carries("demo", 7, &approved, &flows),
            Carries::Matched("merge-train".into())
        );

        // The reviewer says no, and every reading of review moves at once: no approval stands, a
        // refusal does, and the repository's requirement is not met.
        let said_no = facts_of(&pr("CHANGES_REQUESTED"), "me", "main");
        assert!(
            !said_no.approved && said_no.changes_requested,
            "a refusal must outrank any approval standing behind it — `approved` and \
             `changes-requested` are allowed to disagree with each other and never to hold together"
        );
        assert_eq!(
            said_no.review_requirement_met,
            Some(false),
            "a refusal is the repository's requirement NOT met, and `review-satisfied` in the \
             train's `matches` turns on that"
        );
        assert_eq!(
            carries("demo", 7, &said_no, &flows),
            Carries::Matched("merge-train".into()),
            "the pull request left the train the moment a reviewer said no — silently, and one \
             pass before the step written for exactly this could fire"
        );

        let chosen = crate::workflow::next(&flows[0], &said_no).expect("a step must apply");
        // #7, not the shared `subject()` helper's #41: everything below reads the stop and the
        // journal by number, and a mismatch here would assert against an empty file.
        let seven = Subject {
            repo_id: "demo",
            slug: "acme/thing",
            number: 7,
            head_sha: "abc",
            head_ref: "feat",
            reading: None,
        };
        let outcome = perform(&seven, &flows[0], &chosen, &fixture_token());
        assert_eq!(
            outcome,
            Outcome::Stopped("changes were requested - resolve them to rejoin the train".into()),
            "the step that fired was not the one the reviewer's answer is about"
        );

        // On the row, in the timeline, and on the banner — the three places a person looks.
        assert!(stopped("demo", 7).is_some(), "nothing was written down");
        let timeline = journal("demo", 7);
        assert_eq!(
            timeline
                .iter()
                .map(|e| (e.kind.as_str(), e.step))
                .collect::<Vec<_>>(),
            vec![("stopped", 1)],
            "the journal does not say which step stopped it: {timeline:?}"
        );
        remember_open("demo", &[7], true);
        assert_eq!(
            stops("demo").iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![7],
            "the stop never reached the banner the counts poll draws"
        );

        // And the train passes it over rather than parking on it, which is the point of stopping
        // it rather than leaving it carried and idle.
        let line = trains(
            "demo",
            &[
                (7, "merge-train".to_string()),
                (9, "merge-train".to_string()),
            ],
            &flows,
        );
        assert_eq!(
            line[0].front,
            Some(9),
            "a stopped pull request held the line"
        );

        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A clear that could not be written says so, and puts nothing in the timeline (SKEIN-249).
    ///
    /// The reported failure: a person presses "let it run again", the stops file cannot be
    /// written, and all three of the things they can see say it worked — the button answers
    /// `{"ok": true}`, the journal gains a line saying the stop was cleared, and the stop is still
    /// on disk. The train never moves, and nothing anywhere disagrees with the story.
    ///
    /// This is a claim about ORDER, so it is asserted on the two things order decides: what came
    /// back, and what the timeline says. The journal is the sharper of the two — a `Result` nobody
    /// reads is a smaller lie than a record of an act that did not happen, because the record
    /// outlives the press and is what somebody debugging this reads a week later.
    ///
    /// **The DIRECTORY is what is made read-only, and it has to be** (SKEIN-414). This used to
    /// make the stops file itself mode 0400 — the report's own repro, and the right one while the
    /// write was `std::fs::write`, which opens the existing path for truncation and so needs write
    /// permission on the file and none on the directory. The write is now
    /// `util::write_atomic`: a temp file in the same directory, then a rename over the path. A
    /// rename does not open the target at all, so it succeeds on a read-only file and the old setup
    /// stops provoking anything. What it needs is a writable directory, so that is what is taken
    /// away — the temp cannot be created, the rename never happens, and the stop is untouched.
    /// Mode 0500 rather than 0400 because the lock beside the file still has to be opened, and the
    /// test would otherwise be measuring the traverse rather than the write.
    ///
    /// The guarantee is unchanged and so is everything asserted below; only the way a failed write
    /// is arranged moved. It still stands in for the two causes nobody can arrange in a test: a
    /// read-only host state directory, and a full disk.
    #[test]
    fn a_clear_that_could_not_be_written_reports_it_and_journals_nothing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("demo", 41, "CI is red");
        assert_eq!(stopped("demo", 41).as_deref(), Some("CI is red"));

        let dir = crate::prq::review_dir("demo");
        let mode = |bits: u32| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(bits)).unwrap();
        };
        mode(0o500);
        let refused = clear("demo", 41);
        mode(0o700);

        let why = refused.expect_err(
            "a clear that never reached disk answered the button `ok` — the person is told the \
             workflow may act again, and it is still stopped",
        );
        assert!(
            why.contains("#41") && why.contains("still stopped"),
            "the refusal does not say which pull request is still stopped: {why}"
        );
        assert_eq!(
            stopped("demo", 41).as_deref(),
            Some("CI is red"),
            "the stop is gone from disk after a write that failed"
        );
        assert!(
            !journal("demo", 41).iter().any(|e| e.kind == "cleared"),
            "the timeline says a person lifted this stop, and nobody did: {:?}",
            journal("demo", 41)
        );

        // And the same press through the route's own entry point, which is what the cockpit calls
        // — `apply` must not answer Ok for a clear that did not happen.
        mode(0o500);
        let refused = apply("demo", 41, None, false, true);
        mode(0o700);
        assert!(
            refused.is_err(),
            "apply swallowed the failure, so the row reports success"
        );

        // Cleared for real: the write lands, the timeline gains its line, and it is idempotent —
        // a second press has nothing to do and is not a failure.
        clear("demo", 41).expect("a writable stops file must clear");
        assert_eq!(stopped("demo", 41), None);
        assert_eq!(
            journal("demo", 41)
                .iter()
                .filter(|e| e.kind == "cleared")
                .count(),
            1,
            "the clear that DID happen is missing from the timeline, or is in it twice"
        );
        clear("demo", 41).expect("clearing a pull request with no stop is not a failure");

        std::env::remove_var("SKEIN_HOME");
    }

    /// **Two writers at once lose neither a stop, a choice, nor a journal entry** (SKEIN-414).
    ///
    /// All three of this module's files are a read-modify-write over a whole map, and until now
    /// none of them took a lock. Two of those interleaving is last-write-wins where the loser is a
    /// whole entry: a stop somebody's pull request depends on, a person's `Excluded`, or a line of
    /// the timeline. The two writers are the real ones — the tick sweeps every repo on its own
    /// thread while the cockpit's routes call `assign`, `stop` and `clear` from request threads —
    /// and threads here stand in for that, at the only rate that makes a microsecond-wide window
    /// reproducible.
    ///
    /// The journal entries all land on ONE pull request on purpose: the other two files lose an
    /// entry when two writers pick different keys, and the journal loses one when they pick the
    /// same key and both append. Below the fifty-entry cap, so what is asserted is the loss and not
    /// the trim.
    #[test]
    fn two_writers_at_once_lose_neither_a_stop_a_choice_nor_a_journal_entry() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let (writers, each) = (8u64, 6u64);
        let hands: Vec<_> = (0..writers)
            .map(|w| {
                std::thread::spawn(move || {
                    for i in 0..each {
                        let number = w * each + i;
                        assign("race", number, "train").expect("assigned");
                        stop("race", number, "CI is red");
                        record("race", 999, "train", 1, "did", "merged it");
                    }
                })
            })
            .collect();
        for hand in hands {
            hand.join().expect("a writer panicked");
        }

        let total = (writers * each) as usize;
        assert_eq!(
            read_stops("race").len(),
            total,
            "a stop was lost to another writer, and the pull request it belonged to will be \
             attempted again"
        );
        assert_eq!(
            read_assigned("race").len(),
            total,
            "a person's choice about what may touch a pull request was lost to another writer"
        );
        assert_eq!(
            journal("race", 999).len(),
            total,
            "a journal entry was lost to another writer, and a timeline cannot show what is \
             missing from it"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A stop file is replaced whole, never truncated and refilled** (SKEIN-414).
    ///
    /// `std::fs::write` truncates the file and then writes it, so a crash, a kill or a full disk in
    /// that window leaves a half-written file — and a half-written file is exactly the unreadable
    /// input SKEIN-359 spent its length teaching this module to refuse. Refusing is the reading end;
    /// this is the end that manufactures it. `util::write_atomic` writes a temp beside the file and
    /// renames over it, so a reader sees the old whole file or the new one.
    ///
    /// Asserted on the inode, because that is what tells the two mechanisms apart from the outside:
    /// a truncate-and-rewrite keeps it, a rename replaces it. The temp is checked for too — one
    /// left behind is a file nothing will rename into place and nothing will clean up.
    #[test]
    fn a_stop_is_written_by_replacing_the_file_rather_than_truncating_it() {
        use std::os::unix::fs::MetadataExt;
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("demo", 41, "CI is red");
        let path = stops_path("demo");
        let before = std::fs::metadata(&path).expect("the stops file").ino();
        stop("demo", 42, "conflicts");
        let after = std::fs::metadata(&path).expect("the stops file").ino();

        assert_ne!(
            before, after,
            "the stops file was rewritten in place — a crash mid-write leaves half a file, and \
             everything that reads it is then refused"
        );
        assert_eq!(stopped("demo", 41).as_deref(), Some("CI is red"));
        assert_eq!(stopped("demo", 42).as_deref(), Some("conflicts"));

        let strays: Vec<String> = std::fs::read_dir(crate::prq::review_dir("demo"))
            .expect("the review dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".skein.tmp."))
            .collect();
        assert!(
            strays.is_empty(),
            "a temp file was left beside the stops: {strays:?}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A queue that could not see everything silences nothing.
    ///
    /// The direction this must fail in. A refresh that went dark produces the same empty `prs`
    /// list as a repo with nothing open, and reading that as "every stop is dismissible" would
    /// take every banner in the fleet down for one rate-limit window — SKEIN-229's failure through
    /// a different file. Both the partial queue and the repo skein has never read must show
    /// everything the file holds.
    #[test]
    fn a_queue_that_did_not_see_everything_hides_no_stop() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("blind", 7, "CI is red");
        stop("blind", 9, "conflicts");

        // Never read at all — the cold start. Absence is evidence about skein, not about #7.
        assert_eq!(
            stops("blind").iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![7, 9],
            "a repo with no queue on this machine had its stops hidden"
        );

        // Read, and cut off at its page: every pull request past the page is absent for a reason
        // that has nothing to do with it.
        remember_open("blind", &[], false);
        assert_eq!(
            stops("blind").iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![7, 9],
            "a partial queue was read as proof that nothing is open"
        );

        // And the whole answer, which is allowed to be empty: it answered.
        remember_open("blind", &[], true);
        assert!(
            stops("blind").is_empty(),
            "a queue that saw everything and found nothing open still shouted about its stops"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A person's choice about which pull request a workflow may touch survives a file skein
    /// cannot read** (SKEIN-359).
    ///
    /// `assign` is a read-modify-write over every assignment in the repo. Read through
    /// `read_to_string(..).ok()`, an unparseable `workflow-assigned.json` read as *no assignments*,
    /// so putting a workflow on one pull request wrote that single entry over all the others. The
    /// entry that matters most is the empty one — `Excluded`, a person saying no rule may claim
    /// this pull request — and losing it does not leave that pull request idle: it hands it back to
    /// the sweep, which then acts on the one somebody took out of its reach.
    #[test]
    fn an_assignment_file_skein_cannot_read_is_never_written_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        assign("demo", 7, "train").unwrap();
        assign("demo", 9, "").unwrap(); // excluded by hand: the one nobody may override
        let path = assign_path("demo");

        for corrupt in [&b""[..], &b"{\"7\":\"train\""[..]] {
            std::fs::write(&path, corrupt).unwrap();
            let why = assign("demo", 11, "loose")
                .expect_err("assigning over an unreadable file reported success");
            assert!(
                why.contains("cannot read") && why.contains("workflow-assigned.json"),
                "the refusal has to say what skein declined to do and name the file somebody must \
                 go and look at, not just report a parse error: {why}"
            );
            assert!(
                unassign("demo", 7).is_err(),
                "clearing one choice is not how the others are cleared"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the unreadable assignment file was written over"
            );
        }

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A stop file skein cannot read is never replaced by one holding no stops at all**
    /// (SKEIN-359).
    ///
    /// A stop is the record that acting on a pull request went wrong and must not be tried again.
    /// Every stop in the repo is in one file, so writing one stop over a read that answered "no
    /// stops" lets every *other* stopped pull request move again — the re-attempt loop the rule
    /// exists to prevent, arriving for the whole repo at once. `clear` is the sharper half: read as
    /// empty it finds no stop, returns Ok, and tells a person their press worked while the file it
    /// would have written is the one that destroys the rest.
    #[test]
    fn a_stop_file_skein_cannot_read_is_never_written_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("demo", 41, "CI is red");
        stop("demo", 42, "conflicts");
        let path = stops_path("demo");

        for corrupt in [&b""[..], &b"{\"41\":\"CI is red\""[..]] {
            std::fs::write(&path, corrupt).unwrap();

            stop("demo", 43, "a third thing");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "a new stop was written over a stop file skein could not read"
            );

            let why = clear("demo", 41)
                .expect_err("a clear over an unreadable stop file answered the button `ok`");
            assert!(
                why.contains("#41") && why.contains("still stopped"),
                "the refusal has to say which pull request is still stopped: {why}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the unreadable stop file was written over by a clear"
            );
        }

        // And it recovers by itself once the file parses: nothing was destroyed in between.
        std::fs::write(&path, b"{\"41\":\"CI is red\",\"42\":\"conflicts\"}").unwrap();
        clear("demo", 41).unwrap();
        assert_eq!(stopped("demo", 42).as_deref(), Some("conflicts"));

        std::env::remove_var("SKEIN_HOME");
    }

    /// **The reviewer's facts are about YOU, and skein says what it never saw**
    /// (`docs/pr-review.md` §7).
    ///
    /// All four adapter rules asserted where they live — on the fields, not on a step — and each
    /// one as an invariant rather than as a fixture:
    ///
    /// * **§7a** the whole reviewer vocabulary answers identically on a pull request the
    ///   repository is satisfied with and on one nobody has approved. Same inputs, twice,
    ///   identical expectations: that is the assertion, and it is the one thing a per-case test
    ///   cannot make. `Facts::approved` is the pull request's answer and it is not the reviewer's
    ///   question (SKEIN-339).
    /// * **§7b** a short review connection takes `unreviewed` away rather than answering it, for
    ///   every verdict the cap could have cut around — `prq::Pr::reviews_whole` is the same rule
    ///   `labels_whole` already carries.
    /// * **§7c** no queue row can say a reading covered the change, so nothing built from one
    ///   holds `reading-whole` — which makes `post-approval` unreachable rather than permitted.
    /// * **§7d** a verdict whose head has moved is neither standing nor unreviewed, and that
    ///   state is exactly the pull request `prq`'s lane has released (`decided &&
    ///   !my_review_requested` → `Lane::Waiting`, kept in `review::worth_a_visit`'s scope only
    ///   where you authored it) and the engine has undertaken to keep watching.
    ///
    /// What would make each fail: reading `pr.review_decision` into `my_review`; dropping
    /// `pr.reviews_whole()`; defaulting `reading_whole` to `Some(true)`; and folding "you decided"
    /// and "your verdict stands" into one field, which is how the first engine verdict would take
    /// a pull request out of the engine's own scope with nothing able to say so.
    #[test]
    fn the_reviewers_facts_answer_about_you_and_say_what_skein_never_saw() {
        // `review_decision` and `standing_approvals` are the PULL REQUEST's review state;
        // `my_review`, `review_is_current` and `my_review_requested` are yours. They move
        // independently here because that is the whole of §7a.
        let pr = |decision: &str,
                  approvals: u64,
                  my_review: &str,
                  current: bool,
                  requested: bool,
                  reviews: (u64, u64)|
         -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": decision, "standing_approvals": approvals,
                "reviews_total": reviews.0, "reviews_read": reviews.1,
                "mergeable": true, "merge_state": "CLEAN", "checks": "passing",
                "my_review": my_review, "review_is_current": current,
                "my_review_requested": requested,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };
        let reviewer = [
            crate::workflow::Cond::ReviewRequested,
            crate::workflow::Cond::Unreviewed,
            crate::workflow::Cond::ReadingCurrent,
            crate::workflow::Cond::ReadingStale,
            crate::workflow::Cond::ReadingWhole,
            crate::workflow::Cond::FindingsBlocking,
            crate::workflow::Cond::VerdictStanding,
        ];
        let answers = |f: &crate::workflow::Facts| {
            reviewer
                .iter()
                .map(|cond| crate::workflow::holds(cond, f))
                .collect::<Vec<_>>()
        };

        for my_review in ["none", "commented", "approved", "changes-requested"] {
            for current in [false, true] {
                for requested in [false, true] {
                    // **§7a.** The same reviewer facts under three different answers to the pull
                    // request's own question — no requirement and nobody's approval, no
                    // requirement and three approvals, and a repository that is satisfied.
                    let mine = facts_of(
                        &pr("", 0, my_review, current, requested, (2, 2)),
                        "me",
                        "main",
                    );
                    let baseline = answers(&mine);
                    for (decision, approvals) in [("", 3u64), ("APPROVED", 3), ("APPROVED", 0)] {
                        let theirs = facts_of(
                            &pr(decision, approvals, my_review, current, requested, (2, 2)),
                            "me",
                            "main",
                        );
                        assert_ne!(
                            theirs.approved, mine.approved,
                            "the fixture stopped moving `approved`, so the comparison below is \
                             asserting nothing"
                        );
                        assert_eq!(
                            answers(&theirs),
                            baseline,
                            "a reviewer condition changed its answer because somebody ELSE \
                             approved ({decision:?}, {approvals} standing) — the reviewer's \
                             question is `my_review` with `review_is_current`, and reading \
                             `Facts::approved` for it is SKEIN-339 under a new word"
                        );
                    }

                    // **§7b.** The same row with the review connection cut: `unreviewed` is a
                    // claim about the reviews that did NOT arrive, so it may not be made.
                    let cut = facts_of(
                        &pr("", 0, my_review, current, requested, (40, 30)),
                        "me",
                        "main",
                    );
                    assert!(
                        !cut.reviews_whole
                            && !crate::workflow::holds(&crate::workflow::Cond::Unreviewed, &cut),
                        "GitHub counted 40 reviews, 30 arrived, and `unreviewed` answered anyway \
                         — the box this design came from read a short list as absence and took \
                         the missing rows for closed pull requests"
                    );

                    // **§7c.** A queue row cannot say a reading covered the change. `None` is what
                    // "skein does not know" is spelled as, and it holds nothing.
                    assert_eq!(
                        (
                            mine.reading_sha.as_deref(),
                            mine.reading_whole,
                            mine.findings_blocking
                        ),
                        (None, None, None)
                    );
                    assert!(
                        !crate::workflow::holds(&crate::workflow::Cond::ReadingWhole, &mine)
                            && !crate::workflow::holds(
                                &crate::workflow::Cond::ReadingCurrent,
                                &mine
                            )
                            && !crate::workflow::holds(&crate::workflow::Cond::ReadingStale, &mine),
                        "a fact built from a queue row claimed a reading skein has not made — \
                         `reading-whole` is the one condition `post-approval` requires"
                    );
                    // And the head IS on the row, so the guard has its anchor the moment a
                    // reading arrives.
                    assert_eq!(mine.head_sha, "abc");
                }
            }
        }

        // **§7d**, twice — once for each verdict, and identical expectations are the assertion.
        // A decision whose head has moved: your verdict does not stand, and the pull request is
        // not unreviewed either. `prq` has already released it (`decided && !my_review_requested`
        // → `Lane::Waiting`), and on somebody else's pull request `review::worth_a_visit` drops a
        // `Waiting` row — so this state is the engine's own scope hole, and the facts can say it.
        for verdict in ["approved", "changes-requested"] {
            let moved = facts_of(&pr("", 0, verdict, false, false, (2, 2)), "me", "main");
            assert_eq!(
                moved.my_review, verdict,
                "your verdict is carried, not inferred"
            );
            assert!(
                !crate::workflow::holds(&crate::workflow::Cond::VerdictStanding, &moved),
                "a {verdict} left against an older commit read as standing against this one"
            );
            assert!(
                !crate::workflow::holds(&crate::workflow::Cond::Unreviewed, &moved),
                "a pull request you have decided on read as never decided"
            );
            // The same verdict against the head that is there now DOES stand — the counter-case,
            // and the difference §7d needs to be sayable.
            let standing = facts_of(&pr("", 0, verdict, true, false, (2, 2)), "me", "main");
            assert!(crate::workflow::holds(
                &crate::workflow::Cond::VerdictStanding,
                &standing
            ));
        }
    }

    // ─────────────── §7c: did that pass cover the whole change? ───────────────

    /// A pull request at a head of the test's choosing, and nothing else varying.
    fn pr_at(head_sha: &str) -> crate::prq::Pr {
        serde_json::from_value(serde_json::json!({
            "number": 41, "title": "t", "author": "someone", "url": "u",
            "head_ref": "feat", "head_sha": head_sha, "base_ref": "main",
            "draft": false, "updated_at": "", "committed_at": "",
            "labels": [], "labels_total": 0,
            "review_decision": "APPROVED", "standing_approvals": 1,
            "mergeable": true, "merge_state": "CLEAN", "checks": "passing",
            "my_review": "none", "review_is_current": false,
            "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
        }))
        .unwrap()
    }

    /// A reading as `review.rs` would have written it — through `review::Summary` itself, so the
    /// field names on disk are the ones that module owns rather than ones this test invented.
    fn a_reading(
        head_sha: &str,
        depth: crate::review::Depth,
        swept: bool,
    ) -> crate::review::Summary {
        crate::review::Summary {
            owed_triggered: None,
            findings_block: None,
            number: 41,
            head_sha: head_sha.into(),
            depth,
            line: "it changes a thing.".into(),
            detail: String::new(),
            flags: Vec::new(),
            signals: Vec::new(),
            yours: Vec::new(),
            others: 0,
            ownership_unknown: String::new(),
            unread_because: String::new(),
            not_reread: String::new(),
            computed: true,
            budget_stopped: false,
            swept,
        }
    }

    /// Where `review::cache_path` files a reading of exactly this commit.
    ///
    /// **The copy lives here, and only here.** Production reads through `review::cached`, so this
    /// is the one place that still spells the filename by hand — and it has to, because `review`
    /// exposes no writer: a test that wants a reading on disk must know where one goes. That makes
    /// it exactly the right copy to keep. It cannot make production agree with itself while both
    /// drift; it can only disagree with production, which is the failure the pinning test below is
    /// looking for.
    fn reading_path(repo_id: &str, number: u64, head_sha: &str) -> std::path::PathBuf {
        let key: String = head_sha
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(40)
            .collect();
        crate::prq::review_dir(repo_id)
            .join("summaries")
            .join(format!("{number}-{key}.json"))
    }

    /// Filed where the engine looks for it, through the engine's own expression.
    fn file_the_reading(repo_id: &str, s: &crate::review::Summary) {
        let path = reading_path(repo_id, s.number, &s.head_sha);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(s).unwrap()).unwrap();
    }

    /// **The seam between `review` and `prwork` is a path, and this is what holds it shut.**
    ///
    /// [`reading_path`] is a copy of `review::cache_path`, kept in this test module because
    /// `review` exposes no writer — so a fixture that wants a reading on disk has to spell the
    /// filename itself. Production no longer does: [`the_reading_skein_holds_at`] goes through
    /// `review::cached`. The drift this catches is therefore the only one left, and it is silent in
    /// the worst direction: the engine simply stops finding readings and every approval waits for
    /// ever with nothing to say why.
    ///
    /// A test may name `crate::review` — `tools/module-check.py` cuts `#[cfg(test)] mod tests`
    /// before reading the graph, on the argument that a fixture's reach says nothing about the
    /// design — so the two sides can be tied together here even though the code may not.
    ///
    /// **What would make this fail:** renaming `summaries/` or the `{number}-{sha}.json` filename
    /// in `review::cache_path`, or renaming `Summary::swept`, or changing how `Depth` serialises.
    #[test]
    fn the_path_the_engine_reads_a_reading_from_is_the_one_review_writes_it_to() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let swept = a_reading("abc123", crate::review::Depth::Line, true);
        file_the_reading("r", &swept);
        assert!(
            crate::review::cached("r", 41, "abc123").is_some(),
            "the engine reads readings from a path `review::cached` cannot find one at — the two \
             copies of the cache path have drifted, and the engine is blind rather than wrong"
        );
        assert_eq!(
            the_reading_skein_holds_at("r", 41, "abc123"),
            Some(true),
            "a swept reading, serialised by `review::Summary` itself, did not read back as swept \
             — the field the engine looks for is not the field that module writes"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A reading of an older head says nothing about the one that is there now** (§4).
    ///
    /// The sha is part of the program counter: `review.rs` puts it in the cache filename precisely
    /// so a stale reading cannot be read as a fresh one, and the engine's lookup inherits that. A
    /// pull request that gained a commit since it was read is one skein has not read.
    ///
    /// **What would make this fail:** looking a reading up by number alone — a glob over
    /// `summaries/41-*.json`, or a per-PR file with the head compared inside it and the comparison
    /// forgotten. Either would hand `Some(true)` to a head nobody looked at.
    #[test]
    fn a_reading_of_an_older_head_cannot_say_the_change_that_is_there_now_was_wholly_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Read at `aaa`, and swept. This is a reading that CAN say `Some(true)` — asserted first,
        // because an absence that was never a presence proves nothing.
        file_the_reading("r", &a_reading("aaa", crate::review::Depth::Line, true));
        let at_the_head_it_read = facts_of_in("r", &pr_at("aaa"), "me", "main");
        assert_eq!(
            at_the_head_it_read.reading_whole,
            Some(true),
            "the fixture cannot answer at the head it was written for, so the stale case below \
             would pass against a lookup that never works"
        );

        // The same reading, and the pull request has moved on.
        let after_a_push = facts_of_in("r", &pr_at("bbb"), "me", "main");
        assert_eq!(
            after_a_push.reading_whole, None,
            "a reading of an earlier commit was allowed to vouch for the code that is there now — \
             which is the stale-approval hole this whole design exists to close"
        );
        assert_eq!(
            after_a_push.reading_sha, None,
            "skein claimed to hold a reading of a commit it has never read"
        );
        assert!(
            !crate::workflow::holds(&crate::workflow::Cond::ReadingWhole, &after_a_push),
            "`reading-whole` held for a head no reading covers"
        );
        assert!(
            crate::workflow::instead_of_approving_what_was_not_wholly_read(
                &crate::workflow::Act::PostApproval,
                &after_a_push,
            )
            .is_some(),
            "an approval was reachable on a commit skein has not read"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **Every way of not knowing is `None`, and none of them is `Some(true)`** (§7c).
    ///
    /// Four of them, and the third is the hostile one: a `Depth::Unread` summary with `swept` set.
    /// Nothing writes that today, but `Depth::Unread` is skein saying it did not read this PR —
    /// "never a judgement about the PR, always about skein" — so a reading that failed may not be
    /// let through on a flag beside it. `review.rs`'s rule is the one that decides it: **AI may
    /// only add scrutiny, never remove it.**
    ///
    /// `Some(false)` is asserted absent as well as `Some(true)`. Unknown is not false: a reading
    /// skein could not make is blindness, and `workflow::instead_of_approving_what_was_not_wholly_read`
    /// spends the difference — blindness waits, and only a sweep that ran and reported a partial
    /// pass would flag. Nothing here has that to say.
    ///
    /// **What would make this fail:** reading `swept` without first refusing `Depth::Unread`;
    /// treating a missing or unparseable file as anything but unknown; or mapping "a reading with
    /// no sweep behind it" to `Some(false)` instead of `None`.
    #[test]
    fn a_reading_that_failed_or_is_absent_leaves_coverage_unknown_and_never_covered() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let coverage = |repo: &str| facts_of_in(repo, &pr_at("aaa"), "me", "main").reading_whole;

        // 1. Nothing was ever read.
        assert_eq!(
            coverage("absent"),
            None,
            "no reading at all read as an answer"
        );

        // 2. A reading, and no sweep spoke for it — including every reading cached before the
        //    field existed, which deserialises to exactly this.
        file_the_reading(
            "unswept",
            &a_reading("aaa", crate::review::Depth::Line, false),
        );
        assert_eq!(
            coverage("unswept"),
            None,
            "a pass with no sweep behind it was read as having covered the whole change"
        );

        // 3. The reading FAILED, and something set the flag anyway.
        file_the_reading(
            "failed",
            &a_reading("aaa", crate::review::Depth::Unread, true),
        );
        assert_eq!(
            coverage("failed"),
            None,
            "a reading skein could not make was allowed to vouch for coverage because a flag \
             beside it said so — AI may only add scrutiny, never remove it"
        );

        // 4. The file will not parse. Same answer as no file: there is no reader here to tell.
        let broken = reading_path("broken", 41, "aaa");
        std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
        std::fs::write(&broken, "{ not json").unwrap();
        assert_eq!(
            coverage("broken"),
            None,
            "a summary file that will not parse answered a question about coverage"
        );

        for repo in ["absent", "unswept", "failed", "broken"] {
            assert_ne!(
                facts_of_in(repo, &pr_at("aaa"), "me", "main").reading_whole,
                Some(false),
                "{repo}: skein does not know whether the pass was partial, and said it was — \
                 unknown is not false, and `Act::Flag` is not `Act::Wait`"
            );
        }

        std::env::remove_var("SKEIN_HOME");
    }

    /// **Coverage moves the approval and nothing else** (§7c: "`ReadingWhole` is a required
    /// condition of `PostApproval` and of nothing else").
    ///
    /// Two evaluations of the same pull request, differing only in whether the sweep spoke for the
    /// reading. Everything else about the facts must be identical — a fact that changed with
    /// coverage would be coverage leaking into a question it does not answer — and the one thing
    /// that must change is whether an approval can be reached. Findings and a request for changes
    /// are checked to be unmoved: §7c permits both on a partial pass, because a reader who saw
    /// half a change and found a bug in that half has something true to say.
    ///
    /// **What would make this fail:** gating `PostFindings` or `PostChanges` on coverage too;
    /// letting an unswept reading through to `PostApproval`; or deriving any other fact from the
    /// same lookup.
    #[test]
    fn coverage_decides_the_approval_and_leaves_every_other_fact_and_act_alone() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        file_the_reading("swept", &a_reading("aaa", crate::review::Depth::Line, true));
        file_the_reading(
            "partial",
            &a_reading("aaa", crate::review::Depth::Line, false),
        );
        let whole = facts_of_in("swept", &pr_at("aaa"), "me", "main");
        let partial = facts_of_in("partial", &pr_at("aaa"), "me", "main");

        let approval = |f: &crate::workflow::Facts| {
            crate::workflow::instead_of_approving_what_was_not_wholly_read(
                &crate::workflow::Act::PostApproval,
                f,
            )
        };
        assert_eq!(
            approval(&whole),
            None,
            "a reading whose sweep accounted for the whole change still could not approve, so no \
             approval is reachable by any route and the guard is a wall rather than a gate"
        );
        assert!(
            matches!(approval(&partial), Some(crate::workflow::Act::Wait(_))),
            "an approval was reachable from a reading nothing accounted for"
        );

        // And coverage is ALL that moved. A fact that changed with it would be the lookup
        // answering a question it was not asked.
        assert_eq!(whole.reading_whole, Some(true));
        assert_eq!(partial.reading_whole, None);
        assert_eq!(
            crate::workflow::Facts {
                reading_whole: partial.reading_whole,
                ..whole.clone()
            },
            partial,
            "coverage changed something other than coverage — a lookup that answers one question \
             is answering others"
        );

        for act in [
            crate::workflow::Act::PostFindings,
            crate::workflow::Act::PostChanges,
        ] {
            for f in [&whole, &partial] {
                assert_eq!(
                    crate::workflow::instead_of_approving_what_was_not_wholly_read(&act, f),
                    None,
                    "{act:?} was held back by coverage — §7c gates the approval and nothing else"
                );
            }
        }

        std::env::remove_var("SKEIN_HOME");
    }

    // ─────────────── §15 step 5: what a repository owes a reviewer (§8) ───────────────

    /// **A reading made before owed checks existed does not report that nothing is owed.**
    ///
    /// The one direction §8 exists to close, and the one this could most easily have got wrong. A
    /// `Vec<String>` with serde's default would deserialise every summary already on disk to an
    /// empty list — indistinguishable from a diff that fired nothing — and every pull request read
    /// before today would have sailed past `checks-settled` into a verdict. `Option` is what keeps
    /// "nobody computed this" a different answer from "nothing fired", and this is where that is
    /// worth its cost.
    ///
    /// **What would make this fail:** making `Summary::owed_triggered` a plain `Vec`, or reading
    /// `None` as settled here. Either turns the third row below from `None` into `Some(false)`, and
    /// `Cond::ChecksSettled` starts holding on a reading nobody scanned.
    #[test]
    fn a_reading_that_predates_owed_checks_is_unknown_rather_than_settled() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &*home);
        crate::repos::save_repos(&[a_repo_that_may_be_read()]).unwrap();

        // No reading at all: unknown, and a verdict waits.
        assert_eq!(what_this_change_still_owes("demo", 41, "abc"), None);

        // A reading from a skein that never computed the triggers.
        let mut old = a_reading("abc", crate::review::Depth::Line, true);
        old.owed_triggered = None;
        file_the_reading("demo", &old);
        assert_eq!(
            what_this_change_still_owes("demo", 41, "abc"),
            None,
            "a reading nobody scanned was read as one that owes nothing"
        );

        // A reading that WAS scanned and found nothing: settled, and a verdict may go.
        let mut clean = a_reading("abc", crate::review::Depth::Line, true);
        clean.owed_triggered = Some(Vec::new());
        file_the_reading("demo", &clean);
        assert_eq!(
            what_this_change_still_owes("demo", 41, "abc"),
            Some(false),
            "a diff that fired nothing left a verdict unreachable"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **An audit is owed until it is answered, and answered means at THIS commit.**
    ///
    /// §8's sentence, end to end through the three stores it actually spans: the trigger in the
    /// summary `review` wrote, the set in the `Repo`, and the answer `owed::record` leaves.
    ///
    /// **What would make each assertion fail:** dropping the `done` term re-audits the same removal
    /// on every pass for ever; dropping the `set` term ignores a repository that said it does not
    /// want this; keying the record on the pull request rather than on the commit lets one audit
    /// stand for every commit after it, which is §4's anchoring failure in another hat.
    #[test]
    fn a_check_is_owed_until_it_is_answered_at_the_commit_it_was_asked_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &*home);
        crate::repos::save_repos(&[a_repo_that_may_be_read()]).unwrap();

        let mut deleted = a_reading("abc", crate::review::Depth::Line, true);
        deleted.owed_triggered = Some(vec!["deletions".into()]);
        file_the_reading("demo", &deleted);
        assert_eq!(
            what_this_change_still_owes("demo", 41, "abc"),
            Some(true),
            "a change that removes lines owed nothing"
        );
        assert_eq!(
            the_first_check_still_owed(&a_repo_that_may_be_read(), 41, "abc"),
            Some(crate::owed::Check::Deletions)
        );

        crate::owed::record("demo", 41, "abc", crate::owed::Check::Deletions).unwrap();
        assert_eq!(
            what_this_change_still_owes("demo", 41, "abc"),
            Some(false),
            "an answered check stayed owed, so the engine would audit it again for ever"
        );
        assert_eq!(
            the_first_check_still_owed(&a_repo_that_may_be_read(), 41, "abc"),
            None
        );

        // The same reading at a different commit: the answer does not travel with it.
        let mut moved = a_reading("def", crate::review::Depth::Line, true);
        moved.owed_triggered = Some(vec!["deletions".into()]);
        file_the_reading("demo", &moved);
        assert_eq!(
            what_this_change_still_owes("demo", 41, "def"),
            Some(true),
            "an audit of one commit answered for the next one"
        );

        // A repository that has said it owes nothing owes nothing, whatever the diff did.
        let quiet = crate::repos::Repo {
            owed_checks: Some(Vec::new()),
            ..a_repo_that_may_be_read()
        };
        crate::repos::save_repos(std::slice::from_ref(&quiet)).unwrap();
        assert_eq!(
            what_this_change_still_owes("demo", 41, "def"),
            Some(false),
            "a repository that switched this off was still made to audit"
        );
        assert_eq!(the_first_check_still_owed(&quiet, 41, "def"), None);

        std::env::remove_var("SKEIN_HOME");
    }

    // ─────────────── §15 step 3: the one reviewer act that is wired ───────────────

    /// A repo with every flag a reading needs, and nothing else varying.
    fn a_repo_that_may_be_read() -> crate::repos::Repo {
        crate::repos::Repo {
            id: "demo".into(),
            read_prs: true,
            auto_review: true,
            ..Default::default()
        }
    }

    /// A subject carrying everything `Act::Read` needs, anchored at one commit.
    fn readable<'a>(
        repo: &'a crate::repos::Repo,
        pr: &'a crate::prq::Pr,
        head_sha: &'a str,
        facts: &'a crate::workflow::Facts,
    ) -> Subject<'a> {
        Subject {
            repo_id: "demo",
            slug: "acme/thing",
            number: 41,
            head_sha,
            head_ref: "feat",
            reading: Some(Reading {
                repo,
                pr,
                viewer: "owner",
                facts,
            }),
        }
    }

    /// Facts that pass §10's two read-side gates, so a test about anything else is not silently
    /// about them: GitHub asked you by name (the default trigger set's only member) and the pull
    /// request is yours (the default author filter).
    fn woken_and_mine() -> crate::workflow::Facts {
        crate::workflow::Facts {
            review_requested: true,
            mine: true,
            ..Default::default()
        }
    }

    /// Switch the workflow engine on in a home of this test's own.
    fn a_fleet_where_workflows_run(home: &std::path::Path) {
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
    }

    fn and_no_longer(home_keys: &[&str]) {
        for key in home_keys {
            std::env::remove_var(key);
        }
    }

    /// **The money door, on the acting path.** A `read` step against a repo whose automatic review
    /// is off must stop, and the stop must name the switch that is shut.
    ///
    /// **What would make this fail:** deleting the `repos::auto_review_stands` call from
    /// `read_now`. Then a repo with every reviewer flag off would be read anyway, and this asserts
    /// on `Outcome::Stopped` — so the act would come back `Waited` or `Did` and the match panics.
    #[test]
    fn a_read_step_where_automatic_review_is_off_stops_and_names_the_switch() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // Reading is on for the repo; automatic review is not. That is the ordinary state of every
        // repo in the registry, because `auto_review` defaults off and nothing turns it on.
        let repo = crate::repos::Repo {
            auto_review: false,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("automatic review is switched off"),
                "the stop must name the switch a person would go and turn on: {why}"
            ),
            other => panic!("a read ran on a repo that never asked for one: {other:?}"),
        }
        // And it is a stop like any other — written down, so the next pass does not try again.
        assert!(
            stopped("demo", 41).is_some(),
            "the refusal was not written down"
        );

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// The outer switch wins. A repo skein may not read at all must not report the inner flag as
    /// the reason — somebody would go and turn on `auto_review` and watch nothing happen.
    ///
    /// **What would make this fail:** reordering `auto_review_stands` to test `auto_review` before
    /// `read_prs`. Both are off here, so the sentence would name the inner one.
    #[test]
    fn a_read_step_on_a_repo_skein_may_not_read_blames_the_outer_switch() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            read_prs: false,
            auto_review: false,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("reading is switched off"),
                "with both switches shut, the reason must be the outer one: {why}"
            ),
            other => panic!("{other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// **A pull request somebody assigned the reviewer flow to acts in a repo whose engine is
    /// off** — §10's layer 7 over layer 3, and the last unbuilt link in that chain.
    ///
    /// > on, in a repo that is off — assign the reviewer flow to that one pull request
    ///
    /// Observed at the seam rather than at the model call: with `auto_review` off and nothing
    /// assigned, `read_now` refuses at the flag and says which switch is shut. With the same repo
    /// and an assignment on the row it gets past that flag and lands on the NEXT link — the trigger
    /// set — which is a wait rather than a refusal. The two sentences are how you can tell which
    /// layer stopped it, which is the whole reason `auto_review_stands_for` returns prose.
    ///
    /// **What would make this fail:** dropping the `&& !assigned` from layer 3, which makes the
    /// first row stop saying "switched off"; or letting the assignment past layer 1, which the
    /// third row catches — the money door is the one thing a per-PR switch may never open.
    #[test]
    fn a_pull_request_assigned_by_hand_acts_where_the_repo_is_off_but_never_where_reading_is() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let engine_off = crate::repos::Repo {
            auto_review: false,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        // Facts with no trigger fired, so the layer AFTER the one under test is reachable and
        // distinguishable: this must never get as far as a model call.
        let quiet = crate::workflow::Facts::default();

        let refused = perform(
            &readable(&engine_off, &pr, "abc1234", &quiet),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &refused {
            Outcome::Stopped(why) => assert!(
                why.contains("automatic review is switched off"),
                "the wrong layer refused it: {why}"
            ),
            other => panic!("a repo with the engine off acted: {other:?}"),
        }

        // The same repo, with somebody's choice on the row. The stop the refusal above wrote is
        // cleared first: `perform` answers a remembered stop before it evaluates anything, so
        // without this the second call returns the FIRST call's sentence and the assertion below
        // would pass or fail on a decision that was never made again.
        clear("demo", 41).unwrap();
        assign("demo", 41, "the-flow").unwrap();
        assert!(
            chosen_by_hand("demo", 41),
            "the assignment was not written where it is read"
        );
        let now = perform(
            &readable(&engine_off, &pr, "abc1234", &quiet),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &now {
            Outcome::Waited(why) => assert!(
                why.contains("trigger"),
                "the assignment got past layer 3 but stopped somewhere unexpected: {why}"
            ),
            other => panic!(
                "an assigned pull request did not get past `auto_review` being off: {other:?}"
            ),
        }

        // **And an exclusion is not an assignment.** An empty name is a person saying "no rule may
        // touch this one"; reading it as a switch-on would act on exactly the pull request that was
        // taken out of reach.
        assign("demo", 41, "").unwrap();
        assert!(
            !chosen_by_hand("demo", 41),
            "an excluded pull request read as one somebody switched on"
        );

        // **Layer 1 is never opened by layer 7.** A repo skein may not read at all refuses with a
        // sentence naming reading, assignment or no assignment.
        clear("demo", 41).unwrap();
        assign("demo", 41, "the-flow").unwrap();
        let no_reading = crate::repos::Repo {
            read_prs: false,
            ..engine_off.clone()
        };
        match perform(
            &readable(&no_reading, &pr, "abc1234", &quiet),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        ) {
            Outcome::Stopped(why) => assert!(
                why.contains("reading is switched off"),
                "an assignment opened the money door: {why}"
            ),
            other => panic!("an assignment read a repo skein may not read: {other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// A dry run says what it would have done and buys nothing.
    ///
    /// **What would make this fail:** deleting the `auto_review_dry_run` early return. `summarise`
    /// would then run for real — and with this pull request out of reading scope it comes back
    /// `Unread`, so the outcome is still a `Waited` but its sentence is the scope refusal rather
    /// than the dry-run one, and the `contains` assertion fails.
    #[test]
    fn a_read_step_in_dry_run_says_what_it_would_do_and_buys_nothing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            auto_review_dry_run: true,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => {
                assert!(
                    why.contains("dry run") && why.contains("abc1234"),
                    "a dry run must say which commit it would have read: {why}"
                );
            }
            other => panic!("a dry run did something: {other:?}"),
        }
        // Nothing was written anywhere: no stop, and no reading on disk.
        assert_eq!(stopped("demo", 41), None, "a dry run stopped the workflow");
        assert!(
            !reading_path("demo", 41, "abc1234").exists(),
            "a dry run filed a reading"
        );

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// The anchor. A reading filed against a commit this pass did not evaluate is the anchoring
    /// failure the whole reviewer design exists to stop, so it is refused rather than filed.
    ///
    /// **What would make this fail:** deleting the `reading.pr.head_sha != pr.head_sha` guard.
    /// The act would then go on to the dry-run check and this asserts `Stopped`.
    #[test]
    fn a_read_step_refuses_a_commit_the_pass_did_not_evaluate() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // Dry run as well, so that removing the anchor does not merely swap one refusal for
        // another: without the guard this reaches the dry-run wait, which is not a stop.
        let repo = crate::repos::Repo {
            auto_review_dry_run: true,
            ..a_repo_that_may_be_read()
        };
        // The step was decided about `abc1234`; the pull request in hand has moved to `def5678`.
        let moved = pr_at("def5678");
        let out = perform(
            &readable(&repo, &moved, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("abc1234") && why.contains("def5678"),
                "the refusal must name both commits, or nobody can tell which moved: {why}"
            ),
            other => panic!("a reading was filed against a commit nothing evaluated: {other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// A caller with nothing to read with says so, and does not blame a flag.
    ///
    /// **What would make this fail:** treating `Reading: None` as "read it with a default `Repo`".
    /// `Repo::default()` has `auto_review` off, so the refusal would come back naming the flag —
    /// and somebody would go and switch on automatic review for a repo where it was never the
    /// problem. The assertion is that the sentence does NOT name it.
    #[test]
    fn a_read_step_from_a_caller_with_nothing_to_read_with_does_not_blame_a_flag() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // `subject()` is the helper every non-reviewer test uses, and it carries no reading.
        let out = perform(
            &subject("abc1234"),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => {
                assert!(
                    why.contains("passed no repo"),
                    "the refusal must name the caller: {why}"
                );
                assert!(
                    !why.contains("automatic review is switched off"),
                    "a wiring fault was reported as a flag somebody should go and change: {why}"
                );
            }
            other => panic!("{other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// **A reading that did not happen waits; it does not stop the workflow.**
    ///
    /// Every other act in `perform` turns a failure into a stop, and this one deliberately does
    /// not: an unread pull request is `Depth::Unread`, `ReadingWhole` does not hold and an
    /// approval is unreachable, so the fail-closed behaviour is already in the type. Stopping
    /// as well would make a person clear a workflow because a day's budget rolled over.
    ///
    /// Driven through the real refusal rather than a stub: this pull request is not one skein
    /// reads unasked — nobody requested the viewer and the viewer did not open it — so
    /// `review::unasked_scope` turns it away before any model call.
    ///
    /// **What would make this fail:** mapping `Depth::Unread` to `ReadStep::Failed`. The outcome
    /// becomes `Stopped` and a stop appears on disk, and both assertions below catch it.
    #[test]
    fn a_reading_that_did_not_happen_waits_rather_than_stopping_the_workflow() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("#41") && !why.is_empty(),
                "the wait must carry the reason the reading did not happen: {why}"
            ),
            other => panic!("a reading skein declined to make stopped the workflow: {other:?}"),
        }
        assert_eq!(
            stopped("demo", 41),
            None,
            "a pull request skein chose not to read now needs a person to clear it"
        );

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// A reading already on disk at this head is the answer, and is not bought again.
    ///
    /// **What would make this fail:** passing `force: true` to `summarise`, or treating
    /// `Summary::computed` as "a reading exists" rather than "a model call was spent". Either way
    /// the outcome becomes `Did` and the journal gains a line every two minutes for a reading
    /// nobody made.
    #[test]
    fn a_reading_already_on_disk_at_this_head_is_not_bought_again() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // Filed at exactly this head, through the same expression `review::cached` reads.
        file_the_reading(
            "demo",
            &a_reading("abc1234", crate::review::Depth::Line, true),
        );

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("already read"),
                "a cached reading must be reported as one: {why}"
            ),
            other => panic!("skein re-bought a reading it already had: {other:?}"),
        }
        // And the journal is untouched — the whole reason a cache hit is a wait.
        assert!(
            journal("demo", 41).is_empty(),
            "a reading that cost nothing wrote a line into the journal"
        );

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// **A trigger set that decides something.** The owner's third ask — *"the trigger is just
    /// review requested state but not new commits"* — is the default set, so this is the default
    /// behaviour and not an edge.
    ///
    /// **What would make this fail:** deleting the `no_trigger_of_this_repos_fired` call from
    /// `read_now`. The reading would then go ahead on a pull request no trigger in the repo's set
    /// woke, and the outcome would carry the scope refusal rather than the trigger sentence.
    #[test]
    fn a_pull_request_no_trigger_in_this_repos_set_woke_is_not_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        // The head moved on one you approved — a real event, and one this repo did not ask for.
        // The default set is `requested` alone, which has NOT fired: nobody named you.
        let woken_by_something_else = crate::workflow::Facts {
            review_requested: false,
            mine: true,
            my_review: "approved".into(),
            my_review_current: false,
            ..Default::default()
        };
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_by_something_else),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => {
                assert!(
                    why.contains("requested"),
                    "the wait must name the triggers this repo does ask for: {why}"
                );
                assert!(
                    why.contains("approved-commits"),
                    "the wait must name what DID fire, or nobody can tell which line to change: \
                     {why}"
                );
            }
            other => panic!("a trigger this repo never asked for started a reading: {other:?}"),
        }
        assert_eq!(
            stopped("demo", 41),
            None,
            "a quiet trigger stopped the flow"
        );

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// A repo switched on with a trigger set nothing in this build can answer is **on and inert**,
    /// which §10 says must say so rather than present as running.
    ///
    /// **What would make this fail:** `read_wake` guessing rather than answering `None` for a word
    /// it does not know. The words below would then read as real triggers, the sentence would be
    /// the ordinary "no trigger fired" one, and the assertion on "cannot act on" fails.
    #[test]
    fn a_trigger_set_this_build_cannot_act_on_says_so_rather_than_sitting_inert() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            // Two words from nowhere — a trigger set written by a newer skein. This used to say
            // `reply`, which was in §10's table and unanswerable; it is answerable now, so the
            // only inert set left is one this build cannot read at all, which is the case that
            // was always the more likely one to meet in the wild.
            auto_review_on: vec!["reply-with-a-quote".into(), "on-a-tuesday".into()],
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("cannot act on") && why.contains("reply-with-a-quote"),
                "an inert trigger set must name itself: {why}"
            ),
            other => panic!("{other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// `auto_review_authors` defaults to `mine`, and somebody else's pull request is left alone.
    ///
    /// **What would make this fail:** deleting the `not_an_author_this_repo_reviews` call. The
    /// reading would go ahead on a pull request the reader did not open, which is the first place
    /// a wrong verdict is seen by somebody who did not opt into any of this.
    #[test]
    fn a_repo_that_reviews_only_your_own_leaves_somebody_elses_pull_request_alone() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        let theirs = crate::workflow::Facts {
            review_requested: true,
            mine: false,
            ..Default::default()
        };
        let out = perform(
            &readable(&repo, &pr, "abc1234", &theirs),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("only pull requests you opened"),
                "the wait must say whose pull requests this repo reviews: {why}"
            ),
            other => panic!("a contributor's pull request was read unattended: {other:?}"),
        }

        // And `all` opens it, or the flag has one position.
        let open_to_all = crate::repos::Repo {
            auto_review_authors: "all".into(),
            ..a_repo_that_may_be_read()
        };
        let out = perform(
            &readable(&open_to_all, &pr, "abc1234", &theirs),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                !why.contains("only pull requests you opened"),
                "`all` did not open the door: {why}"
            ),
            other => panic!("{other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    /// A word this build does not know reads as `mine`, the narrow one — a permission may never be
    /// widened by a value skein cannot understand.
    ///
    /// **What would make this fail:** writing the check as `authors != "mine"` rather than
    /// `== "all"`. Then anything misspelled would open the repo to every author.
    #[test]
    fn an_author_filter_this_build_does_not_recognise_stays_narrow() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            auto_review_authors: "everyone".into(),
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let theirs = crate::workflow::Facts {
            review_requested: true,
            mine: false,
            ..Default::default()
        };
        let out = perform(
            &readable(&repo, &pr, "abc1234", &theirs),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("only pull requests you opened"),
                "an unrecognised author filter widened what skein does unattended: {why}"
            ),
            other => panic!("{other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    // ─────────────── §15 step 4: the verdicts, and the ceiling on them ───────────────

    /// **The ceiling is the last gate before something appears under somebody's name**, and it
    /// holds by refusing to make the call at all — not by making it and hoping.
    ///
    /// `comment` is what a repo gets when it is switched on (§10), so this is the default state:
    /// findings unattended, verdicts waiting. The assertion that matters is the second one —
    /// nothing reached GitHub — because a ceiling that returned `Waited` after posting would read
    /// exactly the same on the row.
    ///
    /// **What would make this fail:** deleting the `wants > repo.auto_review_ceiling` check, or
    /// writing it as `>=`, which would let a repo post exactly the verdict it is capped at and
    /// nothing beyond — the off-by-one that looks like it works.
    #[test]
    fn a_verdict_past_the_ceiling_waits_and_never_reaches_github() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let repo = a_repo_that_may_be_read();
        assert_eq!(
            repo.auto_review_ceiling,
            crate::repos::Ceiling::Comment,
            "the fixture must be at the default ceiling, or this tests a state nobody starts in"
        );
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("comment") && why.contains("waits for you"),
                "the wait must name the ceiling that stopped it: {why}"
            ),
            other => panic!("an approval went out past the repo's ceiling: {other:?}"),
        }
        assert!(
            heard.lock().unwrap().is_empty(),
            "the ceiling let the call happen: {:?}",
            heard.lock().unwrap()
        );
        assert_eq!(stopped("demo", 41), None, "a ceiling stopped the workflow");

        and_no_longer(&[
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ]);
    }

    /// **A refusal is reachable one notch below an approval**, which is the shape the interviewed
    /// box asked for: everything else unattended, and the verdict that discharges a review held
    /// back. A ceiling can express it and three checkboxes cannot (§10).
    ///
    /// **What would make this fail:** comparing the ceiling by anything but consequence — reversing
    /// `Ceiling`'s variant order, or deriving `Ord` off a different field. `changes` would then
    /// either block a refusal it permits or admit the approval it exists to hold.
    #[test]
    fn a_ceiling_at_changes_posts_a_refusal_and_still_holds_the_approval() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);
        // **No `GH_TOKEN` here, and its absence is the assertion.** This test used to set one,
        // because `prq::submit_review_with_comments` looked the credential up itself — so a test
        // about a CEILING could not run without arranging a credential two modules away. It takes
        // `perform`'s token now, which this test passes as "t" below. If the lookup ever comes
        // back, this test fails on a missing credential and says where.

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Changes,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let facts = woken_and_mine();
        let subject = readable(&repo, &pr, "abc1234", &facts);

        let held = perform(
            &subject,
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        assert!(
            matches!(held, Outcome::Waited(_)),
            "an approval went out at a `changes` ceiling: {held:?}"
        );
        assert!(
            heard.lock().unwrap().is_empty(),
            "the approval reached GitHub"
        );

        let posted = perform(
            &subject,
            &flow(),
            &chosen(Act::PostChanges),
            &fixture_token(),
        );
        assert!(
            matches!(posted, Outcome::Did(_)),
            "a refusal was held back at its own ceiling: {posted:?}"
        );
        let said = heard.lock().unwrap().join("\n");
        assert!(
            said.contains("REQUEST_CHANGES"),
            "the post was not a refusal: {said}"
        );

        and_no_longer(&[
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ]);
    }

    /// **The verdict says what left it**, which is the attribution §13 recorded as missing:
    /// *"an engine verdict is indistinguishable from the owner's, on GitHub and in the queue."*
    ///
    /// On the pull request itself, not only in skein's journal — a verdict that discharges
    /// somebody's review is read by people who cannot see skein's records at all.
    ///
    /// **What would make this fail:** posting an empty body, or one that names neither the step nor
    /// the commit. Either leaves a reader unable to tell an engine's approval from a person's.
    #[test]
    fn a_posted_verdict_names_the_workflow_the_step_and_the_commit() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);
        std::env::set_var("GH_TOKEN", "gho_test");

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Did(_)), "{out:?}");

        let said = heard.lock().unwrap().join("\n");
        assert!(said.contains("APPROVE"), "not an approval: {said}");
        assert!(
            said.contains("skein posted this automatically"),
            "the verdict does not say a machine left it: {said}"
        );
        // `chosen()` is step index 3, so the fourth step.
        assert!(
            said.contains("ship-mine step 4"),
            "the verdict names no workflow and step: {said}"
        );
        assert!(
            said.contains("abc1234"),
            "the verdict names no commit, so nobody can tell what was reviewed: {said}"
        );
        // And a person is told how to stop it, on the artefact itself.
        assert!(
            said.contains("auto_review"),
            "no way out is offered: {said}"
        );

        and_no_longer(&[
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ]);
    }

    /// A verdict is refused against a commit this pass did not evaluate — §3's "a review describing
    /// tree A anchored to tree B", which is the failure a memoryless engine makes and the sha guard
    /// exists to stop.
    ///
    /// **What would make this fail:** deleting the head comparison from `post_verdict`. The post
    /// would then go out against `Subject::head_sha` while the reading it rests on describes
    /// another commit.
    #[test]
    fn a_verdict_is_refused_against_a_commit_the_pass_did_not_evaluate() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let moved = pr_at("def5678");
        let out = perform(
            &readable(&repo, &moved, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("abc1234") && why.contains("def5678"),
                "the refusal must name both commits: {why}"
            ),
            other => panic!("a verdict was posted against an unevaluated commit: {other:?}"),
        }
        assert!(heard.lock().unwrap().is_empty(), "it reached GitHub anyway");

        and_no_longer(&[
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ]);
    }

    /// `post-findings` refuses, and the refusal says it is a vestige rather than a thing not built.
    ///
    /// The distinction is the whole value: "not built yet" invites somebody to wire it, and wiring
    /// it would post the summary beside a review the reading session already left.
    ///
    /// **What would make this fail:** folding this arm back in with `audit`'s, whose refusal says
    /// "nothing is wired to it yet" — true of `audit` and misleading here.
    #[test]
    fn post_findings_refuses_as_a_vestige_rather_than_as_something_unbuilt() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostFindings),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => {
                assert!(
                    why.contains("posts its own comment review"),
                    "the refusal does not say why this is not wanted: {why}"
                );
                assert!(
                    why.contains("`read`"),
                    "the refusal names no step to use instead: {why}"
                );
            }
            other => panic!("{other:?}"),
        }

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }

    // ─────────────── §13's obligation: the loop must not be reachable silently ───────────────

    /// A workflows file with a merge step, written where `workflow::load` reads it.
    fn a_merge_train(home: &std::path::Path) {
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"ship-mine","steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap();
    }

    /// **Every condition of the loop is load-bearing, and the ceiling is why this is not noisy.**
    ///
    /// §13 sketched it as "both on". Built, it is four conditions, and the table walks each one off
    /// on its own so no single arm can be deleted without a row going red.
    ///
    /// The fourth is the one worth having: a repo switched on at the DEFAULT ceiling never posts an
    /// approval, so there is nothing for a train to read and nothing to warn about. Without it,
    /// every repo with auto-review on would carry a warning about a loop it cannot build — and a
    /// warning that fires when nothing is wrong is one people learn to scroll past, which is the
    /// same failure as not warning at all.
    ///
    /// **What would make this fail:** deleting any of the four checks from
    /// `the_loop_this_repo_has_built`; each has a row here that is the ONLY row it decides.
    #[test]
    fn the_self_approving_loop_is_reported_when_every_part_of_it_is_configured() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        a_merge_train(home);

        let looped = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let said = the_loop_this_repo_has_built(&looped).expect("the loop must be reported");
        assert!(
            said.contains("will merge it") && said.contains("ship-mine"),
            "the warning must say what happens and name the train that does it: {said}"
        );
        assert!(
            said.contains("auto_review_ceiling"),
            "the warning names no way out: {said}"
        );

        // The ceiling a repo is actually switched on at. Nothing to warn about: no approval is
        // posted, so no approval is read.
        for ceiling in [
            crate::repos::Ceiling::None,
            crate::repos::Ceiling::Comment,
            crate::repos::Ceiling::Changes,
        ] {
            let held = crate::repos::Repo {
                auto_review_ceiling: ceiling,
                ..a_repo_that_may_be_read()
            };
            assert_eq!(
                the_loop_this_repo_has_built(&held),
                None,
                "a ceiling of {} cannot post an approval, so there is no loop to warn about",
                ceiling.spelled()
            );
        }

        // The engine off, which is every repo by default.
        let engine_off = crate::repos::Repo {
            auto_review: false,
            ..looped.clone()
        };
        assert_eq!(the_loop_this_repo_has_built(&engine_off), None);

        // No workflow that merges: an approval that nothing acts on is just an approval.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"label-only","steps":[{"when":[],"do":"add-label:ci-queue"}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            the_loop_this_repo_has_built(&looped),
            None,
            "a workflow that cannot merge was reported as a merge train"
        );
        a_merge_train(home);

        // And the fleet's one kill switch outranks all of it.
        std::env::set_var("SKEIN_PR_WORKFLOWS", "off");
        assert_eq!(
            the_loop_this_repo_has_built(&looped),
            None,
            "workflows are switched off for the whole fleet, so nothing merges anything"
        );

        and_no_longer(&["SKEIN_HOME", "SKEIN_PR_WORKFLOWS"]);
    }
}
