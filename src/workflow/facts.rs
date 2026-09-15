//! What skein knows about a pull request right now, and when a fact satisfies a condition.
//!
//! [`Facts`] is the whole of what a workflow may ask about — plain data with no GitHub in it, so
//! the deciding can be tested against every state in the owner's example without a network.
//! [`holds`] is the one place a [`Cond`] meets a field.
//!
//! **The rule that binds every three-valued field here: truncation is never absence.** A label list
//! skein saw only part of cannot answer "no such label"; a reading it cannot see is neither current
//! nor stale; a sweep that never ran does not satisfy [`Cond::ReadingWhole`], so the approval it
//! would authorise is unreachable rather than permitted. Each field's own note says which way its
//! unknown falls and why.

use super::Cond;
// In scope for the doc links, not for the code. These items are in sibling files now, and
// rustdoc resolves an intra-doc link against what the file it appears in imports — so
// without this line every `[`Act::Read`]` below silently stops being a link. Nothing gates
// `cargo doc`, which is exactly why the rot would be invisible.
#[allow(unused_imports)]
use super::{instead_of_merging_off_the_trunk, next, Act};

/// Everything a workflow may ask about a pull request, as skein sees it right now.
///
/// Plain data with no GitHub in it, so the deciding can be tested against every state in the
/// owner's example without a network — and so that this module keeps its one dependency. Whoever
/// has a queue builds these; nothing here knows where they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// **Somebody has approved it, and nobody's refusal is standing.** What a person means by
    /// "approved", and nothing more than that.
    ///
    /// This used to say *"Approved against the commit that is there NOW. An approval of an earlier
    /// head is not one."* Both halves were wrong, and the sentence is worth keeping in view
    /// because it is what made the error look settled (SKEIN-339).
    ///
    /// It was built as `pr.review_decision == "APPROVED"` alone, and GitHub's `reviewDecision`
    /// does not answer "has anybody approved this". It answers "is this branch's review
    /// requirement satisfied", so it is `APPROVED` only where branch protection REQUIRES a review
    /// and the requirement is met, and `null` on every repository where review is social — however
    /// many approvals a pull request carries. On the owner's own queue that read `APPROVED` on
    /// **zero of twenty-one** open pull requests, two of which the owner had personally approved
    /// (`GET /api/repos/gadget-demo/review`, 2026-08-26). `CHANGES_REQUESTED` still surfaced,
    /// because a refusal is not gated on a requirement — which is exactly why the field looked
    /// like it worked, and why the documented merge train sat on `matches: ["…", "approved", …]`
    /// claiming nothing at all, with no error, no flag and no stop to notice.
    ///
    /// The second half was wrong for a different reason: whether pushing a commit ends an approval
    /// is the repository's `dismiss_stale_reviews` setting, not a property of the word. With it
    /// off, `reviewDecision` stays `APPROVED` across pushes and GitHub means it. So "standing" here
    /// means *GitHub still counts it*, which is the only sense skein can honestly claim — see
    /// `docs/pr-workflow.md`, "The finding: rebasing and approvals", for the setting and its
    /// citations.
    ///
    /// **A standing refusal outranks a standing approval**, which is why this and
    /// [`Facts::changes_requested`] still cannot both hold. That is now a rule rather than an
    /// accident of both being read off one field: with approvals countable, a workflow written
    /// before SKEIN-339 as `matches: ["approved"] → merge` would otherwise start merging over a
    /// reviewer who had said no, and a behaviour change that merges is the one kind this module
    /// will not make quietly.
    ///
    /// **Anybody's approval, not just yours** (SKEIN-356). This used to say that `prq::Pr` carried
    /// the repository's verdict and *your* last review and nobody else's — so on a repository with
    /// no review requirement a third party's approval was invisible and this read false. That was
    /// the safe direction and still a gap, and it closed the way that paragraph said it would:
    /// `prq::Pr::standing_approvals` counts the approvals GitHub holds against the current head
    /// from any reviewer, off the same `latestOpinionatedReviews` your own verdict comes from, and
    /// `prwork::facts_of` reads it.
    ///
    /// What remains outside this field is what only `reviewDecision` can see — CODEOWNERS, a
    /// required-approvals count, any rule branch protection applies — and that is deliberately
    /// [`Facts::review_requirement_met`]'s job rather than this one.
    pub approved: bool,
    pub changes_requested: bool,
    /// **Is the repository's own review requirement in the way?** `Some(true)` it is met,
    /// `Some(false)` it is not, `None` there is no requirement to meet.
    ///
    /// [`Facts::approved`] is what people decided; this is what the repository demands, and they
    /// are two questions with two answers. On a protected branch GitHub's `reviewDecision` folds
    /// in CODEOWNERS, the required-approvals count and rules skein has no API to read — so it
    /// remains the authority on whether a merge will be allowed at all, and losing that was never
    /// the point of SKEIN-339.
    ///
    /// **`None` means "nothing to satisfy", and [`Cond::ReviewSatisfied`] therefore HOLDS on it.**
    /// That breaks this module's usual discipline — [`Facts::mergeable`], [`Facts::behind`] and
    /// [`Facts::base_is_trunk`] all have a third value that satisfies neither of its conditions —
    /// and it breaks it on purpose. Those three are three-valued because skein does not yet KNOW;
    /// waiting is the honest answer and the answer arrives on a later poll. This one is
    /// three-valued because GitHub has answered and the answer is *there is no requirement here*.
    /// No later poll changes it. Reading that as "not satisfied" is SKEIN-339 itself, re-committed
    /// under a new word: a condition that is false forever on every repository where review is
    /// social, in a `matches` that then claims nothing, silently.
    ///
    /// The cost of that choice, stated rather than left implied: a queue remembered on disk by a
    /// skein from before `review_decision` existed also deserialises to `""` and lands here as
    /// `None`, so such a queue reads as "no review requirement". The pull request still needs
    /// [`Facts::approved`] before a train touches it, and the queue is re-fetched within the
    /// minute.
    pub review_requirement_met: Option<bool>,
    /// The labels skein saw — which may not be all of them. Read with [`Facts::labels_whole`].
    pub labels: Vec<String>,
    /// **Did skein see every label there is?** (SKEIN-373)
    ///
    /// A pair rather than one field, because `labels` alone cannot answer a question about a name
    /// that is not in it. GitHub's `labels` connection is paged — `prq::LABELS_FETCHED` — and the
    /// query used not to ask how many there were, so a pull request with more labels than the page
    /// arrived short and read as complete. [`Cond::NoLabel`] then answered "that label is not on
    /// this pull request" about a label it had never been sent, and a `hold` that happened to sort
    /// past the cap stopped holding anything.
    ///
    /// **False satisfies neither [`Cond::Label`] nor [`Cond::NoLabel`] for a name that was not
    /// seen** — the same discipline as [`Facts::mergeable`], [`Facts::behind`] and
    /// [`Facts::base_is_trunk`], and for the same reason: skein does not know, so it waits rather
    /// than answering. A name that WAS seen is still on the pull request whatever the cap did, so
    /// `label:` on it holds as it always did; truncation can only ever take a condition away.
    ///
    /// **`Default` is false**, which is this module's fail-closed value and not an oversight:
    /// `Facts::default()` is a fact-set nobody looked anything up for, and a default that let
    /// `no-label:` hold would answer, from nothing, the question this field exists to stop being
    /// answered from nothing. `prwork::facts_of` states it from the queue; a fixture that means
    /// "these are all the labels" says so.
    pub labels_whole: bool,
    /// `passing` | `failing` | `pending` | `none`.
    pub checks: String,
    /// `None` when GitHub has not worked it out yet, which it reports as `UNKNOWN` for a while
    /// after every push. **Not the same as "cannot be merged"** — see [`holds`].
    pub mergeable: Option<bool>,
    pub draft: bool,
    /// You opened it.
    pub mine: bool,
    /// Does the base have commits this branch lacks? `None` when GitHub has not said —
    /// `mergeStateStatus: UNKNOWN`, or a queue from before the field existed. Three-valued for
    /// the same reason as [`Facts::mergeable`] — see [`holds`].
    pub behind: Option<bool>,
    /// Is the base ref the repository's default branch? `None` when skein does not know what the
    /// trunk IS — the lookup failed, or has not happened yet. Three-valued for the same reason as
    /// [`Facts::mergeable`] and [`Facts::behind`], and here it earns its third value twice over:
    /// "based on a branch that is not the trunk" is a stacked child, which must never be merged,
    /// while "skein cannot see what the trunk is" is a transient blindness that must not become a
    /// permanent stop. [`next`] tells those two apart — see [`instead_of_merging_off_the_trunk`].
    pub base_is_trunk: Option<bool>,

    // ---- the reviewer's facts (`docs/pr-review.md` §7) --------------------------------------
    //
    // Four rules, and every one of them is a rule about a FIELD rather than about a step. A step
    // vocabulary cannot fix a field that answers the wrong question — which is the whole of §3,
    // bought with SKEIN-339 — so they live here and in `prwork::facts_of`, and nowhere else.
    /// **Is GitHub asking YOU for a review, by name?** `prq::Pr::my_review_requested`, carried
    /// straight across: it is GitHub's own answer and skein infers nothing from it.
    ///
    /// A floor rather than a census, for the reason that field gives — a request made of a team is
    /// not a request naming you, and without `read:org` it arrives with no name at all. False can
    /// therefore mean "GitHub asked, and skein could not see that it did", which errs towards not
    /// claiming you.
    pub review_requested: bool,
    /// **Your own last verdict**: `approved` | `changes-requested` | `commented` | `none`, spelled
    /// exactly as `prq::Pr::my_review` spells it.
    ///
    /// **This is the reviewer's question, and [`Facts::approved`] is not** (§7a). That field
    /// answers *"has anybody approved this, and is nobody's refusal standing"* — a question about
    /// the pull request, built from `reviewDecision` and a count of standing approvals from any
    /// reviewer. Reading it for *"what did I say"* is SKEIN-339 re-committed under a new word: on
    /// every repository where review is social it is the correct answer to a different question,
    /// and re-deriving it every poll gives that answer more often and with more confidence.
    ///
    /// **Deciding is approving or refusing.** A comment is not a decision, which is `prq`'s own
    /// lane rule; keeping the same rule here is what stops the engine and the queue disagreeing
    /// about whether a pull request has been dealt with.
    ///
    /// **§7d lives in the gap between this and [`Facts::my_review_current`]**, and it is the bug
    /// this design would otherwise have shipped. `prq` files a pull request in `Lane::Waiting` the
    /// moment this is a decision and nothing has re-requested you (`decided && !my_review_requested`),
    /// and `review::worth_a_visit` keeps a `Waiting` row in scope only where you authored it — so
    /// on somebody else's pull request **the first verdict the engine posts takes it out of the
    /// engine's own reading scope, permanently**, and §9's "the head moves on one you approved →
    /// re-check" can never fire. The two facts are carried apart so that state is sayable: *you
    /// decided* and *your verdict stands against this head* are different, and a decision whose
    /// head has moved is exactly the pull request the lane has released and the engine must not.
    /// Fixing the scope is a later increment; being able to state the difference is this one.
    pub my_review: String,
    /// **Was that verdict left against the head that is there now?** `prq::Pr::review_is_current`
    /// — evidence the head moved, never a verdict on your review.
    ///
    /// [`Cond::VerdictStanding`] is this and [`Facts::my_review`] together. It is the freshness
    /// half of the compare-and-set the interviewed box asked for; the coordination half is not
    /// needed, because one engine on one tick cannot race itself (`docs/pr-review.md` §12).
    pub my_review_current: bool,
    /// **Did skein see every review there is?** `prq::Pr::reviews_whole` — the reviews' answer to
    /// the question [`Facts::labels_whole`] asks about labels, and read the same way (§7b).
    ///
    /// The box this design was interviewed from read `--limit 60` against 64 open pull requests
    /// and took the missing rows for *"closed or merged"*. **Truncation is never absence.** So a
    /// claim about the reviews that did NOT arrive cannot be made from a short list:
    /// [`Cond::Unreviewed`] requires this, and where it is false the condition holds neither way
    /// and the pull request waits. A verdict that DID arrive is still your verdict whatever the
    /// cap did, so [`Cond::VerdictStanding`] does not require it — exactly the asymmetry between
    /// [`Cond::Label`] and [`Cond::NoLabel`].
    ///
    /// **`Default` is false**, this module's fail-closed value, for [`Facts::labels_whole`]'s
    /// reason: `Facts::default()` is a fact-set nobody looked anything up for, and it must not be
    /// able to answer the question this field exists to stop being answered from nothing.
    pub reviews_whole: bool,
    /// The commit the pull request stands at now. Empty where skein does not know it.
    ///
    /// Half of the sha guard (§4): *the reading step records its findings with the sha it read,
    /// and the posting step's guard is `finding.sha == head`.* An engine that keeps no place needs
    /// both halves as facts, because the head can move between the reading poll and the posting
    /// poll **by design** — and a memoryless engine that could not compare them would post a
    /// review describing tree A anchored to tree B.
    pub head_sha: String,
    /// **The commit skein's reading of this pull request was made against**, or `None` where skein
    /// has no reading it can see.
    ///
    /// The other half of the sha guard. `review.rs` keys every reading on `(number, head_sha)` and
    /// says that key is not an optimisation, so the store was always there; both ends are wired to
    /// it now — `prwork::facts_of_in` is handed the repository as well as the row and fills this
    /// from `review::cached`, and [`Act::Read`] is what puts a reading there in the first place.
    /// `prwork::facts_of`, which has no repository to look one up in, still answers `None`.
    ///
    /// **`None` satisfies neither [`Cond::ReadingCurrent`] nor [`Cond::ReadingStale`]**, and so
    /// does an empty [`Facts::head_sha`]: with nothing to anchor against, "a reading exists at an
    /// older commit" is a claim skein cannot make. Same discipline as [`Facts::mergeable`]'s
    /// unknown, and the same direction — the engine waits rather than answering.
    pub reading_sha: Option<String>,
    /// **Did the sweep run and account for every changed file, at one commit?** `Some(true)` it
    /// did, `Some(false)` it ran and did not, `None` no sweep has answered for this pull request.
    ///
    /// §7c. An earlier draft said a pass is partial when the diff was cut to fit the prompt; that
    /// was written from half the code. Since `f6e922a` the diff is the reviewer's opening summary
    /// and not its only window: it stands in a checkout and is told to go and read. What does say a
    /// pass was partial is the **sweep** (SKEIN-393), the second turn that makes a review account
    /// for what it actually covered.
    ///
    /// **It costs one persisted field**, `review::Summary::swept`, and the design said it would
    /// cost none. That was wrong for a reason worth keeping: `sweep()` discarded its own result, so
    /// the answer was computed and dropped, and every other field of a reading is about what it
    /// *found* rather than what it *read*. Coverage was inferable only as "a summary exists, so
    /// presumably it looked", which is the inference §7c exists to refuse.
    ///
    /// **Three-valued, and the third value is unknown rather than no.** An absent `swept` — every
    /// reading cached before the field existed, and every path that runs no sweep — is `None`, not
    /// `Some(false)`: skein did not look, which is not the same as having looked and come back
    /// short. Neither satisfies [`Cond::ReadingWhole`], so the one action that requires it,
    /// [`Act::PostApproval`], is unreachable rather than permitted — the direction `review.rs`'s
    /// own rule already fixes in the type: *AI may only add scrutiny, never remove it*.
    pub reading_whole: Option<bool>,
    /// **Did that reading find something that must block?** `Some(true)` it did, `Some(false)` it
    /// did not, `None` there is no reading to read findings off.
    ///
    /// Three-valued for [`Facts::reading_whole`]'s reason and not for a condition's: the
    /// vocabulary has no negative of [`Cond::FindingsBlocking`], so `false` would be read by
    /// nothing today — and would be a statement that the reading found nothing blocking, made from
    /// no reading. A fact nobody looked up says so.
    pub findings_blocking: Option<bool>,
    /// **Is something this repository owes still outstanding at this commit?** —
    /// `docs/pr-review.md` §8.
    ///
    /// `Some(true)` a check fired and is unanswered, `Some(false)` nothing is outstanding, `None`
    /// skein cannot say. It is `None` whenever there is no reading at this head, because the
    /// triggers are read off the diff and the diff is only in hand while a reading is being made —
    /// [`crate::review::Summary::owed_triggered`] is where the answer is kept, recorded at the sha
    /// it was computed from.
    ///
    /// **`Default` is `None`**, this module's fail-closed value: a fact-set nobody looked anything
    /// up for must not answer "nothing is owed", which is the answer that lets a verdict out.
    pub checks_owed: Option<bool>,
    /// **Has somebody answered one of your findings since you left it?** — §10's `reply` trigger.
    ///
    /// `Some(true)` a reply is there, `Some(false)` nothing outstanding and skein saw the whole
    /// thread list, `None` it cannot tell. Filled from [`crate::prq::Pr::replied_to`], which is
    /// where the rule lives — a thread you opened whose last comment is somebody else's, written
    /// after your latest review.
    ///
    /// **`Default` is `None`**, this module's fail-closed value: a trigger that fired from a
    /// fact-set nobody looked anything up for would wake a reading and spend for it.
    pub replied_to_me: Option<bool>,
}

