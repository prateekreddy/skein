//! What skein knows about one pull request, in the shape a workflow can ask about.
//!
//! [`facts_of_in`] is the whole of the translation: GitHub's answer for one row of the queue, plus
//! whatever reading skein holds for that commit, becomes a `workflow::Facts`. Nothing here acts and
//! nothing here decides — it answers, and `crate::workflow::holds` does the rest.
//!
//! **The rule every three-valued answer here obeys is that truncation is never absence.** A label
//! list GitHub cut off cannot answer "no such label"; a reading skein cannot find is neither
//! current nor stale; a sweep that never ran leaves coverage unknown, so the approval it would
//! authorise is unreachable rather than permitted. Each reader below says which way its own unknown
//! falls, and why that is the safe direction for the act it gates.

/// **Test-only.** [`facts_of_in`] with no repository, which means no reading can be looked up and
/// `reading_whole` is `None` however well the pull request was actually read.
///
/// **Without a repository, skein cannot see its own reading**, so this answers the reviewer's
/// reading facts as unknown. It is not a shorthand for [`facts_of_in`] with a blank: a reading is
/// filed under `(repo, number, head_sha)` and a caller that cannot name the repo genuinely does
/// not know, which is what `None` is spelled as here. A caller that CAN name it should, and every
/// caller that acts does.
///
/// `#[cfg(test)]` rather than merely discouraged, and that is the whole point of this being its own
/// item: it was public and had exactly one production caller — the cockpit's train panel — which
/// was therefore drawing a preview from strictly weaker facts than the tick acts on. The comment at
/// that call site already forbids that class of thing in as many words, about a different field, so
/// the answer is to make the weaker call unreachable from production rather than to add a rule
/// nobody can see. A `bin` is a separate crate and cannot link a `cfg(test)` item, so this is
/// enforced by the compiler and not by review.
#[cfg(test)]
pub(super) fn facts_of(pr: &crate::prq::Pr, viewer: &str, trunk: &str) -> crate::workflow::Facts {
    facts_of_in("", pr, viewer, trunk)
}

/// What a workflow sees, built from what GitHub said — for a pull request in a **named
/// repository**, so the reviewer's reading facts can be answered instead of confessed
/// (`docs/pr-review.md` §7c, §15 step 3).
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
    // train sat on approved work. On a repo where you are the author and somebody else reviews,
    // that is the ordinary case rather than an edge.
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
/// * **no repository named.** `""` is a caller admitting it cannot say which repository, and a
///   lookup in `review/` itself would find whatever a repo called nothing had.
///
/// Absence is unknown in every one of those, never `Some(false)`, and
/// [`crate::workflow::Cond::ReadingWhole`] is what an approval hangs on.
///
/// **`Some(false)` means one thing only**: a reading exists, and no sweep spoke for it. That is
/// still not an approval, by [`crate::review::Summary::swept`]'s own rule — a sweep that refused,
/// timed out or answered nothing lands on the same `false` — so the caller widens it back to
/// `None`. The distinction is kept here anyway because this function answers "what is on disk"
/// and the widening is a policy, and the two drift when one function does both.
///
/// It does not scan for readings of OTHER heads. `Cond::ReadingStale` therefore still never holds,
/// which is honest: this knows whether the current commit was read, not what came before it.
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
/// What that cost, measured rather than argued. On one live queue —
/// `GET /api/repos/gadget-demo/review`, twenty-one open pull requests, 2026-08-26 —
/// `review_decision` was `""` on twenty and `CHANGES_REQUESTED` on one. `APPROVED` on **none**,
/// including the two a person had personally approved (`my_review` was `approved` on two,
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
#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::prwork::facts::facts_of;
    #[allow(unused_imports)]
    use crate::prwork::testkit::*;
    #[allow(unused_imports)]
    use crate::prwork::*;
    #[allow(unused_imports)]
    use crate::workflow::{Act, Chosen, Merge, MergeAs, Update, Workflow};
    #[allow(unused_imports)]
    use std::io::{Read, Write};
    #[allow(unused_imports)]
    use std::sync::{Arc, Mutex};

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
    /// The state a whole live fleet was in and no test described: `reviewDecision` is `""`,
    /// because there is no branch protection to satisfy, and a person has approved the pull
    /// request anyway. `facts_of` read `== "APPROVED"` and called that not-approved, so the
    /// documented train's `matches` claimed nothing on twenty-one open pull requests — including
    /// the two a person had approved by hand. No error, no flag, no stop: `matches` gates before
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
             that silence as \"not approved\" — measured on a live queue as APPROVED on 0 of 21 \
             open pull requests, two of which the viewer had approved themselves"
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
    /// `my_review` is `"none"` — so on a repository where one person opens the pull requests and
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
}