/// Does this condition hold?
///
/// The one subtlety is `mergeable`. GitHub computes it asynchronously and says `UNKNOWN` for a
/// while after every push, so a workflow that treated unknown as "not mergeable" would rebase a
/// pull request for no reason — and on a repository that dismisses stale approvals, that rebase
/// costs the approval that authorised it (`docs/pr-workflow.md`). **Unknown satisfies neither
/// `mergeable` nor `not-mergeable`**: skein waits until GitHub has an answer.
///
/// `behind` and `current` keep the same discipline, for the merge train's sake: **unknown
/// satisfies neither.** GitHub reports `mergeable: true` for a branch that is merely behind, so
/// the train's merge step requires `current` explicitly — and if unknown counted as current,
/// merging a branch whose behind-ness is unknown could merge code CI never tested against the
/// current trunk (`docs/pr-workflow.md`, "The merge train").
///
/// **The reviewer's words keep exactly that discipline**, which is `docs/pr-review.md` §7b in one
/// line: *truncation is never absence.* A review list skein saw only part of cannot answer
/// [`Cond::Unreviewed`]; a reading skein cannot see answers neither [`Cond::ReadingCurrent`] nor
/// [`Cond::ReadingStale`]; a sweep that never ran does not satisfy [`Cond::ReadingWhole`], so the
/// approval it would authorise is unreachable rather than permitted. And none of them reads
/// [`Facts::approved`] — that field answers a question about the pull request, and the reviewer's
/// question is about you (§7a).
pub fn holds(cond: &Cond, facts: &Facts) -> bool {
    match cond {
        Cond::Approved => facts.approved,
        Cond::NotApproved => !facts.approved,
        Cond::ChangesRequested => facts.changes_requested,
        // The one three-valued fact whose unknown-shaped third value satisfies its condition. Not
        // an oversight and not a shortcut: `None` here is GitHub saying the repository asks for no
        // review, which is a permanent answer, where the `None`s below are skein not knowing yet.
        // Read the other way this condition is false forever on every social-review repo, which is
        // SKEIN-339 with a new spelling — see [`Facts::review_requirement_met`].
        Cond::ReviewSatisfied => facts.review_requirement_met != Some(false),
        // A label skein was sent is on the pull request, and a page that was cut off cannot make
        // that untrue — so this reads the list as it always did.
        Cond::Label(want) => facts.labels.iter().any(|l| l == want),
        // Its opposite is not symmetrical, and that asymmetry is the whole of SKEIN-373. "This
        // label is absent" is a claim about the labels skein did NOT receive, so a short list
        // cannot make it: `labels_whole` is the third value, and unknown satisfies neither
        // condition, exactly as `mergeable`'s does. The cost is stated where it falls — the
        // queue's blind spots name the pull request and the size of the hole — and it is the
        // cheaper of the two errors: a `hold` label past the cap used to read as absent, which is
        // a merge over somebody's hold.
        Cond::NoLabel(want) => facts.labels_whole && !facts.labels.iter().any(|l| l == want),
        Cond::Checks(want) => &facts.checks == want,
        Cond::Mergeable => facts.mergeable == Some(true),
        Cond::NotMergeable => facts.mergeable == Some(false),
        Cond::Draft => facts.draft,
        Cond::Ready => !facts.draft,
        Cond::Mine => facts.mine,
        Cond::Behind => facts.behind == Some(true),
        Cond::Current => facts.behind == Some(false),
        Cond::BaseTrunk => facts.base_is_trunk == Some(true),
        // GitHub's own answer, carried rather than inferred — see [`Facts::review_requested`].
        Cond::ReviewRequested => facts.review_requested,
        // [`Cond::NoLabel`]'s shape, not [`Cond::Label`]'s: "I have never decided" is a claim
        // about the reviews that did not arrive, and a capped connection cannot make it (§7b).
        Cond::Unreviewed => facts.reviews_whole && !a_decision(&facts.my_review),
        // The sha guard (§4). Unknown either side satisfies neither — see [`reading_against`].
        Cond::ReadingCurrent => reading_against(facts) == Some(true),
        Cond::ReadingStale => reading_against(facts) == Some(false),
        // §7c. The sweep has to have said so; nothing else may stand in for it, and no sweep at
        // all is not a yes.
        Cond::ReadingWhole => facts.reading_whole == Some(true),
        Cond::FindingsBlocking => facts.findings_blocking == Some(true),
        // Your verdict, and whether it was left against this code. **Never [`Facts::approved`]**,
        // which answers a question about the pull request rather than about you (§7a).
        Cond::VerdictStanding => a_decision(&facts.my_review) && facts.my_review_current,
        // §8, and three-valued for `ReadingWhole`'s reason: what is owed is read off the diff, so
        // "skein has not looked" is not "nothing is owed". Unknown holds NEITHER, which parks the
        // pull request rather than letting a post through on an answer nobody gave.
        Cond::ChecksOwed => facts.checks_owed == Some(true),
        Cond::ChecksSettled => facts.checks_owed == Some(false),
    }
}

/// **Have you decided on it?** Approving and refusing are decisions; commenting is not.
///
/// One function, because [`Cond::Unreviewed`] and [`Cond::VerdictStanding`] are two halves of the
/// same word and two spellings of it would eventually disagree — and because this is `prq`'s rule
/// (`decided = matches!(my_review, "approved" | "changes-requested")`), which the engine may not
/// answer differently from the queue a person is reading.
///
/// An unknown word is not a decision. It fails towards [`Cond::VerdictStanding`] being false,
/// which is towards looking again.
pub(super) fn a_decision(my_review: &str) -> bool {
    matches!(my_review, "approved" | "changes-requested")
}

/// Where skein's reading stands against the head: `Some(true)` at it, `Some(false)` behind it,
/// `None` when the question cannot be asked.
///
/// **The third value is the point.** No reading, or no head to compare one to, satisfies neither
/// [`Cond::ReadingCurrent`] nor [`Cond::ReadingStale`] — the same rule [`Facts::mergeable`]'s
/// unknown obeys. Read the other way, a pull request skein has never read would answer "a reading
/// exists, at an older commit" and the engine would go and post one.
fn reading_against(facts: &Facts) -> Option<bool> {
    match facts.reading_sha.as_deref() {
        Some(read) if !read.is_empty() && !facts.head_sha.is_empty() => {
            Some(read == facts.head_sha)
        }
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    // `super::*` for this file's own items, private ones included; `crate::workflow::*` for
    // the rest of the engine, which was one namespace before this module became a directory
    // and still is from outside. A test reads the vocabulary the way a caller does.
    use super::*;
    #[allow(unused_imports)]
    use crate::workflow::*;

    /// **A label list that was cut off answers neither question about a label it never saw**
    /// (SKEIN-373).
    ///
    /// `prq::LABELS_FETCHED` pages GitHub's labels connection, and until SKEIN-373 the query did
    /// not ask how many there were — so a pull request with more labels than the page arrived
    /// short and read as complete. `no-label:` then answered a question about names it had never
    /// been sent, and the answer it gave was always the permissive one: absent. Measured on
    /// `acme/testbed#20`, 22 labels arrived as 20.
    ///
    /// Asserted as a property over a set of names rather than as an outcome for one pair, because
    /// the rule is what matters and the rule is asymmetric: **truncation can only ever take a
    /// condition away.** A label that arrived is on the pull request whatever the cap did, so
    /// `label:` is untouched; "this label is absent" is a claim about the part that did not
    /// arrive, so a short list cannot make it.
    #[test]
    fn a_condition_about_a_label_skein_never_saw_holds_neither_way() {
        let facts = |labels: &[&str], labels_whole| Facts {
            approved: true,
            base_is_trunk: Some(true),
            mergeable: Some(true),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            labels_whole,
            ..Default::default()
        };
        let arrived = ["ci", "hold"];
        // Names inside the page and names outside it, plus the empty one — a `Cond` is built from
        // a file people edit and nothing stops it naming a label that does not exist.
        let names = ["ci", "hold", "area/mod-21", "release", ""];

        // **Whole**: every name gets exactly one of the two answers. That is the property a short
        // list was silently claiming.
        let complete = facts(&arrived, true);
        for name in names {
            assert!(
                holds(&Cond::Label(name.into()), &complete)
                    != holds(&Cond::NoLabel(name.into()), &complete),
                "`{name}`: on a list skein saw all of, a label is either there or not there, and \
                 the two conditions must always disagree about it"
            );
        }

        // **Short**: a name that arrived keeps its answer, and a name that did not arrive gets
        // neither — the third value, exactly as `mergeable`'s unknown satisfies neither
        // `mergeable` nor `not-mergeable`.
        let short = facts(&arrived, false);
        for name in arrived {
            assert!(
                holds(&Cond::Label(name.into()), &short),
                "`{name}` arrived, and a cap somewhere past it took the condition away — \
                 truncation may only ever remove an answer skein does not have"
            );
            assert!(!holds(&Cond::NoLabel(name.into()), &short));
        }
        for name in names.iter().filter(|n| !arrived.contains(n)) {
            assert!(
                !holds(&Cond::Label((*name).into()), &short)
                    && !holds(&Cond::NoLabel((*name).into()), &short),
                "`{name}` was never sent to skein and a condition answered about it anyway — the \
                 permissive answer here is a merge over a label nobody could see"
            );
        }

        // And what that is worth, in the one shape it costs something: a workflow told to merge
        // anything nobody has put a hold on. On a complete list it merges; with the list short and
        // `hold` unaccounted for, it does nothing and says nothing — which is the safe direction,
        // and the queue's blind spots are where the silence is broken (`prq::queue_within`).
        let train = &from_bytes(
            br#"{"workflow":[{"name":"w","matches":["mine"],
                 "steps":[{"when":["approved","no-label:hold"],"do":"merge:squash"}]}]}"#,
        )
        .unwrap()[0];
        assert_eq!(
            next(train, &facts(&["ci"], true)).map(|c| c.act),
            Some(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false
            })),
            "the counter-case failed: with every label seen and no hold among them, the merge is \
             the right answer and must still happen"
        );
        assert_eq!(
            next(train, &facts(&["ci"], false)),
            None,
            "a pull request whose label list was cut off was merged on `no-label:hold` — the hold \
             may be one of the labels skein never received, which is SKEIN-373 exactly"
        );
    }

    /// **Not knowing what a change owes is not "it owes nothing"** — `docs/pr-review.md` §8.
    ///
    /// The same three-valued discipline as `reading-whole`, and it matters more here because of
    /// which way the two conditions point: `checks-owed` guards an `audit` step and
    /// `checks-settled` guards a POST. An unknown that satisfied `checks-settled` would release a
    /// verdict on the strength of a scan nobody ran, which is §8's whole subject.
    ///
    /// **What would make this fail:** writing either arm as a negation of the other —
    /// `ChecksSettled => facts.checks_owed != Some(true)` reads `None` as settled and is exactly
    /// the bug. They are two positive tests against a three-valued field, deliberately.
    #[test]
    fn not_knowing_what_is_owed_satisfies_neither_owed_nor_settled() {
        let unknown = Facts {
            checks_owed: None,
            ..Default::default()
        };
        assert!(
            !holds(&Cond::ChecksOwed, &unknown),
            "an unscanned change was made to audit something nobody said was owed"
        );
        assert!(
            !holds(&Cond::ChecksSettled, &unknown),
            "a verdict was released on the strength of a scan nobody ran"
        );
        // And both counter-cases, or the test above passes against a `holds` that answers false
        // for everything.
        assert!(holds(
            &Cond::ChecksOwed,
            &Facts {
                checks_owed: Some(true),
                ..Default::default()
            }
        ));
        assert!(holds(
            &Cond::ChecksSettled,
            &Facts {
                checks_owed: Some(false),
                ..Default::default()
            }
        ));
    }

    /// Unknown behind-ness satisfies neither `behind` nor `current`.
    ///
    /// Same discipline as `mergeable`, and with the same teeth: GitHub reports `mergeable: true`
    /// for a branch that is merely behind, so the train's merge step leans on `current` — and if
    /// unknown counted, the train would merge code CI never tested against the current trunk.
    #[test]
    fn unknown_behindness_satisfies_neither_behind_nor_current() {
        let facts = |behind, base_is_trunk| Facts {
            behind,
            base_is_trunk,
            ..Default::default()
        };
        let off = Some(false);
        assert!(holds(&Cond::Behind, &facts(Some(true), off)));
        assert!(!holds(&Cond::Current, &facts(Some(true), off)));
        assert!(holds(&Cond::Current, &facts(Some(false), off)));
        assert!(!holds(&Cond::Behind, &facts(Some(false), off)));
        assert!(
            !holds(&Cond::Behind, &facts(None, off)) && !holds(&Cond::Current, &facts(None, off)),
            "unknown behind-ness satisfied a condition it must satisfy neither of"
        );
        assert!(holds(&Cond::BaseTrunk, &facts(None, Some(true))));
        assert!(!holds(&Cond::BaseTrunk, &facts(None, Some(false))));
        // And a trunk skein has not learned yet satisfies it no more than a base that is not the
        // trunk does — the third value, kept out of the condition on purpose.
        assert!(
            !holds(&Cond::BaseTrunk, &facts(None, None)),
            "an unknown trunk claimed a base as the trunk anyway"
        );
    }

    /// Every word the reviewer's half adds, so a new one cannot be added without the tests below
    /// seeing it. Enumerated by hand because an enum's cases cannot be walked — the compiler helps
    /// the other way round, since [`spell_cond`] matches [`Cond`] exhaustively.
    const REVIEWER_CONDITIONS: [Cond; 7] = [
        Cond::ReviewRequested,
        Cond::Unreviewed,
        Cond::ReadingCurrent,
        Cond::ReadingStale,
        Cond::ReadingWhole,
        Cond::FindingsBlocking,
        Cond::VerdictStanding,
    ];

    /// **What skein did not see whole answers nothing, in either direction** (`docs/pr-review.md`
    /// §7b, §7c).
    ///
    /// The invariant rather than the case, which is the correction the reviewer interview forced:
    /// the interviewed box read `--limit 60` against 64 open pull requests and took the missing
    /// rows for "closed or merged". Truncation is never absence, and this asserts the rule over the
    /// whole reviewer vocabulary rather than over one fixture — including the fact-set nobody
    /// looked anything up for, where **nothing may hold at all**.
    ///
    /// What would make it fail, named before it was written: dropping `facts.reviews_whole` from
    /// [`Cond::Unreviewed`] (the SKEIN-373 shape, one connection over); [`reading_against`]
    /// answering `Some(false)` where there is no reading, so a pull request skein has never read
    /// reads as "read, at an older commit"; [`Cond::ReadingWhole`] written as `!= Some(false)`,
    /// which would let a sweep that never ran authorise an approval.
    ///
    /// Its counter-cases are the other half: a fact skein DID see keeps its answer, or this test
    /// would pass just as well against a `holds` that returned false for everything.
    #[test]
    fn a_review_fact_skein_did_not_see_whole_satisfies_neither_condition() {
        // A fact-set nobody looked anything up for. Not one reviewer condition may hold on it.
        for cond in REVIEWER_CONDITIONS {
            assert!(
                !holds(&cond, &Facts::default()),
                "`{}` held on facts skein never looked anything up for",
                spell_cond(&cond)
            );
        }

        // **The review list was cut**, around every verdict it could have been cut around. "I have
        // never decided" is a claim about the reviews that did NOT arrive, so a short list cannot
        // make it — and the same fact-set with the list whole must be able to.
        for my_review in ["none", "commented", "approved", "changes-requested"] {
            let cut = Facts {
                my_review: my_review.into(),
                reviews_whole: false,
                ..Default::default()
            };
            assert!(
                !holds(&Cond::Unreviewed, &cut),
                "`unreviewed` held with `my_review` at {my_review:?} out of a capped review \
                 connection — a viewer whose own row was cut reads as never having decided, and \
                 the engine would go and read a pull request it has already refused"
            );
            let whole = Facts {
                reviews_whole: true,
                ..cut.clone()
            };
            assert_eq!(
                holds(&Cond::Unreviewed, &whole),
                !matches!(my_review, "approved" | "changes-requested"),
                "with the whole list seen, `unreviewed` must answer {my_review:?} — a comment is \
                 deliberately not a decision, which is `prq`'s own lane rule"
            );
        }

        // The other direction, and the asymmetry that makes this `no-label:`'s rule rather than
        // `label:`'s: a verdict that ARRIVED is still yours whatever a cap did further down.
        for reviews_whole in [true, false] {
            assert!(
                holds(
                    &Cond::VerdictStanding,
                    &Facts {
                        my_review: "approved".into(),
                        my_review_current: true,
                        reviews_whole,
                        ..Default::default()
                    }
                ),
                "truncation took away an answer skein HAD — it may only ever remove one it does \
                 not have"
            );
        }

        // **A reading skein cannot see**: neither current nor stale, whichever half is missing.
        for head in ["", "abc"] {
            for reading in [None, Some(String::new())] {
                let blind = Facts {
                    head_sha: head.into(),
                    reading_sha: reading.clone(),
                    ..Default::default()
                };
                assert!(
                    !holds(&Cond::ReadingCurrent, &blind) && !holds(&Cond::ReadingStale, &blind),
                    "head {head:?} and reading {reading:?} answered one of the two conditions it \
                     must answer neither of — `reading-stale` on a pull request nobody has read \
                     sends the engine to post what it never wrote"
                );
            }
        }
        // A reading skein HAS, against a head it does not know: still neither. The sha guard needs
        // both halves, and one of them is not an anchor.
        let unanchored = Facts {
            reading_sha: Some("abc".into()),
            ..Default::default()
        };
        assert!(
            !holds(&Cond::ReadingCurrent, &unanchored) && !holds(&Cond::ReadingStale, &unanchored)
        );

        // **The sweep, and the findings.** No answer is not a no and it is not a yes: `None` and
        // `Some(false)` both fail, `Some(true)` is the only thing that holds.
        for (sweep, blocking) in [(None, None), (Some(false), Some(false))] {
            let f = Facts {
                reading_whole: sweep,
                findings_blocking: blocking,
                ..Default::default()
            };
            assert!(
                !holds(&Cond::ReadingWhole, &f) && !holds(&Cond::FindingsBlocking, &f),
                "a sweep that did not account for the change ({sweep:?}) authorised an approval"
            );
        }
        assert!(
            holds(
                &Cond::ReadingWhole,
                &Facts {
                    reading_whole: Some(true),
                    ..Default::default()
                }
            ) && holds(
                &Cond::FindingsBlocking,
                &Facts {
                    findings_blocking: Some(true),
                    ..Default::default()
                }
            ),
            "the counter-case failed: a sweep that DID account for every changed file must hold, \
             or this test passes against a `holds` that answers false for everything"
        );
    }

    /// **No reviewer condition reads whether the pull request is approved** (`docs/pr-review.md`
    /// §7a).
    ///
    /// [`Facts::approved`] answers *"has anybody approved this, and is nobody's refusal
    /// standing"* — a question about the pull request, built from `reviewDecision` and the
    /// standing approvals of any reviewer. The reviewer's question is about YOU. Reading the first
    /// for the second is SKEIN-339 re-committed under a new word, and that field's own doc carries
    /// what it cost: `APPROVED` on zero of twenty-one open pull requests, two of which the owner
    /// had personally approved.
    ///
    /// **The invariant, not the case**, which is the whole reason this is shaped like
    /// `fleet::an_expired_credential_propagates_exactly_as_far_as_a_live_one`: the same reviewer
    /// facts are run twice, once on a pull request nobody has approved and once on one the
    /// repository is satisfied with, and *identical answers ARE the assertion*. A test that
    /// asserted an outcome per fixture would go on passing the day somebody folds `approved` into
    /// [`Cond::VerdictStanding`].
    ///
    /// What would make it fail: `Cond::VerdictStanding => facts.approved && …`, or
    /// `Cond::Unreviewed => … && !facts.approved`.
    #[test]
    fn no_reviewer_condition_reads_whether_the_pull_request_is_approved() {
        // The three facts that answer about the PULL REQUEST rather than about you. Moved
        // together, because they are the three a reviewer condition could be tempted to reach for.
        let pull_request_side = [
            (false, false, None),
            (true, false, Some(true)),
            (false, true, Some(false)),
            (false, false, Some(false)),
        ];
        let answers = |f: &Facts| REVIEWER_CONDITIONS.map(|cond| holds(&cond, f)).to_vec();
        let mut checked = 0usize;
        for my_review in ["none", "commented", "approved", "changes-requested"] {
            for my_review_current in [false, true] {
                for reviews_whole in [false, true] {
                    for review_requested in [false, true] {
                        for reading_sha in [None, Some("abc".to_string()), Some("def".to_string())]
                        {
                            for sweep in [None, Some(false), Some(true)] {
                                let yours = Facts {
                                    my_review: my_review.into(),
                                    my_review_current,
                                    reviews_whole,
                                    review_requested,
                                    head_sha: "abc".into(),
                                    reading_sha: reading_sha.clone(),
                                    reading_whole: sweep,
                                    findings_blocking: sweep,
                                    ..Default::default()
                                };
                                let baseline = answers(&yours);
                                for (approved, changes_requested, review_requirement_met) in
                                    pull_request_side
                                {
                                    let theirs = Facts {
                                        approved,
                                        changes_requested,
                                        review_requirement_met,
                                        ..yours.clone()
                                    };
                                    assert_eq!(
                                        answers(&theirs),
                                        baseline,
                                        "a reviewer condition changed its answer when the PULL \
                                         REQUEST's review state changed (approved={approved}, \
                                         changes_requested={changes_requested}, \
                                         requirement={review_requirement_met:?}) — that is \
                                         `Facts::approved` answering the reviewer's question, \
                                         which is SKEIN-339 under a new word"
                                    );
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 500, "the grid collapsed: {checked} comparisons");

        // The counter-case, or the flip proves nothing: the AUTHOR side's conditions must see
        // exactly the change the reviewer's side must not.
        let approved = Facts {
            approved: true,
            ..Default::default()
        };
        assert!(
            !holds(&Cond::Approved, &Facts::default()) && holds(&Cond::Approved, &approved),
            "the fields being flipped are not the ones the author side reads, so the invariant \
             above is asserting nothing"
        );
    }

    /// **A reading at an older commit is stale, and never current** (`docs/pr-review.md` §4).
    ///
    /// The sha guard, over every pair of shas rather than over one: *the reading step records its
    /// findings with the sha it read, and the posting step's guard is `finding.sha == head`.* A
    /// memoryless engine splits reading from posting across polls and the head can move between
    /// them by design, so without this it posts a review describing tree A anchored to tree B —
    /// the one failure §3 says gets structurally WORSE under a stateless engine.
    ///
    /// What would make it fail: comparing with `starts_with`, which reads a shortened sha as the
    /// sha it was shortened from (this tree has a `short()` for exactly that display, so the
    /// mistake is one keystroke away); comparing case-insensitively; or letting
    /// [`Cond::ReadingStale`] mean "a reading exists" and answer without the head.
    ///
    /// The pairs are chosen to catch those: one a prefix of another, a case variant, and the empty
    /// sha that means skein does not know. And every answer is asserted unchanged by the facts the
    /// guard must NOT read — the same inputs, twice, where identical expectations are the
    /// assertion.
    #[test]
    fn a_reading_at_an_older_commit_is_stale_and_never_current() {
        let shas = ["abc123", "abc124", "abc", "abc123def", "ABC123", ""];
        for head in shas {
            for read in shas {
                for noise in [
                    Facts::default(),
                    Facts {
                        approved: true,
                        my_review: "approved".into(),
                        my_review_current: true,
                        reviews_whole: true,
                        reading_whole: Some(true),
                        findings_blocking: Some(true),
                        ..Default::default()
                    },
                ] {
                    let f = Facts {
                        head_sha: head.into(),
                        reading_sha: Some(read.into()),
                        ..noise
                    };
                    let (current, stale) = (
                        holds(&Cond::ReadingCurrent, &f),
                        holds(&Cond::ReadingStale, &f),
                    );
                    assert!(
                        !(current && stale),
                        "reading {read:?} against head {head:?} is both current and stale"
                    );
                    match (head.is_empty() || read.is_empty(), head == read) {
                        (true, _) => assert!(
                            !current && !stale,
                            "with one sha missing there is nothing to anchor to, and {read:?} \
                             against {head:?} answered anyway"
                        ),
                        (false, true) => assert!(
                            current && !stale,
                            "a reading of the head that is there now is current"
                        ),
                        (false, false) => assert!(
                            stale && !current,
                            "a reading of {read:?} where the head is {head:?} was read as \
                             current — the post it authorises describes a commit that is not \
                             there any more"
                        ),
                    }
                }
            }
        }
    }
}
