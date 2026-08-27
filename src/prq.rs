//! The review queue: every open PR in a repo that is *yours*, and which lane it sits in.
//!
//! skein's PR surface used to hang off a box — `box → branch → gh pr view`, in a `ship` module
//! since retired — which makes a PR nobody in the fleet authored invisible. That is fatal for
//! review, where most of the queue is other people's work. So the spine here is inverted: the **repo** owns the list, a **PR**
//! is the object, and a box is something you summon onto a branch when one turns out to need hands.
//!
//! Three rules decide membership, and all three are GitHub's answer, not skein's: you were asked to
//! review it, you opened it, or you were mentioned in it. CODEOWNERS is not consulted for membership
//! — only for how deeply to explain a change once it's already in your queue. See
//! [`crate::codeowners`] for why that split matters.
//!
//! **Identity lives on the host.** Every `gh` call here runs as *you*, on your own login — not on a
//! box's scoped installation token. That is the deliberate opposite of [`crate::gitgate`], which
//! exists to stop boxes from acting as you. An approval that isn't yours is worth nothing when the
//! base branch is protected, so the review path stays on your side of that line.
//!
//! Nothing in this module needs AI. A queue that lists and lanes PRs correctly is already the
//! product; summaries in [`crate::review`] only decide how much reading each row saves you.

use crate::config::skein_home;
use crate::repos::Repo;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Which lane a PR sits in. Derived on every fetch, never stored — the only lane skein has an
/// opinion about is [`Lane::Archived`], and even that is cleared the moment GitHub stops calling
/// the PR open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    /// **Your move**: somebody is waiting on your review and nothing stops you giving it — not a
    /// draft, not conflicted. This is the lane the badge counts, so it holds only what you can
    /// actually act on now. **Failing checks do not move a PR out of here**: on this fleet CI runs
    /// only after review (a workflow applies the CI label on approval), so red is the *ordinary*
    /// state of a PR awaiting you, and demoting it hid exactly the rows the queue exists to show
    /// — reported live as "some PRs are cut out from the view, including 577".
    NeedsYou,
    /// **Their move**: you authored it, or you already decided on the *current* head commit
    /// (approved or requested changes). Either way the next act belongs to somebody else.
    Waiting,
    /// **Not ready for review**: a draft, or unmergeable — reviewing it now would be reviewing
    /// something its author is still going to change. Shown as a count with its reasons rather
    /// than as rows: nothing is hidden, it is just not claiming to be your problem.
    NotReady,
    /// You have set it aside by hand — it is open, but not going to move for reasons skein has no
    /// way to know.
    Archived,
}

/// Why a PR is in your queue. Kept as a list rather than one value because a PR is routinely more
/// than one of these at once, and collapsing them would break the filter you actually asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// You opened it.
    Author,
    /// Your review was requested, personally.
    Reviewer,
    /// You have reviewed it — commented, approved, or asked for changes — and it is still open.
    ///
    /// Its own search because of a GitHub semantic that silently empties the queue: submitting ANY
    /// review, a comment-only one included, removes you from `review-requested:`. So the moment
    /// you acted on a pull request it vanished from every query this queue ran — reported live as
    /// "PR 577 is still not visible while it is clearly open", the day after a drafted comment was
    /// posted to it. Acting on your queue must never be what empties it.
    Reviewed,
    /// You were mentioned in the body or a comment.
    Mentioned,
    /// A team you belong to was asked to review — invisible to the personal query, see [`viewer`].
    Team(String),
}

/// One failing context out of the check rollup: the name a human knows the check by, and where
/// its log lives.
///
/// [`Pr::checks`] keeps the one-word verdict — lanes and sorting want a word — but a word cannot
/// answer the question a red row actually raises, "which one?", and answering it today costs a
/// click through to GitHub per row (SKEIN-153).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCheck {
    pub name: String,
    /// Where the failure's detail lives. Empty when the rollup carried no link — absence stays
    /// absent, and the page renders a name without a link rather than a link to nowhere.
    #[serde(default)]
    pub url: String,
}

/// How many failing contexts a row names. A cap, not a summary: fifty red checks are one broken
/// pipeline, and naming five says "at least these" without turning the row into a log.
pub const FAILING_CHECKS_SHOWN: usize = 5;

/// One review thread on a pull request — **without a word of what anybody said in it**.
///
/// The omission is the design (SKEIN-300, SKEIN-301), not a shortcut. An inline thread is drawn as
/// who opened it, when, a link, and a resolve button; its comment bodies are never rendered, so
/// fetching them would buy nothing and cost the most expensive thing in the queue. SKEIN-287 cut
/// this list's payload from 155 KB to 12 KB, and [`PR_FRAGMENT`] is asked for up to
/// [`SEARCH_PAGE`] pull requests at a time across every membership rule — so a body added here is
/// a body multiplied by a hundred, on the one request `acme/thing` already answers with a 504
/// (SKEIN-278). If a thread's text is ever wanted, it is one PR's own request, not this one's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewThread {
    /// GitHub's node id for the thread. Load-bearing rather than decoration: it is the argument
    /// `resolveReviewThread` takes, so a thread fetched without it cannot be resolved from skein.
    pub id: String,
    /// Has somebody marked it resolved?
    #[serde(default)]
    pub resolved: bool,
    /// Does it hang off lines the head has since replaced? An outdated thread is still open, and
    /// still yours to answer — it is a different sentence, not a resolved one.
    #[serde(default)]
    pub outdated: bool,
    /// Who opened it, and when — the FIRST comment's author and `createdAt`. Empty when GitHub did
    /// not say, which is the same rule every other field here follows: absence stays absent.
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub started_at: String,
    /// Where the thread lives on GitHub — the first comment's permalink, which is what a
    /// `PullRequestReviewThread` has instead of a url of its own.
    #[serde(default)]
    pub url: String,
}

/// One PR-level comment — the conversation, not the code review. **These carry their bodies**,
/// because these are the ones the panel renders.
///
/// The asymmetry with [`ReviewThread`] above is deliberate and is the whole cost decision: bodies
/// are fetched exactly where they are drawn and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrComment {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub url: String,
}

/// Somebody GitHub is still waiting on for a review — a person, or a team.
///
/// [`Pr::review_decision`] answers "does this need somebody"; it cannot answer "who", and "who" is
/// the question that was actually asked. A team is kept as a team rather than flattened into a
/// login, because the sentence a row wants to write is different: *"waiting on @alice"* and
/// *"waiting on acme/core"* are not interchangeable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// A user's login, or a team as `org/team`.
    pub name: String,
    /// Is this a team rather than a person?
    #[serde(default)]
    pub team: bool,
}

/// One PR in the queue.
///
/// Fields are pulled defensively from `gh`'s JSON: a field this version of `gh` does not emit
/// degrades that one value, rather than dropping the PR. A PR you never saw is the failure mode
/// that costs something; a PR with an unknown check state is merely less useful.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub draft: bool,
    pub updated_at: String,
    /// When the head COMMIT landed, RFC 3339 — not when the pull request was last touched.
    ///
    /// These are different questions and only one of them is about commits. `updated_at` moves when
    /// somebody leaves a comment, so a branch nobody has pushed to in a day reads as hot the moment
    /// it is discussed — which is exactly backwards for deciding whether a PR has settled enough to
    /// be worth reading.
    ///
    /// Has it been quiet long enough to be worth reading without being asked?
    ///
    /// **Decided here, once.** The rule started in the page, and the moment a server-side reader
    /// existed there were two copies of "an hour" — which is not a bug yet and is exactly how one
    /// starts: the pane says "waiting" while the server is already reading, and nobody can say
    /// which is right. Now the page renders this answer and the reader acts on it.
    ///
    /// An unknown commit date reads as settled, which is the opposite of what it looks like it
    /// should be — see [`settled`].
    ///
    /// Defaulted, and defaulted to TRUE: a queue remembered on disk by an older skein has no such
    /// field, and without a default the whole remembered queue fails to parse — which turns a new
    /// field into an empty pane. The value matches the rule: what skein does not know about does
    /// not hold anything back.
    #[serde(default = "settled_by_default")]
    pub settled: bool,
    /// Empty when GitHub did not say. "Do not know" is not "long ago" and must never be REPORTED as
    /// one — nothing may tell somebody a branch is still moving on the strength of an absent field.
    /// What a caller DOES about it is a separate decision, and the review pane makes the opposite
    /// one to the obvious: it falls back to what it did before the settle rule existed, because a
    /// rule that switches a feature off when its input is missing is worse than the churn it was
    /// written to stop.
    pub committed_at: String,
    /// "passing" | "pending" | "failing" | "none".
    pub checks: String,
    /// WHICH contexts are behind a "failing", by name — capped at [`FAILING_CHECKS_SHOWN`],
    /// deduplicated, in rollup order. Empty whenever `checks` is not "failing". Defaulted so a
    /// queue remembered by an older skein still parses; empty renders as nothing, which is what an
    /// older queue honestly knew.
    #[serde(default)]
    pub failing_checks: Vec<FailedCheck>,
    /// Every label on it, by name. What a workflow adds to start CI and reads to know it did.
    ///
    /// **Up to [`LABELS_FETCHED`] of them**, so this list can be short — read
    /// [`Pr::labels_total`] before treating a name's ABSENCE from it as evidence.
    #[serde(default)]
    pub labels: Vec<String>,
    /// How many labels GitHub says there are, where it said — the count beside the names, for the
    /// same reason [`Pr::review_threads_total`] sits beside the threads (SKEIN-373).
    ///
    /// `None` is "nobody said": a queue remembered by an older skein, or an answer from before the
    /// query asked for `totalCount`. Not `Some(0)`, which would be the claim that the pull request
    /// carries no labels — the exact claim a truncated list was making silently.
    ///
    /// Nothing reads this raw. [`Pr::labels_whole`] is the one rule, so the sentence the queue says
    /// out loud and the fact the merge train acts on cannot disagree about what "short" means.
    #[serde(default)]
    pub labels_total: Option<u64>,
    /// GitHub's verdict on the pull request as a whole: `APPROVED`, `CHANGES_REQUESTED`,
    /// `REVIEW_REQUIRED`, or empty where the repository asks for no review.
    ///
    /// Distinct from [`Pr::my_review`], which is what YOU last said. A workflow that merges cares
    /// about the repository's answer — being one of six reviewers who approved is not the same fact
    /// as the pull request being approved.
    #[serde(default)]
    pub review_decision: String,
    /// **How many approvals are standing against the head that is there now — anybody's, not just
    /// yours** (SKEIN-356).
    ///
    /// The gap this closes: [`Pr::review_decision`] is the REPOSITORY's verdict and is empty
    /// wherever review is social, and [`Pr::my_review`] is the viewer's own. Between them they
    /// cannot see a third party's approval on a repository that requires no review, so
    /// `prwork::facts_of` built `Facts::approved` from a pair of fields that were both silent and
    /// the merge train sat on approved work. Under-reporting is the safe direction — it holds a
    /// pull request rather than shipping one — but on a repo where the owner is the author and
    /// somebody else reviews, it is the ordinary case, which makes it a gap rather than a design.
    ///
    /// **Counted from `latestOpinionatedReviews`**, the same connection [`my_review_state`] reads
    /// and for the same reason (SKEIN-354): it is the latest review per author that DECIDED
    /// something, so a note left after an approval does not demote it and an approval GitHub has
    /// DISMISSED has already dropped out. The fallback to `latestReviews` is the same one too, so
    /// an answer that carries no opinionated connection reads exactly as it always did.
    ///
    /// **"Standing" here is the review's commit against the head**, derived exactly as
    /// [`Pr::review_is_current`] is — so the invariant `my_review == "approved" &&
    /// review_is_current` ⟹ this is at least 1 holds by construction, and
    /// `prwork::tests::an_approval_is_still_an_approval_where_the_repository_asks_for_none`'s
    /// stale-approval half keeps its answer for everybody rather than only for you.
    ///
    /// `None` is "nobody counted": a queue remembered by an older skein. `prwork::facts_of` falls
    /// back to [`Pr::my_review`] there, which is what that queue already knew — `Some(0)` would be
    /// the claim that skein looked and found none.
    ///
    /// **A floor where [`Pr::reviews_whole`] is false**, and only ever a floor: the connection is
    /// capped, so an approval past the cap is not counted (SKEIN-386). The error is in the holding
    /// direction — a merge train sits on a pull request rather than shipping one — and the queue
    /// says the hole out loud rather than this field claiming a number it cannot stand behind.
    #[serde(default)]
    pub standing_approvals: Option<u64>,
    /// **How many reviews GitHub says this pull request has, and how many of them arrived**
    /// (SKEIN-386).
    ///
    /// [`PR_FRAGMENT`] asks both review connections for [`REVIEWS_FETCHED`] nodes and nothing said
    /// how many it capped, so a pull request reviewed by more people than that lost the rest on the
    /// way in with no trace anywhere: [`Pr::my_review`] reads `"none"` for a viewer whose own row
    /// was cut, and [`Pr::standing_approvals`] under-counts by however many approvals were.
    ///
    /// The shape is [`Pr::labels_total`]'s, for the reason that field's doc gives — the cap is said
    /// out loud rather than raised, and [`REVIEWS_FETCHED`] has the measurement that makes raising
    /// it unavailable. `reviews_read` stands in for the `len()` [`Pr::labels`] supplies, because
    /// the reviews themselves are not carried on the row; the two are only ever read together, and
    /// only through [`Pr::reviews_whole`].
    ///
    /// **From whichever of the two connections lost the most** — see [`reviews_counted`]. `None` is
    /// "nobody said": a queue remembered by an older skein, or an answer from before the query
    /// asked for `totalCount`. Not `Some(0)`, which would be the claim that nobody has reviewed it.
    #[serde(default)]
    pub reviews_total: Option<u64>,
    /// How many of those reviews arrived — never read apart from [`Pr::reviews_total`], which
    /// carries the whole of why both exist.
    #[serde(default)]
    pub reviews_read: Option<u64>,
    /// Can GitHub merge it as it stands? `None` where GitHub has not worked it out yet, which it
    /// reports as `UNKNOWN` for a while after every push.
    ///
    /// **`Option`, not `bool`.** Unknown is not "no": a workflow that read it as a conflict would
    /// rebase on a guess, and on a repository that dismisses stale approvals that rebase destroys
    /// the approval authorising the merge. Flattening it here would undo `workflow::holds` quietly,
    /// one layer down from the test that protects it.
    #[serde(default)]
    pub mergeable: Option<bool>,
    /// GitHub's raw verdict on how this head sits against its base — `mergeStateStatus`, kept
    /// uppercase exactly as GitHub spells it: `BEHIND`, `CLEAN`, `DIRTY`, `BLOCKED`, `UNSTABLE`,
    /// `UNKNOWN`, …. `BEHIND` is the one a merge train reads: the base has moved, so this cannot
    /// ride until the base is merged in.
    ///
    /// Empty when GitHub did not say — and a queue remembered by an older skein has no such field,
    /// so it parses as `""`. Same rule as [`Pr::mergeable`]'s `None`: empty is "not known", never
    /// "current", and a caller that read `""` as `CLEAN` would advance a train on a guess.
    #[serde(default)]
    pub merge_state: String,
    /// The reviewer's first question is "can I do this now?", and that is size before anything
    /// else. `Option` so a queue remembered from before these fields is honest: absent renders as
    /// nothing, where a defaulted 0 would claim an empty change.
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub changed_files: Option<u64>,
    /// "approved" | "changes-requested" | "commented" | "none" — *your* last review.
    ///
    /// **"none" is only as good as [`Pr::reviews_whole`]** (SKEIN-386): it is read out of a capped
    /// connection, so a viewer whose own row sorted past [`REVIEWS_FETCHED`] reads as never having
    /// decided. That leaves the pull request in [`Lane::NeedsYou`] — claiming your attention rather
    /// than releasing it, which is the direction to be wrong in — and the queue says so out loud.
    pub my_review: String,
    /// Was that review submitted against the current head?
    ///
    /// **It no longer decides whose move the pull request is** (SKEIN-354). It used to: a head that
    /// moved past the sha you reviewed returned the PR to [`Lane::NeedsYou`], so any push took your
    /// approval off you. Measured on the owner's live queue on 2026-08-26 this was false on all 26
    /// rows — including the two he had approved himself — because GitHub reports the commit a
    /// review was left against and branches move. His rule instead: "approved should come only if
    /// my review status on the PR is approved rn, if I approved and then some file I own changed,
    /// so github asks me to review again then it should show that." That is [`Pr::my_review`] and
    /// [`Pr::my_review_requested`], both of them GitHub's own answers.
    ///
    /// What it is still for is everything that is about the CODE rather than about you: the "new
    /// commits" mark, the clock the your-move lane sorts on, and `review::worth_reading` deciding
    /// that a reading of an older commit is stale. Evidence the head moved, not a verdict on your
    /// review.
    pub review_is_current: bool,
    /// Is GitHub asking YOU for a review right now — you by name, in `reviewRequests`?
    ///
    /// This is the other half of the rule above, and the only thing that puts a pull request you
    /// have already decided back into your hands. A CODEOWNERS re-request when a file you own
    /// changes lands here; so does a human asking you again after a rewrite. Nothing else does, and
    /// that is deliberate — the alternative is skein deciding for itself that a push was big enough
    /// to matter, which is the inference this item removed.
    ///
    /// **A floor, not a census.** A request made of a TEAM you belong to arrives as the team, not
    /// as you, and without `read:org` it arrives with no name at all (`normalise`, and the blind
    /// spot the queue already reports) — so this can be false when GitHub would say you were asked.
    /// The error therefore only ever falls on the side of NOT claiming you, which is the side the
    /// owner asked for: "theirs until they ask again".
    ///
    /// Defaulted, like every field added after the first remembered queue was written: a queue on
    /// disk from an older skein has no such key, and `false` there means an approval you gave
    /// before this existed goes on standing, which is the same answer that queue was already giving.
    #[serde(default)]
    pub my_review_requested: bool,
    pub reasons: Vec<Reason>,
    pub lane: Lane,
    /// Why an [`Lane::Archived`] row is there: `true` when it was set aside *until the head moves*
    /// (SKEIN-144) rather than archived outright. The lane is deliberately shared — both mean "not
    /// claiming your attention" — but the endings differ (a human act versus the author's next
    /// push), so the page needs to know which story to tell. Defaulted for remembered queues.
    #[serde(default)]
    pub snoozed: bool,
    /// The review threads on this pull request, newest [`REVIEW_THREADS_FETCHED`] of them, with no
    /// comment bodies — see [`ReviewThread`] for why the bodies are not here.
    ///
    /// Defaulted, like every field added after the first remembered queue was written: a queue on
    /// disk from an older skein has no such key, and without a default **the whole queue fails to
    /// parse**, which turns a new field into an empty pane rather than a missing line.
    #[serde(default)]
    pub review_threads: Vec<ReviewThread>,
    /// How many threads there are in total, where GitHub said. The cap above is a cap, and a list
    /// that is short must be able to say so rather than read as a pull request with nothing open
    /// on it. `None` for a queue remembered before this field existed — not `0`, which would be a
    /// claim.
    #[serde(default)]
    pub review_threads_total: Option<u64>,
    /// The pull request's own conversation, the last [`PR_COMMENTS_FETCHED`] of them, bodies
    /// included.
    ///
    /// The LAST rather than the first: a conversation is read from its end, and the comment that
    /// decides anything is the recent one. A pull request with four hundred comments is exactly
    /// what this cap exists for — the whole thread would be fetched a hundred times over in one
    /// batched request.
    #[serde(default)]
    pub comments: Vec<PrComment>,
    /// How many comments there are in total, where GitHub said — see
    /// [`Pr::review_threads_total`] for why it is an `Option`.
    #[serde(default)]
    pub comments_total: Option<u64>,
    /// Who still owes a review — people and teams GitHub is waiting on. See [`ReviewRequest`].
    #[serde(default)]
    pub review_requests: Vec<ReviewRequest>,
    /// The deterministic box name for this branch — whether or not one exists yet.
    pub box_name: String,
}

impl Pr {
    /// **Did skein see every label this pull request carries?** (SKEIN-373)
    ///
    /// The one place "the list is short" is decided, so the blind spot the queue says out loud and
    /// the fact `prwork::facts_of` hands the merge train cannot disagree — the failure that
    /// version would have is a row promising completeness while a workflow acts on a hole, which
    /// is the defect this came from wearing a second face.
    ///
    /// Compared against what actually ARRIVED rather than against [`LABELS_FETCHED`], the same way
    /// [`truncated_rollup`] is, so it stays true if the cap ever moves.
    ///
    /// **An unknown total reads as whole**, which is the opposite of the fail-closed answer and is
    /// deliberate: `None` is a queue remembered by a skein from before this was asked for, and
    /// [`Queue::whole`] and [`Pr::settled`] both make the same choice for the same reason — what
    /// skein could not know must not hold back anything it was not already holding back. A live
    /// refresh always has the number, so the honest-but-cautious answer is available exactly where
    /// it can be acted on.
    pub fn labels_whole(&self) -> bool {
        match self.labels_total {
            Some(total) => total as usize <= self.labels.len(),
            None => true,
        }
    }

    /// **Did skein see every review on this pull request?** (SKEIN-386)
    ///
    /// The one place "the review list is short" is decided, for the reason [`Pr::labels_whole`]
    /// gives: the sentence the queue says out loud and the two answers derived from the same
    /// connections — [`Pr::my_review`] and [`Pr::standing_approvals`] — must not be able to
    /// disagree about what "short" means.
    ///
    /// Compared against what actually ARRIVED rather than against [`REVIEWS_FETCHED`], so it stays
    /// true if the cap ever moves, and **an unknown total reads as whole** — both the same choices
    /// [`Pr::labels_whole`] makes, and the second for the same reason: `None` is a queue remembered
    /// by a skein from before this was asked for, and what skein could not know must not hold back
    /// anything it was not already holding back.
    ///
    /// **What `false` costs is a sentence, not a refusal**, and that is the difference from the
    /// label case. There, a truncated list made `workflow::Cond::NoLabel` answer PERMISSIVELY and a
    /// `hold` could be merged over. Here every reader errs towards holding — a review skein never
    /// saw cannot promote a lane or authorise a merge — so what truncation actually takes away is a
    /// person's ability to tell "nobody has approved this" from "skein read thirty of forty
    /// reviews", which is what `queue_within` says out loud.
    pub fn reviews_whole(&self) -> bool {
        match (self.reviews_total, self.reviews_read) {
            (Some(total), Some(read)) => total <= read,
            _ => true,
        }
    }
}

/// A placeholder pull request for a test to build on, with the fields nobody can guess supplied.
///
/// **Why this is here rather than in each test module.** `Pr` is built by hand in four fixtures
/// across `src/review.rs` and `src/queue.rs`, every one of them exhaustive — so adding a field to
/// it broke three files that had no opinion about the field (SKEIN-301). The fixtures in
/// `src/prwork.rs` never broke, because they build theirs through `serde_json::from_value` and the
/// `#[serde(default)]`s absorb a new key; this is the same tolerance for the ones that want a
/// struct literal.
///
/// Used as `Pr { lane: Lane::Waiting, ..blank_pr(7, "abc") }`, so what a test cares about stays
/// visible on the line and everything else stops being its problem. `NeedsYou` and `Reviewer` are
/// the values a queue fixture wants most often, and both are stated rather than defaulted where a
/// test turns on them.
#[cfg(test)]
pub(crate) fn blank_pr(number: u64, head_sha: &str) -> Pr {
    Pr {
        number,
        title: "t".into(),
        author: "someone".into(),
        url: String::new(),
        head_ref: "feat".into(),
        head_sha: head_sha.into(),
        base_ref: "main".into(),
        draft: false,
        updated_at: String::new(),
        committed_at: String::new(),
        settled: true,
        labels: Vec::new(),
        labels_total: None,
        review_decision: String::new(),
        standing_approvals: None,
        reviews_total: None,
        reviews_read: None,
        mergeable: None,
        merge_state: String::new(),
        additions: None,
        deletions: None,
        changed_files: None,
        checks: "none".into(),
        failing_checks: Vec::new(),
        my_review: "none".into(),
        review_is_current: false,
        my_review_requested: false,
        snoozed: false,
        review_threads: Vec::new(),
        review_threads_total: None,
        comments: Vec::new(),
        comments_total: None,
        review_requests: Vec::new(),
        reasons: vec![Reason::Reviewer],
        lane: Lane::NeedsYou,
        box_name: String::new(),
    }
}

/// A repo's queue, plus an honest account of what could not be looked at.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub repo_id: String,
    pub slug: String,
    pub viewer: String,
    /// Is skein allowed to read PRs? Without it every summary is [`crate::review::Depth::Unread`],
    /// so the page says so once instead of asking the server thirty times to be told the same thing.
    pub ai: bool,
    pub prs: Vec<Pr>,
    /// What this queue could **not** see, in plain words.
    ///
    /// A review queue that silently under-reports is worse than no queue: you would trust it and
    /// miss things. Every path that can fail partially — team review requests needing `read:org`,
    /// a query that errored — states itself here rather than returning a shorter list.
    pub blind_spots: Vec<String>,
    /// When this was read from GitHub, RFC 3339.
    ///
    /// Load-bearing rather than decoration. A queue may now be served from the copy on disk before
    /// the fresh one exists, and showing somebody yesterday's pull requests as if they were today's
    /// is the same failure as a board that looks calm because its server died (SKEIN-128). Stale is
    /// only safe when its age is visible.
    #[serde(default)]
    pub as_of: String,
    /// Was this read from GitHub just now, or handed over while a fresh one is being fetched?
    #[serde(default = "yes")]
    pub fresh: bool,
    /// Did the searches behind this queue see every pull request there was?
    ///
    /// The same fact [`Found::whole`] carries for one membership search, ANDed across all of them
    /// and across the failures: `false` where a search errored, where the whole request did, or
    /// where one filled its page and GitHub said there was another (SKEIN-231).
    ///
    /// It is what anyone must read before treating a pull request's **absence** from `prs` as
    /// evidence about it. The archive and snooze prunes inside [`queue_within`] have read it since
    /// SKEIN-229; `review::prune` runs outside this module — it is handed this list by the server —
    /// and had no way to ask until this field existed, so it deleted nothing but paid a REST call
    /// per summary file, for ever, for pull requests that were merely past the page.
    ///
    /// `blind_spots` is not a substitute: it is non-empty for things that say nothing about
    /// completeness, `read:org` among them, so a caller reading it as this flag stands down for the
    /// wrong reasons. Defaults to true for a queue remembered by an older skein — the same choice
    /// [`Pr::settled`] makes, for the same reason: what skein could not know does not hold anything
    /// back that it was not already holding back.
    #[serde(default = "yes")]
    pub whole: bool,
    /// The repository's default branch — the trunk a merge train advances. Filled during a
    /// refresh from `GET /repos/{slug}` (its `default_branch`), remembered per process like
    /// [`renamed_to`]'s answer beside it.
    ///
    /// Empty is honest "not known", never a branch name: a queue remembered by an older skein has
    /// no such field and parses as `""`, and a lookup that failed is remembered as `""` rather
    /// than retried on every poll. A caller treats `""` as "ask again after a restart", not as a
    /// trunk called nothing.
    #[serde(default)]
    pub trunk: String,
}

/// `Queue::fresh` defaults true: everything that computes one directly has just read GitHub, and a
/// field that quietly defaulted to "stale" would put an age warning on every honest answer.
fn yes() -> bool {
    true
}

/// Where the token the host talks to GitHub with came from, in the order it is looked for.
///
/// The order is the point: skein offers three ways to give it GitHub access, and the review queue
/// used to require a fourth — `gh auth login` — because it was built out of the `gh` CLI and `gh`
/// only knows its own store. One credential the user chose should do every job it is capable of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhToken {
    /// `$GH_TOKEN` / `$GITHUB_TOKEN`.
    Environment,
    /// The read token stored in Settings. A user's own PAT, which is what a review queue needs: it
    /// answers "who are you" and can see the repositories its owner can.
    ReadToken,
    /// A per-repo write token, used because it is also a user's PAT and no read token was stored.
    /// Narrower than a read token — what it cannot see is reported as a blind spot rather than
    /// quietly missing from the queue.
    WritePat,
    /// The host's own `gh` login, asked for last.
    ///
    /// It was missing, and its absence contradicted this list's own reason for existing. Skein
    /// already reads this login — `repos::ensure_gh_secret` puts it in front of every box — so a
    /// fleet whose boxes push as you necessarily has a credential here that can say who you are.
    /// Reported from a live fleet: `skein doctor` showing `gh secret seeded` and `boxes push with
    /// this account's gh token` three lines above `github token none`, with every pull request
    /// queue answering 502.
    GhCli,
    /// Nothing. The queue says so instead of reporting an empty queue, which is the one failure it
    /// must never look like.
    None,
}

impl GhToken {
    pub fn label(self) -> &'static str {
        match self {
            GhToken::Environment => "$GH_TOKEN",
            GhToken::ReadToken => "the read token in Settings",
            GhToken::WritePat => "a repository write token you stored",
            GhToken::GhCli => "the host's `gh` login",
            GhToken::None => "no token at all",
        }
    }
}

/// The token and where it came from, resolved **once per process**.
///
/// An App is deliberately absent from this list. An installation token authenticates an
/// installation, not a person, so it cannot answer "whose review is this waiting on" — the queue's
/// whole question. That limit is the App's, and saying so beats falling back to something that
/// half-works.
fn host_credential() -> (GhToken, Option<String>) {
    let mut slot = match GH_TOKEN.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.get_or_insert_with(|| {
        for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
            if let Ok(value) = std::env::var(key) {
                if !value.trim().is_empty() {
                    return (GhToken::Environment, Some(value.trim().to_string()));
                }
            }
        }
        if let Some(pat) = crate::gitgate::read_pat() {
            return (GhToken::ReadToken, Some(pat));
        }
        // Any write PAT they stored. It belongs to a person, so it can say who that person is —
        // which is the whole of what this needs.
        if let Some(pat) = crate::gitgate::any_user_pat() {
            return (GhToken::WritePat, Some(pat));
        }
        // Last, and last for a reason rather than by accident: `gh` keeps its token in the system
        // keyring on a modern Linux, so asking can unlock one — which is why skein's own startup
        // stopped asking once the fleet secret was seeded. Every source above costs nothing, so
        // this is reached only by a host that would otherwise have no credential at all, and the
        // answer is remembered for the life of the process.
        if let Some(token) = crate::repos::gh_cli_token() {
            return (GhToken::GhCli, Some(token));
        }
        (GhToken::None, None)
    })
    .clone()
}

/// Which credential the host's GitHub calls are running on, for the places that report it.
pub fn host_token_source() -> GhToken {
    host_credential().0
}

/// The token itself, or the sentence to show instead of an empty queue.
///
/// Public because the workflow tick acts as you — a label, a merge, a deleted branch are all things
/// GitHub attributes to whoever's credential asked. There is deliberately no second, quieter
/// credential for automation: everything skein does on its own is done as you, and shows up in the
/// repository's history under your name where you can see it.
pub fn host_token() -> Result<String, String> {
    host_credential().1.ok_or_else(|| {
        "no GitHub token: the review queue reads pull requests as you, and nothing here names a \
         user. Any of these does it — `gh auth login` on the host, exporting GH_TOKEN, or a read \
         token in Settings → GitHub & keys. A GitHub App cannot: an installation token is not a \
         person."
            .to_string()
    })
}

/// **A per-process memo here holds only what GitHub actually said.**
///
/// The rule, and the reason it is a function rather than a comment. Two lookups below are
/// remembered for the life of the process because their answers change about once a year and the
/// queue is polled from a board: [`renamed_to`] and [`trunk_of`]. Both used to swallow a failure
/// into a sentinel — `None` for "not renamed", `""` for "trunk unknown" — and then cache the
/// sentinel exactly as they would cache an answer.
///
/// That is worse than it sounds, because of WHEN it happens. `crate::github::call` refuses every
/// request while the rate-limit hold is engaged, without asking GitHub at all, and the hold is
/// engaged by the GraphQL search that runs *before* both of these inside [`queue_within`]. So one
/// rate-limited refresh does not fail these lookups: it fills them, permanently, with answers
/// nobody was given. The limit lifts, the queue comes back, and skein goes on believing a thing it
/// was never told until somebody restarts it. Nothing logs, nothing shows a blind spot, and there
/// is nothing for a person to clear because nothing says it is there. Found three times — the
/// merge train dead until a restart (SKEIN-238), a renamed repo reading as an empty queue
/// (SKEIN-281), and once avoided on purpose in `crate::prwork::facts_of`, where an unknown trunk
/// is `None` rather than "not the trunk" so that blindness cannot become a stop.
///
/// So: `ask` returns `Ok` only when GitHub answered. An `Err` — a refusal, a hold, a missing
/// token, a body that did not carry the field — is returned to the caller and **not written down**,
/// so the next refresh asks again. During a hold that retry costs nothing: it is refused before it
/// is spent, in exactly the condition that produces it.
///
/// Anything else remembered per process from a GitHub answer belongs here too. Reading a local
/// credential does not ([`host_credential`]): "no token at all" is a real answer, it is reported as
/// one, and no rate limit can manufacture it.
fn what_github_said<T: Clone>(
    memo: &std::sync::Mutex<std::collections::BTreeMap<String, T>>,
    slug: &str,
    ask: impl FnOnce() -> Result<T, String>,
) -> Option<T> {
    // Poison-tolerant like the hold in `crate::github`: the value is a cached answer, and there is
    // no invariant a panicking caller could have left half-written.
    let mut seen = match memo.lock() {
        Ok(seen) => seen,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(known) = seen.get(slug) {
        return Some(known.clone());
    }
    let answer = ask().ok()?;
    seen.insert(slug.to_string(), answer.clone());
    Some(answer)
}

/// The repository's current name when it differs from the one skein holds, else `None`.
///
/// Remembered per process like the token beside it: this is a REST round trip and the queue is
/// polled from the board, so asking per refresh would spend a call on an answer that changes about
/// once a year. Only through [`remembered`], so the thing written down is always a name GitHub
/// gave — **"GitHub says it is still called this" is an answer and is cached; "GitHub would not
/// tell me" is not.** The two used to be the same `None`, and on the one repo the owner had the
/// queue switched on for — renamed, with the old slug still in the registry — a single refused
/// lookup meant the search kept asking `repo:<the old name>` and the queue read empty until a
/// restart. GitHub's *search* does not follow a rename the way its REST redirect does, which is
/// what `crate::github::canonical_repo` exists to read.
fn renamed_to(slug: &str) -> Option<String> {
    what_github_said(&RENAMES, slug, || {
        let token = host_token()?;
        let now = crate::github::canonical_repo(slug, &token)?;
        // The name GitHub gave, and `None` when that is the name skein already holds. This `None`
        // is an ANSWER — it is inside the `Ok`, so it is remembered.
        Ok(Some(now).filter(|now| !now.eq_ignore_ascii_case(slug)))
    })
    .flatten()
}

static RENAMES: std::sync::Mutex<std::collections::BTreeMap<String, Option<String>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Forget the resolved names, for tests and for a caller that has just been told one changed.
pub fn forget_renames() {
    if let Ok(mut seen) = RENAMES.lock() {
        seen.clear();
    }
}

/// The repository's default branch — `main`, `master`, whatever the repo says — for
/// [`Queue::trunk`].
///
/// Remembered per process for [`renamed_to`]'s reason: this is a REST round trip whose answer
/// changes about never, asked from a poll. Through [`what_github_said`], so only a branch GitHub named
/// is written down — a refusal is not, and the next refresh asks again (SKEIN-238: cached, one
/// rate-limited refresh made `base_is_trunk` false on every pull request in the repo, the merge
/// train's `base:trunk` claimed nothing, and the train was dead until a restart).
///
/// `""` for a repo whose trunk skein has not been told, which is [`Queue::trunk`]'s contract for
/// "not known" — and the empty string is never what gets remembered, because a body with no
/// `default_branch` is an `Err` here rather than an answer.
///
/// **`pub` for the merge a person presses** (SKEIN-338). [`crate::prwork::merge_by_hand`] needs the
/// trunk and must not refresh the queue to get it, and this is already the memoised answer the
/// queue itself uses — so both roads to a merge read the repository's default branch from the same
/// place and cannot disagree about what the trunk is.
pub fn trunk_of(slug: &str) -> String {
    what_github_said(&TRUNKS, slug, || {
        let token = host_token()?;
        let repo = crate::github::get_json(&format!("/repos/{slug}"), &token)?;
        repo.get("default_branch")
            .and_then(|b| b.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("GitHub did not say what {slug}'s default branch is"))
    })
    .unwrap_or_default()
}

static TRUNKS: std::sync::Mutex<std::collections::BTreeMap<String, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Forget the resolved trunks — for tests, like [`forget_renames`] above.
pub fn forget_trunks() {
    if let Ok(mut seen) = TRUNKS.lock() {
        seen.clear();
    }
}

/// The one resolution, remembered. A `Mutex<Option<_>>` rather than a `OnceLock` so a test can
/// forget it; the outer `Option` is "have we looked yet".
static GH_TOKEN: std::sync::Mutex<Option<(GhToken, Option<String>)>> = std::sync::Mutex::new(None);

/// Forget it, so the next call resolves again.
///
/// Public because `tests/review_queue.rs` is a separate crate and points skein at a different stub
/// per test: a token resolved once for the process would be the first test's, in every test.
pub fn forget_host_token() {
    if let Ok(mut slot) = GH_TOKEN.lock() {
        *slot = None;
    }
}

/// The GitHub repository a managed repo maps to, as `owner/name`.
///
/// One resolver, in [`crate::gitgate`], because the queue and the write token must agree on what repo
/// this is. They did not: this module fell back to the clone's `origin` while the token path read
/// only `repo.source`, so an adopted-in-place repo had a review queue *and* no way to push to the
/// repository that queue was listing.
pub fn repo_slug(repo: &Repo) -> Option<String> {
    crate::gitgate::repo_slug(repo)
}

/// The repository a WRITE should address, derived without refreshing the queue. (SKEIN-272)
///
/// The post path used to take its slug from `queue(repo, false)`, which is a full refresh past its
/// sixty-second cache: the viewer lookup, the rename check, and the five membership searches in one
/// request. So pressing "post comments" more than a minute after the last refresh inherited every
/// way a GitHub *read* can fail, and the `?` on that line turned "skein could not re-read your
/// queue" into "your review was not posted" — reported in the refresh's own words, which name a
/// repository and five membership searches nobody asked about. Reported live: the owner posted by
/// hand instead.
///
/// So the slug is derived from what a write actually needs. The remote is read from the checkout,
/// which is local and cannot fail over the network. The rename is followed because a POST to a
/// stale name is not redirected the way a GET is — but [`renamed_to`] is memoised per process and
/// answers `None` when GitHub cannot be asked, so a read that fails costs the stored name and never
/// the post. The only error left is the one that is genuinely about posting: there is nowhere to
/// post to.
pub fn slug_for_write(repo: &Repo) -> Result<String, String> {
    let stored = repo_slug(repo)
        .ok_or("this repo has no GitHub remote, so there is no pull request to post to")?;
    match renamed_to(&stored) {
        Some(now) => {
            // Best-effort, exactly as in [`queue_within`]: a rename skein cannot write down is one
            // it looks up again next time, which is not a reason to lose a review.
            let _ = crate::repos::follow_rename(&repo.id, &stored, &now);
            Ok(now)
        }
        None => Ok(stored),
    }
}

// ───────────────────────────── viewer identity ─────────────────────────────

/// Your GitHub login, and the teams you belong to **when GitHub would say** — `None` when it
/// would not.
///
/// Teams are best-effort: `user/teams` needs `read:org`, which a perfectly good login may lack.
/// The two outcomes used to be the same empty list, and they are different facts (SKEIN-262):
/// *"you are in no teams"* means the team rules that exist have all been asked, and *"GitHub would
/// not tell me"* means a whole class of membership was never asked about at all. Only the second
/// makes the queue's open list incomplete, and only the second earns the `read:org` sentence — a
/// solo account was getting a permanent instruction to fix something that was not broken.
///
/// This is [`what_github_said`]'s rule in the shape a `Result` inside a `Result` would give: an
/// empty list is an ANSWER and is used as one; a refusal is not turned into one.
pub fn viewer() -> Result<(String, Option<Vec<String>>), String> {
    let token = host_token()?;
    let user = crate::github::get_json("/user", &token).map_err(|e| {
        // Which credential this ran on, and every way to change it. The queue is about *your* pull
        // requests, so it needs a token that names a user — and the answer used to be "run
        // `gh auth login`", as if that were the only one.
        format!(
            "GitHub could not identify you from {}: {e}. The review queue needs a token that names \
             a user — export GH_TOKEN, or add a read token in Settings → GitHub & keys (a PAT of \
             your own, which is what a fleet on the PAT path already has).",
            host_token_source().label()
        )
    })?;
    let login = user
        .get("login")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if login.is_empty() {
        return Err("GitHub returned no login for this token".into());
    }
    // Best-effort, and the failure is returned AS a failure. A body that is not an array is the
    // same non-answer as a 403: neither is GitHub telling us the list is empty.
    let teams = crate::github::get_json("/user/teams?per_page=100", &token)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|teams| {
            teams
                .iter()
                .filter_map(|t| {
                    let org = t.get("organization")?.get("login")?.as_str()?;
                    let slug = t.get("slug")?.as_str()?;
                    Some(format!("{org}/{slug}"))
                })
                .collect()
        });
    Ok((login, teams))
}

// ───────────────────────────── the archive ─────────────────────────────

/// Where a repo's review state lives: `~/.skein/review/<repo-id>/`.
///
/// Host-side and private, never the repo and never the shared `.claude` store — that store is
/// mounted into every box for the repo, and skein's rule is that runtime state and caches do not go
/// there. It is also the answer you gave for module docs: private first.
pub fn review_dir(repo_id: &str) -> PathBuf {
    skein_home().join("review").join(repo_id)
}

fn archive_path(repo_id: &str) -> PathBuf {
    review_dir(repo_id).join("archived.json")
}

/// PR numbers you have set aside in this repo.
pub fn archived(repo_id: &str) -> Vec<u64> {
    fs::read_to_string(archive_path(repo_id))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<u64>>(&t).ok())
        .unwrap_or_default()
}

/// Archive or unarchive one PR. Idempotent in both directions.
pub fn set_archived(repo_id: &str, number: u64, on: bool) -> Result<(), String> {
    let mut list = archived(repo_id);
    let had = list.contains(&number);
    match (on, had) {
        (true, false) => list.push(number),
        (false, true) => list.retain(|n| *n != number),
        _ => return Ok(()),
    }
    write_archive(repo_id, &list)
}

fn write_archive(repo_id: &str, list: &[u64]) -> Result<(), String> {
    let dir = review_dir(repo_id);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    write_atomic(&archive_path(repo_id), &dir, &bytes)
}

fn snooze_path(repo_id: &str) -> PathBuf {
    review_dir(repo_id).join("snoozed.json")
}

/// PRs set aside *until their head moves*: number → the head sha it was set aside at.
///
/// A second store beside [`archived`] rather than a flag on it, because the two end differently
/// and mixing them loses the ending: an archive holds until a human undoes it, a snooze holds
/// until the BRANCH answers — the next push is the author acting on the red the snooze was
/// waiting out, which is exactly the moment the row should return by itself (SKEIN-144). The sha
/// is what makes that automatic: an entry whose sha no longer matches the open PR's head is
/// simply ignored, so un-snoozing needs no poller and no act.
pub fn snoozed(repo_id: &str) -> BTreeMap<u64, String> {
    fs::read_to_string(snooze_path(repo_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Snooze one PR at a head, or (`None`) bring it back by hand. Idempotent, like [`set_archived`]:
/// a retried request must not flip a row back out of where you already moved it.
///
/// The ordinary ending is nobody calling the `None` arm at all — a push stops the sha matching
/// and the row returns on its own.
pub fn set_snoozed(repo_id: &str, number: u64, head_sha: Option<&str>) -> Result<(), String> {
    let mut map = snoozed(repo_id);
    let changed = match head_sha {
        // An empty sha would hide the row forever on a PR whose head GitHub did not report —
        // build_pr refuses to match it, so refusing to store it keeps the file free of dead weight.
        Some(sha) if !sha.is_empty() => map.insert(number, sha.to_string()).as_deref() != Some(sha),
        _ => map.remove(&number).is_some(),
    };
    if !changed {
        return Ok(());
    }
    write_snoozed(repo_id, &map)
}

fn write_snoozed(repo_id: &str, map: &BTreeMap<u64, String>) -> Result<(), String> {
    let dir = review_dir(repo_id);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(map).map_err(|e| e.to_string())?;
    write_atomic(&snooze_path(repo_id), &dir, &bytes)
}

// ───────────────────────────── fetching ─────────────────────────────

/// 60s micro-cache **per repo**, for the same reason [`crate::repos::REPOS_CACHE`] exists: the
/// cockpit re-renders far more often than GitHub changes, and each fetch is three network round
/// trips.
///
/// Keyed by repo rather than holding one entry, because the badge poller walks every repo that has
/// the queue switched on. A single slot would let each repo evict the last one and turn a cache
/// into a guaranteed miss — the exact opposite of what it is for.
static QUEUE_CACHE: Mutex<Option<HashMap<String, (Instant, Queue)>>> = Mutex::new(None);

/// Repos with a background refresh already in flight, so [`merged`] never runs two at once for
/// one repo (SKEIN-206). A `Vec` because `Mutex::new(Vec::new())` is const and the fleet has
/// single-digit repos — a set would buy nothing.
static REFRESHING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Membership in [`REFRESHING`] for one repo, ended by `Drop` — so the slot frees on the
/// refresh thread's every exit, an errored fetch and a panic included. A slot that leaked would
/// be worse than the duplicate it prevents: that repo would never refresh in the background
/// again, and the pane would repaint yesterday's queue forever.
struct RefreshRunning(String);

impl RefreshRunning {
    /// Take the slot for `repo_id`, or `None` when a refresh is already running there.
    fn begin(repo_id: &str) -> Option<Self> {
        let mut running = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        if running.iter().any(|id| id == repo_id) {
            return None;
        }
        running.push(repo_id.to_string());
        Some(Self(repo_id.to_string()))
    }
}

impl Drop for RefreshRunning {
    fn drop(&mut self) {
        let mut running = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|id| id != &self.0);
    }
}

/// Drop one repo's cached queue — after an act that changes a PR's state, so the next read shows it.
pub fn invalidate(repo_id: &str) {
    if let Some(map) = QUEUE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        map.remove(repo_id);
    }
}

/// Build a repo's review queue. `force` skips the micro-cache.
pub fn queue(repo: &Repo, force: bool) -> Result<Queue, String> {
    // `ZERO`: nothing is younger than no time at all, so force refreshes past whatever is cached.
    let max_age = match force {
        true => Duration::ZERO,
        false => Duration::from_secs(60),
    };
    queue_within(repo, max_age)
}

/// Build a repo's review queue, serving the remembered in-process copy while it is younger than
/// `max_age`.
///
/// One cache, two budgets. [`queue`]'s sixty seconds fits a pane somebody is looking at; the badge
/// poller passes ten minutes, because a badge is a number acted on within minutes and every refresh
/// behind it is a GitHub round trip **per repo, per open tab, every three minutes** — the
/// steady-state spend that got the owner rate-limited, back when each refresh was five separate
/// GraphQL searches rather than [`search_prs_all`]'s one.
pub fn queue_within(repo: &Repo, max_age: Duration) -> Result<Queue, String> {
    if queues_are_cached() {
        if let Some(young) = unexpired_within(&repo.id, max_age) {
            return Ok(young);
        }
    }
    let stored = repo_slug(repo)
        .ok_or("this repo has no GitHub remote, so it has no pull requests to review")?;
    let (login, teams) = viewer()?;
    // `None` is "GitHub would not say", which is a different fact from "you are in no teams" — see
    // [`viewer`]. Everything below reads the list; only the prunes read the difference.
    let teams_unknown = teams.is_none();
    let teams = teams.unwrap_or_default();
    let mut blind_spots = Vec::new();
    // What this repository is called NOW. A name is not an identifier: `acme/gadget-demo`
    // became `acme/thing`, and because GitHub's search matches a stale name against nothing
    // — HTTP 200, zero results, no error — the queue rendered empty while twenty-three pull
    // requests waited on a review. An empty queue is the one thing this module must never be able
    // to show by accident.
    //
    // Written back into the repo, not just used here. Everything else keyed on the slug follows it:
    // `gitgate`'s per-repo write credentials, the mirror's origin, what a box may push to.
    let slug = match renamed_to(&stored) {
        Some(now) => {
            if let Err(why) = crate::repos::follow_rename(&repo.id, &stored, &now) {
                blind_spots.push(format!(
                    "{stored} is now {now}, and skein could not record that ({why}) — it will look \
                     it up again every time until it can"
                ));
            }
            now
        }
        None => stored,
    };
    if teams_unknown {
        // Short, and it names the cure. A warning that cannot be acted on is shown on every load
        // forever, and a banner that is always there stops being read — so the fix belongs in the
        // sentence, not in documentation somewhere behind it.
        //
        // On `teams_unknown` rather than on an empty list: an account that is genuinely in no teams
        // was being told, on every refresh for ever, to fix a scope that was not the problem.
        blind_spots.push(
            "team review requests are missing — `gh` cannot list your teams. Fix: gh auth refresh -s read:org"
                .into(),
        );
    }

    // One query per membership rule — GitHub's search cannot express the union, and a client-side
    // filter over every open PR would be far more expensive on a busy repo than these narrow
    // searches. They all travel in ONE GraphQL request (`search_prs_all`), aliased q0..qN in this
    // order — which is also the Reason precedence order, because the merge below keeps reasons in
    // the order the searches answered.
    let mut searches: Vec<(String, Reason)> = vec![
        (format!("review-requested:{login}"), Reason::Reviewer),
        // Both, because GitHub moves a PR from one to the other the moment you submit any review
        // — see [`Reason::Reviewed`]. Without the second, acting on your queue empties it.
        (format!("reviewed-by:{login}"), Reason::Reviewed),
        (format!("author:{login}"), Reason::Author),
        (format!("mentions:{login}"), Reason::Mentioned),
    ];
    for team in &teams {
        searches.push((
            format!("team-review-requested:{team}"),
            Reason::Team(team.clone()),
        ));
    }

    let archived_numbers = archived(&repo.id);
    let snoozed_shas = snoozed(&repo.id);
    let mut prs: Vec<Pr> = Vec::new();
    let texts: Vec<String> = searches.iter().map(|(s, _)| s.clone()).collect();
    // Does this refresh know what is open?
    //
    // Every prune below deletes one of the owner's own decisions because a pull request did not
    // appear — and "did not appear" only means "is not open" when the searches actually answered.
    // A whole-request failure produces exactly the same empty list as a repo with nothing waiting,
    // so the count cannot tell them apart; the searches can, and they say so here rather than
    // leaving the prune to infer it (SKEIN-229).
    //
    // **A rule that could not be WRITTEN counts too** (SKEIN-262). Without `read:org` the loop
    // below adds no `team-review-requested:` search at all, so a pull request whose only claim on
    // you is a team review request cannot appear in this list — and the four personal searches all
    // answer, so nothing here noticed. The prunes then read that absence as "closed" and deleted
    // the owner's set-aside and its snooze, silently, on every badge poll, permanently on a fleet
    // whose token lacks the scope (SKEIN-239). A search that failed and a search that was never
    // possible are different things and the same hole.
    let mut answered = !teams_unknown;
    // A whole-request failure — the network, a 5xx, the rate-limit hold — is every search failing
    // at once, and it is said ONCE.
    //
    // It used to be mapped onto each rule, so one dead request printed five near-identical alarms.
    // The owner saw exactly that on a cold load: five lines that read as five broken things, none
    // of which said the two facts a reader needs — that it was one failure, and that it took the
    // whole refresh with it (SKEIN-258). A per-ALIAS failure keeps its own sentence in the loop
    // below, because "which membership went dark" is real information there and the batching was
    // careful to keep it answerable.
    let outcomes = match search_prs_all(&slug, &texts) {
        Ok(outcomes) => outcomes,
        Err(e) => {
            answered = false;
            let n = searches.len();
            blind_spots.push(match crate::github::connection_died(&e) {
                // Said as the transport failure it is (SKEIN-271). "GitHub did not answer" sends
                // whoever reads it to look at GitHub — at a token, a rate limit, a refusal — and a
                // connection that died is not GitHub answering anything. The two ask different
                // things of a reader: this one says the request never completed, so ask again.
                // Skein already has, once, by the time this line is written.
                true => format!(
                    "{e} — and again when skein asked a second time, so all {n} of this \
                     refresh's membership searches for {slug} are missing; they travel in one \
                     request, so this is one failure and not {n}"
                ),
                false => format!(
                    "GitHub did not answer for {slug}, so all {n} of this refresh's membership \
                     searches are missing — they travel in one request, so this is one failure \
                     and not {n}: {e}"
                ),
            });
            Vec::new()
        }
    };
    // **A repo that is being asked in narrow batches says so** (SKEIN-278). The narrowing is
    // adaptive and invisible from the outside: the queue looks identical whether it cost one
    // request or four, so a repository that has quietly become expensive to refresh would never be
    // anything the owner could read. It is not a blind spot in the completeness sense — every
    // search still answered — which is exactly what this list is for beside `whole`.
    //
    // It ends itself. The memo behind it holds a width GitHub ANSWERED and expires, so the sentence
    // is gone the refresh after the wide request works again; nothing needs clearing by hand.
    if let Some(width) = answered_batch_width(&slug).filter(|w| *w < searches.len()) {
        blind_spots.push(format!(
            "{slug}'s {} membership searches are being asked {width} at a time — GitHub would not \
             answer them in one request, so every refresh of this repo costs more than one. Skein \
             tries the single request again within the hour.",
            searches.len()
        ));
    }
    for ((search, reason), outcome) in searches.iter().zip(outcomes) {
        let found = match outcome {
            Ok(found) => found,
            Err(e) => {
                answered = false;
                blind_spots.push(format!(
                    "the `{search}` query failed, so those PRs are missing: {e}"
                ));
                continue;
            }
        };
        // A search cut off at the page is an answer about what it returned and no answer at all
        // about what it did not reach, which is the half the prune reads.
        //
        // And it is said out loud (SKEIN-231). Silence here is the rename bug in a quieter form —
        // HTTP 200, a plausible list, nothing wrong to see — except that instead of an empty queue
        // it shows a queue that looks complete. The pull requests past the page are absent from the
        // rows, absent from the badge, and until this line nothing anywhere said a number had been
        // cut off. GitHub is asked how many it matched, so the sentence can carry the size of the
        // hole rather than only its existence.
        //
        // The count it says out loud is what actually ARRIVED, not `SEARCH_PAGE` (SKEIN-280). The
        // sentence used to name the page size because the page was all a refresh ever read; now it
        // follows the cursor, so a rule that is still short after five pages has read five hundred
        // and saying "the first 100" would understate its own queue by four hundred pull requests.
        let read = found.items.len();
        if !found.whole {
            blind_spots.push(match found.matched {
                Some(n) => format!(
                    "the `{search}` query matched {n} pull requests and skein read {read} of them \
                     — the rest are missing from this queue and from its count"
                ),
                None => format!(
                    "the `{search}` query filled every page skein followed ({read} pull requests) \
                     and GitHub says there are more — they are missing from this queue"
                ),
            });
        }
        answered &= found.whole;
        for item in found.items {
            let Some(number) = item.get("number").and_then(|v| v.as_u64()) else {
                continue;
            };
            if let Some(existing) = prs.iter_mut().find(|p| p.number == number) {
                if !existing.reasons.contains(reason) {
                    existing.reasons.push(reason.clone());
                }
                continue;
            }
            // A rollup whose contexts were cut off AND whose verdict GitHub did not give is the one
            // case [`rollup`] cannot answer from either source, so it says "pending" — and this is
            // the sentence that stops that reading as "CI is still running" (SKEIN-232). Said only
            // then: where GitHub gave its `state`, the cap costs the row a NAME and nothing else,
            // and a blind spot for every matrix build would be noise over a verdict that is right.
            if truncated_rollup(&item) && rollup_state_missing(&item) {
                blind_spots.push(format!(
                    "#{number}'s checks: GitHub listed {} contexts, skein read {}, and no rollup \
                     verdict came with them — so its checks read `pending` rather than a colour \
                     nothing here can stand behind",
                    rollup_total(&item).unwrap_or_default(),
                    item.get("statusCheckRollup")
                        .and_then(|v| v.as_array())
                        .map(|c| c.len())
                        .unwrap_or_default(),
                ));
            }
            let pr = build_pr(
                &item,
                number,
                &login,
                &repo.id,
                reason,
                &archived_numbers,
                &snoozed_shas,
            );
            // A label list cut off at [`LABELS_FETCHED`] (SKEIN-373). Said out loud for the same
            // reason the truncated rollup above is: the row would otherwise show a set of labels
            // that looks like the whole set, and `workflow::Cond::NoLabel` would answer a question
            // about a label skein never received. The sentence names what it costs, because the
            // cost is not a missing chip — it is a merge train that will not act, on purpose,
            // rather than acting on a hole.
            if !pr.labels_whole() {
                blind_spots.push(format!(
                    "#{number}'s labels: GitHub says it has {}, skein read {} — so no workflow \
                     condition of the form `no-label:` holds on this pull request, and a label \
                     past the {LABELS_FETCHED}th is not on its row",
                    pr.labels_total.unwrap_or_default(),
                    pr.labels.len(),
                ));
            }
            // A review list cut off at [`REVIEWS_FETCHED`] (SKEIN-386). The third truncation said
            // out loud here, and the one whose cost is hardest to see from the row: unlike the
            // labels above, nothing acts WRONGLY on it — every reader of these two connections errs
            // towards holding, so a review skein never saw cannot promote a lane or authorise a
            // merge. What it takes away is the reading of the row. "none" and a standing-approval
            // count of nought are what a pull request nobody has looked at shows, and without this
            // sentence they are also what a pull request thirty-one people reviewed shows.
            if !pr.reviews_whole() {
                blind_spots.push(format!(
                    "#{number}'s reviews: GitHub counted {} and skein read {} — so this row's \
                     `my review` and its count of standing approvals are floors rather than \
                     answers, and a reviewer past the {REVIEWS_FETCHED}th is invisible to skein \
                     and to any workflow reading it",
                    pr.reviews_total.unwrap_or_default(),
                    pr.reviews_read.unwrap_or_default(),
                ));
            }
            prs.push(pr);
        }
    }

    newest_first(&mut prs);

    // An archived PR that is no longer open cannot be in this list, so its entry is dead weight.
    // Pruning is safe in the direction that matters: if a PR is ever reopened it comes back
    // unarchived, which is *more* of your attention, not less.
    //
    // Safe in that direction only once `answered` holds. A refresh that went dark has an empty
    // list too, and reading it as "nothing is open" rewrote both files to nothing — fleet-wide,
    // because the badge poll runs this for every repo every three minutes, so one rate-limit
    // window erased every set-aside and every snooze the owner had (SKEIN-229). A queue that
    // genuinely has nothing open still prunes: it answered.
    let open: Vec<u64> = prs.iter().map(|p| p.number).collect();
    if answered && archived_numbers.iter().any(|n| !open.contains(n)) {
        let kept: Vec<u64> = archived_numbers
            .into_iter()
            .filter(|n| open.contains(n))
            .collect();
        let _ = write_archive(&repo.id, &kept);
    }

    // A snooze ends itself. An entry stops matching the moment the PR closes or its head moves,
    // and from then on it is dead weight that could only ever do harm — a branch reverted to the
    // old sha would re-hide a row nobody asked to hide. Kept only while the sha still names an
    // open PR's current head. Same safety direction as the archive prune above: this can only
    // ever DROP a hold, which returns a row, which is more of your attention rather than less.
    let live = |n: &u64, sha: &String| prs.iter().any(|p| p.number == *n && &p.head_sha == sha);
    if answered && snoozed_shas.iter().any(|(n, sha)| !live(n, sha)) {
        let kept: BTreeMap<u64, String> = snoozed_shas
            .into_iter()
            .filter(|(n, sha)| live(n, sha))
            .collect();
        let _ = write_snoozed(&repo.id, &kept);
    }

    // Looked up during the refresh, so an answer served from the cache never pays for it — and
    // the lookup itself is remembered per process besides.
    let trunk = trunk_of(&slug);
    let q = Queue {
        repo_id: repo.id.clone(),
        slug,
        trunk,
        viewer: login,
        ai: crate::review::summaries_enabled(),
        prs,
        blind_spots,
        as_of: chrono::Utc::now().to_rfc3339(),
        fresh: true,
        whole: answered,
    };
    if queues_are_cached() {
        QUEUE_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(repo.id.clone(), (Instant::now(), q.clone()));
        // And to disk, so the tab has something to paint after a server restart. The in-process
        // cache is the fast path; this is the one that means "cold" does not mean "blank".
        remember(&q);
    }
    Ok(q)
}

/// Does [`queue_within`] use the caches it is written around?
///
/// Always, except in this crate's own unit tests, where it is off unless a test has asked for it
/// with [`CachedQueues`]. Note the narrowness: `cfg(test)` is set only when the library is
/// compiled as its own test binary, so every integration test in `tests/` already runs against
/// the real thing.
///
/// **Why it is off by default** — the cache is a process-global `static` and unit tests share one
/// process, so a queue built by one test would be served to another under the same repo id, and
/// a test that never mentions caching would fail because of one that does.
///
/// **Why it can be switched on** (SKEIN-314). Off unconditionally, no test could put a queue that
/// is OLD in front of a caller, so three stated defences had nothing that could fail on them: the
/// `invalidate` after a workflow acts (`crate::prwork::sweep`), the 60s-vs-600s split between the
/// pane and the badge poll, and anything else that turns on a queue being stale rather than
/// absent. Each was pinned by reading the source instead — which catches a call being deleted and
/// nothing about whether it works.
fn queues_are_cached() -> bool {
    #[cfg(not(test))]
    {
        true
    }
    #[cfg(test)]
    {
        CACHE_UNDER_TEST.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Whether this crate's unit tests have asked for the queue cache. See [`CachedQueues`].
#[cfg(test)]
static CACHE_UNDER_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Test-only: [`queue_within`] caches exactly as it does in production, for the life of this
/// guard, and a queue can be planted in the cache already old (SKEIN-314).
///
/// **A guard rather than a pair of functions**, because the thing being switched on is
/// process-global: it has to go off again on the way out, including out of a panicking test, or
/// every test that runs afterwards in this process inherits a cache it never asked for. `Drop`
/// empties [`QUEUE_CACHE`] as well as clearing the flag, so nothing this test planted can be
/// served to the next one.
///
/// **Take [`crate::testutil::env_lock`] first.** It is this crate's serialization for
/// process-global state, which is what this is, and every test that seeds a queue is setting
/// `$SKEIN_HOME` anyway.
///
/// Seeding is a method rather than a free function so it cannot be called without holding the
/// guard — planting an entry while the cache is off would be a fixture nothing reads, which is
/// the failure this whole seam exists to stop being possible.
#[cfg(test)]
pub(crate) struct CachedQueues(());

#[cfg(test)]
impl CachedQueues {
    /// Switch the cache on until this value is dropped.
    pub(crate) fn live() -> Self {
        CACHE_UNDER_TEST.store(true, std::sync::atomic::Ordering::SeqCst);
        Self(())
    }

    /// Put `q` in the cache stamped `age` ago — the one thing a test cannot otherwise do, since
    /// an `Instant` cannot be set and a real test cannot wait ten minutes.
    pub(crate) fn stamped(&self, repo_id: &str, age: Duration, q: &Queue) {
        let at = Instant::now()
            .checked_sub(age)
            .expect("this process has not been up long enough to stamp a queue that far back");
        QUEUE_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(repo_id.to_string(), (at, q.clone()));
    }
}

#[cfg(test)]
impl Drop for CachedQueues {
    fn drop(&mut self) {
        CACHE_UNDER_TEST.store(false, std::sync::atomic::Ordering::SeqCst);
        *QUEUE_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

// ───────────────────────────── what to show before the answer ─────────────────────────────

/// Where a repo's last queue is kept between runs.
fn remembered_path(repo_id: &str) -> PathBuf {
    review_dir(repo_id).join("queue.json")
}

/// Keep this queue for the next cold start. Best-effort: failing to cache is not failing.
fn remember(q: &Queue) {
    let dir = review_dir(&q.repo_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(q) {
        let _ = write_atomic(&remembered_path(&q.repo_id), &dir, &bytes);
    }
}

/// Test-only: put a queue where [`remembered`] reads it, for tests elsewhere in the crate.
///
/// [`queue_within`] neither caches nor remembers in this crate's unit tests unless one asks with
/// [`CachedQueues`], so a test's world has no remembered queue in it unless it says so — and "no
/// remembered queue" is a state a real post is almost never in, because the pane must have
/// rendered this repo for a draft to exist at all. A write path's test that wants the state it
/// will actually run in seeds it here; a test that wants the whole cache, live, takes the guard.
#[cfg(test)]
pub(crate) fn remember_for_test(q: &Queue) {
    remember(q);
}

/// The last queue read for this repo, however old — marked as not fresh.
///
/// **Whatever exists, immediately.** Opening the tab used to block on three GraphQL searches per
/// repo plus the viewer lookup, and on a cold cache — a fresh server, a repo not looked at yet,
/// any refresh past the micro-cache — it painted nothing until they all came back. The thing it was
/// being compared against was a blank panel, and last night's pull requests beat a blank panel every
/// time so long as their age is on screen.
///
/// `fresh` is forced false here rather than trusted from the file: what was written was fresh when
/// it was written, and the one thing this must never do is hand somebody an old queue that claims
/// to be current.
pub fn remembered(repo_id: &str) -> Option<Queue> {
    let text = std::fs::read_to_string(remembered_path(repo_id)).ok()?;
    let mut q: Queue = serde_json::from_str(&text).ok()?;
    q.fresh = false;
    Some(q)
}

/// The in-process copy, if one is young enough to be worth calling fresh.
///
/// Separate from [`queue`] so a caller can ask "would this cost a network round trip" without
/// taking one. That is the whole difference between painting now and painting in four seconds.
pub fn unexpired(repo_id: &str) -> Option<Queue> {
    unexpired_within(repo_id, Duration::from_secs(60))
}

/// The TTL rule itself: the in-process copy, if it is younger than `max_age`.
///
/// Its own function rather than three lines inside [`queue_within`], because until SKEIN-314 that
/// path bypassed the cache in unit tests altogether and this was the only piece a test could hold.
/// It is still the smallest statement of the rule, and [`CachedQueues`] is how a test now asks the
/// same question of `queue_within` itself.
fn unexpired_within(repo_id: &str, max_age: Duration) -> Option<Queue> {
    let cache = QUEUE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let (at, q) = cache.as_ref()?.get(repo_id)?;
    (at.elapsed() < max_age).then(|| q.clone())
}

/// The head this repo's queue last SAW for one pull request, from what is already on this machine.
/// (SKEIN-272)
///
/// **Reads nothing over the network, and never refreshes.** That is the point: it is the fallback
/// [`head_to_post_against`] uses when GitHub will not say what the live head is, and a fallback
/// that could itself fail over the network would put the read failure back on the write path this
/// exists to take it off.
///
/// Why a write wants it at all is SKEIN-230. The comparison that decides whether anything needs
/// re-anchoring is "the sha the draft was read at" against "the sha being posted against", and
/// handing the drafted sha in as its own fallback makes those two equal by construction: nothing
/// re-anchors, and vetted comments post at line numbers computed against a diff that no longer
/// exists. A remembered sha is independent evidence — possibly stale, but never the same value by
/// accident.
///
/// `None` when nothing about this pull request is remembered, which is honest: the caller then has
/// no second opinion and must say so rather than invent one.
pub fn remembered_head(repo_id: &str, number: u64) -> Option<String> {
    let known = unexpired(repo_id).or_else(|| remembered(repo_id))?;
    known
        .prs
        .iter()
        .find(|p| p.number == number)
        .map(|p| p.head_sha.clone())
}

/// Every field the queue's parser needs from one pull request — the node body every search alias
/// in [`batched_query`] shares.
///
/// GraphQL rather than REST, and not as a preference: a pull request's reviews, the commit each was
/// left against, and its check rollup are three more REST calls **per pull request**. One search
/// returns all of it for a hundred at once. It is also, underneath, exactly what `gh pr list
/// --json` did — its field names *are* these — which is why [`shape`] below is almost an identity.
///
/// **The rollup asks for GitHub's own verdict as well as the contexts** (SKEIN-232). `contexts` is
/// capped at a hundred and a matrix build (`os × rust-version × feature`) reaches three digits
/// routinely, so a verdict computed only from that array reads a pull request whose 101st context
/// is red as green — and `docs/pr-workflow.md`'s merge train reads exactly that field, so the
/// failure is not a wrong dot but a merge of a pull request whose CI failed. `state` is GitHub's
/// answer over ALL of them and costs nothing to ask for; `totalCount` says how much of the list
/// this page is. Both are read by [`rollup`]; the contexts are left to NAME what failed.
/// How many review threads one pull request contributes to the batched answer.
///
/// A cap on a list that has no natural end, and it is the SIZE of this request that sets it, not
/// taste: [`PR_FRAGMENT`] is asked for up to [`SEARCH_PAGE`] pull requests per membership rule, so
/// every thread here is multiplied by a hundred. Twenty is more open threads than a reviewable pull
/// request has, and [`Pr::review_threads_total`] carries GitHub's own count beside them so a list
/// that IS short says so rather than reading as "nothing open".
///
/// Threads are cheap only because they carry no bodies — see [`ReviewThread`]. Ten, not twenty:
/// the measurement in
/// `the_conversation_is_measured_against_the_answer_it_grew_from` is what set it.
const REVIEW_THREADS_FETCHED: usize = 10;

/// How many PR-level comments one pull request contributes. **These carry bodies**, so this is the
/// expensive cap and it is deliberately the smallest one.
///
/// The LAST five, not the first five: a conversation is read from its end. The item this came from
/// names the case exactly — a pull request with four hundred comments must not be the thing that
/// makes the queue slow — and without a cap that PR would ship its whole history inside a request
/// that already carries ninety-nine others. [`Pr::comments_total`] carries GitHub's own count
/// beside the five, so a conversation that was cut says how much of it is missing.
///
/// **Five, not ten (SKEIN-316).** This is the first lever that item names, and it was pulled on a
/// measurement rather than on taste:
/// `the_conversation_is_measured_against_the_answer_it_grew_from` prints the worst case the caps
/// allow — [`SEARCH_PAGE`] pull requests saturating this and [`REVIEW_THREADS_FETCHED`] — and ten
/// put it at **583,887 bytes for ONE alias**, roughly 2.9 MB for a five-alias batch, on the same
/// request `acme/thing` answers with a 504 (SKEIN-278). Five puts it at **450,827**, and
/// that test now holds a ceiling rather than only printing the number. It is this cap and not
/// [`REVIEW_THREADS_FETCHED`] because a comment node carries a body and a thread node deliberately
/// does not (SKEIN-301) — at the 120-character body that measurement uses they are already 264
/// bytes against 226, and a real comment body is several times that, so the gap this closes is
/// wider in the fleet than in the fixture.
const PR_COMMENTS_FETCHED: usize = 5;

/// How many outstanding review requests are listed. People and teams together; a pull request
/// waiting on more than this many reviewers is not a row anybody reads a list of names off.
const REVIEW_REQUESTS_FETCHED: usize = 20;

/// How many labels one pull request contributes (SKEIN-373).
///
/// **The number is unchanged; what changed is that hitting it is now audible.** The query asked
/// `labels(first: 20)` with no `totalCount`, so a pull request with more had the rest deleted on
/// the way in and nothing — not the row, not the blind spots, not [`Pr`] — could tell. Measured
/// against `acme/testbed#20`: GitHub's REST answer carries 22 labels, the queue
/// payload carried 20 (`area/mod-01` … `area/mod-20`). That is not cosmetic, because
/// [`Pr::labels`] is what `prwork::facts_of` turns into `workflow::Facts::labels`, and
/// `workflow::Cond::NoLabel` then read a `hold` label that sorted past the twentieth as *absent*.
///
/// **Twenty rather than GitHub's hundred, and that is a measurement rather than a taste.**
/// [`PR_FRAGMENT`] travels once per pull request for up to [`SEARCH_PAGE`] of them per membership
/// rule, in the one request `acme/thing` already answers with a 504 (SKEIN-278).
/// `the_conversation_is_measured_against_the_answer_it_grew_from` saturates this cap along with
/// the other two and holds the total under half a megabyte per alias: at twenty the worst case the
/// caps allow leaves single-digit thousands of bytes of headroom under that ceiling, so a hundred
/// would not fit and no rearrangement of the other caps makes it fit. The fix is therefore the
/// same one [`SEARCH_PAGE`] took (SKEIN-231) — ask GitHub how many there were, carry the number,
/// and say the hole out loud — and not a bigger page.
const LABELS_FETCHED: usize = 20;

/// How many reviews one pull request contributes, on **each** of the two review connections
/// (SKEIN-386). Both are one review per author, so thirty is thirty distinct reviewers on one pull
/// request — rare, and the fleet's own repos review by area.
///
/// **The number is unchanged; what changed is that hitting it is now audible** — the same answer
/// SKEIN-373 gave the label cap, for a harder reason. The query asked `latestReviews(first: 30)`
/// with no `totalCount`, so the thirty-first reviewer's row was deleted on the way in and nothing
/// — not [`Pr::my_review`], not [`Pr::standing_approvals`], not the blind spots — could tell.
///
/// **Raising it is not available.** `the_conversation_is_measured_against_the_answer_it_grew_from`
/// holds the worst case the caps allow at 496,727 bytes for ONE alias against a 500,000-byte
/// ceiling (run it with `--nocapture`; measured 2026-08-26, after SKEIN-373 saturated the label
/// cap), on the request `acme/thing` already answers with a 504 (SKEIN-278) — and a review
/// node carries a state, a login and a commit oid, on each of two connections, for up to
/// [`SEARCH_PAGE`] pull requests. That measurement's fixture holds no review nodes at all, so the
/// sixty this cap allows per pull request are money it has not counted and the true figure is
/// ABOVE it, not below (SKEIN-398) — which only makes the case harder. So the fix is `totalCount`
/// beside the nodes, [`Pr::reviews_whole`], and a sentence a person reads, which is what
/// [`SEARCH_PAGE`] and [`LABELS_FETCHED`] both settled on before it.
const REVIEWS_FETCHED: usize = 30;

/// Built from the caps above rather than spelling them twice. A number written once in the query
/// and again in the field's doc is a number that drifts, and the thing it would drift about is how
/// much this request costs.
///
/// **Two review connections are asked for, and the second is not a duplicate of the first**
/// (SKEIN-354). `latestReviews` is the latest review per author *whatever it said*, so a note you
/// left after approving comes back as `COMMENTED` and would demote your own approval;
/// `latestOpinionatedReviews` is the latest review per author that DECIDED something, which is the
/// one question "is my review standing right now" is asking. [`my_review_state`] reads the
/// opinionated one for the verdict and the other only to know that you commented — which is a real
/// thing to know and the opinionated connection deliberately cannot say. Nothing else in skein
/// reads either, so this is thirty extra nodes on a fragment that already carries a hundred check
/// contexts, and it buys the difference between "you decided" and "you said something".
static PR_FRAGMENT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r#"
fragment PrFields on PullRequest {{
  number title url isDraft updatedAt
  headRefName headRefOid baseRefName reviewDecision mergeable mergeStateStatus
  additions deletions changedFiles
  labels(first: {labels}) {{ totalCount nodes {{ name }} }}
  author {{ login }}
  latestReviews(first: {reviews}) {{ totalCount nodes {{ state author {{ login }} commit {{ oid }} }} }}
  latestOpinionatedReviews(first: {reviews}) {{ totalCount nodes {{ state author {{ login }} commit {{ oid }} }} }}
  reviewRequests(first: {asked}) {{ totalCount nodes {{ requestedReviewer {{
    ... on User {{ login }}
    ... on Team {{ slug organization {{ login }} }}
  }} }} }}
  reviewThreads(first: {threads}) {{ totalCount nodes {{
    id isResolved isOutdated
    comments(first: 1) {{ nodes {{ author {{ login }} createdAt url }} }}
  }} }}
  comments(last: {comments}) {{ totalCount nodes {{ author {{ login }} body createdAt url }} }}
  commits(last: 1) {{ nodes {{ commit {{ committedDate statusCheckRollup {{ state contexts(first: 100) {{ totalCount nodes {{
    ... on CheckRun {{ name detailsUrl status conclusion }}
    ... on StatusContext {{ context targetUrl state }}
  }} }} }} }} }} }}
}}"#,
        asked = REVIEW_REQUESTS_FETCHED,
        threads = REVIEW_THREADS_FETCHED,
        comments = PR_COMMENTS_FETCHED,
        labels = LABELS_FETCHED,
        reviews = REVIEWS_FETCHED,
    )
});

/// The refresh's one request: `q0..qN`, each an aliased `search` over its own membership rule,
/// every alias reading the same node body through [`PR_FRAGMENT`].
///
/// Built per refresh rather than kept as a constant because `count` moves with your teams — and
/// text is all a GraphQL POST is, so there is nothing a constant would buy.
///
/// `issueCount` and `pageInfo` are asked for beside the nodes (SKEIN-231). Both are scalars on the
/// connection — they add nothing to the answer's size, which matters here more than it looks:
/// this request is already the heaviest thing skein sends, and the owner's repo has answered it
/// with a 504 (SKEIN-278). They are what turns "a hundred came back" from a guess into GitHub's
/// own statement of how many there were, and give the blind spot a number to say out loud.
///
/// `endCursor` is asked for beside `hasNextPage`, and every alias takes an `after` (SKEIN-280).
/// Knowing a search was cut off is not the same as reading the rest of it, and until this the queue
/// did the first and never the second: it said "43 are missing" on every refresh, for ever. The
/// `after` variables are declared `String` rather than `String!` because the FIRST page passes
/// `null` for every one of them — `after: null` is GraphQL's "from the beginning", so the first
/// request is byte-for-byte the request it always was apart from these declarations, and a repo
/// whose searches all fit in one page still costs exactly one request.
fn batched_query(count: usize) -> String {
    use std::fmt::Write as _;
    let mut vars = String::from("$n: Int!");
    let mut body = String::new();
    for i in 0..count {
        let _ = write!(vars, ", $q{i}: String!, $a{i}: String");
        let _ = writeln!(
            body,
            "  q{i}: search(query: $q{i}, type: ISSUE, first: $n, after: $a{i}) {{ issueCount \
             pageInfo {{ hasNextPage endCursor }} nodes {{ ...PrFields }} }}"
        );
    }
    format!("query({vars}) {{\n{body}}}\n{}", *PR_FRAGMENT)
}

/// One membership search's answer: the pull requests it returned, and whether that is all of them.
///
/// The two are separate facts because they decide different things. `items` is what fills the
/// queue. `whole` is what lets the queue act on a pull request's **absence** — and the prunes in
/// [`queue_within`] delete one of the owner's own decisions on exactly that evidence, so they may
/// only read a search that saw everything there was.
///
/// `whole` is now GitHub's answer rather than an inference: `pageInfo { hasNextPage }` says whether
/// a page is the end of the list, where "came back short of a hundred" only ever guessed it — and
/// guessed wrong, in the safe direction, on a search that matched exactly a hundred. It falls back
/// to the length test when nothing said, because an answer that predates the field is still an
/// answer. `matched` is `issueCount`: how many the search found, which is what lets the blind spot
/// in [`queue_within`] say how many pull requests are missing rather than merely that some are.
struct Found {
    items: Vec<serde_json::Value>,
    whole: bool,
    matched: Option<u64>,
    /// Where the next page of THIS search starts, from `pageInfo { endCursor }`.
    ///
    /// The only thing that can continue a search, and it is deliberately the only thing: a page is
    /// followed when GitHub both said there is more AND handed back somewhere to carry on from.
    /// An answer that says `hasNextPage` and gives no cursor — an older fixture, a shape GitHub
    /// changes under us — stops the paging rather than guessing an offset, and `whole` stays false
    /// so the blind spot still says what could not be seen.
    cursor: Option<String>,
}

/// How many pull requests one membership search asks GitHub for. A search that comes back with
/// exactly this many has been cut off at the page far more often than it has landed on it exactly.
///
/// **Not the lever for a truncated queue.** Raising it is the obvious fix for SKEIN-231 and the
/// wrong one: the answer is already the heaviest thing skein sends — five searches × this many
/// nodes × [`PR_FRAGMENT`] — and `acme/thing` answered that with a 504 that only
/// [`search_prs_all`]'s split-in-halves recovered (SKEIN-278). A bigger page makes the outage more
/// likely in order to make the truncation rarer, and an outage is the failure that hides MORE. So
/// the page stays where it is and the queue says what it could not see.
const SEARCH_PAGE: usize = 100;

/// How many pages of one membership rule a refresh will follow — the first plus this many more.
///
/// A ceiling rather than "until GitHub stops", because this runs from a poll: the badge refreshes
/// every repo every few minutes, and a rule matching four thousand open pull requests would spend
/// forty requests per repo per refresh to build a review queue no person is going to read to the
/// end of. Five pages is five hundred pull requests **per rule**, which is far past any queue the
/// owner has and still a bounded worst case.
///
/// Hitting it is not silence. The last page's `whole` is false, so [`queue_within`]'s blind spot
/// says how many were matched and how many were read — the SKEIN-231 sentence, with the hole now
/// as small as this ceiling can make it.
const SEARCH_PAGES: usize = 5;

/// Every membership search of one refresh, in ONE GraphQL request — five requests per repo per
/// refresh was where nearly all of skein's quota went (SKEIN-209) — followed to the END of any
/// rule GitHub says has more (SKEIN-280).
///
/// The outer `Result` is the request: an `Err` means nothing was asked or nothing answered, and
/// the caller must report **every** search as missing. The inner ones are per search, in the order
/// given: GraphQL delivers a failed alias as `data.qN: null` plus an `errors` entry whose `path`
/// names the alias, and that mapping is what keeps each failure its own blind spot — four good
/// answers are still four good answers, exactly as they were when each search was its own request.
///
/// **Paging is the second half of SKEIN-231, not a second mechanism.** That one taught the queue to
/// say "143 matched, I read 100"; it said it again on every refresh, for ever, because nothing ever
/// asked for the other 43. Here the cut-off rules — and ONLY those — are asked again with their own
/// `endCursor`, so a repo whose searches all fit in one page still costs exactly one request, and a
/// repo with one busy rule costs one more request rather than a bigger one. That direction matters:
/// [`SEARCH_PAGE`] argues at length that a BIGGER page is the wrong lever, because the batched
/// request is already the heaviest thing skein sends and `acme/thing` answered it with a 504.
/// A follow-up page carries one alias, so it is the smallest request in the refresh, not the
/// largest.
fn search_prs_all(slug: &str, searches: &[String]) -> Result<Vec<Result<Found, String>>, String> {
    // The widest batch GitHub answered anywhere in THIS refresh — the first request, a half after a
    // split, a follow-up page. Accumulated across the whole refresh rather than written per request
    // because the halves of a split answer narrower than the batch they came from, and a memo that
    // believed each half in turn would ratchet a repo down to one search per request (SKEIN-278).
    let widest = std::cell::Cell::new(0usize);
    let answered = search_pages(slug, searches, &widest);
    learn_batch_width(slug, widest.get(), searches.len());
    answered
}

/// The paging itself, with the refresh's widest answered batch accumulating into `widest`.
fn search_pages(
    slug: &str,
    searches: &[String],
    widest: &std::cell::Cell<usize>,
) -> Result<Vec<Result<Found, String>>, String> {
    let mut out = one_batch(slug, searches, &vec![None; searches.len()], widest)?;
    for _ in 0..SEARCH_PAGES {
        // Which rules GitHub says it has more of AND handed a cursor back for. A `hasNextPage`
        // with no `endCursor` is not a page anyone can ask for, so it ends the paging with
        // `whole` still false rather than being guessed at.
        let more: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, found)| found.as_ref().is_ok_and(|f| !f.whole && f.cursor.is_some()))
            .map(|(i, _)| i)
            .collect();
        if more.is_empty() {
            break;
        }
        let again: Vec<String> = more.iter().map(|&i| searches[i].clone()).collect();
        let after: Vec<Option<String>> = more
            .iter()
            .map(|&i| out[i].as_ref().ok().and_then(|f| f.cursor.clone()))
            .collect();
        // A page that will not come is where this stops. Everything already read stays in the
        // queue and every unfinished rule keeps `whole: false`, so the refresh degrades into
        // exactly the answer it gave before paging existed rather than into an error.
        let Ok(pages) = one_batch(slug, &again, &after, widest) else {
            break;
        };
        for (&i, page) in more.iter().zip(pages) {
            // A page that failed on its own leaves the rule where it was: partial, and saying so.
            // Its earlier pages are real pull requests and are not thrown away over a later one.
            let Ok(page) = page else { continue };
            let Ok(sofar) = out[i].as_mut() else { continue };
            sofar.items.extend(page.items);
            sofar.whole = page.whole;
            sofar.cursor = page.cursor;
            // `matched` is GitHub's count of the whole rule and is the same on every page; the
            // first page's answer is kept so a later page that omits it cannot erase the number
            // the blind spot is built from.
            sofar.matched = sofar.matched.or(page.matched);
        }
    }
    Ok(out)
}

/// One batch of searches at one set of cursors, halved and re-asked when GitHub refuses to take
/// it whole. The paging above calls this once per page.
fn one_batch(
    slug: &str,
    searches: &[String],
    after: &[Option<String>],
    widest: &std::cell::Cell<usize>,
) -> Result<Vec<Result<Found, String>>, String> {
    // **A repo that has to be asked in halves is asked in halves, without failing first**
    // (SKEIN-278). The split below recovers a refresh; it does not remember anything, so
    // `acme/thing` re-learned it by 504 on every single refresh — one wasted heavy request
    // per poll per repo, for ever, announcing itself in the fleet's log each time.
    //
    // What is remembered is a WIDTH GITHUB ANSWERED, never a refusal — the rule
    // [`what_github_said`] states for the lookups above it, and the pattern SKEIN-281 names. So the
    // memo cannot pin a repo shut over a bad minute: the worst it can say is "the last thing that
    // worked here was three searches at a time", it expires ([`BATCH_WIDTH_LIFE`]) so the wide
    // batch is tried again, and [`forget_batch_widths`] clears it by hand.
    if searches.len() > 1 && answered_batch_width(slug).is_some_and(|w| searches.len() > w) {
        return Ok(split_in_two(slug, searches, after, widest));
    }
    match one_request(slug, searches, after) {
        Ok(found) => {
            widest.set(widest.get().max(searches.len()));
            Ok(found)
        }
        // **Too heavy is not the same as unavailable** (SKEIN-266). Batching took five requests per
        // repo down to one — and made that one the most expensive thing skein sends: five `search`
        // connections of up to a hundred nodes each, every node carrying the whole PR fragment.
        // GitHub sheds those at the edge, twice on the owner's fleet within an hour: once as a 200
        // with no body, once as nginx's own `502 Bad Gateway`. `github` retries such a shrug once
        // already; when the retry fails too, the batch itself is the thing to give up on, not the
        // refresh.
        //
        // So halve it and ask again. The quota win survives where it was won — one request whenever
        // one request works — and where it does not, skein spends two, or four, rather than showing
        // an empty queue over a repo full of pull requests. A single search that still fails is
        // reported as itself, which is the per-alias blind spot the batching was careful to keep.
        // Only when GitHub refused to TAKE it. An outage, a rate-limit hold or a 500 that carries
        // a real message is GitHub answering, and asking those again in halves would spend more
        // requests to be told the same thing twice — and would turn SKEIN-258's one honest
        // sentence back into five. `github::edge_refused` owns that distinction, beside the words
        // it is reading.
        Err(why) if searches.len() > 1 && crate::github::edge_refused(&why) => {
            let out = split_in_two(slug, searches, after, widest);
            // Said where a refusal actually happened, and nowhere else. It used to be said on every
            // split — which, once a repo needed splitting, was every refresh for ever: the owner
            // read this line about `acme/thing` over and over, and it was reporting skein
            // asking a question it already knew the answer to. A split skein chose from what it
            // learned is not news; a refusal it had not seen coming is.
            eprintln!(
                "skein: GitHub would not take {slug}'s {} searches in one request ({why}) — asked \
                 in two, and the next refresh will start there",
                searches.len()
            );
            Ok(out)
        }
        Err(why) => Err(why),
    }
}

/// Ask the same searches as two narrower batches. Each half goes back through [`one_batch`], so a
/// half GitHub also refuses splits again, and a half it answers records its width.
fn split_in_two(
    slug: &str,
    searches: &[String],
    after: &[Option<String>],
    widest: &std::cell::Cell<usize>,
) -> Vec<Result<Found, String>> {
    let (left, right) = searches.split_at(searches.len() / 2);
    let (left_after, right_after) = after.split_at(searches.len() / 2);
    let mut out = one_batch(slug, left, left_after, widest)
        .unwrap_or_else(|e| left.iter().map(|_| Err(e.clone())).collect());
    out.extend(
        one_batch(slug, right, right_after, widest)
            .unwrap_or_else(|e| right.iter().map(|_| Err(e.clone())).collect()),
    );
    out
}

/// How long a batch width GitHub answered at stands in for asking again.
///
/// The same hour, and the same trade, as `crate::ai`'s `REFUSAL_LIFE` — stated rather than tuned.
/// What it costs is ONE wide request per hour per repo on a fleet whose GitHub genuinely sheds
/// them. What it buys is that nothing skein learned from a bad afternoon can outlive the afternoon:
/// a repo narrowed to two searches at a time widens back on its own, with nobody pressing anything.
const BATCH_WIDTH_LIFE: Duration = Duration::from_secs(60 * 60);

/// The widest batch of membership searches GitHub has **answered** for a repository, and when.
///
/// Every number in here is an answer, never a refusal — see [`one_batch`]. It is read to decide
/// where to START a refresh, and a stale one costs the refresh nothing worse than a split it did
/// not need.
static BATCH_WIDTHS: Mutex<BTreeMap<String, (usize, i64)>> = Mutex::new(BTreeMap::new());

/// Forget where the refreshes start, for tests and for a person who has just fixed their GitHub.
pub fn forget_batch_widths() {
    if let Ok(mut seen) = BATCH_WIDTHS.lock() {
        seen.clear();
    }
}

/// The remembered width, **if it still describes anything**.
fn answered_batch_width(slug: &str) -> Option<usize> {
    let seen = BATCH_WIDTHS.lock().unwrap_or_else(|e| e.into_inner());
    let (width, at_ms) = seen.get(slug).copied()?;
    (now_ms().saturating_sub(at_ms) <= BATCH_WIDTH_LIFE.as_millis() as i64).then_some(width)
}

/// Write down what this refresh managed, once the refresh is over.
///
/// **Only a NARROWING is remembered.** `widest >= asked` means GitHub took everything it was
/// handed, and there is nothing about this repo worth writing down — so the entry is removed
/// rather than set to the number of searches this particular refresh happened to have. Recording
/// that number would cap the repo at it: a fleet whose token could not list teams asks four, and
/// the day `read:org` arrives the fifth search would be "wider than GitHub has answered" and split
/// for no reason at all.
///
/// The clock is the other load-bearing part, and it moves in one direction. A refresh that got
/// WIDER than the standing memo restarts it: GitHub took more than skein expected, which is the
/// condition healing, and the new answer deserves its own full hour. A refresh that got narrower —
/// or exactly as narrow as last time, which is what a repo that splits on every poll produces —
/// updates the width and leaves the clock alone. Otherwise a repo would keep its own cap alive by
/// confirming it every three minutes, which is the memo pattern (SKEIN-281) rebuilt out of
/// successes: a note that outlives its cause with nothing able to end it.
fn learn_batch_width(slug: &str, widest: usize, asked: usize) {
    // A refresh where nothing answered learned nothing. Leaving the memo alone is what keeps a
    // rate-limit hold or a dead network from being read as "GitHub will not take one search".
    if widest == 0 {
        return;
    }
    let mut seen = BATCH_WIDTHS.lock().unwrap_or_else(|e| e.into_inner());
    if widest >= asked {
        seen.remove(slug);
        return;
    }
    match seen.get_mut(slug) {
        Some((known, at_ms)) => {
            if widest > *known {
                *at_ms = now_ms();
            }
            *known = widest;
        }
        None => {
            seen.insert(slug.to_string(), (widest, now_ms()));
        }
    }
}

/// Now, in epoch milliseconds — the one spelling this module compares memo ages against.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One batched request, as it has always been — the recursion above is what turns a refusal of the
/// whole batch into halves, and `after` is what turns it into the next page.
fn one_request(
    slug: &str,
    searches: &[String],
    after: &[Option<String>],
) -> Result<Vec<Result<Found, String>>, String> {
    let token = host_token()?;
    let mut variables = serde_json::Map::new();
    variables.insert("n".into(), serde_json::json!(SEARCH_PAGE));
    for (i, search) in searches.iter().enumerate() {
        // `is:pr is:open` and the repo are what `gh pr list --repo … --state open` added for us.
        // Spelled out here because the search string is now ours to build rather than gh's.
        variables.insert(
            format!("q{i}"),
            serde_json::json!(format!("repo:{slug} is:pr is:open {search}")),
        );
        // `null` on the first page, which is GraphQL's "from the beginning" — so the first request
        // of a refresh is the request it always was.
        variables.insert(
            format!("a{i}"),
            match after.get(i).and_then(|c| c.clone()) {
                Some(cursor) => serde_json::Value::String(cursor),
                None => serde_json::Value::Null,
            },
        );
    }
    let (data, errors) = crate::github::graphql_partial(
        &batched_query(searches.len()),
        serde_json::Value::Object(variables),
        &token,
    )?;
    Ok((0..searches.len())
        .map(|i| {
            let alias = format!("q{i}");
            match data.get(&alias) {
                Some(chunk) if !chunk.is_null() => {
                    let nodes = chunk
                        .get("nodes")
                        .and_then(|n| n.as_array())
                        .cloned()
                        .unwrap_or_default();
                    // A search that matches an issue rather than a pull request comes back as an
                    // empty object — the fragment simply does not apply — so those are dropped
                    // rather than parsed into a PR with number 0.
                    let more = chunk
                        .get("pageInfo")
                        .and_then(|p| p.get("hasNextPage"))
                        .and_then(|v| v.as_bool());
                    Ok(Found {
                        matched: chunk.get("issueCount").and_then(|v| v.as_u64()),
                        cursor: chunk
                            .get("pageInfo")
                            .and_then(|p| p.get("endCursor"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        // GitHub's own word for it where there is one. The fallback counts before
                        // the filter below, because the page is what GitHub filled against
                        // `first: $n` — dropping a non-PR from it makes the answer shorter without
                        // making it any more complete.
                        whole: match more {
                            Some(more) => !more,
                            None => nodes.len() < SEARCH_PAGE,
                        },
                        items: nodes
                            .iter()
                            .filter(|node| node.get("number").is_some())
                            .map(shape)
                            .collect(),
                    })
                }
                // This alias came back null or absent: find ITS errors by path. An error that
                // names no alias is ambient — attributed to every failed alias rather than
                // dropped, because a blind spot with no reason reads as skein's own fault.
                _ => {
                    let mine = errors
                        .iter()
                        .filter(|e| {
                            e.get("path")
                                .and_then(|p| p.as_array())
                                .and_then(|p| p.first())
                                .and_then(|s| s.as_str())
                                == Some(alias.as_str())
                        })
                        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                        .collect::<Vec<_>>()
                        .join("; ");
                    Err(match mine.is_empty() {
                        false => mine,
                        true => {
                            let ambient = errors
                                .iter()
                                .filter(|e| e.get("path").is_none())
                                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                                .collect::<Vec<_>>()
                                .join("; ");
                            match ambient.is_empty() {
                                false => ambient,
                                true => "GitHub returned no answer for this search".into(),
                            }
                        }
                    })
                }
            }
        })
        .collect())
}

/// GraphQL's nesting, flattened into the shape `gh --json` produced.
///
/// Two differences, both structural rather than semantic: a GraphQL connection is `{nodes: […]}`
/// where gh gave a bare array, and the check rollup hangs off the last commit rather than off the
/// pull request. Everything else is the same name and the same value, which is what made this port
/// a translation rather than a rewrite — and what lets every test of [`build_pr`],
/// [`my_review_state`] and [`rollup`] keep asserting on the fixtures they always had.
///
/// Two keys have no `gh` ancestor: `statusCheckRollupState` and `statusCheckRollupTotal`, which
/// carry what the flattening would otherwise throw away — see [`PR_FRAGMENT`]. They are written as
/// `null` when GitHub did not say, because a fixture from before SKEIN-232 has neither and the
/// difference between "GitHub says this is green" and "nobody said" is the whole point of them.
fn shape(node: &serde_json::Value) -> serde_json::Value {
    let mut out = node.clone();
    let reviews = node
        .get("latestReviews")
        .and_then(|r| r.get("nodes"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    // Flattened beside it under its own name, never merged into it: the two connections answer
    // different questions and [`my_review_state`] asks them in order. An answer that carries no
    // opinionated connection — an older fixture, a GitHub that stopped sending it — flattens to an
    // empty array and the verdict falls back to `latestReviews`, which is what skein always read.
    let opinionated = node
        .get("latestOpinionatedReviews")
        .and_then(|r| r.get("nodes"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let rollup_of = node
        .get("commits")
        .and_then(|c| c.get("nodes"))
        .and_then(|n| n.as_array())
        .and_then(|n| n.first())
        .and_then(|c| c.get("commit"))
        .and_then(|c| c.get("statusCheckRollup"));
    let checks = rollup_of
        .and_then(|r| r.get("contexts"))
        .and_then(|c| c.get("nodes"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    // The two facts the contexts array cannot carry, lifted out beside it under names of this
    // module's own (SKEIN-232): GitHub's uncapped verdict, and how many contexts there were to
    // read. Absent — from an older answer, or a GitHub that did not say — is a real state and
    // [`rollup`] treats it as one; it must not read as `SUCCESS` or as `totalCount: 0`.
    let rollup_state = rollup_of
        .and_then(|r| r.get("state"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let rollup_total = rollup_of
        .and_then(|r| r.get("contexts"))
        .and_then(|c| c.get("totalCount"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    // The head commit's own date, lifted out before `commits` is dropped. Same node the check
    // rollup comes from, so it costs nothing to ask for and would cost a second query to add later.
    let committed = node
        .get("commits")
        .and_then(|c| c.get("nodes"))
        .and_then(|n| n.as_array())
        .and_then(|n| n.first())
        .and_then(|c| c.get("commit"))
        .and_then(|c| c.get("committedDate"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let labels = node
        .get("labels")
        .and_then(|l| l.get("nodes"))
        .and_then(|n| n.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|l| l.get("name").and_then(|v| v.as_str()))
                .map(|name| serde_json::Value::String(name.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // The three shapes SKEIN-301 added, flattened the same way `labels` is: the connection wrapper
    // goes, the array stays under the name the fragment asked for, and the `totalCount` beside it
    // is lifted to a key of this module's own — absent stays absent, because a cap that cannot say
    // how much it cut reads as a pull request with nothing on it.
    let nodes_of = |key: &str| {
        node.get(key)
            .and_then(|c| c.get("nodes"))
            .and_then(|n| n.as_array())
            .cloned()
            .unwrap_or_default()
    };
    let total_of = |key: &str| {
        node.get(key)
            .and_then(|c| c.get("totalCount"))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let threads: Vec<serde_json::Value> = nodes_of("reviewThreads")
        .iter()
        .map(|t| {
            // The thread's own author, timestamp and permalink are its FIRST comment's — a
            // `PullRequestReviewThread` carries none of the three itself. Its body is not read
            // here and is not asked for; see `ReviewThread`.
            let first = t
                .get("comments")
                .and_then(|c| c.get("nodes"))
                .and_then(|n| n.as_array())
                .and_then(|n| n.first());
            let from = |k: &str| {
                first
                    .and_then(|c| c.get(k))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
            };
            serde_json::json!({
                "id": t.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                "resolved": t.get("isResolved").and_then(|v| v.as_bool()).unwrap_or(false),
                "outdated": t.get("isOutdated").and_then(|v| v.as_bool()).unwrap_or(false),
                "author": first
                    .and_then(|c| c.get("author"))
                    .and_then(|a| a.get("login"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
                "started_at": from("createdAt"),
                "url": from("url"),
            })
        })
        .collect();
    let comments: Vec<serde_json::Value> = nodes_of("comments")
        .iter()
        .map(|c| {
            let from = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or_default();
            serde_json::json!({
                "author": c
                    .get("author")
                    .and_then(|a| a.get("login"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
                "body": from("body"),
                "created_at": from("createdAt"),
                "url": from("url"),
            })
        })
        .collect();
    let asked: Vec<serde_json::Value> = nodes_of("reviewRequests")
        .iter()
        .filter_map(|r| {
            let who = r.get("requestedReviewer")?;
            // A user has a login; a team has a slug and an organization. A reviewer that is
            // neither — GitHub adds types to this union — is dropped rather than rendered as an
            // empty name.
            match who.get("login").and_then(|v| v.as_str()) {
                Some(login) => Some(serde_json::json!({ "name": login, "team": false })),
                None => {
                    let slug = who.get("slug").and_then(|v| v.as_str())?;
                    let org = who
                        .get("organization")
                        .and_then(|o| o.get("login"))
                        .and_then(|v| v.as_str())?;
                    Some(serde_json::json!({ "name": format!("{org}/{slug}"), "team": true }))
                }
            }
        })
        .collect();
    let threads_total = total_of("reviewThreads");
    let comments_total = total_of("comments");
    if let Some(map) = out.as_object_mut() {
        map.insert("reviewThreads".into(), serde_json::Value::Array(threads));
        map.insert("reviewThreadsTotal".into(), threads_total);
        map.insert("comments".into(), serde_json::Value::Array(comments));
        map.insert("commentsTotal".into(), comments_total);
        map.insert("reviewRequests".into(), serde_json::Value::Array(asked));
        map.insert("labels".into(), serde_json::Value::Array(labels));
        // Beside the names, the same way `reviewThreadsTotal` sits beside its threads: a label
        // list cut off at [`LABELS_FETCHED`] must be able to say so, because the alternative is a
        // workflow reading a label it never saw as one the pull request does not carry
        // (SKEIN-373). `null` where GitHub did not say — a fixture from before this asked for
        // `totalCount` has no such key, and "nobody said" is not "there are none".
        map.insert("labelsTotal".into(), total_of("labels"));
        map.insert("latestReviews".into(), reviews);
        map.insert("latestOpinionatedReviews".into(), opinionated);
        // And beside each of them, GitHub's count of the reviews it capped — the same lift
        // `labelsTotal` gets above, for the same reason one layer along: a connection cut at
        // [`REVIEWS_FETCHED`] must be able to say how many reviewers it did not reach, or the row's
        // `my_review` and `standing_approvals` are answers about a list nobody can size (SKEIN-386).
        // `null` where GitHub did not say — every fixture from before this was asked for.
        map.insert("latestReviewsTotal".into(), total_of("latestReviews"));
        map.insert(
            "latestOpinionatedReviewsTotal".into(),
            total_of("latestOpinionatedReviews"),
        );
        map.insert("statusCheckRollup".into(), checks);
        map.insert("statusCheckRollupState".into(), rollup_state);
        map.insert("statusCheckRollupTotal".into(), rollup_total);
        map.insert("committedDate".into(), committed);
        map.remove("commits");
    }
    out
}

fn build_pr(
    item: &serde_json::Value,
    number: u64,
    login: &str,
    repo_id: &str,
    reason: &Reason,
    archived_numbers: &[u64],
    snoozed_shas: &BTreeMap<u64, String>,
) -> Pr {
    let s = |k: &str| {
        item.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let head_sha = s("headRefOid");
    let head_ref = s("headRefName");
    let (my_review, review_is_current) = my_review_state(item, login, &head_sha);
    // Read off the same two connections, before `head_sha` is moved into the row it describes.
    let standing_approvals = standing_approvals(item, &head_sha);
    // How much of those two connections arrived (SKEIN-386). Both answers above are drawn from a
    // capped list, so the row carries the size of the list beside them — otherwise a `my_review` of
    // "none" reads the same whether nobody asked you or your review sorted past the cap.
    let (reviews_total, reviews_read) = reviews_counted(item);
    // Is GitHub asking YOU, by name, right now? Read off the flattened `reviewRequests` rather than
    // off the raw connection, so it asks the same list the roster on the row is drawn from and the
    // two cannot disagree about who was asked. A TEAM entry is skipped deliberately — see
    // [`Pr::my_review_requested`] for why this is a floor and why the error may only fall towards
    // leaving you alone.
    let my_review_requested = item
        .get("reviewRequests")
        .and_then(|v| v.as_array())
        .is_some_and(|asked| {
            asked.iter().any(|r| {
                r.get("team").and_then(|v| v.as_bool()) != Some(true)
                    && r.get("name")
                        .and_then(|v| v.as_str())
                        .is_some_and(|n| n.eq_ignore_ascii_case(login))
            })
        });
    let author = item
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let draft = item
        .get("isDraft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // GitHub's enum, kept as three states rather than two. See the field.
    let mergeable = match item.get("mergeable").and_then(|v| v.as_str()) {
        Some("MERGEABLE") => Some(true),
        Some("CONFLICTING") => Some(false),
        _ => None,
    };
    let checks = rollup(item);
    let review_decision = s("reviewDecision");
    // Set aside until the head moves (SKEIN-144): the snooze names the sha it was taken at, so
    // the author's next push — not a timer, not an act — is what brings the row back: the entry
    // stops matching and is ignored. An empty head matches nothing on purpose: "GitHub did not
    // say" must never be what keeps a row hidden.
    let snoozed = !head_sha.is_empty() && snoozed_shas.get(&number) == Some(&head_sha);
    // Whose move is it? Decided from READINESS, not from whether you have acted — the change that
    // took a 29-row "needs you" on the live fleet down to the ones actually yours to do.
    // Yours-or-decided outranks not-ready on purpose: your own red PR is your problem as an
    // AUTHOR, and this queue is the reviewer's; it must not resurface there as review work.
    //
    // Failing checks are deliberately NOT here. The owner's fleets run CI only after review — a
    // workflow applies the CI label on approval — so an unreviewed PR being red says nothing
    // about whether it can be reviewed, and treating red as not-ready removed live PRs from the
    // reviewer's view. The dot on the row still says red; the lane says whose move it is.
    //
    // **Your verdict stands until GitHub asks you again** (SKEIN-354). This used to read
    // `review_is_current && …`: skein compared the sha you reviewed with the head that is there
    // now, so a rebase or a typo fix took your approval off you and put the row back in your queue.
    // The owner, verbatim: "approved should come only if my review status on the PR is approved rn,
    // if I approved and then some file I own changed, so github asks me to review again then it
    // should show that." Both halves of that are GitHub's own answer now — `my_review` from
    // `latestOpinionatedReviews`, and `my_review_requested` from `reviewRequests` — and skein
    // infers neither. On his live queue `review_is_current` was false on all 26 rows, so the old
    // rule cleared nothing he ever did.
    let decided = matches!(my_review.as_str(), "approved" | "changes-requested");
    let lane = if archived_numbers.contains(&number) || snoozed {
        Lane::Archived
    } else if author == login || (decided && !my_review_requested) {
        Lane::Waiting
    } else if draft || mergeable == Some(false) {
        Lane::NotReady
    } else if review_decision == "APPROVED" && !my_review_requested {
        // GitHub's own verdict is read, not just fetched (SKEIN-142). `reviewDecision` is the
        // repository's authority on "does this still need somebody" — branch protection and
        // CODEOWNERS, rules skein cannot see — where `my_review` is the authority on "does it
        // need ME". APPROVED means someone's review already satisfied the repo, so the PR is not
        // review work any more; it waits on a merge, not on you.
        //
        // Two deliberate asymmetries:
        //   - Empty means the repo REQUIRES no review, and must not demote: the queue's whole
        //     purpose is repos where review is social rather than enforced, and demoting on
        //     silence would empty it exactly there. CHANGES_REQUESTED / REVIEW_REQUIRED fall
        //     through to the behaviour that always held.
        //   - Where the two authorities disagree — the repository is satisfied but GitHub is
        //     asking YOU again — the request for you by name wins and the PR stays yours. Being
        //     one of six reviewers whose approval satisfied a rule is not the same fact as nobody
        //     wanting anything from you, and a re-request is somebody wanting something. This used
        //     to test `my_review && !review_is_current` instead — the same asymmetry argued from
        //     skein's own sha comparison rather than from the ask (SKEIN-354).
        Lane::Waiting
    } else {
        Lane::NeedsYou
    };
    Pr {
        number,
        title: s("title"),
        author,
        url: s("url"),
        box_name: crate::repos::box_name(repo_id, &head_ref),
        head_ref,
        head_sha,
        base_ref: s("baseRefName"),
        draft,
        updated_at: s("updatedAt"),
        committed_at: s("committedDate"),
        labels: item
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|l| {
                l.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        labels_total: item.get("labelsTotal").and_then(|v| v.as_u64()),
        settled: settled(&s("committedDate")),
        review_decision,
        standing_approvals,
        reviews_total,
        reviews_read,
        mergeable,
        merge_state: s("mergeStateStatus"),
        additions: item.get("additions").and_then(|v| v.as_u64()),
        deletions: item.get("deletions").and_then(|v| v.as_u64()),
        changed_files: item.get("changedFiles").and_then(|v| v.as_u64()),
        checks,
        failing_checks: failing_contexts(item),
        my_review,
        review_is_current,
        my_review_requested,
        reasons: vec![reason.clone()],
        lane,
        snoozed,
        // Parsed off the flattened shape, and defaulting to an EMPTY list rather than failing the
        // pull request: an answer from a GitHub that did not carry these — an older fixture, a
        // schema that moves — costs the row its threads and nothing else. Same defensiveness the
        // struct's own doc argues for.
        review_threads: item
            .get("reviewThreads")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        review_threads_total: item.get("reviewThreadsTotal").and_then(|v| v.as_u64()),
        comments: item
            .get("comments")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        comments_total: item.get("commentsTotal").and_then(|v| v.as_u64()),
        review_requests: item
            .get("reviewRequests")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
    }
}

/// Your last review on this PR, and whether it was submitted against the current head.
///
/// A `COMMENTED` review is deliberately **not** a decision: leaving a note is not the same as
/// clearing the PR, so it stays in [`Lane::NeedsYou`]. Approving and requesting changes both are
/// decisions — in each case the ball is in the author's court, which is what the lane means.
///
/// **Your VERDICT comes from `latestOpinionatedReviews`, and only what is left over comes from
/// `latestReviews`** (SKEIN-354). GraphQL's `latestReviews` is the latest review per author
/// whatever it said, so approving a pull request and then leaving a note on it reports `COMMENTED`
/// and takes your own approval away — the demotion is silent, it looks exactly like never having
/// decided, and `latestOpinionatedReviews` exists in the schema precisely to exclude it. Asking the
/// opinionated connection first also gets the dismissal case right for free: an approval GitHub has
/// DISMISSED is not an opinionated review any more, so it stops standing, which is what "is my
/// review status approved right now" has to mean. The fallback keeps the one fact the opinionated
/// connection cannot carry — that you commented — and keeps every fixture written before this
/// working, since an item with no opinionated key reads exactly as it always did.
fn my_review_state(item: &serde_json::Value, login: &str, head_sha: &str) -> (String, bool) {
    let mine_in = |key: &str| {
        item.get(key)
            .and_then(|v| v.as_array())
            .and_then(|reviews| {
                reviews.iter().find(|r| {
                    r.get("author")
                        .and_then(|a| a.get("login"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|l| l.eq_ignore_ascii_case(login))
                })
            })
            .cloned()
    };
    let Some(mine) = mine_in("latestOpinionatedReviews").or_else(|| mine_in("latestReviews"))
    else {
        return ("none".into(), false);
    };
    let state = match mine.get("state").and_then(|v| v.as_str()).unwrap_or("") {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes-requested",
        "COMMENTED" => "commented",
        _ => "none",
    };
    // No commit on the review means we cannot prove it covers the current head. Treating that as
    // "not current" sends the PR back to Needs you — the over-flag direction, on purpose.
    let at = mine
        .get("commit")
        .and_then(|c| c.get("oid"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // And a review skein cannot name — a DISMISSED approval, a state GitHub adds later — leaves you
    // with nothing standing, so there is nothing for the head to be current WITH. Saying otherwise
    // would be a flag about a review that is not there (SKEIN-354).
    let current = state != "none" && !at.is_empty() && !head_sha.is_empty() && at == head_sha;
    (state.into(), current)
}

/// How many approvals are standing against `head_sha` — **anybody's**, the viewer's included.
///
/// See [`Pr::standing_approvals`] for why this exists. Two decisions are worth reading here rather
/// than at the field, because they are what make it agree with the answers beside it:
///
/// * **Which connection.** `latestOpinionatedReviews` if the answer carried one, else
///   `latestReviews` — the same order, and the same `or_else`, as [`my_review_state`]. Reversing
///   it or asking only one would make this and [`Pr::my_review`] able to disagree about the same
///   person's review, which is the one thing a second reader of the same data must not do.
///   [`shape`] flattens a missing connection to an empty array, so "carried one" is
///   non-emptiness — an answer with neither leaves this `Some(0)`, and `my_review` is `"none"`
///   there, so the two still agree.
/// * **What "standing" means.** The review's own commit equals the head, exactly as
///   [`my_review_state`] decides [`Pr::review_is_current`]. A review with no commit on it cannot
///   be proved to cover this head and is not counted — the under-report direction, which for a
///   merge train means holding a pull request rather than shipping one.
///
/// `None` only where there is no head to compare against: without one, every review would be
/// judged against an empty string, and a count of nought derived from skein not knowing the head
/// is a claim it has no business making.
fn standing_approvals(item: &serde_json::Value, head_sha: &str) -> Option<u64> {
    if head_sha.is_empty() {
        return None;
    }
    let nodes = |key: &str| {
        item.get(key)
            .and_then(|v| v.as_array())
            .filter(|reviews| !reviews.is_empty())
            .cloned()
    };
    let reviews = nodes("latestOpinionatedReviews")
        .or_else(|| nodes("latestReviews"))
        .unwrap_or_default();
    Some(
        reviews
            .iter()
            .filter(|r| r.get("state").and_then(|v| v.as_str()) == Some("APPROVED"))
            .filter(|r| {
                r.get("commit")
                    .and_then(|c| c.get("oid"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|at| at == head_sha)
            })
            .count() as u64,
    )
}

/// GitHub's count of the reviews on one pull request and how many arrived — the pair
/// [`Pr::reviews_total`] and [`Pr::reviews_read`] carry (SKEIN-386).
///
/// **From whichever of the two connections lost the most.** Both are capped at
/// [`REVIEWS_FETCHED`] and both are read: [`my_review_state`] asks them in turn, so a viewer's own
/// row can be cut from either, and [`standing_approvals`] counts from whichever answered. A hole in
/// either is therefore a hole in the row's answers, and the widest one is the honest thing to
/// report. Ties — and the ordinary case, where neither was cut — fall to `latestReviews`, because
/// `max_by_key` keeps the last of equal keys and it is listed second on purpose: it is the
/// connection every reader falls back to, and its count is the number of people who have reviewed.
///
/// `(None, None)` where GitHub said nothing: a fixture or an answer from before the query asked for
/// `totalCount`, which [`Pr::reviews_whole`] reads as whole for the reason stated there. Never
/// `Some(0)` out of silence — that is the claim that nobody has reviewed it.
fn reviews_counted(item: &serde_json::Value) -> (Option<u64>, Option<u64>) {
    let counted = |nodes: &str, total: &str| {
        let read = item.get(nodes).and_then(|v| v.as_array())?.len() as u64;
        let total = item.get(total).and_then(|v| v.as_u64())?;
        Some((total, read))
    };
    let widest = [
        counted("latestOpinionatedReviews", "latestOpinionatedReviewsTotal"),
        counted("latestReviews", "latestReviewsTotal"),
    ]
    .into_iter()
    .flatten()
    .max_by_key(|(total, read)| total.saturating_sub(*read));
    match widest {
        Some((total, read)) => (Some(total), Some(read)),
        None => (None, None),
    }
}

/// Serde default for [`Pr::settled`] — see that field for why an absent date reads as settled.
fn settled_by_default() -> bool {
    true
}

/// How long a pull request must go without a commit before skein reads it unasked.
///
/// A branch somebody is actively pushing to is the worst thing to spend a reading on: the reading
/// describes a commit that is about to stop being the head, and the next poll spends another. The
/// owner asked for an hour, which is also about the shortest gap that reliably means "they have
/// stopped for now" rather than "they are between commits".
pub const SETTLE: Duration = Duration::from_secs(60 * 60);

/// Has this head commit been sitting still for [`SETTLE`]?
///
/// **An unknown date reads as SETTLED**, which is the opposite of the obvious answer. GitHub's
/// silence is not evidence of age — true — but treating it as "not settled" makes one missing field
/// switch the whole feature off: nothing is read, on any pull request, with the row explaining the
/// silence by a branch movement skein has no evidence for. A browser suite caught exactly that. The
/// rule applies where there is something to apply it to; where there is not, skein does what it did
/// before the rule existed.
///
/// Reporting is a separate matter and unchanged: nothing may TELL somebody a branch is still moving
/// on the strength of an absent field.
pub fn settled(committed_at: &str) -> bool {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(committed_at) else {
        return true;
    };
    match (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).to_std() {
        Ok(since) => since >= SETTLE,
        // A commit dated in the future is a clock skew, not a settled branch.
        Err(_) => false,
    }
}

/// **Newest pull request first, by number.** The owner's own ordering.
///
/// It was `updated_at` descending, which sounds like the same thing and is not: a comment, a label,
/// a bot's push all move a pull request to the top of that order without changing what it is, so the
/// queue reshuffled between two looks and nothing stayed where it had been put. A number never
/// moves — the row you looked at yesterday is where you left it.
///
/// Its own function so a test can assert the QUEUE's ordering rather than assert that `sort_by`
/// sorts.
pub(crate) fn newest_first(prs: &mut [Pr]) {
    prs.sort_by_key(|pr| std::cmp::Reverse(pr.number));
}

/// One context's verdict. The single place "failing" is defined, shared by [`rollup`] (the word
/// on the row) and [`failing_contexts`] (the names under it) — two copies of this classification
/// is a row that says "failing" while naming nothing, or names a check its own dot calls green.
enum CheckVerdict {
    Failing,
    Pending,
    Passing,
}

fn verdict(c: &serde_json::Value) -> CheckVerdict {
    // A CheckRun carries `status`/`conclusion`; a classic StatusContext carries only `state`,
    // whose values (SUCCESS, FAILURE, ERROR, PENDING…) overlap enough to share the match.
    let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let conclusion = c
        .get("conclusion")
        .and_then(|v| v.as_str())
        .or_else(|| c.get("state").and_then(|v| v.as_str()))
        .unwrap_or("");
    match conclusion {
        "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "ERROR" => {
            CheckVerdict::Failing
        }
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => CheckVerdict::Passing,
        // A conclusion this code does not know on a COMPLETED run is treated as failing — the
        // over-report direction — where an incomplete run is merely pending.
        _ if status == "COMPLETED" => CheckVerdict::Failing,
        _ => CheckVerdict::Pending,
    }
}

/// Reduce `statusCheckRollup` to four words.
///
/// Any failure anywhere is failing; otherwise any incomplete run is pending. Failing wins over
/// pending because a red check is information you act on now, and a queue that showed "pending"
/// for a PR with a broken build would be hiding the useful half.
///
/// **Two sources, and the more cautious of them wins** (SKEIN-232). The contexts array is a page of
/// a hundred; GitHub's own `state` is its verdict over all of them however many there are. Read
/// from the page alone, a pull request whose 101st context is red reads "passing" — and
/// `docs/pr-workflow.md`'s merge train acts on `checks:passing` by merging and deleting the branch,
/// so that is not a wrong dot on a row, it is a merge performed on a guarantee that was never
/// checked. So a red in either source is "failing", and where they disagree in the other direction
/// — GitHub says green while a context this page holds has not finished — the answer is "pending".
/// Both of those are the over-report direction this function already takes for a conclusion it does
/// not recognise (see [`verdict`]): more of your attention, and never a merge on a check nobody read.
///
/// `state` absent is a real case, not a nuisance: an answer from before this field was asked for,
/// and every fixture written against the old shape. Then the page is all there is — and if the page
/// was TRUNCATED (`statusCheckRollupTotal` past its length) a walk that found nothing wrong has not
/// earned "passing", so it says "pending" and [`queue_within`] adds the blind spot that says why.
fn rollup(item: &serde_json::Value) -> String {
    let Some(checks) = item.get("statusCheckRollup").and_then(|v| v.as_array()) else {
        return "none".into();
    };
    let state = item
        .get("statusCheckRollupState")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    // No contexts and none claimed: nothing has ever run against this commit. Unchanged, and it is
    // why `total` may not simply default to zero — an absent `totalCount` means "not said".
    if checks.is_empty() && rollup_total(item).unwrap_or(0) == 0 {
        return "none".into();
    }
    let mut pending = false;
    for c in checks {
        match verdict(c) {
            CheckVerdict::Failing => return "failing".into(),
            CheckVerdict::Pending => pending = true,
            CheckVerdict::Passing => {}
        }
    }
    match state {
        // GitHub's own words for a red commit. `EXPECTED` is a context somebody promised and has
        // not sent, which is pending by every reading.
        Some("FAILURE" | "ERROR") => "failing".into(),
        Some("SUCCESS") if !pending => "passing".into(),
        // `PENDING`, `EXPECTED`, a green rollup over a context this page has not seen finish, or a
        // word this code does not know — none of which is a check somebody may merge on.
        Some(_) => "pending".into(),
        None if pending || truncated_rollup(item) => "pending".into(),
        None => "passing".into(),
    }
}

/// Did the answer carry GitHub's own rollup verdict at all? The one state [`rollup`] cannot decide
/// from either source, and the queue says so rather than letting its "pending" pass for CI running.
fn rollup_state_missing(item: &serde_json::Value) -> bool {
    !item
        .get("statusCheckRollupState")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// How many contexts GitHub says the rollup has, where it said — see [`PR_FRAGMENT`].
fn rollup_total(item: &serde_json::Value) -> Option<usize> {
    item.get("statusCheckRollupTotal")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
}

/// Did the check rollup have more contexts than the answer carried?
///
/// The comparison is against what actually arrived rather than against [`SEARCH_PAGE`]'s sibling
/// hundred, so it stays true if the page size ever moves.
fn truncated_rollup(item: &serde_json::Value) -> bool {
    let read = item
        .get("statusCheckRollup")
        .and_then(|v| v.as_array())
        .map(|c| c.len())
        .unwrap_or(0);
    rollup_total(item).is_some_and(|total| total > read)
}

/// WHICH contexts are behind a red rollup — name and detail link, first [`FAILING_CHECKS_SHOWN`]
/// in rollup order, deduplicated by name (SKEIN-153).
///
/// Deduplicated because re-runs of one check arrive as repeated contexts, and a row that says
/// "build, build, build" answers the question worse than one that says "build". A failing context
/// GitHub gave no name for is skipped rather than shown blank: the one-word `checks` verdict
/// still says "failing", so nothing is hidden — there is just no name to show for it.
///
/// The same holds for the red that lives past the hundredth context: [`rollup`] says "failing" on
/// GitHub's verdict, and this returns nothing to name it by, because the name is in the part of the
/// list nobody read. An empty list under a red verdict is that, and it is the right way round — a
/// verdict with no names sends you to GitHub; names with no verdict would have sent you nowhere.
fn failing_contexts(item: &serde_json::Value) -> Vec<FailedCheck> {
    let Some(checks) = item.get("statusCheckRollup").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<FailedCheck> = Vec::new();
    for c in checks {
        if !matches!(verdict(c), CheckVerdict::Failing) {
            continue;
        }
        // A CheckRun names itself `name` and links `detailsUrl`; a StatusContext is named by its
        // `context` and links `targetUrl`. Same fields the query asks for, per branch.
        let Some(name) = c
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| c.get("context").and_then(|v| v.as_str()))
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        if out.iter().any(|f| f.name == name) {
            continue;
        }
        let url = c
            .get("detailsUrl")
            .and_then(|v| v.as_str())
            .or_else(|| c.get("targetUrl").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        out.push(FailedCheck {
            name: name.to_string(),
            url,
        });
        if out.len() == FAILING_CHECKS_SHOWN {
            break;
        }
    }
    out
}

/// How many PRs are waiting on you, per repo — the badge's whole content.
#[derive(Debug, Clone, Serialize)]
pub struct Count {
    pub repo_id: String,
    pub needs_you: usize,
    /// Set when this repo's count could not be taken. Rendered rather than swallowed: a badge that
    /// silently shows nothing because `gh` is broken is indistinguishable from an empty queue, and
    /// that is the one thing this whole feature must never be.
    pub error: String,
    /// Set when this repo was **not asked** — the queue is switched off, or it has no GitHub remote.
    ///
    /// Not an error, and not nothing either. These repos used to be filtered out before the list was
    /// built, which made "you have no PRs waiting" and "skein never looked" the same empty badge. That
    /// is the same failure `error` exists to prevent, arriving one step earlier: a repo whose queue is
    /// quietly off looks exactly like a repo with a clean queue, and the only way to tell was to open
    /// the pane and notice the repo was missing from it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub skipped: String,
    /// PRs whose workflow has stopped and is waiting on a person — the merge train's skips. On the
    /// badge poll rather than the pane's answer, because the pane is only open when somebody is
    /// already looking: this is the row that has to reach them when they are not.
    ///
    /// **Filled by the counts route, not here.** The stops live in `prwork`'s file, and `prq`
    /// reading them would put `prq` inside the module cycle (`docs/modules.toml`); the server
    /// already stands on both modules, so the decoration is its one line. Everything `prq` builds
    /// leaves this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<StoppedPr>,
    /// What this repo's queue could **not** see, in the queue's own words — carried onto the
    /// badge rather than left behind in the pane.
    ///
    /// `error` above says the count could not be taken at all. This says it WAS taken and is
    /// incomplete, which is the harder failure and the one that had no field: `prq::queue` records
    /// a blind spot and still returns `Ok`, so every blind spot it recorded arrived here as a
    /// plain integer with nothing attached. On the owner's own fleet a token without `read:org`
    /// means the `team-review-requested:` searches are never issued at all, so the badge read 2
    /// while 11 were waiting — unmarked, with a tooltip that said nothing — and under a
    /// rate-limit hold that same zero is then served from the ten-minute cache. That is the
    /// invariant `error`'s comment states in as many words, broken one field over.
    ///
    /// Empty means the count is whole, and that is the only case a bare number may be drawn for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blind_spots: Vec<String>,
}

/// One pull request a workflow has stopped on, and why — the shape the counts poll carries to the
/// cockpit's banner row. Defined here beside [`Count`], the payload it rides in; `prwork::stops`
/// produces it, because the stops file is that module's.
#[derive(Debug, Clone, Serialize)]
pub struct StoppedPr {
    pub number: u64,
    pub why: String,
}

/// Take the count for every repo that has a review queue switched on.
///
/// Skips repos with no GitHub remote before touching the network — they cannot have PRs, so asking
/// would be a guaranteed error rather than a real one. Goes through the same per-repo cache as the
/// pane, but under a **ten-minute** budget where the pane insists on sixty seconds: the badge is
/// "a number you act on within minutes" — the UI's own words for it — so a ten-minute-old answer
/// is the right answer, and it cuts the steady-state GraphQL spend of a poll that runs every
/// three minutes per open tab by ~10x. That spend is what got the owner rate-limited.
pub fn counts() -> Vec<Count> {
    crate::repos::load_repos()
        .into_iter()
        .map(|repo| {
            // Why a repo was not asked, before spending anything on it. Reported rather than filtered
            // away: a repo skein never looked at is a different answer from a repo with nothing
            // waiting, and the badge could not tell them apart because only one of them was on the
            // list at all.
            let skipped = match (repo.review_queue, repo_slug(&repo)) {
                (false, _) => "review queue is switched off for this repo".to_string(),
                (true, None) => {
                    "no GitHub remote, so there are no pull requests to list".to_string()
                }
                (true, Some(_)) => String::new(),
            };
            if !skipped.is_empty() {
                return Count {
                    repo_id: repo.id,
                    needs_you: 0,
                    error: String::new(),
                    skipped,
                    stopped: Vec::new(),
                    // Nothing was looked at, so there is nothing this repo failed to see.
                    // `skipped` is the whole story for it.
                    blind_spots: Vec::new(),
                };
            }
            match queue_within(&repo, Duration::from_secs(600)) {
                Ok(q) => Count {
                    repo_id: repo.id,
                    needs_you: q.prs.iter().filter(|p| p.lane == Lane::NeedsYou).count(),
                    error: String::new(),
                    skipped: String::new(),
                    stopped: Vec::new(),
                    // The number and what it is missing travel together, or the number is a
                    // claim the queue never made (SKEIN-239).
                    blind_spots: q.blind_spots,
                },
                Err(e) => Count {
                    repo_id: repo.id,
                    needs_you: 0,
                    error: e,
                    skipped: String::new(),
                    stopped: Vec::new(),
                    // No queue was built, so there are no blind spots to report — `error` is
                    // already the strongest thing this can say.
                    blind_spots: Vec::new(),
                },
            }
        })
        .collect()
}

/// Every repo's queue, in one answer — the merged review the pane opens on (SKEIN-146).
///
/// **This costs no GitHub call the badge was not already costing.** `counts()` builds the complete
/// queue for every repo each poll and throws away everything but one integer; this returns what it
/// built. Same per-repo cache, same remembered copies — one repo answering slowly (`fresh: false`)
/// or failing does not stale or sink the others, which is why the shape is a list of queues and a
/// list of failures rather than one flattened result that could only be as good as its worst repo.
#[derive(Debug, Clone, Serialize)]
pub struct MergedQueue {
    /// The one global switch, said once — the per-queue `ai` repeats it, but the pane asks the
    /// merged answer, not a queue it may not have.
    pub ai: bool,
    pub queues: Vec<Queue>,
    /// Repos that could not be read, each with its reason. Attributed, never pooled: "a repo
    /// failed" hides exactly the information that decides whether you care.
    pub failed: Vec<Count>,
    /// Repos skein deliberately did not ask about — queue switched off, or no GitHub remote.
    /// Reported rather than omitted, same rule as `counts()`: "never looked" and "nothing waiting"
    /// must not be the same silence.
    pub skipped: Vec<Count>,
}

pub fn merged(force: bool) -> MergedQueue {
    let mut out = MergedQueue {
        ai: crate::review::summaries_enabled(),
        queues: Vec::new(),
        failed: Vec::new(),
        skipped: Vec::new(),
    };
    for repo in crate::repos::load_repos() {
        let skipped = match (repo.review_queue, repo_slug(&repo)) {
            (false, _) => "review queue is switched off for this repo".to_string(),
            (true, None) => "no GitHub remote, so there are no pull requests to list".to_string(),
            (true, Some(_)) => String::new(),
        };
        if !skipped.is_empty() {
            out.skipped.push(Count {
                stopped: Vec::new(),
                blind_spots: Vec::new(),
                repo_id: repo.id,
                needs_you: 0,
                error: String::new(),
                skipped,
            });
            continue;
        }
        // **Paint now, refresh behind — per repo**, the same rule the per-repo route has. The
        // first version of this called `queue()` cold and the pane blocked on every repo's three
        // GraphQL searches again, which is the exact regression the remembered copies exist to
        // prevent. `force` is the explicit refresh and always waits.
        if !force {
            if let Some(fresh) = unexpired(&repo.id) {
                out.queues.push(fresh);
                continue;
            }
            if let Some(old) = remembered(&repo.id) {
                // The refresh nobody is waiting for: it lands in the cache and on disk, so the
                // pane's follow-up ask (it retries a stale answer on its own) is a hit. A plain
                // thread, because this module is synchronous and the caller already runs it off
                // the async runtime.
                //
                // At most ONE per repo (SKEIN-206). The pane retries a stale answer at
                // 4s/8s/16s/…, and every retry lands here — without the guard each one spawned
                // its own refresh, so a single pane-open ran several concurrent fetches per repo,
                // four GraphQL searches each: "we aren't bombarding github right?" We were. The
                // refresh is also NOT forced: force is for a human's explicit "try again", and a
                // sibling's refresh landing first should be answered from the cache it just
                // filled, not fetched a second time.
                if let Some(running) = RefreshRunning::begin(&repo.id) {
                    let refresh = repo.clone();
                    std::thread::spawn(move || {
                        let _running = running;
                        let _ = queue(&refresh, false);
                    });
                }
                out.queues.push(old);
                continue;
            }
        }
        match queue(&repo, force) {
            Ok(q) => out.queues.push(q),
            Err(e) => out.failed.push(Count {
                stopped: Vec::new(),
                // The queues this repo's siblings DID build carry their own blind spots on the
                // `Queue` itself; a repo that built none has only its error.
                blind_spots: Vec::new(),
                repo_id: repo.id,
                needs_you: 0,
                error: e,
                skipped: String::new(),
            }),
        }
    }
    out
}

// ───────────────────────────── acting on a PR ─────────────────────────────

/// The three things a review can say, in GitHub's own vocabulary.
///
/// One function with three verbs rather than three functions, because they differ only in a flag
/// and they must stay consistent: `request-changes` exists precisely so that "not yet" moves the PR
/// out of your lane. Offering only approve and a plain comment would leave a PR you had answered
/// sitting in Needs you forever, with the ball visibly in the wrong court.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

/// Is this pull request still open? `None` when GitHub could not say.
///
/// Asked directly rather than inferred from the queue's absence, and the distinction is the whole
/// point: this queue is **personal** — `review-requested:you`, `author:you`, `mentions:you` — so a
/// PR leaving it means "no longer involves you" at least as often as it means "closed". Anything
/// that pruned on absence would delete the reading of a live PR whose review request moved to
/// somebody else.
///
/// `None` rather than a guess when the call fails: every caller keeps what it has on `None`, so a
/// GitHub that is down costs nothing and deletes nothing.
pub fn pr_is_open(slug: &str, number: u64) -> Option<bool> {
    let value = crate::github::get_json(
        &format!("/repos/{slug}/pulls/{number}"),
        &host_token().ok()?,
    )
    .ok()?;
    Some(value.get("state").and_then(|s| s.as_str())? == "open")
}

/// Submit a review as **you**, with the token the host holds.
///
/// GitHub refuses an empty body on `--request-changes` and `--comment`, so this refuses first with a
/// sentence you can act on rather than passing the rejection through.
pub fn submit_review(
    slug: &str,
    number: u64,
    verdict: Verdict,
    body: &str,
) -> Result<String, String> {
    let body = body.trim();
    if body.is_empty() && verdict != Verdict::Approve {
        return Err(
            "GitHub needs a body for anything but a bare approval — say what you want changed."
                .into(),
        );
    }
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    crate::github::send_json(
        "POST",
        &format!("/repos/{slug}/pulls/{number}/reviews"),
        &host_token()?,
        &serde_json::json!({ "event": event, "body": body }),
    )?;
    Ok(match verdict {
        Verdict::Approve => "approved",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "commented",
    }
    .into())
}

/// One vetted line comment on its way to GitHub. Defined here rather than borrowed from
/// [`crate::review`] because review depends on this module, not the other way round.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReviewComment {
    pub path: String,
    pub line: u64,
    pub body: String,
    /// The drafted line's own content — the text the reviewer was looking at, without the diff's
    /// `+`/` ` marker. It travels with the comment because it is the only durable anchor a moving
    /// branch leaves: a line NUMBER is a coordinate into one commit's diff and dies with it, but
    /// the line's text survives a rebase, a force-push, an insertion above it. `re_anchor` finds
    /// it again in the new diff by this text. Empty means "unknown" — an old client, or a draft
    /// that never captured it — and such a comment cannot be re-anchored, only displaced.
    #[serde(default)]
    pub text: String,
}

/// The first seven characters of a sha — the length `git log --oneline` taught everyone to read —
/// whole if it is somehow shorter.
fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Re-anchor drafted comments against a NEWER diff, by line text. Returns
/// `(anchored, displaced)`: the anchored carry updated line numbers valid in the new diff, the
/// displaced could not be placed and belong in the review body instead.
///
/// The rule, and its trade-offs, plainly:
///
/// - A comment's candidates are the new diff's RIGHT-side lines in the SAME path whose content
///   equals `comment.text` exactly (marker stripped, trailing newline ignored). Exact equality,
///   not fuzzy matching: a near-miss anchor puts a review sentence on a line it was not about,
///   which is worse than the honest fallback of naming it in the body.
/// - Exactly one candidate → anchored there. Several → the one nearest the old line number
///   (tie → the earlier), on the theory that most pushes move a line a little, not far; a
///   same-text line far away is likelier a different occurrence.
/// - None — the line was edited, deleted, or its file left the diff — or `comment.text` is empty
///   (nothing to search for) → displaced. Deliberately conservative: displacement costs a little
///   reading, a wrong anchor costs trust in every anchor.
/// - A comment whose text appears verbatim in an unrelated spot of the same file WILL anchor
///   there if its own line vanished. That is the price of text-only matching; the nearest-line
///   rule bounds it, and the (read at…, posted against…) note in the body names the commit that
///   was actually reviewed either way.
pub fn re_anchor(
    comments: &[ReviewComment],
    new_diff: &str,
) -> (Vec<ReviewComment>, Vec<ReviewComment>) {
    // ONE diff grammar, and it lives where the vetting happens (SKEIN-233). This was a
    // second body with the same name: identical code, and `review::commentable` had a
    // THIRD reading of the same bytes that lacked the `\ No newline at end of file` case
    // and silently discarded the rest of its hunk. `prq -> review` is already declared.
    let lines = crate::review::right_side_lines(new_diff);
    let mut anchored = Vec::new();
    let mut displaced = Vec::new();
    for c in comments {
        let want = c.text.trim_end_matches(['\n', '\r']);
        if want.is_empty() {
            displaced.push(c.clone());
            continue;
        }
        let best = lines
            .iter()
            .filter(|(p, _, t)| *p == c.path && t.trim_end_matches(['\n', '\r']) == want)
            // Nearest to the old number wins; on a tie min_by_key keeps the FIRST seen, and the
            // lines arrive in file order, so the earlier line wins the tie.
            .min_by_key(|(_, n, _)| (n.abs_diff(c.line), *n));
        match best {
            Some((_, n, _)) => anchored.push(ReviewComment {
                line: *n,
                ..c.clone()
            }),
            None => displaced.push(c.clone()),
        }
    }
    (anchored, displaced)
}

/// The head sha GitHub holds for this PR right now — one REST call, for the moment before a
/// review posts. The queue's cached sha can be a minute old, and a review posted against a sha
/// nobody verified is how the 422 this module just removed used to be born.
pub fn live_head_sha(slug: &str, number: u64) -> Result<String, String> {
    let v = crate::github::get_json(&format!("/repos/{slug}/pulls/{number}"), &host_token()?)?;
    v.pointer("/head/sha")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .ok_or_else(|| "GitHub's answer named no head commit".into())
}

/// What a merge has to check before it happens: `(base_ref, head_sha)`, live, in one request.
/// (SKEIN-338)
///
/// **Not from the queue, and that is the point.** The two facts a merge turns on are exactly the
/// two the queue is worst at: `base_ref` moves under a stacked child the moment its parent lands
/// (GitHub retargets it onto the trunk), and `head_sha` moves on every push. `queue`'s micro-cache
/// is sixty seconds and a remembered queue is older than that, so a merge decided from it is a
/// merge decided from a photograph.
///
/// **Not `queue(repo, false)` either**, on SKEIN-272's rule: a write must not inherit the ways a
/// full refresh fails — the viewer lookup, the rename check, five membership searches — and then
/// report them in the refresh's words. This is one `GET /repos/{slug}/pulls/{number}`, whose only
/// failure is about the pull request being merged.
///
/// An `Err` here STOPS a merge rather than falling back to anything, which is the opposite of
/// [`head_to_post_against`]'s choice next door and is deliberate. A review that cannot verify its
/// head still posts, because losing a review a person just vetted is worse than a stale
/// `commit_id`. A merge that cannot verify its base does not happen, because there is nothing worse
/// than the wrong merge.
pub fn base_and_head(slug: &str, number: u64) -> Result<(String, String), String> {
    let v = crate::github::get_json(&format!("/repos/{slug}/pulls/{number}"), &host_token()?)?;
    let at = |p: &str| {
        v.pointer(p)
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match (at("/base/ref"), at("/head/sha")) {
        (Some(base), Some(head)) => Ok((base, head)),
        _ => Err(format!(
            "GitHub did not say what #{number} is based on or what its head commit is, so skein \
             will not merge it"
        )),
    }
}

/// The sha a review is posted against — the ONE way either write path learns it.
///
/// `remembered` is the queue's sha, and it is the fallback rather than the answer. It is up to a
/// minute old (`queue`'s micro-cache) and older still whenever the pane is painting a remembered
/// copy, so inside that window it names a commit the branch has already left. That is not only a
/// wrong `commit_id`: [`submit_review_with_comments`] decides whether to re-anchor by comparing
/// the sha the draft was read at against this one, and a draft read from the same stale queue
/// carries the same stale sha — so the two agree, `moved` reads false, nothing re-anchors, and
/// vetted comments post at line numbers computed against a diff that no longer exists. GitHub
/// resolves them against the CURRENT diff, so they land on whatever text now occupies those
/// numbers and the post reports success (SKEIN-230).
///
/// The two write paths had two answers to this and only one of them made the call. One function,
/// so they cannot drift apart again. If GitHub will not answer, the remembered sha is the best
/// truth available and the post still goes — refusing to post because a verification call failed
/// would lose the review the person just vetted.
pub fn head_to_post_against(slug: &str, number: u64, remembered: &str) -> String {
    live_head_sha(slug, number).unwrap_or_else(|_| remembered.to_string())
}

/// Post one review carrying line comments — the vetted output of `crate::review::critique`.
///
/// `head_sha` is the LIVE head, sent as `commit_id` — always. `drafted_at` is the head the
/// comments were drafted against; empty means "assume current". When they differ, the review is
/// not refused (a dynamically moving PR made that refusal a treadmill — SKEIN-214): the new diff
/// is fetched and each comment is re-anchored by its line's text via [`re_anchor`]. Comments that
/// survive post as line comments at their NEW numbers; the displaced fold into the body under a
/// "Reviewed at {sha} — the branch has moved since" heading, and whenever the head moved at all
/// the body names both commits, because the GitHub record must say what was actually reviewed.
/// A diff that cannot be fetched (the 20k-line 406, a network refusal) displaces every comment
/// rather than failing the post — the review always lands.
pub fn submit_review_with_comments(
    slug: &str,
    number: u64,
    head_sha: &str,
    verdict: Verdict,
    body: &str,
    comments: &[ReviewComment],
    drafted_at: &str,
) -> Result<String, String> {
    // A bare approval is a complete statement; anything else with neither words nor comments is a
    // press with nothing behind it.
    if body.trim().is_empty() && comments.is_empty() && verdict != Verdict::Approve {
        return Err("nothing to post — every comment was dropped and the note is empty.".into());
    }
    let moved = !drafted_at.is_empty() && drafted_at != head_sha;
    let (anchored, displaced) = match moved {
        false => (comments.to_vec(), Vec::new()),
        true => match pr_diff_text(slug, number) {
            Ok(diff) => re_anchor(comments, &diff),
            // The owner's ask is that the review always lands: an unreadable diff means no
            // anchor can be trusted, so everything travels in the body instead of a 422 or an
            // error nobody can act on.
            Err(_) => (Vec::new(), comments.to_vec()),
        },
    };
    let mut full = body.trim().to_string();
    if !displaced.is_empty() {
        if !full.is_empty() {
            full.push_str("\n\n");
        }
        full.push_str(&format!(
            "Reviewed at {} — the branch has moved since, and these lines changed:",
            short_sha(drafted_at)
        ));
        for c in &displaced {
            full.push_str(&format!("\n• {}:{} — {}", c.path, c.line, c.body));
        }
    }
    if moved {
        if !full.is_empty() {
            full.push_str("\n\n");
        }
        full.push_str(&format!(
            "(read at {}, posted against {})",
            short_sha(drafted_at),
            short_sha(head_sha)
        ));
    }
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    let mut payload = serde_json::json!({
        "event": event,
        "commit_id": head_sha,
        "body": full,
    });
    if !anchored.is_empty() {
        payload["comments"] = anchored
            .iter()
            .map(|c| {
                serde_json::json!({
                    "path": c.path, "line": c.line, "side": "RIGHT", "body": c.body,
                })
            })
            .collect();
    }
    let token = host_token()?;
    let path = format!("/repos/{slug}/pulls/{number}/reviews");
    // **A dead connection here is ambiguous, and that is the whole difference from the read side**
    // (SKEIN-271). Posting a review is not idempotent: the peer cancels the stream after the
    // headers, so GitHub may well have created the review before the answer was lost, and asking
    // again on that evidence posts a second review onto somebody's pull request. Nothing is ever
    // re-sent until [`review_already_landed`] has been asked what actually happened — and when it
    // cannot answer, skein stops and says so rather than guessing in the direction that duplicates.
    let mut landed_first_time = true;
    let mut tries = 0;
    loop {
        tries += 1;
        match crate::github::send_json("POST", &path, &token, &payload) {
            Ok(_) => break,
            Err(why) if crate::github::connection_died(&why) => {
                landed_first_time = false;
                match review_already_landed(slug, number, head_sha, &full, &token) {
                    // It was created before the stream died. The press succeeded; saying otherwise
                    // would send the person to post it a second time by hand.
                    Ok(true) => break,
                    // GitHub has no such review, so nothing is duplicated by asking again.
                    Ok(false) if tries < 2 => continue,
                    Ok(false) => {
                        return Err(format!(
                            "the connection to GitHub died twice while posting this review, so it \
                             was not posted — skein checked both times and nothing landed, so \
                             nothing is duplicated and it is safe to press again ({why})"
                        ))
                    }
                    Err(look) => {
                        return Err(format!(
                            "the connection to GitHub died while posting this review ({why}), and \
                             skein could not then find out whether it landed ({look}) — open \
                             {slug}#{number} and look before pressing again, because if it did \
                             land, pressing again posts it twice"
                        ))
                    }
                }
            }
            Err(why) => return Err(why),
        }
    }
    let said = match verdict {
        Verdict::Approve => "approved",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "posted the review",
    };
    let mut told = match anchored.len() {
        0 => said.to_string(),
        1 => format!("{said} — with 1 line comment"),
        n => format!("{said} — with {n} line comments"),
    };
    if !displaced.is_empty() {
        told.push_str(&format!(
            " ({} moved into the note — the branch has new commits)",
            displaced.len()
        ));
    }
    if !landed_first_time {
        told.push_str(" — the connection died mid-post, and skein checked GitHub rather than posting it twice");
    }
    Ok(told)
}

/// Is the review skein was posting when the connection died already on GitHub? (SKEIN-271)
///
/// The question a write has to answer before it may ask again. Three facts have to agree, and the
/// third is the one that makes this safe in both directions:
///
/// * the **viewer** wrote it — this token's own login, since another reviewer's review at the same
///   commit says nothing about ours;
/// * the **commit** is the one this post named as `commit_id`;
/// * the **body is byte-for-byte what was sent**. Viewer-and-commit alone is too loose: a person
///   who approved at this head an hour ago and is now leaving comments on it would match, and
///   declining then would silently throw away the review they had just vetted. It is also exact
///   rather than approximate — a stream that dies mid-send truncates the JSON, which GitHub rejects
///   as a 400 rather than storing half a review, so the body GitHub holds is either the whole of
///   what was sent or there is no review at all.
///
/// **`Err` means "could not find out", never "no"** — that is why the pages are followed to the
/// end rather than reading the first thirty. A partial listing that happens not to contain the
/// review is indistinguishable from one that would have, and treating it as absence is precisely
/// the double post this exists to prevent.
fn review_already_landed(
    slug: &str,
    number: u64,
    head_sha: &str,
    body: &str,
    token: &str,
) -> Result<bool, String> {
    let login = crate::github::get_json("/user", token)?
        .get("login")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if login.is_empty() {
        return Err("GitHub named no login for this token".into());
    }
    // Line endings are the one thing a round trip may normalise; everything else is compared as
    // sent.
    let same = |a: &str| a.replace("\r\n", "\n") == body.replace("\r\n", "\n");
    const PER_PAGE: usize = 100;
    // Ten pages of a hundred. A pull request with a thousand reviews on it is not a thing, and an
    // unbounded loop on an error path is.
    for page in 1..=10 {
        let listed = crate::github::get_json(
            &format!("/repos/{slug}/pulls/{number}/reviews?per_page={PER_PAGE}&page={page}"),
            token,
        )?;
        let reviews = listed
            .as_array()
            .ok_or("GitHub's answer was not a list of reviews")?;
        if reviews.iter().any(|r| {
            r.pointer("/user/login").and_then(|v| v.as_str()) == Some(login.as_str())
                && r.get("commit_id").and_then(|v| v.as_str()) == Some(head_sha)
                && r.get("body").and_then(|v| v.as_str()).is_some_and(same)
        }) {
            return Ok(true);
        }
        if reviews.len() < PER_PAGE {
            return Ok(false);
        }
    }
    Err("this pull request has more reviews than skein will page through".into())
}

/// A pull request's diff, as a diff — the media type is the whole of what `gh pr diff` did.
pub fn pr_diff_text(slug: &str, number: u64) -> Result<String, String> {
    let token = host_token()?;
    match crate::github::get_text(
        &format!("/repos/{slug}/pulls/{number}"),
        &token,
        "application/vnd.github.diff",
    ) {
        Ok(diff) => Ok(diff),
        // **GitHub refuses to serve a diff over 20,000 lines**, and answers 406:
        //
        //     Sorry, the diff exceeded the maximum number of lines (20000)
        //
        // Reported live as a pull request that could not be read at all. That refusal is about
        // SERVING it, not about size being a problem here: `review` truncates every diff to a byte
        // cap before it reaches a model anyway, so a change this big was always going to be read in
        // part. The only thing the 406 actually cost was reading it at all.
        //
        // So it is assembled from the per-file endpoint, which serves the same hunks a file at a
        // time. Marked as assembled, because a reader has to know it is looking at part of a change
        // and not the whole of a small one.
        Err(why) if why.contains("too_large") || why.contains("exceeded the maximum") => {
            assembled_diff(slug, number, &token).map_err(|e| {
                format!(
                    "its diff is too large for GitHub to serve, and the file list would not \
                         read either: {e}"
                )
            })
        }
        Err(why) => Err(why),
    }
}

/// A diff put back together from `/pulls/{n}/files`, for the ones GitHub will not serve whole.
///
/// Each file comes with its own patch, so this is the same text arriving in pieces — with the header
/// lines `diff --git` and `+++` that everything downstream keys on, because `shape` and `contracts`
/// read a diff by those and a stream of bare hunks would parse as nothing.
///
/// A file whose patch GitHub also omits (binary, or too large on its own) is named with its
/// numbers rather than dropped: "this file changed and you cannot see it here" is a fact a reviewer
/// needs, and silence would read as "nothing happened here".
fn assembled_diff(slug: &str, number: u64, token: &str) -> Result<String, String> {
    // 120s, not the default 30: a hundred files each carrying its own patch is megabytes of JSON,
    // and this runs on the background reader's clock, not a cockpit poll's.
    let files = crate::github::get_json_within(
        &format!("/repos/{slug}/pulls/{number}/files?per_page=100"),
        token,
        std::time::Duration::from_secs(120),
    )?;
    let files = files.as_array().ok_or("GitHub did not list the files")?;
    if files.is_empty() {
        return Err("GitHub listed no files for it".into());
    }
    let mut out = String::new();
    for file in files {
        let name = file
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)");
        out.push_str(&format!("diff --git a/{name} b/{name}\n"));
        match file.get("patch").and_then(|v| v.as_str()) {
            Some(patch) => {
                out.push_str(&format!("--- a/{name}\n+++ b/{name}\n"));
                out.push_str(patch);
                out.push('\n');
            }
            None => {
                let n = |k: &str| file.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                out.push_str(&format!(
                    "--- a/{name}\n+++ b/{name}\n@@ no patch available @@\n\
                     (+{} -{}, {} — GitHub did not include this file's contents)\n",
                    n("additions"),
                    n("deletions"),
                    file.get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("changed"),
                ));
            }
        }
    }
    // One page. A change across more than a hundred files is not one this tool is helping with, and
    // saying so beats a reader assuming they have seen all of it.
    if files.len() >= 100 {
        out.push_str(
            "\n(this pull request touches more than 100 files; only the first 100 are here)\n",
        );
    }
    Ok(out)
}

/// The paths a pull request touches.
///
/// Its own endpoint rather than parsing the diff for `+++` lines: a rename, a binary file and a
/// mode-only change are all files GitHub names here and none of them appear the way a parser would
/// expect. One page of 100 — a review over that many files is not one this tool is helping with.
pub fn pr_files(slug: &str, number: u64) -> Result<Vec<String>, String> {
    // Same budget as `assembled_diff`, for the same reason: the listing carries each file's patch
    // whether or not the caller wants it, so on a big change this answer is big.
    Ok(crate::github::get_json_within(
        &format!("/repos/{slug}/pulls/{number}/files?per_page=100"),
        &host_token()?,
        std::time::Duration::from_secs(120),
    )?
    .as_array()
    .map(|files| {
        files
            .iter()
            .filter_map(|f| f.get("filename").and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default())
}

/// Merge a PR by number. Separate from approving on purpose: with a protected base branch your
/// approval is one of several, and merging is a different decision that is often not yours to make.
///
/// `$SKEIN_MERGE_METHOD` (default `--squash`) picks the method — the same variable the retired
/// box-level merge honoured, so an existing setting keeps working.
///
/// **`expected_head` is not optional, and the empty string is refused rather than sent** (SKEIN-338).
/// It goes out as GitHub's `sha`, which makes the merge conditional on the branch still being what
/// the person read: GitHub answers 409 if somebody pushed in between, and this translates that 409
/// into the sentence a reader can act on instead of leaving GitHub's own wording — *"Head branch
/// was modified"* — to stand for it. Until SKEIN-338 this function sent `merge_method` and nothing
/// else, while `prwork::merge_pr` beside it sent `sha`; the automated merge could not land a
/// revision nobody had looked at and the merge a PERSON pressed could, which is the wrong way round.
///
/// The empty string is refused rather than defaulted to the live head because "assume current" is
/// precisely the hole: a caller with no idea what the reader saw must say so and be stopped, not
/// have skein invent an answer that agrees with whatever GitHub has now. The one caller is
/// [`crate::prwork::merge_by_hand`], which checks the trunk as well — there is no merge in this
/// crate that goes to GitHub past neither guard.
pub fn merge(slug: &str, number: u64, expected_head: &str) -> Result<String, String> {
    if expected_head.trim().is_empty() {
        return Err(format!(
            "skein does not know which commit of #{number} you are looking at, and will not merge \
             a revision it cannot name. Refresh the queue and read the change again."
        ));
    }
    let method = std::env::var("SKEIN_MERGE_METHOD")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "--squash".into());
    // The same values, without the leading dashes `gh` wanted. An unrecognised setting is refused
    // here rather than sent: GitHub answers a bad `merge_method` with a 422 whose message is about
    // JSON, which reads as a skein bug.
    let method = match method.trim().trim_start_matches("--") {
        "squash" => "squash",
        "merge" => "merge",
        "rebase" => "rebase",
        other => {
            return Err(format!(
                "$SKEIN_MERGE_METHOD is {other:?}; GitHub takes squash, merge or rebase"
            ))
        }
    };
    let out = crate::github::send_json(
        "PUT",
        &format!("/repos/{slug}/pulls/{number}/merge"),
        &host_token()?,
        // `sha` is the head the person read. Same field, same reason, as `prwork::merge_pr`: GitHub
        // refuses with a 409 if the branch has moved, and a merge decided about code that is no
        // longer what would be merged is the one outcome this whole path exists to prevent.
        &serde_json::json!({ "merge_method": method, "sha": expected_head }),
    )
    // Two translations, and they cannot both fire: one is gated on a 409 and the other on a 405,
    // so the order here is readability and nothing else.
    .map_err(|e| the_branch_moved(number, expected_head, e))
    .map_err(|e| it_conflicts_with_its_base(number, e))?;
    Ok(out
        .get("message")
        .and_then(|m| m.as_str())
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("merged")
        .to_string())
}

/// GitHub's 409 on a conditional merge, said in the reader's terms.
///
/// **Matched on the status skein itself formatted, not on GitHub's prose.** `crate::github` turns a
/// non-2xx into one of two sentences — `format!("GitHub said {status}: {m}")` when the body carries
/// a `message`, and `complaint`'s `format!("GitHub answered {status}: …")` when it does not — so
/// those two prefixes are the whole surface, and both are checked. Reading GitHub's own words
/// (*"Head branch was modified. Review and try the merge again."*) instead would be a string match
/// on somebody else's copy, which changes without notice and would fail open into "merge failed,
/// unclear why" on the one act that cannot be taken back.
///
/// Every other error is passed through untouched: a 405 (not mergeable), a 422, a rate limit and a
/// dead connection are all real answers and none of them mean the branch moved. The 405 that names
/// conflicts is [`it_conflicts_with_its_base`] below — a second translation on the same rule, never
/// a widening of this one.
fn the_branch_moved(number: u64, expected_head: &str, said: String) -> String {
    let conflict = said.starts_with("GitHub said 409") || said.starts_with("GitHub answered 409");
    match conflict {
        false => said,
        true => format!(
            "the branch moved since you read it — #{number} is no longer at {}, so nothing was \
             merged. Read the new code, then merge.",
            short_sha(expected_head)
        ),
    }
}

/// GitHub's 405 on a merge it will not attempt, when the reason is conflicts (SKEIN-411).
///
/// A reader pressing merge on a conflicted pull request was shown `GitHub said 405: Pull Request
/// has merge conflicts` — measured on `acme/testbed#20` and quoted in SKEIN-385's
/// commit. That is a status code and somebody else's noun phrase, and it does not say what to do.
///
/// **The status is the gate, and it is one skein formatted itself.** Same rule as
/// [`the_branch_moved`] above, same two prefixes: `crate::github` turns a non-2xx into `GitHub said
/// {status}: {m}` or `complaint`'s `GitHub answered {status}: …` and nothing else, so matching
/// those is matching skein's own words.
///
/// **GitHub's prose narrows WITHIN that status; it never opens the gate.** A 405 on a merge means
/// "not mergeable", which is more than one situation — conflicts, a draft, a blocking rule — and
/// only conflicts are answered by going and resolving conflicts. The `message` is the one thing
/// that tells them apart, so it is read, and it is read for the word alone rather than for the
/// whole sentence. If that copy changes, a 405 stops matching and the reader gets the raw sentence
/// they get today: the failure available here is the one that under-translates, and telling
/// somebody to resolve conflicts that are not there is not.
///
/// Every other status is untouched. A 409 is the branch moving, a 422, a rate limit and a dead
/// connection are all real answers, and none of them are conflicts.
fn it_conflicts_with_its_base(number: u64, said: String) -> String {
    match refused_for_conflicts(&said) {
        false => said,
        true => format!(
            "#{number} has conflicts with its base, so GitHub will not merge it until they are \
             resolved. Resolve them on the branch, push, then merge."
        ),
    }
}

/// Is this GitHub's refusal to merge a branch that conflicts with its base? (SKEIN-411, SKEIN-423)
///
/// The gate [`it_conflicts_with_its_base`] above states in full, and nothing but the gate — the
/// rule written out there is what this holds, and the paragraphs there are its documentation.
///
/// **Shared because the wire shape is one fact and the sentence is two.** There are two merges in
/// this crate: [`merge`], which a person presses, and `prwork::merge_pr`, which a train drives, and
/// SKEIN-423 is the second one arriving at the same 405. What they must agree about is which
/// answers from GitHub *are* this refusal — that is knowledge of `crate::github`'s two wrappers,
/// and a second copy of it is a second thing to miss when a wrapper changes. What they must NOT
/// share is the words: "resolve them on the branch, push, then merge" is advice to somebody
/// standing at a button, and the reader of a stop is somebody who was not watching, reading later.
/// So the predicate is `pub(crate)` and each caller writes its own sentence
/// (`prwork::conflicts_stopped_the_train` is the other one).
pub(crate) fn refused_for_conflicts(said: &str) -> bool {
    let refused = said.starts_with("GitHub said 405") || said.starts_with("GitHub answered 405");
    refused && said.to_ascii_lowercase().contains("conflict")
}

/// Mark a review thread resolved on GitHub (SKEIN-305).
///
/// `thread_id` is [`ReviewThread::id`] — GitHub's node id, which is exactly the argument
/// `resolveReviewThread` takes, so this needs no lookup and no slug. That is why the whole
/// conversation carries thread ids: the panel draws a thread from its author, time and permalink,
/// and the id is the one field on it that exists only so this call can be made.
///
/// **Here rather than in the caller**, because this is the only module that may reach GitHub:
/// [`crate::github::graphql`] is `pub(crate)`, so `src/bin/skein-server.rs` is a different crate
/// and cannot call it, and `review` reaching `github` is an edge `docs/modules.toml` does not
/// declare — `tools/module-check.py` fails on it. `prq -> github` is declared, so this is where it
/// goes.
///
/// **No queue read.** A thread id and nothing else, so a resolve costs one request and cannot
/// inherit the ways a refresh fails (the SKEIN-272 lesson, in the form it takes here). Invalidating
/// the cached queue afterwards is the caller's, on the same rule as every other act that touched
/// GitHub.
pub fn resolve_review_thread(thread_id: &str) -> Result<(), String> {
    set_thread_resolved(thread_id, true)
}

/// The inverse, so the panel's eight-second undo (SKEIN-162) is a real retraction rather than a
/// row that redraws itself while GitHub still says resolved.
pub fn unresolve_review_thread(thread_id: &str) -> Result<(), String> {
    set_thread_resolved(thread_id, false)
}

/// The one mutation both directions send, with only its name and the state it asserts differing.
///
/// **[`crate::github::graphql`], never `graphql_partial`.** The sibling asks a dead connection
/// again, and its own doc says why that must not carry a mutation: an ambiguous failure may be one
/// that already ran. It is also the half that fails the whole request on any `errors` entry, which
/// is what this needs — an `Ok(())` on a GraphQL error would let the undo window close over a
/// resolve that never happened.
///
/// GitHub's answer is read back rather than discarded, on the rule this file uses everywhere: what
/// GitHub SAID is used, and what it did not say is not invented. `isResolved` coming back against
/// what was asked is a write that did not take, and it is reported as one; `isResolved` absent is
/// not a contradiction, so it is accepted.
fn set_thread_resolved(thread_id: &str, resolved: bool) -> Result<(), String> {
    if thread_id.trim().is_empty() {
        return Err("no review thread was named, so there is nothing to resolve".into());
    }
    let field = match resolved {
        true => "resolveReviewThread",
        false => "unresolveReviewThread",
    };
    let query = format!(
        "mutation($id: ID!) {{\n\
        \x20 {field}(input: {{threadId: $id}}) {{ thread {{ id isResolved }} }}\n\
        }}"
    );
    let out = crate::github::graphql(
        &query,
        serde_json::json!({ "id": thread_id }),
        &host_token()?,
    )?;
    let said = out
        .get(field)
        .and_then(|v| v.get("thread"))
        .and_then(|t| t.get("isResolved"))
        .and_then(serde_json::Value::as_bool);
    match said {
        Some(is) if is != resolved => Err(format!(
            "GitHub accepted the {} and reports the thread as {}",
            match resolved {
                true => "resolve",
                false => "unresolve",
            },
            match is {
                true => "still resolved",
                false => "still open",
            }
        )),
        _ => Ok(()),
    }
}

/// A `Deserialize` twin of [`Lane`], so a route can accept a lane name as input.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaneInput {
    NeedsYou,
    Waiting,
    Archived,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny GitHub that records what it was handed. Returns `(base_url, seen)`.
    ///
    /// A real socket rather than a stubbed function, because the thing worth testing after this port
    /// is the wire: which credential reached the API, in which header. A stub would agree with
    /// whatever the client did.
    fn fake_github(body: &'static str) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                // Headers only: every request here either has no body or one this does not read,
                // and the connection is closed immediately after answering.
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(rest) = line.to_ascii_lowercase().strip_prefix("authorization:") {
                        recorder.lock().unwrap().push(rest.trim().to_string());
                    }
                    line.clear();
                }
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// A GitHub server that answers by path, so a rename can be told from an empty repository.
    ///
    /// The single-body stub beside this cannot express the bug: it needs `/repos/<old>` to redirect
    /// while `search` answers differently for the old name and the new one.
    fn routing_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = asked.clone();
        let mine = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder.lock().unwrap().push(format!("{path} {body}"));

                let (status, answer) = match path.as_str() {
                    "/user" => (200, r#"{"login":"me"}"#.to_string()),
                    p if p.starts_with("/user/teams") => (200, "[]".to_string()),
                    // The rename, exactly as GitHub reports it.
                    "/repos/acme/old-name" => (
                        301,
                        format!(
                            r#"{{"message":"Moved Permanently","url":"{mine}/repositories/42"}}"#
                        ),
                    ),
                    "/repositories/42" => (200, r#"{"full_name":"acme/new-name"}"#.to_string()),
                    "/repos/acme/new-name" => (200, r#"{"full_name":"acme/new-name"}"#.to_string()),
                    "/graphql" => {
                        // The heart of it: the stale name matches nothing, with no error — which is
                        // what GitHub really does and why the queue went quietly empty. The batched
                        // wire: one request, aliases q0..q3 (no teams here), each its own search.
                        let hit =
                            body.contains("acme/new-name") && body.contains("review-requested");
                        (
                            200,
                            match hit {
                                true => r#"{"data":{"q0":{"nodes":[{"number":7,"title":"a pull request","url":"u","isDraft":false,"author":{"login":"someone"},"headRefOid":"abc","updatedAt":"2026-08-01T00:00:00Z","latestReviews":{"nodes":[]},"reviewRequests":{"nodes":[]}}]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                                false => r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                            },
                        )
                    }
                    _ => (200, "{}".to_string()),
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
        (base, asked)
    }

    /// A repository that was renamed fills its queue, and skein records the new name.
    ///
    /// The bug this is about produced no error anywhere. `acme/gadget-demo` became
    /// `acme/thing`; GitHub's REST redirects, so diffs and merges kept working, while its
    /// SEARCH matches a stale name against nothing and answers 200 with zero results. The queue
    /// collected nothing, recorded no blind spot, and rendered empty — with twenty-three pull
    /// requests waiting on a review behind it.
    #[test]
    fn a_renamed_repository_fills_its_queue_and_the_new_name_is_written_down() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, asked) = routing_github();
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        // Built from JSON like the other fixtures here: `Repo` gains fields regularly and a
        // struct literal is the thing that stops compiling for a reason unrelated to this test.
        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/old-name.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        let answered = queue(&repo, true).expect("the queue answered");
        assert!(
            !answered.prs.is_empty(),
            "the queue is empty on a repository that was renamed — which is the failure it must \
             never be able to show by accident. Asked: {:?}",
            asked.lock().unwrap()
        );

        // And the new name is recorded, so everything else keyed on the slug follows it — the write
        // credential a box pushes with, the mirror's origin, the next queue refresh.
        let after = crate::repos::load_repos();
        let stored = &after.iter().find(|r| r.id == "demo").unwrap().source;
        assert!(
            stored.contains("acme/new-name"),
            "the rename was used and not written down, so every restart pays for it again: {stored}"
        );
        // The URL's shape survives — skein does not own it, and rebuilding one would change a
        // repo's transport along with its name.
        assert!(
            stored.starts_with("https://") && stored.ends_with(".git"),
            "the URL was rebuilt rather than edited: {stored}"
        );
        // The id is untouched. Box names, box roots and placement records are built from it.
        assert_eq!(after.iter().find(|r| r.id == "demo").unwrap().id, "demo");

        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("SKEIN_GITHUB_API");
        forget_host_token();
        forget_renames();
    }

    /// A GitHub that can be taken away and given back, answering a rename either way.
    ///
    /// Beside [`routing_github`] rather than folded into it: that one exists to prove a rename is
    /// followed at all, and this one exists to prove that FAILING to ask about one is not an
    /// answer. It also counts what was asked, because the other half of the rule — an answer, even
    /// "not renamed", is still remembered — is a claim about how often GitHub is paid.
    fn flaky_rename_github(
        down: std::sync::Arc<std::sync::Mutex<bool>>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = asked.clone();
        let mine = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder.lock().unwrap().push(path.clone());
                let out = *down.lock().unwrap();
                let (status, answer) = match path.as_str() {
                    // The one call that is taken away. 504 rather than a 403, because that is what
                    // the owner's fleet actually got the day this was written — and because the
                    // rule is about caching a failure, not about which failure it was.
                    "/repos/acme/old-name" if out => {
                        (504, r#"{"message":"Gateway Timeout"}"#.to_string())
                    }
                    "/user" => (200, r#"{"login":"me"}"#.to_string()),
                    p if p.starts_with("/user/teams") => (200, "[]".to_string()),
                    "/repos/acme/old-name" => (
                        301,
                        format!(
                            r#"{{"message":"Moved Permanently","url":"{mine}/repositories/42"}}"#
                        ),
                    ),
                    "/repositories/42" => (200, r#"{"full_name":"acme/new-name"}"#.to_string()),
                    // `default_branch` included, or `trunk_of` would rightly keep asking and the
                    // paid-once assertion below would be counting its retries.
                    "/repos/acme/new-name" => (
                        200,
                        r#"{"full_name":"acme/new-name","default_branch":"main"}"#.to_string(),
                    ),
                    "/graphql" => {
                        let hit =
                            body.contains("acme/new-name") && body.contains("review-requested");
                        (
                            200,
                            match hit {
                                true => r#"{"data":{"q0":{"nodes":[{"number":7,"title":"a pull request","url":"u","isDraft":false,"author":{"login":"someone"},"headRefOid":"abc","updatedAt":"2026-08-01T00:00:00Z","latestReviews":{"nodes":[]},"reviewRequests":{"nodes":[]}}]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                                false => r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                            },
                        )
                    }
                    _ => (200, "{}".to_string()),
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
        (base, asked)
    }

    /// A rename lookup that failed is asked again — and one that answered is not.
    ///
    /// Both halves, in one test, because either alone is a bug. This is [`what_github_said`]'s
    /// rule asserted on the memo that had it wrong (SKEIN-281): `renamed_to` swallowed a refusal
    /// into the same `None` it uses for "GitHub says this repo is still called that", and cached
    /// it for the life of the process. The failure is not hypothetical — the one repository the
    /// owner has the review queue switched on for was renamed, its registry entry still carried
    /// the old slug, and GitHub 504'd on it the day this was written. GitHub's SEARCH does not
    /// follow a rename the way its REST redirect does, so the queue reads 200 with zero results
    /// and renders empty, with no error and no blind spot — until somebody restarts skein.
    ///
    /// The second half is what stops the fix being "cache nothing": `Ok(None)` — a repository
    /// GitHub says was NOT renamed — is an answer, and must still be paid for only once, or every
    /// poll on every fleet buys a call per repo to be told nothing changed.
    #[test]
    fn a_rename_lookup_that_failed_is_asked_again_and_one_that_answered_is_not() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let down = std::sync::Arc::new(std::sync::Mutex::new(true));
        let (base, asked) = flaky_rename_github(down.clone());
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();
        forget_trunks();

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/old-name.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        // The refresh that lands while the lookup is refused. An empty queue here is honest:
        // skein was not told the new name, so it searched the one it holds.
        let during = queue(&repo, true).expect("a blind queue still answers");
        assert!(
            during.prs.is_empty(),
            "the fixture did not reproduce the outage: {:?}",
            asked.lock().unwrap()
        );

        // GitHub comes back.
        *down.lock().unwrap() = false;

        let after = queue(&repo, true).expect("a healthy GitHub answers");
        assert!(
            !after.prs.is_empty(),
            "one refused lookup was remembered as \"this repo was not renamed\", so the search \
             keeps asking a name GitHub matches against nothing and the queue reads empty until \
             the process restarts. Asked: {:?}",
            asked.lock().unwrap()
        );
        // And the recovered name is written down, exactly as it is on the path that never failed.
        let stored = crate::repos::load_repos()
            .into_iter()
            .find(|r| r.id == "demo")
            .map(|r| r.source)
            .unwrap_or_default();
        assert!(
            stored.contains("acme/new-name"),
            "the rename was recovered and not written down: {stored}"
        );

        // The other half. `acme/new-name` answers "still called that" — an answer, and remembered:
        // a second refresh must not pay for it again.
        let repo = crate::repos::load_repos().remove(0);
        let _settled = queue(&repo, true).expect("the queue answered under the new name");
        let asks_of_new = || {
            asked
                .lock()
                .unwrap()
                .iter()
                .filter(|path| path.as_str() == "/repos/acme/new-name")
                .count()
        };
        let paid = asks_of_new();
        assert!(paid >= 1, "nothing ever asked GitHub about the new name");
        let _again = queue(&repo, true).expect("and again");
        assert_eq!(
            asks_of_new(),
            paid,
            "\"not renamed\" stopped being remembered, so every poll now buys a call per repo to \
             be told nothing changed"
        );

        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("SKEIN_GITHUB_API");
        forget_host_token();
        forget_renames();
        forget_trunks();
    }

    /// **A queue inside the budget is served from memory; one outside it is fetched again**
    /// (SKEIN-314).
    ///
    /// The rule asserted where every caller meets it — [`queue_within`] — rather than on the
    /// helper underneath. That distinction is the whole item: the cache was skipped outright in
    /// this crate's unit tests, so nothing could put a queue that is OLD in front of a caller, and
    /// the 60s-vs-600s split between the pane and the badge poll had no test that could fail on
    /// it. [`CachedQueues`] is the seam; this is the first thing it makes sayable.
    ///
    /// Measured on the wire and not on the answer, because "did it refetch" is a request to
    /// GitHub. The planted queue carries a pull request the fake never serves, so the two cases
    /// are also told apart by what comes back: the seeded row on a hit, the fake's on a miss.
    #[test]
    fn a_queue_inside_the_budget_is_served_and_one_outside_it_is_refetched() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, asked) = routing_github();
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/new-name.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        let cache = CachedQueues::live();
        // Through serde, so fields this test has no opinion about keep their real defaults.
        let planted: Queue = serde_json::from_value(serde_json::json!({
            "repo_id": "demo",
            "slug": "acme/new-name",
            "viewer": "me",
            "ai": false,
            "prs": [{
                "number": 4242, "title": "planted", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "2026-08-01T00:00:00Z", "committed_at": "",
                "checks": "none", "my_review": "none", "review_is_current": false,
                "reasons": ["reviewer"], "lane": "needs-you", "box_name": "b"
            }],
            "blind_spots": [],
        }))
        .unwrap();
        let graphqls = || {
            asked
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.starts_with("/graphql"))
                .count()
        };

        // Ninety seconds old: inside the badge's ten minutes, outside the pane's sixty seconds.
        cache.stamped("demo", Duration::from_secs(90), &planted);
        let badge = queue_within(&repo, Duration::from_secs(600)).expect("the badge's read");
        assert_eq!(
            graphqls(),
            0,
            "a ninety-second-old queue cost a GitHub round trip on the ten-minute budget — that \
             spend is per repo, per open tab, every three minutes: {:?}",
            asked.lock().unwrap()
        );
        assert_eq!(
            badge.prs.first().map(|p| p.number),
            Some(4242),
            "the badge was served something other than the queue that was in the cache"
        );

        // The same entry, the same moment, read on the pane's budget: too old, so it is refetched.
        let pane = queue_within(&repo, Duration::from_secs(60)).expect("the pane's read");
        assert_eq!(
            graphqls(),
            1,
            "the pane's sixty seconds served a ninety-second-old answer as fresh: {:?}",
            asked.lock().unwrap()
        );
        assert!(
            pane.prs.iter().all(|p| p.number != 4242),
            "the refetched queue still carries the planted row, so nothing was actually refetched"
        );

        // And the refetch replaced the entry, so the badge's next read is inside the budget again
        // — the write half of the cache, which was skipped in tests along with the read half.
        let after = queue_within(&repo, Duration::from_secs(600)).expect("the badge again");
        assert_eq!(
            graphqls(),
            1,
            "a queue built one line ago was not put in the cache: {:?}",
            asked.lock().unwrap()
        );
        assert!(after.prs.iter().all(|p| p.number != 4242));

        // Dropping the guard puts the process back as it was for every test that runs after this
        // one: the cache off, and nothing this test planted left in it.
        drop(cache);
        assert!(
            unexpired_within("demo", Duration::from_secs(600)).is_none(),
            "the guard left this test's queue in a process-global cache"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
        forget_trunks();
    }

    /// The TTL rule the badge rides on: a remembered in-process queue is served only while it is
    /// younger than the caller's age budget.
    ///
    /// Asserted on `unexpired_within` directly: the rule itself, in isolation, with no GitHub and
    /// no repo. `a_queue_inside_the_budget_is_served_and_one_outside_it_is_refetched` asks the
    /// same question of [`queue_within`], which is the function every caller actually reaches.
    #[test]
    fn an_in_process_queue_is_served_only_within_the_callers_age_budget() {
        // The cache is a process-wide static; the env lock is this file's serialization for those.
        let _g = crate::testutil::env_lock();
        // Through serde like the other fixtures here, so fields this test does not care about keep
        // their real defaults.
        let remembered: Queue = serde_json::from_value(serde_json::json!({
            "repo_id": "ttl-probe",
            "slug": "acme/ttl",
            "viewer": "me",
            "ai": false,
            "prs": [],
            "blind_spots": [],
        }))
        .unwrap();
        let stamp = |age: Duration| {
            let at = Instant::now()
                .checked_sub(age)
                .expect("this host has been up longer than eleven minutes");
            QUEUE_CACHE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(HashMap::new)
                .insert("ttl-probe".to_string(), (at, remembered.clone()));
        };

        // Nine minutes old: young enough for the badge's ten minutes, far too old for the pane.
        stamp(Duration::from_secs(9 * 60));
        assert!(
            unexpired_within("ttl-probe", Duration::from_secs(600)).is_some(),
            "a nine-minute-old queue is within a ten-minute budget and was going to be refetched"
        );
        assert!(
            unexpired_within("ttl-probe", Duration::from_secs(60)).is_none(),
            "the pane's sixty seconds served a nine-minute-old answer as fresh"
        );

        // Eleven minutes old: past even the badge's budget.
        stamp(Duration::from_secs(11 * 60));
        assert!(
            unexpired_within("ttl-probe", Duration::from_secs(600)).is_none(),
            "an eleven-minute-old queue outlived the ten-minute budget"
        );

        invalidate("ttl-probe");
    }

    /// The badge reads through the ten-minute budget, not the pane's sixty seconds.
    ///
    /// Asserted against the source, the way `nothing_here_shells_out_to_gh` is. What this pins is
    /// the call itself — pointing `counts` back at `queue(&repo, false)` is the regression that
    /// rebuilt every repo's queue through a 60s cache every three minutes per open tab, and it is
    /// exactly what this fails on. That the two budgets then behave differently is no longer taken
    /// on trust either: since SKEIN-314 it is measured against `queue_within` in
    /// `a_queue_inside_the_budget_is_served_and_one_outside_it_is_refetched`.
    #[test]
    fn the_badge_reads_through_a_ten_minute_budget() {
        let source = std::fs::read_to_string(file!()).expect("this file");
        let counts = source
            .split("pub fn counts()")
            .nth(1)
            .and_then(|after| after.split("\npub fn ").next())
            .expect("counts() is in this file");
        assert!(
            counts.contains("queue_within(&repo, Duration::from_secs(600))"),
            "counts() no longer reads through the ten-minute budget:\n{counts}"
        );
        assert!(
            !counts.contains("queue(&repo,"),
            "counts() went back to the sixty-second path the badge was rate-limited on:\n{counts}"
        );
    }

    /// A GitHub whose repository answer carries a default branch, recording every path asked.
    ///
    /// Beside [`routing_github`] rather than folded into it: that one exists to tell a rename from
    /// an empty repository, and this one exists to count how often `/repos/<slug>` is paid for.
    fn trunk_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                if length > 0 {
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).ok();
                }
                recorder.lock().unwrap().push(path.clone());
                let answer = match path.as_str() {
                    "/user" => r#"{"login":"me"}"#.to_string(),
                    p if p.starts_with("/user/teams") => "[]".to_string(),
                    "/repos/acme/trunky" => {
                        r#"{"full_name":"acme/trunky","default_branch":"main"}"#.to_string()
                    }
                    // The batched wire: every alias answers, or the miss reads as a blind spot.
                    "/graphql" => {
                        r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                            .to_string()
                    }
                    _ => "{}".to_string(),
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
        (format!("http://127.0.0.1:{port}"), asked)
    }

    /// A refresh fills the repo's trunk, and pays for the lookup once per process.
    #[test]
    fn a_refresh_learns_the_trunk_once_and_remembers_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, asked) = trunk_github();
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();
        forget_trunks();

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "trunky",
            "source": "https://github.com/acme/trunky.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        let first = queue(&repo, true).expect("the queue answered");
        assert_eq!(
            first.trunk, "main",
            "the refresh did not learn the repository's default branch"
        );
        let repo_asks = || {
            asked
                .lock()
                .unwrap()
                .iter()
                .filter(|path| path.as_str() == "/repos/acme/trunky")
                .count()
        };
        let after_first = repo_asks();
        assert!(after_first >= 1, "nothing ever asked GitHub for the repo");

        let second = queue(&repo, true).expect("the queue answered again");
        assert_eq!(second.trunk, "main", "the remembered trunk was dropped");
        assert_eq!(
            repo_asks(),
            after_first,
            "a second refresh paid for the trunk lookup again instead of remembering it"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
        forget_trunks();
    }

    /// The host uses the credential you already gave it, and never asks for another.
    ///
    /// This replaces a test about `gh`'s keyring, because the keyring is no longer reachable from
    /// here: the queue talks to the API with a token skein holds. What survives is the property
    /// that mattered — one credential the user chose, doing every job it is capable of — and it is
    /// now asserted on the wire rather than on a subprocess's environment.
    /// The host's own `gh` login counts as a credential the user already gave skein.
    ///
    /// It did not, and the contradiction was visible in one `skein doctor`: `gh secret seeded` and
    /// `boxes push with this account's gh token` three lines above `github token none`, with every
    /// review queue answering 502. Skein was reading that login to put a credential in front of
    /// every box and refusing to read it to answer "who are you".
    ///
    /// Driven with a stub `gh` on PATH, because the property is that the CLI is ASKED — a test that
    /// injected the token would pass on a version that never ran anything.
    #[test]
    fn the_hosts_gh_login_is_the_last_credential_tried_and_it_is_tried() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) = fake_github(r#"{"login":"prateek"}"#);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("gh"),
            "#!/usr/bin/env bash\n[ \"$1 $2\" = \"auth token\" ] || exit 1\necho gho_from_the_cli\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        // Nothing stored anywhere: the state a fleet is in when it has only ever been set up with
        // `gh auth login`, which is the commonest way there is.
        forget_host_token();
        assert_eq!(viewer().unwrap().0, "prateek");
        assert_eq!(
            host_token_source(),
            GhToken::GhCli,
            "the host has a `gh` login and the queue still reports no credential"
        );
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|h| h == "bearer gho_from_the_cli"),
            "the CLI's token never reached GitHub: {:?}",
            seen.lock().unwrap()
        );

        // And it is LAST. Asking `gh` can unlock a system keyring, so anything already stored has
        // to win — otherwise every board poll pays for a credential skein was already holding.
        crate::gitgate::set_read_pat("github_pat_read").unwrap();
        forget_host_token();
        seen.lock().unwrap().clear();
        assert_eq!(viewer().unwrap().0, "prateek");
        assert_eq!(
            host_token_source(),
            GhToken::ReadToken,
            "the `gh` CLI was asked while a stored token was sitting right there"
        );

        std::env::set_var("PATH", path);
        crate::gitgate::set_read_pat("").unwrap();
        forget_host_token();
    }

    #[test]
    fn the_host_reads_github_with_the_credential_you_already_gave_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) = fake_github(r#"{"login":"prateek"}"#);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // A read token: the credential someone stores when they want cross-repo reads without an App.
        crate::gitgate::set_read_pat("github_pat_read").unwrap();
        forget_host_token();
        assert_eq!(viewer().unwrap().0, "prateek");
        assert_eq!(host_token_source(), GhToken::ReadToken);
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|h| h == "bearer github_pat_read"),
            "the token the user chose never reached GitHub: {:?}",
            seen.lock().unwrap()
        );

        // And with only a per-repo write token stored: it belongs to a person too, so it can say
        // who that person is. Nothing else is asked for.
        crate::gitgate::set_read_pat("").unwrap();
        crate::gitgate::set_write_credential("mine", "mine", &["me/repo".into()]).unwrap();
        crate::gitgate::set_credential_token("mine", "github_pat_write").unwrap();
        forget_host_token();
        seen.lock().unwrap().clear();
        assert_eq!(viewer().unwrap().0, "prateek");
        assert_eq!(host_token_source(), GhToken::WritePat);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|h| h == "bearer github_pat_write"));

        // The environment wins over both, for headless and CI.
        std::env::set_var("GH_TOKEN", "gho_exported");
        forget_host_token();
        seen.lock().unwrap().clear();
        let _ = viewer();
        assert_eq!(host_token_source(), GhToken::Environment);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|h| h == "bearer gho_exported"));

        // With nothing at all, the queue says what is missing — and says which credential cannot
        // cover it, because an App is the one path that genuinely cannot.
        std::env::remove_var("GH_TOKEN");
        let bare = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", bare.as_ref() as &std::path::Path);
        forget_host_token();
        let why = viewer().expect_err("no token, no queue");
        assert!(
            why.contains("GH_TOKEN") && why.contains("read token"),
            "{why}"
        );
        assert!(
            why.contains("App"),
            "the one path that cannot do this: {why}"
        );

        std::env::remove_var("SKEIN_GITHUB_API");
        std::env::remove_var("SKEIN_HOME");
        forget_host_token();
    }

    /// GraphQL's nesting, flattened into what the parser has always read.
    ///
    /// The load-bearing part of the port: `gh --json` gave `latestReviews` as a bare array and
    /// `statusCheckRollup` on the pull request, while GraphQL gives connections and hangs the rollup
    /// off the last commit. Everything downstream — lanes, "is your approval current", the check
    /// summary — reads those two keys, so this is the seam where a port either preserves behaviour
    /// or silently changes it.
    #[test]
    fn a_graphql_pull_request_reads_as_the_one_the_parser_knows() {
        let node = item(
            r#"{
              "number": 7, "title": "t", "url": "u", "isDraft": false,
              "updatedAt": "2026-08-18T00:00:00Z",
              "headRefName": "feat", "headRefOid": "abc", "baseRefName": "main",
              "reviewDecision": "REVIEW_REQUIRED",
              "author": {"login": "someone"},
              "latestReviews": {"nodes": [
                {"state": "APPROVED", "author": {"login": "me"}, "commit": {"oid": "abc"}}
              ]},
              "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
                {"status": "COMPLETED", "conclusion": "SUCCESS"}
              ]}}}}]}
            }"#,
        );
        let flat = shape(&node);

        assert!(flat.get("latestReviews").unwrap().is_array(), "{flat}");
        assert!(flat.get("statusCheckRollup").unwrap().is_array(), "{flat}");
        assert!(flat.get("commits").is_none(), "the nesting is gone: {flat}");
        // Identity for everything else, which is what made this a translation and not a rewrite.
        assert_eq!(flat.get("headRefOid").unwrap(), "abc");

        // And the parsers that read those two keys still agree with what they always said.
        assert_eq!(
            my_review_state(&flat, "me", "abc"),
            ("approved".into(), true)
        );
        assert_eq!(rollup(&flat), "passing");
    }

    /// **The conversation shapes survive the wire, and the one that costs money is not on it**
    /// (SKEIN-301).
    ///
    /// Three things a pull request carries that `review_decision` cannot say: which review threads
    /// are open, what was said on the pull request itself, and **who** still owes an approval.
    /// Each is asserted through the real path — GitHub's nesting, [`shape`]'s flattening,
    /// [`build_pr`]'s parse — because every one of those three is a place a field can be fetched
    /// and then dropped, and a dropped field looks exactly like a pull request with nothing open
    /// on it.
    ///
    /// The fourth assertion is the expensive one, and it is about what is NOT here: an inline
    /// thread's comment bodies. The panel draws a thread as who, when, a link and a resolve button
    /// — never its text — and [`PR_FRAGMENT`] travels once per pull request for up to
    /// [`SEARCH_PAGE`] of them per membership rule. SKEIN-287 cut this payload from 155 KB to
    /// 12 KB; a body added here is a body multiplied by a hundred, on the request
    /// `acme/thing` already answers with a 504 (SKEIN-278).
    #[test]
    fn a_pull_request_carries_its_threads_its_comments_and_who_still_owes_a_review() {
        let node = item(
            r#"{
              "number": 7, "title": "t", "url": "u", "isDraft": false,
              "updatedAt": "2026-08-18T00:00:00Z",
              "headRefName": "feat", "headRefOid": "abc", "baseRefName": "main",
              "author": {"login": "someone"},
              "latestReviews": {"nodes": []},
              "reviewRequests": {"totalCount": 2, "nodes": [
                {"requestedReviewer": {"login": "alice"}},
                {"requestedReviewer": {"slug": "core", "organization": {"login": "acme"}}},
                {"requestedReviewer": {"somethingElse": true}}
              ]},
              "reviewThreads": {"totalCount": 9, "nodes": [
                {"id": "PRRT_1", "isResolved": false, "isOutdated": true,
                 "comments": {"nodes": [{"author": {"login": "bob"},
                                         "createdAt": "2026-08-17T09:00:00Z",
                                         "url": "https://github.com/acme/t/pull/7#discussion_r1"}]}},
                {"id": "PRRT_2", "isResolved": true, "isOutdated": false,
                 "comments": {"nodes": []}}
              ]},
              "comments": {"totalCount": 412, "nodes": [
                {"author": {"login": "carol"}, "body": "ship it",
                 "createdAt": "2026-08-18T10:00:00Z", "url": "https://github.com/acme/t/pull/7#issuecomment-1"}
              ]},
              "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": []}}}}]}
            }"#,
        );
        let pr = build_pr(
            &shape(&node),
            7,
            "me",
            "repo",
            &Reason::Reviewer,
            &[],
            &BTreeMap::new(),
        );

        // **Who owes an approval.** A person and a team, and the team is still a team: "waiting on
        // @alice" and "waiting on acme/core" are not the same sentence, so the two must not be
        // flattened into one list of names. A reviewer that is neither is dropped rather than
        // rendered as a blank row.
        assert_eq!(
            pr.review_requests,
            vec![
                ReviewRequest {
                    name: "alice".into(),
                    team: false
                },
                ReviewRequest {
                    name: "acme/core".into(),
                    team: true
                },
            ],
            "the outstanding reviewers did not survive the wire"
        );

        // **The threads**, with the id the resolve mutation needs, and with each thread's author,
        // timestamp and permalink taken from its FIRST comment — a `PullRequestReviewThread` has
        // none of the three of its own.
        assert_eq!(pr.review_threads.len(), 2, "{:?}", pr.review_threads);
        assert_eq!(
            pr.review_threads[0],
            ReviewThread {
                id: "PRRT_1".into(),
                resolved: false,
                outdated: true,
                author: "bob".into(),
                started_at: "2026-08-17T09:00:00Z".into(),
                url: "https://github.com/acme/t/pull/7#discussion_r1".into(),
            },
            "a thread reached the row without what the panel draws it from"
        );
        assert!(
            pr.review_threads[0].id != pr.review_threads[1].id
                && !pr.review_threads[1].id.is_empty(),
            "every thread needs its own id or it cannot be resolved from skein"
        );
        // A thread whose first comment GitHub did not return keeps its id and loses only the
        // sentence — absence stays absent, and the resolve button still works.
        assert_eq!(pr.review_threads[1].author, "");
        assert!(pr.review_threads[1].resolved);
        assert_eq!(
            pr.review_threads_total,
            Some(9),
            "the cap cut seven threads and the row cannot say so"
        );

        // **The comments**, bodies included, because these are the ones that get rendered.
        assert_eq!(pr.comments.len(), 1);
        assert_eq!(pr.comments[0].author, "carol");
        assert_eq!(pr.comments[0].body, "ship it");
        assert_eq!(pr.comments[0].created_at, "2026-08-18T10:00:00Z");
        assert!(pr.comments[0].url.contains("issuecomment"));
        assert_eq!(
            pr.comments_total,
            Some(412),
            "a four-hundred-comment pull request must be able to say so from ten of them"
        );

        // **And the thing that must NOT be asked for.** Read off the query itself: the inline
        // threads' selection carries no `body`, while the pull request's own comments do. Both
        // halves, because "no body anywhere" would pass the first and break the panel.
        let threads = PR_FRAGMENT
            .split("reviewThreads(")
            .nth(1)
            .and_then(|after| after.split("comments(last:").next())
            .expect("the fragment asks for review threads");
        assert!(
            !threads.contains("body"),
            "inline review-comment bodies are being fetched — SKEIN-287 cut this payload from \
             155 KB to 12 KB and PR_FRAGMENT is asked for up to {SEARCH_PAGE} pull requests at a \
             time: {threads}"
        );
        assert!(
            PR_FRAGMENT.contains(&format!(
                "comments(last: {PR_COMMENTS_FETCHED}) {{ totalCount nodes {{ author {{ login }} \
                 body createdAt url }} }}"
            )),
            "the pull request's own comments are rendered, so they must carry their bodies: {}",
            *PR_FRAGMENT
        );
        // The caps are the query's, not a doc comment's — one number, written once.
        assert!(
            PR_FRAGMENT.contains(&format!("reviewThreads(first: {REVIEW_THREADS_FETCHED})"))
                && PR_FRAGMENT
                    .contains(&format!("reviewRequests(first: {REVIEW_REQUESTS_FETCHED})")),
            "a cap in the doc that the query does not apply is not a cap: {}",
            *PR_FRAGMENT
        );
        // SKEIN-354: the verdict comes from the connection that only carries verdicts. Without this
        // line in the query, `my_review_state` falls back to `latestReviews` and a note left after
        // an approval demotes it — silently, and identically to never having decided.
        assert!(
            PR_FRAGMENT.contains(&format!(
                "latestOpinionatedReviews(first: {REVIEWS_FETCHED})"
            )),
            "the query stopped asking which reviews DECIDED something: {}",
            *PR_FRAGMENT
        );
    }

    /// **A queue remembered by an older skein still parses** — the failure that turns a new field
    /// into an empty pane (prq.rs's `settled` field says it first, and it has been true of every
    /// field added since).
    ///
    /// Not a claim about `serde(default)` attributes: this is the actual JSON an older skein wrote,
    /// parsed by today's [`Queue`]. Missing the whole conversation, it comes back as a queue with
    /// the pull request in it and nothing said about threads or comments — which is honestly what
    /// that skein knew. `None` totals rather than `Some(0)`, because zero would be a claim.
    #[test]
    fn a_remembered_queue_written_before_the_conversation_existed_still_parses() {
        let older = r#"{
          "repo_id": "r", "slug": "acme/thing", "viewer": "me", "ai": true,
          "blind_spots": [], "as_of": "2026-08-01T00:00:00Z",
          "prs": [{
            "number": 4, "title": "t", "author": "someone", "url": "u",
            "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
            "draft": false, "updated_at": "2026-08-01T00:00:00Z", "committed_at": "",
            "checks": "none", "my_review": "none", "review_is_current": false,
            "reasons": ["reviewer"], "lane": "needs-you", "box_name": "b"
          }]
        }"#;
        let q: Queue = serde_json::from_str(older).expect(
            "a queue remembered before these fields existed no longer parses — every row in it \
             disappears, which is a blank pane rather than a missing line",
        );
        let pr = &q.prs[0];
        assert!(pr.review_threads.is_empty() && pr.comments.is_empty());
        assert!(pr.review_requests.is_empty());
        assert_eq!(
            (pr.review_threads_total, pr.comments_total),
            (None, None),
            "an older queue knew nothing about the counts, and `Some(0)` would be a claim"
        );
        // The same rule for the two counts added after it (SKEIN-373, SKEIN-356), and the same
        // reason: `Some(0)` here would say "GitHub has no labels on this and nobody has approved
        // it", which is a claim nobody made. What each absence then MEANS is decided where it is
        // read — `labels_whole` reads it as whole, `prwork::facts_of` falls back to your own
        // review — and both of those are the answer that queue was already giving.
        assert_eq!(
            (pr.labels_total, pr.standing_approvals),
            (None, None),
            "a queue remembered before these counts existed was given them anyway"
        );
        assert!(
            pr.labels_whole(),
            "a remembered queue started reporting every row's labels as short, which stops every \
             `no-label:` a workflow asks about on evidence nobody has"
        );
        // And the pair added by SKEIN-386, which is the same rule a third time: an older queue was
        // never told how many reviews there were, and `Some(0)` here would say GitHub counted none.
        assert_eq!(
            (pr.reviews_total, pr.reviews_read),
            (None, None),
            "a queue remembered before the review counts existed was given them anyway"
        );
        assert!(
            pr.reviews_whole(),
            "a remembered queue started reporting every row's reviews as cut off, so every row \
             carries a blind spot about a truncation nobody has evidence of"
        );
    }

    /// What the conversation costs on the wire, measured rather than asserted (SKEIN-301).
    ///
    /// [`PR_FRAGMENT`] travels once per pull request, up to [`SEARCH_PAGE`] of them per membership
    /// rule, in one request — the request `acme/thing` already answers with a 504
    /// (SKEIN-278). So "does this make it worse" is a number, and the number is built here from a
    /// stated profile rather than from a guess: **54 pull requests, each with 2 review threads,
    /// 3 PR comments of 120 characters, and 1 outstanding reviewer.** That is the owner's own
    /// queue size (SKEIN-301's brief) with a conversation load a busy repo would recognise.
    ///
    /// The ceiling is what the test enforces. It is deliberately loose — the point is not the exact
    /// byte count, which moves with every field anybody adds, but that this change stays in the
    /// same order of magnitude as the answer it grew from. The measured numbers go in the item.
    ///
    /// **Every cap is saturated, `LABELS_FETCHED` among them since SKEIN-373.** That cap is why
    /// the answer to a label list that overflows is to say so rather than to ask for GitHub's
    /// hundred: at twenty the worst case sits a few thousand bytes under the ceiling below, so a
    /// hundred does not fit and no rearrangement of the other two makes it fit.
    ///
    /// **The worst case has a ceiling too, since SKEIN-316.** Both numbers were printed and only
    /// the profile one was checked, so the number the 504 is actually about — every pull request
    /// saturating both caps, a whole page of them — could be tripled by a cap nobody re-measured
    /// and the test would still pass. It is the per-ALIAS figure that carries the ceiling because
    /// that is what a cap multiplies; a refresh sends five of these in one request.
    #[test]
    fn the_conversation_is_measured_against_the_answer_it_grew_from() {
        let thread = |n: usize| {
            format!(
                r#"{{"id":"PRRT_kwDOAbCdEf4A{n:04}","isResolved":false,"isOutdated":false,"comments":{{"nodes":[{{"author":{{"login":"reviewer"}},"createdAt":"2026-08-18T09:00:00Z","url":"https://github.com/acme/thing/pull/{n}#discussion_r1234567890"}}]}}}}"#
            )
        };
        let comment = |n: usize| {
            format!(
                r#"{{"author":{{"login":"someone"}},"body":"{body}","createdAt":"2026-08-18T10:00:00Z","url":"https://github.com/acme/thing/pull/{n}#issuecomment-1234567890"}}"#,
                body = "x".repeat(120)
            )
        };
        // What the two SKEIN-301 figures were measured with, unchanged so they stay comparable
        // with the numbers in that item: one short label, and no count beside it.
        const ONE_LABEL: &str = r#""nodes":[{"name":"ready"}]"#;
        // A pull request's labels, as a page of `count` of them (SKEIN-373). The names are the
        // fixture's own — `acme/testbed#20` labels by area, which is what put 22 on
        // one pull request — so the worst case is measured against a real naming scheme rather
        // than a short word chosen to flatter the number.
        let labels = |count: usize| {
            let nodes = (1..=count)
                .map(|i| format!(r#"{{"name":"area/mod-{i:02}"}}"#))
                .collect::<Vec<_>>()
                .join(",");
            format!(r#""totalCount":{count},"nodes":[{nodes}]"#)
        };
        let base = |n: usize, labels: &str| {
            format!(
                r#""number":{n},"title":"a change to something","url":"https://github.com/acme/thing/pull/{n}","isDraft":false,"updatedAt":"2026-08-18T10:00:00Z","headRefName":"feat-{n}","headRefOid":"0123456789abcdef0123456789abcdef01234567","baseRefName":"main","reviewDecision":"REVIEW_REQUIRED","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","additions":120,"deletions":30,"changedFiles":4,"labels":{{{labels}}},"author":{{"login":"someone"}},"latestReviews":{{"nodes":[]}},"commits":{{"nodes":[{{"commit":{{"committedDate":"2026-08-18T09:00:00Z","statusCheckRollup":{{"state":"SUCCESS","contexts":{{"totalCount":3,"nodes":[{{"name":"build","detailsUrl":"https://ci/1","status":"COMPLETED","conclusion":"SUCCESS"}}]}}}}}}}}]}}"#
            )
        };
        let before: String = (1..=54)
            .map(|n| format!("{{{}}}", base(n, ONE_LABEL)))
            .collect::<Vec<_>>()
            .join(",");
        let after: String = (1..=54)
            .map(|n| {
                format!(
                    r#"{{{base},"reviewRequests":{{"totalCount":1,"nodes":[{{"requestedReviewer":{{"login":"alice"}}}}]}},"reviewThreads":{{"totalCount":2,"nodes":[{t1},{t2}]}},"comments":{{"totalCount":3,"nodes":[{c},{c},{c}]}}}}"#,
                    base = base(n, ONE_LABEL),
                    t1 = thread(n),
                    t2 = thread(n + 100),
                    c = comment(n),
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        // And the worst case the caps allow, which is the number the 504 risk is actually about:
        // every pull request saturating every cap, in a page of `SEARCH_PAGE` rather than 54.
        // `LABELS_FETCHED` is one of them since SKEIN-373 — the cheapest node in the fragment, and
        // still 20 of them on 100 pull requests, which is what makes "just ask for a hundred" a
        // measurable answer rather than an opinion.
        let saturated: String = (1..=SEARCH_PAGE)
            .map(|n| {
                let threads = (0..REVIEW_THREADS_FETCHED)
                    .map(|i| thread(n + i * 1000))
                    .collect::<Vec<_>>()
                    .join(",");
                let comments = (0..PR_COMMENTS_FETCHED)
                    .map(|_| comment(n))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{{base},"reviewRequests":{{"totalCount":1,"nodes":[{{"requestedReviewer":{{"login":"alice"}}}}]}},"reviewThreads":{{"totalCount":{tc},"nodes":[{threads}]}},"comments":{{"totalCount":{cc},"nodes":[{comments}]}}}}"#,
                    base = base(n, &labels(LABELS_FETCHED)),
                    tc = REVIEW_THREADS_FETCHED,
                    cc = PR_COMMENTS_FETCHED,
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let (was, now) = (before.len(), after.len());
        println!("SKEIN-301 payload for 54 pull requests: {was} bytes -> {now} bytes");
        println!(
            "SKEIN-301 worst case, {SEARCH_PAGE} pull requests at every cap: {} bytes",
            saturated.len()
        );
        // Both parse, which is what makes the two numbers comparable rather than two strings.
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&format!("[{after}]")).expect("the after shape is real JSON");
        assert_eq!(parsed.len(), 54);
        assert!(
            now < was * 4,
            "the conversation more than quadrupled the answer ({was} -> {now} bytes for 54 pull \
             requests) — PR_FRAGMENT travels for up to {SEARCH_PAGE} of them per membership rule, \
             and this is the request that already 504s (SKEIN-278)"
        );

        // **The worst case the caps allow, held under half a megabyte per alias** (SKEIN-316). A
        // refresh sends one of these per membership rule in ONE request, so this figure is a fifth
        // of what GitHub is asked to compute — and `acme/thing` already answers that with a
        // 504. The ceiling is a round number rather than the measurement plus a margin, so that
        // reading it says what is being defended instead of what today happens to cost; the
        // headroom under it is stated in the message, so a failure says how far past it went and
        // which lever SKEIN-316 names first.
        const WORST_CASE_CEILING: usize = 500_000;
        assert!(
            saturated.len() < WORST_CASE_CEILING,
            "the worst case the caps allow is {} bytes for ONE alias ({} for a five-alias \
             refresh), past the {WORST_CASE_CEILING}-byte ceiling — this is the request \
             acme/thing answers with a 504 (SKEIN-278). The levers, in SKEIN-316's order: \
             PR_COMMENTS_FETCHED (now {PR_COMMENTS_FETCHED}, the only cap whose nodes carry \
             bodies), then REVIEW_THREADS_FETCHED (now {REVIEW_THREADS_FETCHED}), then \
             LABELS_FETCHED (now {LABELS_FETCHED}). Do NOT raise SEARCH_PAGE (now {SEARCH_PAGE}) — its own doc argues a bigger page trades a rare \
             truncation for a likelier outage",
            saturated.len(),
            saturated.len() * 5,
        );
    }

    /// **A label list cut off at its page is never reported as the whole set** (SKEIN-373).
    ///
    /// The defect, measured against `acme/testbed#20`: GitHub's REST answer carries
    /// 22 labels and the queue payload carried 20 (`area/mod-01` … `area/mod-20`). The query asked
    /// `labels(first: 20)` with no `totalCount`, so the two that were dropped left no trace in the
    /// row, in the payload, or in the blind spots — and [`Pr::labels`] is what the merge train
    /// decides on, so a `hold` sorting past the twentieth stopped holding anything.
    ///
    /// The assertion is the RULE and not the measured pair: a list is whole exactly when every
    /// label GitHub counted arrived. A test that only checked 22-against-20 would pass with the
    /// truncation restored on any other number.
    #[test]
    fn a_label_page_that_was_cut_off_is_never_reported_as_the_whole_set() {
        // The cap is the query's, written once — and it is asked for WITH GitHub's own count of
        // what it capped. Without the `totalCount` nothing downstream can tell a short list from a
        // complete one, which is the whole of the defect.
        assert!(
            PR_FRAGMENT.contains(&format!(
                "labels(first: {LABELS_FETCHED}) {{ totalCount nodes {{ name }} }}"
            )),
            "the labels connection is asked for without GitHub's count of them, so a pull request \
             with more than {LABELS_FETCHED} loses the rest and nothing anywhere can say so: {}",
            *PR_FRAGMENT
        );

        let row = |labels: serde_json::Value| {
            let node = serde_json::json!({
                "number": 20, "title": "t", "url": "u", "isDraft": false,
                "headRefName": "feat", "headRefOid": "abc", "baseRefName": "main",
                "author": { "login": "someone" },
                "latestReviews": { "nodes": [] },
                "labels": labels,
            });
            build_pr(
                &shape(&node),
                20,
                "me",
                "acme",
                &Reason::Author,
                &[],
                &BTreeMap::new(),
            )
        };
        let page = |total: u64, read: usize| {
            let nodes: Vec<serde_json::Value> = (1..=read)
                .map(|i| serde_json::json!({ "name": format!("area/mod-{i:02}") }))
                .collect();
            serde_json::json!({ "totalCount": total, "nodes": nodes })
        };

        for (total, read) in [
            (0, 0),
            (1, 1),
            (3, 3),
            (20, 20),
            (21, 20),
            (22, 20),
            (200, 20),
        ] {
            let pr = row(page(total, read));
            assert_eq!(pr.labels.len(), read, "the page itself is carried in full");
            assert_eq!(
                pr.labels_total,
                Some(total),
                "GitHub's count of the labels did not survive the trip to the row"
            );
            assert_eq!(
                pr.labels_whole(),
                read as u64 == total,
                "GitHub said {total} labels and {read} arrived — `labels_whole` disagrees with \
                 what that means, and it is what decides whether `no-label:` may be answered at all"
            );
        }

        // The measured case, named. Two labels nothing could see, and the row now says so.
        let cut = row(page(22, LABELS_FETCHED));
        assert!(
            !cut.labels_whole(),
            "a pull request GitHub says has 22 labels, whose row carries {LABELS_FETCHED}, is \
             claiming to carry all of them — the shape measured on acme/testbed#20"
        );
        assert!(
            !cut.labels.contains(&"area/mod-21".to_string()),
            "the fixture must actually be short, or this proves nothing"
        );

        // And an answer from before the count was asked for — every queue remembered on disk is in
        // this state. `None` rather than `Some(0)`: zero is the claim the truncation was making
        // silently, and nobody said it here. It reads as whole for the same reason `Queue::whole`
        // and `Pr::settled` default the way they do — what skein could not know must not start
        // holding back work it was already letting through.
        let older = row(serde_json::json!({ "nodes": [{ "name": "ci" }] }));
        assert_eq!(
            older.labels_total, None,
            "an answer that carried no count was given one"
        );
        assert!(
            older.labels_whole(),
            "a queue remembered before this field existed started reporting every pull request as \
             short, which stops every `no-label:` in the fleet on evidence nobody has"
        );
    }

    /// **A review list cut off at its page is never reported as the whole set** (SKEIN-386).
    ///
    /// The label defect wearing its second face, on the last two connections in [`PR_FRAGMENT`]
    /// that paged without saying how much they paged. Both are one review per author, so the case
    /// is a pull request more than [`REVIEWS_FETCHED`] *people* have reviewed — and what it cost
    /// could not be seen from the row: [`my_review_state`] finds no row of yours in the page and
    /// reports `"none"`, and [`standing_approvals`] counts the approvals that arrived and hands the
    /// merge train the total.
    ///
    /// The assertion is the RULE rather than the measured pair: whole exactly when every review
    /// GitHub counted arrived. A test pinned to 31-against-30 passes with the truncation restored
    /// on any other number.
    #[test]
    fn a_review_page_that_was_cut_off_is_never_reported_as_the_whole_set() {
        // The cap is the query's, written once — and asked for WITH GitHub's own count of what it
        // capped, on BOTH connections, because both are read.
        for connection in ["latestReviews", "latestOpinionatedReviews"] {
            assert!(
                PR_FRAGMENT.contains(&format!(
                    "{connection}(first: {REVIEWS_FETCHED}) {{ totalCount nodes {{ state"
                )),
                "`{connection}` is asked for without GitHub's count of the reviews on it, so a \
                 pull request with more than {REVIEWS_FETCHED} reviewers loses the rest on the way \
                 in and nothing — not the row, not the blind spots — can tell: {}",
                *PR_FRAGMENT
            );
        }

        const HEAD: &str = "abc";
        // `read` reviews arrived out of the `total` GitHub says there are, by other people, all
        // against the head that is there now.
        let page = |total: u64, read: usize, state: &str| {
            let nodes: Vec<serde_json::Value> = (1..=read)
                .map(|i| {
                    serde_json::json!({
                        "state": state,
                        "author": { "login": format!("reviewer-{i:02}") },
                        "commit": { "oid": HEAD },
                    })
                })
                .collect();
            serde_json::json!({ "totalCount": total, "nodes": nodes })
        };
        let row = |latest: serde_json::Value, opinionated: Option<serde_json::Value>| {
            let mut node = serde_json::json!({
                "number": 31, "title": "t", "url": "u", "isDraft": false,
                "headRefName": "feat", "headRefOid": HEAD, "baseRefName": "main",
                "author": { "login": "someone" },
                "latestReviews": latest,
            });
            if let Some(op) = opinionated {
                node["latestOpinionatedReviews"] = op;
            }
            build_pr(
                &shape(&node),
                31,
                "me",
                "acme",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new(),
            )
        };

        for (total, read) in [(0, 0), (1, 1), (30, 30), (31, 30), (40, 30), (200, 30)] {
            let pr = row(
                page(total, read, "APPROVED"),
                Some(page(total, read, "APPROVED")),
            );
            assert_eq!(
                (pr.reviews_total, pr.reviews_read),
                (Some(total), Some(read as u64)),
                "GitHub's count of the reviews, and how many arrived, did not survive the trip to \
                 the row"
            );
            assert_eq!(
                pr.reviews_whole(),
                read as u64 == total,
                "GitHub counted {total} reviews and {read} arrived — `reviews_whole` disagrees \
                 with what that means, and it is the only thing standing between a reviewer and a \
                 row that says `nobody has approved this` about a pull request skein simply did \
                 not read to the end of"
            );
        }

        // **The defect, named.** Thirty other people's reviews arrived and the thirty-first is
        // yours, so it is in neither page: your own approval is invisible, nobody's approval is
        // counted past the cap, and before this the row said both of those things as facts.
        let cut = row(
            page(31, REVIEWS_FETCHED, "APPROVED"),
            Some(page(31, REVIEWS_FETCHED, "APPROVED")),
        );
        assert_eq!(cut.my_review, "none");
        assert!(matches!(cut.lane, Lane::NeedsYou));
        assert!(
            !cut.reviews_whole(),
            "a pull request GitHub says 31 people have reviewed, whose row was built from \
             {REVIEWS_FETCHED} of them, is claiming it read every review — so the reviewer is \
             shown `my review: none` on a pull request they approved, `{}` standing approvals on \
             one the whole team approved, and no way to tell either from the truth",
            cut.standing_approvals.unwrap_or_default()
        );

        // **Whichever connection lost the most is the one reported**, because both are read and a
        // hole in either is a hole in the row's answers. The whole one must not cover for the cut
        // one — that is this defect with an extra step.
        let opinionated_cut = row(page(30, 30, "COMMENTED"), Some(page(35, 30, "APPROVED")));
        assert_eq!(
            (
                opinionated_cut.reviews_total,
                opinionated_cut.reviews_read,
                opinionated_cut.reviews_whole()
            ),
            (Some(35), Some(30), false),
            "one review connection arrived whole and the other was cut off, and the row reported \
             the whole one — the five reviews nobody read are exactly the ones the standing \
             approvals are counted from"
        );

        // And an answer from before the counts were asked for — every queue remembered on disk is
        // in this state. `None` rather than `Some(0)`, and whole for the reason `labels_whole`
        // gives: what skein could not know must not start holding back work it already let through.
        let older = row(serde_json::json!({ "nodes": [] }), None);
        assert_eq!(
            (older.reviews_total, older.reviews_read),
            (None, None),
            "an answer that carried no count was given one"
        );
        assert!(
            older.reviews_whole(),
            "an answer from before this was asked for started reporting every pull request's \
             reviews as cut off, so every row in the fleet carries a blind spot about a \
             truncation nobody has evidence of"
        );
    }

    /// **An approval is counted for everybody, not just for you — and only against the head that
    /// is there now** (SKEIN-356).
    ///
    /// [`Pr`] carried the repository's verdict and the viewer's own review and nothing else, so on
    /// a repository that requires no review — `reviewDecision` is `""` there — a third party's
    /// standing approval was invisible and `prwork::facts_of` had to read `approved` as false. The
    /// merge train then held approved work, for ever, on a repo where the owner is the author and
    /// somebody else reviews: the ordinary case rather than an edge.
    ///
    /// The load-bearing assertion is the last one: **this and [`Pr::my_review`] read the same two
    /// connections and may never disagree about the same person's review.** Everything else here
    /// is a case; that is the rule, and it is what keeps
    /// `prwork::tests::an_approval_is_still_an_approval_where_the_repository_asks_for_none`'s
    /// stale half true for everybody rather than only for the viewer.
    #[test]
    fn an_approval_from_anybody_is_counted_and_a_stale_one_is_not() {
        const HEAD: &str = "abc";
        let review = |state: &str, login: &str, at: &str| serde_json::json!({ "state": state, "author": { "login": login }, "commit": { "oid": at } });
        // `opinionated` is `None` for an answer that carried no such connection at all — the older
        // fixture, and the fallback `my_review_state` keeps for it.
        let row = |reviews: Vec<serde_json::Value>, opinionated: Option<Vec<serde_json::Value>>| {
            let mut node = serde_json::json!({
                "number": 7, "title": "t", "url": "u", "isDraft": false,
                "headRefName": "feat", "headRefOid": HEAD, "baseRefName": "main",
                "author": { "login": "someone" },
                "latestReviews": { "nodes": reviews },
            });
            if let Some(op) = opinionated {
                node["latestOpinionatedReviews"] = serde_json::json!({ "nodes": op });
            }
            build_pr(
                &shape(&node),
                7,
                "me",
                "acme",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new(),
            )
        };
        let old = "0000000000000000000000000000000000000000";

        let cases: Vec<(&str, Pr, Option<u64>)> =
            vec![
            ("nobody has looked at it", row(vec![], Some(vec![])), Some(0)),
            (
                "a third party approved the head that is there now — the case skein could not see",
                row(
                    vec![review("APPROVED", "alice", HEAD)],
                    Some(vec![review("APPROVED", "alice", HEAD)]),
                ),
                Some(1),
            ),
            (
                "your own approval, which was already visible, counts once and not twice",
                row(
                    vec![review("APPROVED", "me", HEAD)],
                    Some(vec![review("APPROVED", "me", HEAD)]),
                ),
                Some(1),
            ),
            (
                "two people approved it",
                row(
                    vec![],
                    Some(vec![
                        review("APPROVED", "alice", HEAD),
                        review("APPROVED", "bob", HEAD),
                    ]),
                ),
                Some(2),
            ),
            (
                "an approval left on a head that has since been pushed over is not standing",
                row(vec![], Some(vec![review("APPROVED", "alice", old)])),
                Some(0),
            ),
            (
                "a refusal and a note are not approvals",
                row(
                    vec![review("COMMENTED", "carol", HEAD)],
                    Some(vec![review("CHANGES_REQUESTED", "bob", HEAD)]),
                ),
                Some(0),
            ),
            (
                // GitHub drops a DISMISSED review from the opinionated connection and keeps it in
                // the other, which is why the count is taken from the opinionated one (SKEIN-354).
                "an approval GitHub has dismissed has stopped standing",
                row(
                    vec![
                        review("APPROVED", "alice", HEAD),
                        review("CHANGES_REQUESTED", "bob", HEAD),
                    ],
                    Some(vec![review("CHANGES_REQUESTED", "bob", HEAD)]),
                ),
                Some(0),
            ),
            (
                // No opinionated connection at all: the same fallback `my_review_state` makes, so
                // an answer from before SKEIN-354 reads exactly as it always did.
                "an older answer with only `latestReviews` still counts what it has",
                row(vec![review("APPROVED", "alice", HEAD)], None),
                Some(1),
            ),
        ];

        for (what, pr, expected) in &cases {
            assert_eq!(
                &pr.standing_approvals, expected,
                "{what}: the standing approvals were counted wrong"
            );
            // **The rule.** Two readers of the same two connections, and the one thing they may
            // never do is disagree about a review both of them saw.
            if pr.my_review == "approved" && pr.review_is_current {
                assert!(
                    pr.standing_approvals.unwrap_or(0) >= 1,
                    "{what}: your own approval is standing and the count of standing approvals \
                     does not include it — the two fields are reading the same connections and \
                     have come apart"
                );
            }
            if pr.standing_approvals == Some(0) {
                assert!(
                    !(pr.my_review == "approved" && pr.review_is_current),
                    "{what}: nothing is standing, and yet your own approval is — same two fields, \
                     same two connections, opposite answers"
                );
            }
        }

        // And without a head there is nothing for an approval to stand against, so nothing is
        // counted rather than nought being claimed.
        let headless = build_pr(
            &shape(&serde_json::json!({
                "number": 8, "title": "t", "url": "u", "isDraft": false,
                "headRefName": "feat", "baseRefName": "main",
                "author": { "login": "someone" },
                "latestReviews": { "nodes": [review("APPROVED", "alice", HEAD)] },
            })),
            8,
            "me",
            "acme",
            &Reason::Reviewer,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(
            headless.standing_approvals, None,
            "with no head sha every approval was judged against an empty string, and the nought \
             that comes out of that is skein's own blindness reported as a fact about the reviews"
        );
    }

    /// Nothing in skein runs `gh` any more.
    ///
    /// The queue was built out of the CLI, which made a third-party binary a hard requirement of a
    /// default-on feature — announced nowhere, met as a bug — and dragged in its credential store:
    /// `gh` keeps its token in the system keyring on Linux, so every call was a keyring read and a
    /// locked keyring answered each with a password dialog, every three minutes, for ever.
    ///
    /// Asserted against the source because the way it comes back is a single convenient call in a
    /// module that has no other reason to think about it — the same shape as the bypass that made
    /// most of the earlier keyring fix a no-op.
    #[test]
    fn nothing_here_shells_out_to_gh() {
        let mut offenders = Vec::new();
        for file in std::fs::read_dir("src").expect("src") {
            let path = file.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // The one legitimate `gh`: seeding the *account token* into sbx so boxes can push. That
            // path is about `gh`'s own login by definition, it is opt-in, and it is not this — the
            // queue's dependency was the hidden one.
            if path.ends_with("repos.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            // Production code only: this test names `gh` in its own strings.
            let source = source.split("\nmod tests {").next().unwrap_or_default();
            for (n, line) in source.lines().enumerate() {
                let runs_gh = line.contains("Command::new(\"gh\")")
                    || line.contains("run_capture(\"gh\"")
                    || line.contains("run_capture_for(\"gh\"")
                    || line.contains("gh_bin()");
                if runs_gh {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "skein reads GitHub over its own API client; these run the CLI instead:\n{}",
            offenders.join("\n")
        );
    }

    fn item(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    /// A `Repo` through serde, so fields this test does not care about keep their real defaults.
    fn repo_at(id: &str, work: &std::path::Path, queue_on: bool) -> Repo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "source": work.to_string_lossy(),
            "work": work.to_string_lossy(),
            "store": "",
            "review_queue": queue_on,
        }))
        .unwrap()
    }

    /// A repo skein did not look at must say so, rather than vanishing from the list.
    ///
    /// This is the bug the whole `skipped` field exists for: both states below produced a count list
    /// that simply did not mention the repo, so the badge showed nothing — identical to a queue with
    /// nothing waiting in it. Someone whose only repo had its queue switched off, or whose clone had
    /// no GitHub remote, saw a clean board while PRs piled up on GitHub, with nowhere to find out why.
    #[test]
    fn a_repo_that_was_never_asked_says_so_instead_of_disappearing() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let root = home.as_ref() as &std::path::Path;
        // SAFETY: guarded by the crate-wide env lock, as every $SKEIN_HOME test is.
        unsafe { std::env::set_var("SKEIN_HOME", root) };

        // A git repo with a GitHub origin, so only the *switch* is what stops it being asked.
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:acme/thing.git",
        ]);

        // And one with no remote at all, which cannot have pull requests.
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&bare)
            .output()
            .unwrap();

        crate::repos::save_repos(&[
            repo_at("queue-off", &work, false),
            repo_at("no-remote", &bare, true),
        ])
        .unwrap();

        let counts = counts();
        let of = |id: &str| {
            counts
                .iter()
                .find(|c| c.repo_id == id)
                .unwrap_or_else(|| panic!("{id} is missing from the counts entirely: {counts:?}"))
        };
        assert!(
            of("queue-off").skipped.contains("switched off"),
            "a repo with the queue off must name that, not read as an empty queue: {:?}",
            of("queue-off")
        );
        assert!(
            of("no-remote").skipped.contains("no GitHub remote"),
            "and a repo with nowhere to look must say which: {:?}",
            of("no-remote")
        );
        // Neither is a *fault* — the badge paints `error` red, and being switched off is a choice.
        for id in ["queue-off", "no-remote"] {
            assert!(of(id).error.is_empty(), "{id} is not broken");
            assert_eq!(of(id).needs_you, 0);
        }
        // Nothing reached the network: `gh` is never invoked for a repo that was not asked, which is
        // what makes reporting them free rather than three round trips each.
        unsafe { std::env::remove_var("SKEIN_HOME") };
    }

    /// The badge's number and what the queue could not see travel together (SKEIN-239).
    ///
    /// Both halves of this test report `needs_you: 0` with no `error` and nothing `skipped`. The
    /// ONLY thing telling "nothing is waiting on you" apart from "skein could not look at the
    /// searches where something might be waiting" is the blind spots — and `counts()` used to drop
    /// them on the floor, so the two were the same integer. On the owner's fleet the second half
    /// is the everyday state: no `read:org`, so the `team-review-requested:` searches are never
    /// issued and every team-requested PR is absent from the count with nothing saying so.
    #[test]
    fn a_count_carries_what_its_queue_could_not_see() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::repos::save_repos(&[batched_repo("acme/thing")]).unwrap();

        // Teams listable, every search answering: a genuinely empty queue. Five aliases, because
        // the team the viewer belongs to adds its own.
        let (base, _seen) = batched_github(
            true,
            200,
            r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]},"q4":{"nodes":[]}}}"#
                .to_string(),
        );
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let whole = counts();
        let whole = whole
            .iter()
            .find(|c| c.repo_id == "thing")
            .expect("counted");
        assert_eq!(whole.needs_you, 0);
        assert!(
            whole.blind_spots.is_empty(),
            "a queue that saw everything and found nothing must carry NO blind spot, or the badge              can never draw a plain zero: {:?}",
            whole.blind_spots
        );

        // Same repo, same empty answers — but the token cannot list teams, so a whole class of
        // pull request was never searched for.
        let (base, _seen) = batched_github(
            false,
            200,
            r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                .to_string(),
        );
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let blind = counts();
        let blind = blind
            .iter()
            .find(|c| c.repo_id == "thing")
            .expect("counted");
        assert_eq!(
            blind.needs_you, 0,
            "the count is still zero — that is the whole problem, and why the zero needs company"
        );
        assert!(
            blind.error.is_empty() && blind.skipped.is_empty(),
            "neither existing field fires here, which is how this stayed invisible: {blind:?}"
        );
        assert!(
            blind
                .blind_spots
                .iter()
                .any(|b| b.contains("team review requests")),
            "the count must say it could not see team review requests: {:?}",
            blind.blind_spots
        );

        // The wire contract the badge reads, pinned by name: the cockpit renders from this JSON,
        // so a renamed field is a badge that silently goes back to a bare number.
        let on_the_wire = serde_json::to_value(blind).unwrap();
        assert!(
            on_the_wire["blind_spots"]
                .as_array()
                .is_some_and(|b| !b.is_empty()),
            "blind_spots must reach the client under that name: {on_the_wire}"
        );
        assert!(
            serde_json::to_value(whole)
                .unwrap()
                .get("blind_spots")
                .is_none(),
            "and a whole count must not carry an empty array — absent is what lets the page tell \
             `nothing missing` from `this skein is too old to say`"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// The rate-limited half, which is the one the owner hit: GitHub answers 200 carrying
    /// `RATE_LIMITED`, so the whole batched request fails, `queue_within` still returns `Ok` with
    /// an empty list, and the badge showed a confident zero — then served it from the ten-minute
    /// cache for the next ten minutes.
    #[test]
    fn a_rate_limited_count_is_not_reported_as_an_empty_queue() {
        let _lock = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::repos::save_repos(&[batched_repo("acme/thing")]).unwrap();

        let (base, _seen) = batched_github(
            false,
            200,
            r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 123"}]}"#
                .to_string(),
        );
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let counted = counts();
        let counted = counted
            .iter()
            .find(|c| c.repo_id == "thing")
            .expect("counted");
        assert_eq!(counted.needs_you, 0);
        assert!(
            counted.error.is_empty(),
            "the queue returned Ok, so `error` is empty — the field that was supposed to catch              this never fires: {counted:?}"
        );
        assert!(
            counted
                .blind_spots
                .iter()
                .any(|b| b.contains("GitHub did not answer for acme/thing")),
            "a zero standing on a refresh that answered nothing must say so: {:?}",
            counted.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// The queue is newest-first by number, and stays that way.
    ///
    /// The owner's ordering, and it replaced `updated_at` descending — which sounds like the same
    /// thing and is not. A comment, a label, a bot's push all move a pull request to the top of that
    /// order without changing what it is, so the queue reshuffled between two looks and nothing
    /// stayed where it had been put. A number never moves.
    #[test]
    fn the_queue_is_newest_first_by_number() {
        let node = |number: u64, updated: &str| {
            serde_json::json!({
                "number": number, "title": "t", "url": "u", "isDraft": false,
                "author": { "login": "someone" }, "headRefName": "f",
                "headRefOid": format!("sha{number}"), "baseRefName": "main",
                "updatedAt": updated, "latestReviews": { "nodes": [] },
            })
        };
        // The two orderings must DISAGREE here, or the test passes on a coincidence — which the
        // first version of it did. #7 is the oldest pull request and was commented on a minute ago;
        // #41 is the newest and has been quiet. By number: 41, 12, 7. By activity: 7, 12, 41.
        let mut prs: Vec<Pr> = [
            (7, "2026-08-24T00:00:00Z"),
            (41, "2020-01-01T00:00:00Z"),
            (12, "2024-01-01T00:00:00Z"),
        ]
        .iter()
        .map(|(n, at)| {
            build_pr(
                &shape(&node(*n, at)),
                *n,
                "me",
                "acme",
                &Reason::Author,
                &[],
                &BTreeMap::new(),
            )
        })
        .collect();
        newest_first(&mut prs);
        assert_eq!(
            prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![41, 12, 7],
            "the queue is not newest-first by number"
        );
    }

    /// A pull request too big for GitHub to serve a diff for is still readable.
    ///
    /// Reported live:
    ///
    /// ```text
    /// not summarised — its diff could not be read: GitHub answered 406: {"message":"Sorry, the
    /// diff exceeded the maximum number of lines (20000)", … "code":"too_large"}
    /// ```
    ///
    /// GitHub declines to SERVE a diff over 20,000 lines. That is not the same as the size being a
    /// problem here — `review` truncates every diff to a byte cap before a model sees it, so a
    /// change this big was always going to be read in part. The 406 cost reading it at all.
    ///
    /// Assembled from `/files` instead, and the shape matters as much as the content: everything
    /// downstream reads a diff by its `diff --git` and `+++` lines, so a stream of bare hunks would
    /// parse as an empty change and summarise as "nothing here".
    #[test]
    fn a_diff_too_large_to_serve_is_assembled_from_its_files() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        forget_host_token();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::{Read as _, Write as _};
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                // GitHub's own answer, word for word.
                let (status, answer) = if head.contains("/files") {
                    (
                        200,
                        r#"[{"filename":"src/a.rs","status":"modified","additions":2,"deletions":1,
                             "patch":"@@ -1,3 +1,4 @@\n kept\n-old\n+new\n+more"},
                           {"filename":"assets/logo.png","status":"modified","additions":0,
                             "deletions":0}]"#
                            .to_string(),
                    )
                } else {
                    (
                        406,
                        r#"{"message":"Sorry, the diff exceeded the maximum number of lines (20000)",
                            "errors":[{"resource":"PullRequest","field":"diff","code":"too_large"}]}"#
                            .to_string(),
                    )
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
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let diff =
            pr_diff_text("acme/thing", 7).expect("a diff GitHub will not serve is still read");

        // The headers everything downstream keys on. Without them `shape` and `contracts` read this
        // as an empty change, and the pull request summarises as though nothing had happened in it.
        assert!(
            diff.contains("diff --git a/src/a.rs b/src/a.rs") && diff.contains("+++ b/src/a.rs"),
            "the assembled diff is not shaped like a diff: {diff}"
        );
        assert!(diff.contains("+new"), "the hunk itself was dropped: {diff}");

        // A file GitHub gave no patch for is NAMED, with its numbers. Dropping it would read as
        // "nothing happened here", and a binary asset changing is a thing a reviewer wants to know.
        assert!(
            diff.contains("assets/logo.png") && diff.contains("no patch available"),
            "a file with no patch vanished instead of being named: {diff}"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
    }

    // ---- SKEIN-214: a review drafted against one commit still lands after the branch moves ----

    /// A comment as the reading view drafts it: `text` is the line the reviewer was looking at.
    fn drafted(path: &str, line: u64, body: &str, text: &str) -> ReviewComment {
        ReviewComment {
            path: path.into(),
            line,
            body: body.into(),
            text: text.into(),
        }
    }

    /// The PR after one more push: one line replaced by two above `fn target() {}`, so everything
    /// below shifted down, and the old `fn gone() {}` no longer exists. The `-` line is
    /// deliberate: it must NOT advance the new-file counter, and only a diff that has one can
    /// catch a counter that thinks otherwise.
    const MOVED_DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
                              --- a/src/lib.rs\n\
                              +++ b/src/lib.rs\n\
                              @@ -1,4 +1,5 @@\n \
                              fn keep() {}\n\
                              -fn old() {}\n\
                              +fn added() {}\n\
                              +fn extra() {}\n \
                              fn target() {}\n \
                              tail\n";

    /// **A merge with no expected head never reaches the network.**
    ///
    /// The backstop, one layer below `prwork::merge_by_hand`'s own refusal, and it is here rather
    /// than only there because this is the function holding the `PUT`. Until SKEIN-338 it sent
    /// `{"merge_method": …}` and nothing else, so every caller — present and future — merged
    /// whatever HEAD happened to be. Making the argument required is only half of that; refusing
    /// the empty string is the other half, because `""` is what a caller with no idea passes.
    ///
    /// `$SKEIN_GITHUB_API` points at an address nothing is listening on, so the assertion is not
    /// "it returned an error" — it would do that anyway — but that the error is the guard's and not
    /// a connection's. A refusal that had gone to the wire would say so.
    #[test]
    fn a_merge_that_cannot_name_a_commit_is_refused_before_the_wire() {
        let _g = crate::testutil::env_lock();
        // Port 1 on loopback: nothing listens there, so any request at all fails loudly and
        // differently from the refusal being asserted.
        std::env::set_var("SKEIN_GITHUB_API", "http://127.0.0.1:1");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        forget_host_token();

        for empty in ["", " ", "\t", "\n  "] {
            let why = merge("acme/thing", 41, empty).unwrap_err();
            assert!(
                why.contains("which commit") && why.contains("#41"),
                "a merge with head {empty:?} was refused for the wrong reason — this reads like it \
                 reached GitHub: {why}"
            );
            assert!(
                !why.contains("GitHub"),
                "a merge that could not name a commit was still sent: {why}"
            );
        }

        for key in ["SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        forget_host_token();
    }

    /// **Only a 409 becomes "the branch moved", and it never says 409.**
    ///
    /// The translation is matched against the sentences `crate::github` itself formats — `GitHub
    /// said {status}: …` from `json`, `GitHub answered {status}: …` from `complaint` — rather than
    /// against GitHub's prose, which is somebody else's copy and changes without notice. So the
    /// thing worth asserting is that the match is exact in both directions: every other status a
    /// merge can draw passes through whole, because a 405 (not mergeable), a 422 and a rate limit
    /// are real answers and none of them mean somebody pushed.
    ///
    /// A status this turned into "the branch moved" wrongly would send a reader to re-read a change
    /// that was never the problem; one it failed to translate would leave the single most likely
    /// merge failure reading as an unexplained error.
    #[test]
    fn only_a_conflict_is_reported_as_the_branch_having_moved() {
        // Both shapes `crate::github` produces, across the statuses a merge actually draws.
        for status in [401, 403, 404, 405, 409, 422, 500, 502] {
            for said in [
                format!("GitHub said {status}: Head branch was modified. Review and try the merge again."),
                format!("GitHub answered {status}: <html>no</html>"),
            ] {
                let out = the_branch_moved(41, "abc1234def", said.clone());
                let translated = out != said;
                assert_eq!(
                    translated,
                    status == 409,
                    "status {status} was {} translated into \"the branch moved\": {out}",
                    match translated {
                        true => "wrongly",
                        false => "not",
                    }
                );
                if translated {
                    assert!(
                        out.contains("moved since you read it")
                            && out.contains("abc1234")
                            && out.contains("#41"),
                        "the translation lost the pull request or the commit it was read at: {out}"
                    );
                    assert!(
                        !out.contains("409"),
                        "the raw status survived into the reader's sentence: {out}"
                    );
                }
            }
        }

        // Not a status at all — a dead connection, a rate limit — is untouched. There is no number
        // in these, and inventing a merge conflict out of one would be the worst kind of guess.
        for said in [
            "GitHub sent nothing at all".to_string(),
            "GitHub is rate limiting skein — resuming in about 15m".to_string(),
            "the 409 in this sentence is not a status".to_string(),
        ] {
            assert_eq!(
                the_branch_moved(41, "abc1234def", said.clone()),
                said,
                "an answer that was not a 409 was reported as the branch moving"
            );
        }
    }

    /// **Only a 405 that names conflicts becomes a sentence about conflicts, and it never says
    /// 405.** (SKEIN-411)
    ///
    /// Two things have to hold at once and they pull in opposite directions. The status is the
    /// gate — a 409, a 422 or a rate limit whose body happens to contain the word "conflict" must
    /// not be turned into "go and resolve conflicts", because none of them are that. And the gate
    /// is not enough on its own — a 405 is GitHub's answer to every kind of "not mergeable", so a
    /// draft or a blocking rule must still arrive verbatim rather than sending the reader to look
    /// for conflicts that do not exist.
    #[test]
    fn only_a_405_naming_conflicts_is_reported_as_conflicts_with_the_base() {
        // GitHub's own words for a conflicted merge, measured on the testbed. Both shapes
        // `crate::github` wraps them in, across the statuses a merge actually draws.
        for status in [401, 403, 404, 405, 409, 422, 500, 502] {
            for said in [
                format!("GitHub said {status}: Pull Request has merge conflicts"),
                format!("GitHub answered {status}: <html>merge conflicts</html>"),
            ] {
                let out = it_conflicts_with_its_base(41, said.clone());
                let translated = out != said;
                assert_eq!(
                    translated,
                    status == 405,
                    "status {status} was {} translated into a sentence about conflicts: {out}",
                    match translated {
                        true => "wrongly",
                        false => "not",
                    }
                );
                if translated {
                    assert!(
                        out.contains("conflicts with its base") && out.contains("#41"),
                        "the translation lost the pull request or what is wrong with it: {out}"
                    );
                    assert!(
                        out.contains("resolved") || out.contains("Resolve"),
                        "the reader was told what is wrong and not what to do about it: {out}"
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
        ] {
            assert_eq!(
                it_conflicts_with_its_base(41, said.clone()),
                said,
                "a 405 that says nothing about conflicts was reported as a conflict"
            );
        }

        // Not a status at all, and the word appearing anywhere else. `the_branch_moved`'s own
        // sentence is the one that matters here: the two translations run one after the other on
        // the same merge, and the first one's output must not be eaten by the second.
        for said in [
            "GitHub sent nothing at all".to_string(),
            "the branch moved since you read it — #41 is no longer at abc1234, so nothing was \
             merged. Read the new code, then merge."
                .to_string(),
            "the 405 in this sentence is not a status, and neither is this conflict".to_string(),
        ] {
            assert_eq!(
                it_conflicts_with_its_base(41, said.clone()),
                said,
                "an answer that was not a 405 was reported as conflicts with the base"
            );
        }
    }

    #[test]
    fn re_anchor_keeps_an_unmoved_line_at_its_number() {
        let (kept, gone) = re_anchor(
            &[drafted("src/lib.rs", 1, "note", "fn keep() {}")],
            MOVED_DIFF,
        );
        assert!(gone.is_empty());
        assert_eq!((kept[0].line, kept[0].path.as_str()), (1, "src/lib.rs"));
    }

    #[test]
    fn re_anchor_follows_a_line_pushed_down_by_an_insertion_above() {
        // Drafted at line 3; one line above became two, so it now lives at 4 — and the `-` line
        // between must not be counted on the way there.
        let (kept, gone) = re_anchor(
            &[drafted("src/lib.rs", 3, "note", "fn target() {}")],
            MOVED_DIFF,
        );
        assert!(
            gone.is_empty(),
            "the line still exists and was displaced anyway"
        );
        assert_eq!(
            kept[0].line, 4,
            "the comment did not follow its line to its new number"
        );
        assert_eq!(kept[0].body, "note", "the body must travel untouched");
    }

    #[test]
    fn re_anchor_prefers_the_duplicate_nearest_the_old_line_and_the_earlier_on_a_tie() {
        let twice = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
                     @@ -1,9 +1,9 @@\n one\n+same\n three\n four\n five\n six\n+same\n eight\n nine\n";
        // `+same` sits at new lines 2 and 7. Old line 8 → 7 is nearer than 2.
        let (kept, _) = re_anchor(&[drafted("a.rs", 8, "n", "same")], twice);
        assert_eq!(kept[0].line, 7, "nearest-to-old did not win");
        // Old line 4 or 5 is a near-tie; make it exact: |2-4|=2 vs |7-4|=3 → 2. And a true tie —
        // candidates 2 and 7 from old line 4.5 cannot be written, so test equidistance directly:
        // old line at the midpoint via a diff whose duplicates sit at 2 and 6, old 4 → tie → earlier.
        let tie = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n\
                   @@ -1,7 +1,7 @@\n one\n+same\n three\n four\n five\n+same\n seven\n";
        let (kept, _) = re_anchor(&[drafted("a.rs", 4, "n", "same")], tie);
        assert_eq!(kept[0].line, 2, "a tie must break toward the earlier line");
    }

    #[test]
    fn re_anchor_displaces_a_deleted_line() {
        let (kept, gone) = re_anchor(&[drafted("src/lib.rs", 9, "n", "fn gone() {}")], MOVED_DIFF);
        assert!(
            kept.is_empty(),
            "anchored a comment to a line that no longer exists"
        );
        assert_eq!(gone.len(), 1);
        assert_eq!(
            gone[0].line, 9,
            "the displaced comment must keep its original coordinates"
        );
    }

    #[test]
    fn re_anchor_displaces_a_comment_with_no_text_to_search_for() {
        // `text` empty means an old client or a draft that never captured the line — matching
        // by nothing would anchor everywhere, so it anchors nowhere.
        let (kept, gone) = re_anchor(&[drafted("src/lib.rs", 1, "n", "")], MOVED_DIFF);
        assert!(kept.is_empty() && gone.len() == 1);
    }

    #[test]
    fn re_anchor_displaces_a_comment_on_a_file_the_new_diff_no_longer_touches() {
        // Same text exists — in a DIFFERENT file. Text matching never crosses paths.
        let (kept, gone) = re_anchor(
            &[drafted("src/other.rs", 1, "n", "fn keep() {}")],
            MOVED_DIFF,
        );
        assert!(kept.is_empty() && gone.len() == 1);
    }

    /// A GitHub for the moved-head posting path: serves one PR's diff (or refuses with a 500 when
    /// `diff` is `None`), answers every POST with `{}`, and records `"METHOD path body"` — the
    /// wire is the thing under test, exactly as `fake_github` argues above.
    fn reanchor_github(
        diff: Option<&'static str>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder
                    .lock()
                    .unwrap()
                    .push(format!("{method} {path} {body}"));
                let (status, answer) = match (method.as_str(), diff) {
                    ("POST", _) => (200, "{}".to_string()),
                    (_, Some(d)) => (200, d.to_string()),
                    (_, None) => (500, r#"{"message":"boom"}"#.to_string()),
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
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// Env plumbing every wire test here shares. Returns the guard that must stay alive.
    fn wired(base: &str) -> impl Drop {
        // The env lock, held for its Drop and never read — which is the whole point of it, and
        // what the dead-code warning was about. `crate::testutil::env_lock` is the one mechanism;
        // this only ties its lifetime to the environment it guards.
        struct Undo(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
        impl Drop for Undo {
            fn drop(&mut self) {
                for key in ["GH_TOKEN", "SKEIN_GITHUB_API"] {
                    std::env::remove_var(key);
                }
                forget_host_token();
            }
        }
        let guard = crate::testutil::env_lock();
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::set_var("SKEIN_GITHUB_API", base);
        forget_host_token();
        Undo(guard)
    }

    /// The recorded review POST, parsed. Panics with the whole record if none was made.
    fn posted_review(seen: &std::sync::Mutex<Vec<String>>) -> serde_json::Value {
        let seen = seen.lock().unwrap();
        let post = seen
            .iter()
            .find(|r| r.starts_with("POST "))
            .unwrap_or_else(|| panic!("no review reached GitHub: {seen:?}"));
        serde_json::from_str(post.splitn(3, ' ').nth(2).unwrap()).unwrap()
    }

    /// The commit every SKEIN-271 test below posts against, and the review the viewer had already
    /// left on it before any of this — the decoy that makes "a review by me at this head" the
    /// wrong test to write.
    const POST_HEAD: &str = "cccccccc333333333333333333333333333333333";
    const DECOY: &str = "I approved this an hour ago";

    /// A GitHub whose review POST dies MID-ANSWER, the way the owner's did (SKEIN-271).
    ///
    /// `creates_before_dying` is the ambiguity itself: GitHub cancels the stream after the headers,
    /// so from skein's side "the review exists" and "the review does not exist" are the same
    /// failure. Both halves are served from the reviews this fixture actually holds — seeded with
    /// [`DECOY`], a review by the same viewer at the same commit — so a test reads exactly the
    /// evidence skein reads. `lookup_dies` is the third case: the connection is gone and stays
    /// gone, so the question cannot be answered at all.
    ///
    /// Returns the request record and the reviews GitHub ends up holding.
    #[allow(clippy::type_complexity)]
    fn dying_review_github(
        deaths: usize,
        creates_before_dying: bool,
        lookup_dies: bool,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let held = std::sync::Arc::new(std::sync::Mutex::new(vec![serde_json::json!({
            "user": { "login": "me" }, "commit_id": POST_HEAD, "body": DECOY,
        })]));
        let (recorder, reviews) = (seen.clone(), held.clone());
        std::thread::spawn(move || {
            let mut posts = 0usize;
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder
                    .lock()
                    .unwrap()
                    .push(format!("{method} {path} {body}"));
                // A length promised and not delivered, then the socket goes: curl exits non-zero
                // with no status and no body.
                let die = |stream: &mut std::net::TcpStream| {
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\nhalf an ans");
                    let _ = stream.flush();
                };
                let posting = method == "POST" && path.ends_with("/reviews");
                let listing = method == "GET" && path.contains("/reviews");
                let keep = |body: &str| {
                    let sent: serde_json::Value = serde_json::from_str(body).unwrap();
                    reviews.lock().unwrap().push(serde_json::json!({
                        "user": { "login": "me" },
                        "commit_id": sent["commit_id"],
                        "body": sent["body"],
                    }));
                };
                if posting {
                    posts += 1;
                    if posts <= deaths {
                        if creates_before_dying {
                            keep(&body);
                        }
                        die(&mut stream);
                        continue;
                    }
                    keep(&body);
                }
                if listing && lookup_dies {
                    die(&mut stream);
                    continue;
                }
                let answer = match (posting, listing, path.as_str()) {
                    (true, _, _) => "{}".to_string(),
                    (_, true, _) => {
                        serde_json::Value::Array(reviews.lock().unwrap().clone()).to_string()
                    }
                    (_, _, "/user") => r#"{"login":"me"}"#.to_string(),
                    _ => "{}".to_string(),
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
        (format!("http://127.0.0.1:{port}"), seen, held)
    }

    /// How many reviews actually reached GitHub, and how many times skein pressed.
    fn posts_and_reviews(
        seen: &std::sync::Mutex<Vec<String>>,
        held: &std::sync::Mutex<Vec<serde_json::Value>>,
    ) -> (usize, usize) {
        let posts = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count();
        (posts, held.lock().unwrap().len())
    }

    /// **The one SKEIN-271 exists to prevent.** The stream dies AFTER GitHub created the review, so
    /// the failure skein sees is indistinguishable from one where nothing happened. A blind retry
    /// — which is what the read half does, correctly, for a query — posts the owner's review onto
    /// their pull request twice. Skein must go and look instead, find it, and stop.
    #[test]
    fn a_review_created_before_the_stream_died_is_found_rather_than_posted_again() {
        let (base, seen, held) = dying_review_github(1, true, false);
        let _env = wired(&base);

        let said = submit_review_with_comments(
            "acme/thing",
            7,
            POST_HEAD,
            Verdict::Comment,
            "looks fine",
            &[drafted("src/lib.rs", 2, "tighten this", "fn target() {}")],
            POST_HEAD,
        )
        .expect("a review that GitHub already holds is a success, not a failure to report");

        let (posts, reviews) = posts_and_reviews(&seen, &held);
        assert_eq!(
            posts, 1,
            "the review was posted onto the pull request twice"
        );
        assert_eq!(
            reviews, 2,
            "GitHub holds more than the decoy and the one review that was meant"
        );
        assert!(
            said.contains("posted the review") && said.contains("connection died"),
            "the answer must say it landed AND that skein had to go and check: {said}"
        );
    }

    /// The other half of the same ambiguity: the stream died before GitHub created anything, so
    /// there is nothing to find and the review must actually be posted. The decoy is what makes
    /// this a real test — a review by the same viewer at the same commit is already there, and
    /// matching on that alone would silently discard the review the person had just vetted.
    #[test]
    fn a_review_the_dead_stream_never_created_is_posted_on_the_second_attempt() {
        let (base, seen, held) = dying_review_github(1, false, false);
        let _env = wired(&base);

        submit_review_with_comments(
            "acme/thing",
            7,
            POST_HEAD,
            Verdict::Comment,
            "looks fine",
            &[],
            POST_HEAD,
        )
        .expect("nothing landed, so the review must be posted rather than declined");

        let (posts, reviews) = posts_and_reviews(&seen, &held);
        assert_eq!(posts, 2, "the retry never happened");
        assert_eq!(reviews, 2, "the vetted review never reached GitHub");
        assert!(
            held.lock()
                .unwrap()
                .iter()
                .any(|r| r["body"] == "looks fine"),
            "the review that landed is not the one that was written"
        );
    }

    /// When the ambiguity cannot be resolved, skein stops. A second press might be a duplicate and
    /// might be the only copy, and the one thing it must not do is choose for the owner in the
    /// direction that writes.
    #[test]
    fn a_post_that_cannot_be_verified_refuses_to_press_again_and_says_where_to_look() {
        let (base, seen, held) = dying_review_github(1, true, true);
        let _env = wired(&base);

        let why = submit_review_with_comments(
            "acme/thing",
            7,
            POST_HEAD,
            Verdict::Comment,
            "looks fine",
            &[],
            POST_HEAD,
        )
        .expect_err("an unresolvable ambiguity is not a success");

        let (posts, _) = posts_and_reviews(&seen, &held);
        assert_eq!(
            posts, 1,
            "skein pressed again without knowing what happened"
        );
        assert!(
            why.contains("connection to GitHub died") && why.contains("acme/thing#7"),
            "the reader is not told what happened or where to look: {why}"
        );
        assert!(
            why.contains("pressing again posts it twice"),
            "the reader is not told what the risk of pressing again is: {why}"
        );
    }

    /// SKEIN-214, the whole ask on one wire: the branch moved after drafting, and the review still
    /// lands — the comment whose line survives follows it to its NEW number, the one whose line
    /// changed folds into the body naming the commit it was read at, `commit_id` is the LIVE head,
    /// and the body says read-at/posted-against so the GitHub record is honest about what was
    /// actually reviewed.
    #[test]
    fn a_review_of_a_moved_branch_lands_with_reanchored_lines_and_an_honest_body() {
        let (base, seen) = reanchor_github(Some(MOVED_DIFF));
        let _env = wired(&base);

        let drafted_at = "aaaaaaa1111111111111111111111111111111111";
        let live_head = "bbbbbbb2222222222222222222222222222222222";
        let said = submit_review_with_comments(
            "acme/thing",
            7,
            live_head,
            Verdict::Comment,
            "overall: fine",
            &[
                drafted("src/lib.rs", 3, "tighten this", "fn target() {}"),
                drafted("src/lib.rs", 9, "dead code?", "fn gone() {}"),
            ],
            drafted_at,
        )
        .expect("a moved branch must not make the review unpostable");

        let payload = posted_review(&seen);
        assert_eq!(
            payload["commit_id"], *live_head,
            "commit_id must be the live head, never the drafted one"
        );
        let comments = payload["comments"].as_array().unwrap();
        assert_eq!(
            comments.len(),
            1,
            "the displaced comment leaked into the line comments"
        );
        assert_eq!(
            (comments[0]["line"].as_u64(), comments[0]["side"].as_str()),
            (Some(4), Some("RIGHT")),
            "the surviving comment did not move to its new line number"
        );
        assert_eq!(comments[0]["body"], "tighten this");
        let body = payload["body"].as_str().unwrap();
        assert!(
            body.contains(
                "Reviewed at aaaaaaa — the branch has moved since, and these lines changed:"
            ),
            "the displaced heading is missing: {body}"
        );
        assert!(
            body.contains("• src/lib.rs:9 — dead code?"),
            "the displaced comment's bullet is missing: {body}"
        );
        assert!(
            body.contains("(read at aaaaaaa, posted against bbbbbbb)"),
            "the record does not say what was actually reviewed: {body}"
        );
        assert!(
            said.contains("1 line comment"),
            "the answer under-reports: {said}"
        );
    }

    /// The unmoved case pays nothing: same head → no diff fetch, and the payload is byte-for-byte
    /// today's shape — no heading, no read-at line, the drafted numbers as given.
    #[test]
    fn a_review_of_an_unmoved_branch_posts_exactly_as_before() {
        let (base, seen) = reanchor_github(Some(MOVED_DIFF));
        let _env = wired(&base);

        let head = "cccccccc333333333333333333333333333333333";
        submit_review_with_comments(
            "acme/thing",
            7,
            head,
            Verdict::Comment,
            "looks fine",
            &[drafted("src/lib.rs", 2, "tighten this", "fn target() {}")],
            head,
        )
        .unwrap();

        assert_eq!(
            posted_review(&seen),
            serde_json::json!({
                "event": "COMMENT",
                "commit_id": head,
                "body": "looks fine",
                "comments": [
                    { "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": "tighten this" }
                ],
            }),
            "the unmoved payload must be identical to the pre-SKEIN-214 shape"
        );
        let requests = seen.lock().unwrap().clone();
        assert_eq!(
            requests.len(),
            1,
            "an unmoved head must cost no diff fetch: {requests:?}"
        );
    }

    /// A diff GitHub will not serve (the 20k-line 406, a network refusal) displaces EVERY comment
    /// into the body — the review lands anyway, because "post it" was the whole of the ask, and an
    /// error here would strand a finished review behind an unreadable diff.
    #[test]
    fn an_unfetchable_diff_moves_every_comment_into_the_body_and_still_posts() {
        let (base, seen) = reanchor_github(None);
        let _env = wired(&base);

        submit_review_with_comments(
            "acme/thing",
            7,
            "bbbbbbb2222222222222222222222222222222222",
            Verdict::Comment,
            "",
            &[
                drafted("src/lib.rs", 2, "tighten this", "fn target() {}"),
                drafted("src/lib.rs", 9, "dead code?", "fn gone() {}"),
            ],
            "aaaaaaa1111111111111111111111111111111111",
        )
        .expect("an unreadable diff must not make the review unpostable");

        let payload = posted_review(&seen);
        assert!(
            payload.get("comments").is_none(),
            "with no diff to anchor against, no line comment can be trusted: {payload}"
        );
        let body = payload["body"].as_str().unwrap();
        assert!(
            body.contains("• src/lib.rs:2 — tighten this")
                && body.contains("• src/lib.rs:9 — dead code?"),
            "a comment vanished instead of riding in the body: {body}"
        );
        assert!(body.contains("(read at aaaaaaa, posted against bbbbbbb)"));
    }

    /// A pull request says when its HEAD COMMIT landed, not when the pull request was last touched.
    ///
    /// The two are different questions and only one of them is about commits. `updatedAt` moves on
    /// a comment, so a branch nobody has pushed to in days reads as hot the moment somebody
    /// discusses it — backwards for deciding whether a PR has settled enough to be worth reading,
    /// which is what this field exists for.
    ///
    /// Free: `commits(last: 1)` is already fetched for the check rollup, so this is one more field
    /// inside a node skein asks for anyway. Asserted through `shape`, because `shape` DROPS
    /// `commits` after flattening it — anything not lifted out there is gone by the time a `Pr` is
    /// built, and it would be gone silently.
    #[test]
    fn a_pull_request_carries_its_head_commits_date_and_not_its_own() {
        let node = serde_json::json!({
            "number": 7,
            "title": "a pull request",
            "url": "u",
            "isDraft": false,
            "author": { "login": "someone" },
            "headRefName": "feat",
            "headRefOid": "abc",
            "baseRefName": "main",
            // Touched a minute ago…
            "updatedAt": "2026-08-23T12:00:00Z",
            "latestReviews": { "nodes": [] },
            "reviewDecision": "APPROVED",
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "BEHIND",
            "labels": { "nodes": [{ "name": "ci" }, { "name": "needs docs" }] },
            // …and last pushed to three days before that.
            "commits": { "nodes": [{ "commit": {
                "committedDate": "2026-08-20T09:00:00Z",
                "statusCheckRollup": null,
            }}]},
        });
        // And it is actually ASKED for. Everything above works on a node handed to it, so without
        // this the whole feature can be reading a field GitHub was never told to send — every PR
        // would report "do not know", the settle rule would decline to read anything, and the
        // queue would look thoughtfully quiet rather than broken.
        assert!(
            PR_FRAGMENT.contains("commit { committedDate"),
            "the head commit's date is read but never requested: {}",
            *PR_FRAGMENT
        );

        let shaped = shape(&node);
        let pr = build_pr(
            &shaped,
            7,
            "me",
            "acme",
            &Reason::Author,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(
            pr.committed_at, "2026-08-20T09:00:00Z",
            "the queue is carrying the pull request's own timestamp, so a PR that was merely \
             commented on reads as freshly pushed"
        );
        assert_eq!(
            pr.updated_at, "2026-08-23T12:00:00Z",
            "both are still reported"
        );

        // The three facts a workflow decides on, off the same node. GitHub's `mergeable` is an enum
        // of three and stays three: UNKNOWN is what it says for a while after every push, and an
        // answer invented here would reach a workflow as a conflict — which rebases, which on a
        // repository that dismisses stale approvals throws away the approval that authorised the
        // merge. Two layers below the test that protects that rule, so it is asserted here as well.
        assert_eq!(pr.labels, vec!["ci".to_string(), "needs docs".to_string()]);
        assert_eq!(pr.review_decision, "APPROVED");
        assert_eq!(pr.mergeable, Some(true));
        assert_eq!(
            pr.merge_state, "BEHIND",
            "GitHub's merge-state verdict was parsed away — the merge train reads BEHIND to know \
             the base must be merged in first"
        );
        // **And a node that CARRIES neither is not given a verdict** (SKEIN-257). The assertions
        // above run on a fixture this test wrote, so on their own they say what happens when
        // GitHub answers — and the other thing that decides `behind` is GitHub not answering at
        // all, which is an everyday state: `mergeStateStatus` is absent for a while after every
        // push and on a token that cannot see it. Empty and `None` are what "not known" looks
        // like here, and both fields' own docs turn on it: a caller reading `""` as `CLEAN` or
        // `None` as "not mergeable" advances a merge train on a guess.
        let silent = build_pr(
            &shape(&item(
                r#"{"number":8,"title":"t","url":"u","isDraft":false,
                    "headRefName":"feat","headRefOid":"abc","baseRefName":"main",
                    "author":{"login":"someone"},"latestReviews":{"nodes":[]}}"#,
            )),
            8,
            "me",
            "acme",
            &Reason::Author,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(
            silent.merge_state, "",
            "a pull request GitHub said nothing about was given a merge-state verdict anyway"
        );
        assert_eq!(
            silent.mergeable, None,
            "silence became an answer — `UNKNOWN` read as a conflict is a rebase on a guess, and \
             on a repo that dismisses stale approvals that rebase destroys the approval"
        );
        assert_eq!(
            silent.review_decision, "",
            "an unstated review decision must not read as a repository that requires none"
        );

        // And it is actually ASKED for, same trap as `committedDate` above: everything here works
        // on a node handed to it, so without this the field could be one GitHub was never told to
        // send, and every PR would read as merge-state unknown.
        assert!(
            PR_FRAGMENT.contains("mergeStateStatus"),
            "merge_state is read but never requested: {}",
            *PR_FRAGMENT
        );
        let conflicting = build_pr(
            &shape(&serde_json::json!({
                "number": 9, "title": "t", "url": "u", "isDraft": false,
                "author": { "login": "someone" }, "headRefName": "f", "headRefOid": "d",
                "baseRefName": "main", "updatedAt": "2026-08-23T12:00:00Z",
                "latestReviews": { "nodes": [] }, "mergeable": "CONFLICTING",
            })),
            9,
            "me",
            "acme",
            &Reason::Author,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(conflicting.mergeable, Some(false));
        assert!(
            conflicting.labels.is_empty(),
            "a PR with no labels must not invent one"
        );

        // GitHub answering without one is "skein does not know", never "long ago" — a guess in that
        // direction reads a pull request somebody is still pushing to.
        let bare = shape(&serde_json::json!({
            "number": 8, "title": "t", "url": "u", "isDraft": false,
            "author": { "login": "someone" }, "headRefName": "f", "headRefOid": "d",
            "baseRefName": "main", "updatedAt": "2026-08-23T12:00:00Z",
            "latestReviews": { "nodes": [] },
        }));
        let bare = build_pr(
            &bare,
            8,
            "me",
            "acme",
            &Reason::Author,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(bare.committed_at, "");
        assert_eq!(
            bare.mergeable, None,
            "GitHub saying nothing about mergeability became an answer"
        );
        assert_eq!(
            bare.merge_state, "",
            "GitHub saying nothing about the merge state must read as not known, never as current"
        );
    }

    #[test]
    fn an_approval_on_the_current_head_is_a_decision() {
        let v = item(
            r#"{"headRefOid":"abc","latestReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"abc"}}]}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("approved".into(), true));
    }

    #[test]
    fn new_commits_undo_your_approval() {
        let v = item(
            r#"{"latestReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}"#,
        );
        assert_eq!(my_review_state(&v, "me", "new"), ("approved".into(), false));
    }

    #[test]
    fn a_comment_is_not_a_decision() {
        let v = item(
            r#"{"latestReviews":[{"author":{"login":"me"},"state":"COMMENTED","commit":{"oid":"abc"}}]}"#,
        );
        let (state, current) = my_review_state(&v, "me", "abc");
        assert_eq!(state, "commented");
        assert!(current);
        // The lane, not the flag, is what matters: commented never reaches Waiting.
        assert!(!matches!(state.as_str(), "approved" | "changes-requested"));
    }

    #[test]
    fn someone_elses_approval_is_not_yours() {
        let v = item(
            r#"{"latestReviews":[{"author":{"login":"her"},"state":"APPROVED","commit":{"oid":"abc"}}]}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("none".into(), false));
    }

    #[test]
    fn a_review_with_no_commit_is_treated_as_stale() {
        let v = item(r#"{"latestReviews":[{"author":{"login":"me"},"state":"APPROVED"}]}"#);
        assert_eq!(my_review_state(&v, "me", "abc"), ("approved".into(), false));
    }

    #[test]
    fn login_case_does_not_hide_your_own_review() {
        let v = item(
            r#"{"latestReviews":[{"author":{"login":"Me"},"state":"APPROVED","commit":{"oid":"abc"}}]}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("approved".into(), true));
    }

    /// SKEIN-354. GraphQL's `latestReviews` is the latest review per author WHATEVER it said, so a
    /// note left after an approval comes back as `COMMENTED` and silently demotes your own verdict
    /// — it reads exactly like never having decided. `latestOpinionatedReviews` exists to exclude
    /// that, and the verdict is read from it.
    ///
    /// Confirmed against GitHub rather than argued from the schema: on `acme/thing` #693 the
    /// two connections disagree — `latestReviews` carries a `COMMENTED` review by the viewer and
    /// `latestOpinionatedReviews` carries nothing of his at all.
    #[test]
    fn a_comment_after_your_approval_does_not_take_the_approval_away() {
        let v = item(
            r#"{"headRefOid":"abc",
                "latestReviews":[{"author":{"login":"me"},"state":"COMMENTED","commit":{"oid":"abc"}}],
                "latestOpinionatedReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"abc"}}]}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("approved".into(), true));
    }

    /// The fallback, and the one fact the opinionated connection cannot carry: that you commented.
    /// It matters because "you said something and decided nothing" is a different row from "you
    /// have not looked", and because every fixture written before the second connection existed
    /// must go on reading the way it always did.
    #[test]
    fn a_comment_with_no_verdict_behind_it_still_reads_as_a_comment() {
        let v = item(
            r#"{"headRefOid":"abc",
                "latestReviews":[{"author":{"login":"me"},"state":"COMMENTED","commit":{"oid":"abc"}}],
                "latestOpinionatedReviews":[]}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("commented".into(), true));
        // An approval GitHub DISMISSED is not opinionated any more, and skein must not remember it:
        // "is my review status approved right now" is the whole question.
        let dismissed = item(
            r#"{"headRefOid":"abc",
                "latestReviews":[{"author":{"login":"me"},"state":"DISMISSED","commit":{"oid":"abc"}}],
                "latestOpinionatedReviews":[]}"#,
        );
        assert_eq!(
            my_review_state(&dismissed, "me", "abc"),
            ("none".into(), false)
        );
    }

    /// SKEIN-354, the lane half. The owner: "approved should come only if my review status on the
    /// PR is approved rn, if I approved and then some file I own changed, so github asks me to
    /// review again then it should show that."
    ///
    /// Measured on his live queue on 2026-08-26 (26 rows): `review_is_current` was false on ALL of
    /// them, including the two he had approved himself — so the rule this replaces returned every
    /// decided pull request to him on the next push, which is why his approvals never cleared
    /// anything. If this test fails and the change that broke it put `review_is_current` back into
    /// the lane rule, the change is wrong and the rule is right.
    #[test]
    fn your_verdict_stands_until_github_asks_you_again() {
        let build = |json: &str| {
            build_pr(
                &item(json),
                5,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new(),
            )
        };
        // Approved, and the branch has moved several commits past what you read.
        let moved = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},
                "latestOpinionatedReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}"#,
        );
        assert_eq!(
            moved.lane,
            Lane::Waiting,
            "a push took away an approval GitHub still holds"
        );
        assert!(
            !moved.review_is_current,
            "the head moving is still knowable — it just no longer decides whose move this is"
        );
        assert!(!moved.my_review_requested);

        // The counter-case, and the half he asked for by name: a file he owns changed, so
        // CODEOWNERS asked him again. The row is his.
        let again = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},
                "reviewRequests":[{"name":"ME","team":false}],
                "latestOpinionatedReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}"#,
        );
        assert_eq!(
            again.lane,
            Lane::NeedsYou,
            "GitHub asked you again and the row did not come back"
        );
        assert!(
            again.my_review_requested,
            "and the row can say why it is back"
        );

        // A conflicted pull request you approved is NOT dragged back into the reviewer's queue as
        // not-ready either: the owner, on somebody else's conflicted PR — "as far as I am concerned
        // my work there is done".
        let dirty = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},"mergeable":"CONFLICTING",
                "latestOpinionatedReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}"#,
        );
        assert_eq!(dirty.lane, Lane::Waiting, "their conflict, your decision");

        // A comment is still not a verdict, so a row you only remarked on stays yours.
        let noted = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},
                "latestReviews":[{"author":{"login":"me"},"state":"COMMENTED","commit":{"oid":"new"}}]}"#,
        );
        assert_eq!(noted.lane, Lane::NeedsYou);
    }

    /// Who GitHub is asking, read off the same list the row's roster is drawn from — and a floor
    /// rather than a census. A TEAM you are in arrives as the team, so it cannot be attributed to
    /// you; the error may only fall towards leaving you alone, which is the side the owner chose:
    /// "theirs until they ask again".
    #[test]
    fn a_review_request_names_you_or_it_does_not_count() {
        let asked = |json: &str| {
            build_pr(
                &item(json),
                6,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new(),
            )
            .my_review_requested
        };
        assert!(asked(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                "reviewRequests":[{"name":"her","team":false},{"name":"me","team":false}]}"#
        ));
        assert!(
            asked(
                r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                    "reviewRequests":[{"name":"Me","team":false}]}"#
            ),
            "GitHub's casing of your own login must not hide a request for you"
        );
        assert!(
            !asked(
                r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                    "reviewRequests":[{"name":"acme/me","team":true}]}"#
            ),
            "a team is not you, however its slug reads"
        );
        assert!(!asked(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},"reviewRequests":[]}"#
        ));
        assert!(
            !asked(r#"{"number":6,"headRefOid":"a","author":{"login":"someone"}}"#),
            "an answer with no requests in it is not an answer that you were asked"
        );
    }

    #[test]
    fn a_red_check_beats_a_pending_one() {
        let v = item(
            r#"{"statusCheckRollup":[{"status":"COMPLETED","conclusion":"FAILURE"},{"status":"IN_PROGRESS"}]}"#,
        );
        assert_eq!(rollup(&v), "failing");
    }

    #[test]
    fn checks_vocabulary_matches_ship() {
        assert_eq!(rollup(&item(r#"{}"#)), "none");
        assert_eq!(rollup(&item(r#"{"statusCheckRollup":[]}"#)), "none");
        assert_eq!(
            rollup(&item(
                r#"{"statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS"}]}"#
            )),
            "passing"
        );
        assert_eq!(
            rollup(&item(r#"{"statusCheckRollup":[{"status":"IN_PROGRESS"}]}"#)),
            "pending"
        );
        assert_eq!(
            rollup(&item(r#"{"statusCheckRollup":[{"state":"SUCCESS"}]}"#)),
            "passing"
        );
    }

    #[test]
    fn a_completed_check_with_an_unknown_conclusion_is_failing_not_passing() {
        // Unknown must not read as green: a check state skein does not recognise is exactly the
        // case where it should defer to you rather than clear the PR.
        let v = item(r#"{"statusCheckRollup":[{"status":"COMPLETED","conclusion":"WEIRD"}]}"#);
        assert_eq!(rollup(&v), "failing");
    }

    /// A fresh `$SKEIN_HOME`, plus the guard that puts it back — the same fixture
    /// [`crate::gitgate`]'s tests use, for the same reason: the archive is a file under it.
    fn fresh_home() -> (std::sync::MutexGuard<'static, ()>, crate::testutil::TempDir) {
        let lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        (lock, home)
    }

    #[test]
    fn archiving_is_idempotent_in_both_directions() {
        let _home = fresh_home();
        set_archived("r", 7, true).unwrap();
        set_archived("r", 7, true).unwrap();
        assert_eq!(archived("r"), vec![7]);
        set_archived("r", 7, false).unwrap();
        set_archived("r", 7, false).unwrap();
        assert!(archived("r").is_empty());
    }

    #[test]
    fn archives_are_per_repo() {
        let _home = fresh_home();
        set_archived("one", 4, true).unwrap();
        assert_eq!(archived("one"), vec![4]);
        assert!(archived("two").is_empty());
    }

    #[test]
    fn an_archived_pr_lands_in_the_archived_lane() {
        let v = item(r#"{"number":3,"headRefOid":"abc","title":"t"}"#);
        let pr = build_pr(
            &v,
            3,
            "me",
            "repo",
            &Reason::Reviewer,
            &[3],
            &BTreeMap::new(),
        );
        assert_eq!(pr.lane, Lane::Archived);
    }

    #[test]
    fn an_unreviewed_pr_needs_you() {
        let v = item(r#"{"number":3,"headRefOid":"abc","title":"t"}"#);
        let pr = build_pr(
            &v,
            3,
            "me",
            "repo",
            &Reason::Reviewer,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(pr.lane, Lane::NeedsYou);
        assert_eq!(pr.checks, "none");
    }

    #[test]
    fn a_decided_pr_waits() {
        let v = item(
            r#"{"number":3,"headRefOid":"abc","latestReviews":[{"author":{"login":"me"},"state":"CHANGES_REQUESTED","commit":{"oid":"abc"}}]}"#,
        );
        let pr = build_pr(&v, 3, "me", "repo", &Reason::Author, &[], &BTreeMap::new());
        assert_eq!(pr.lane, Lane::Waiting);
    }

    /// The done-when fixture from SKEIN-139: a red PR, a draft, a conflicted one, one of yours,
    /// and one genuinely awaiting you — readiness decides the lane, not whether you have acted.
    #[test]
    fn a_lane_says_whose_move_it_is_not_whether_you_acted() {
        // Red is the ORDINARY state of an unreviewed PR here: CI runs only after review (the
        // workflow applies the CI label on approval), so failing checks must not take a PR off
        // the reviewer. The first version of this rule did, and live PRs vanished from the view.
        let red = item(
            r#"{"number":1,"headRefOid":"a","author":{"login":"someone"},
                "statusCheckRollup":[{"status":"COMPLETED","conclusion":"FAILURE"}]}"#,
        );
        assert_eq!(
            build_pr(
                &red,
                1,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NeedsYou,
            "failing checks do not excuse the review — on this fleet CI follows review"
        );

        let draft =
            item(r#"{"number":2,"headRefOid":"a","author":{"login":"someone"},"isDraft":true}"#);
        assert_eq!(
            build_pr(
                &draft,
                2,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NotReady,
            "a draft is its author saying it is not finished"
        );

        let conflicted = item(
            r#"{"number":3,"headRefOid":"a","author":{"login":"someone"},"mergeable":"CONFLICTING"}"#,
        );
        assert_eq!(
            build_pr(
                &conflicted,
                3,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NotReady,
            "unmergeable: the branch has to move before a review of it means anything"
        );

        // Yours, even red: your problem as an AUTHOR, and this queue is the reviewer's.
        let yours = item(
            r#"{"number":4,"headRefOid":"a","author":{"login":"me"},
                "statusCheckRollup":[{"status":"COMPLETED","conclusion":"FAILURE"}]}"#,
        );
        assert_eq!(
            build_pr(
                &yours,
                4,
                "me",
                "repo",
                &Reason::Author,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::Waiting,
            "you authored it — the next review is somebody else's to give"
        );

        // UNKNOWN is what GitHub says for a while after every push — it is "not yet computed",
        // never "conflicted", and a freshly pushed PR must not fall out of your lane for it.
        let fresh = item(
            r#"{"number":5,"headRefOid":"a","author":{"login":"someone"},"mergeable":"UNKNOWN"}"#,
        );
        assert_eq!(
            build_pr(
                &fresh,
                5,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NeedsYou,
            "mergeability GitHub has not computed is not a reason to demote"
        );
    }

    /// The reviewer's first question is "can I do this now?" — size, before anything else. The
    /// search answers it in the same call, and absence stays absent: a queue remembered from
    /// before these fields must render nothing rather than claim an empty change.
    #[test]
    fn a_row_can_say_how_big_the_change_is_before_it_is_opened() {
        let sized = item(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                "additions":120,"deletions":18,"changedFiles":6}"#,
        );
        let pr = build_pr(
            &sized,
            6,
            "me",
            "repo",
            &Reason::Reviewer,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(
            (pr.additions, pr.deletions, pr.changed_files),
            (Some(120), Some(18), Some(6)),
            "the size GitHub already sent never made it onto the row"
        );
        assert!(
            PR_FRAGMENT.contains("additions deletions changedFiles"),
            "the fields are read but never requested: {}",
            *PR_FRAGMENT
        );

        let bare = item(r#"{"number":7,"headRefOid":"a","author":{"login":"someone"}}"#);
        let pr = build_pr(
            &bare,
            7,
            "me",
            "repo",
            &Reason::Reviewer,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(
            (pr.additions, pr.deletions, pr.changed_files),
            (None, None, None),
            "absent size must stay absent — a defaulted 0 claims an empty change"
        );

        let awaiting = item(r#"{"number":5,"headRefOid":"a","author":{"login":"someone"}}"#);
        assert_eq!(
            build_pr(
                &awaiting,
                5,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NeedsYou,
            "green, settled, not yours, undecided: genuinely your move"
        );

        // Pending checks are not failing checks: a PR mid-CI is still yours to start reading.
        let pending = item(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                "statusCheckRollup":[{"status":"IN_PROGRESS"}]}"#,
        );
        assert_eq!(
            build_pr(
                &pending,
                6,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NeedsYou,
            "pending is not failing — waiting for green to read is a choice, not a gate"
        );
    }

    /// SKEIN-153: a red row says WHICH check failed, not just that something did. The one-word
    /// `checks` stays for lanes and sorting; the names and links are what turn "failing" from a
    /// dot into an answer. Both context shapes must survive [`shape`]'s flattening — a CheckRun
    /// names itself `name`/`detailsUrl`, a classic StatusContext `context`/`targetUrl`.
    #[test]
    fn a_red_row_names_the_checks_that_failed_with_their_links() {
        let node = serde_json::json!({
            "number": 8, "title": "t", "url": "u", "isDraft": false,
            "author": {"login": "someone"}, "headRefName": "f", "headRefOid": "a",
            "baseRefName": "main", "updatedAt": "2026-08-23T12:00:00Z",
            "latestReviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
                {"name": "build", "detailsUrl": "https://ci/build/1",
                 "status": "COMPLETED", "conclusion": "FAILURE"},
                {"name": "lint", "detailsUrl": "https://ci/lint/1",
                 "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"context": "deploy/preview", "targetUrl": "https://status/preview",
                 "state": "FAILURE"},
            ]}}}}]},
        });
        let pr = build_pr(
            &shape(&node),
            8,
            "me",
            "acme",
            &Reason::Reviewer,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(pr.checks, "failing", "the one-word verdict is unchanged");
        assert_eq!(
            pr.failing_checks,
            vec![
                FailedCheck {
                    name: "build".into(),
                    url: "https://ci/build/1".into()
                },
                FailedCheck {
                    name: "deploy/preview".into(),
                    url: "https://status/preview".into()
                },
            ],
            "the failing contexts by name and link, in rollup order — the green one is not news"
        );

        // And the fields are actually ASKED for: everything above works on a node handed to it,
        // so without this the names would be read from a reply GitHub was never told to include.
        assert!(
            PR_FRAGMENT.contains("... on CheckRun { name detailsUrl status conclusion }"),
            "the CheckRun name/link is read but never requested: {}",
            *PR_FRAGMENT
        );
        assert!(
            PR_FRAGMENT.contains("... on StatusContext { context targetUrl state }"),
            "the StatusContext name/link is read but never requested: {}",
            *PR_FRAGMENT
        );
    }

    /// The cap and the dedupe: re-runs of one check arrive as repeated contexts, and fifty red
    /// checks are one broken pipeline — the row names the first [`FAILING_CHECKS_SHOWN`] distinct
    /// ones and stops. Absence stays absent throughout: a green rollup names nothing, a missing
    /// link renders as no link, and a queue remembered before the field existed still parses.
    #[test]
    fn failing_check_names_are_deduplicated_capped_and_absent_when_green() {
        // Seven failing contexts, but "build" three times (re-runs) and one nameless: five slots,
        // taken in order by the distinct named ones.
        let red = item(
            r#"{"statusCheckRollup":[
                {"name":"build","detailsUrl":"https://ci/1","status":"COMPLETED","conclusion":"FAILURE"},
                {"name":"build","detailsUrl":"https://ci/2","status":"COMPLETED","conclusion":"FAILURE"},
                {"status":"COMPLETED","conclusion":"FAILURE"},
                {"name":"unit","status":"COMPLETED","conclusion":"FAILURE"},
                {"name":"e2e","status":"COMPLETED","conclusion":"TIMED_OUT"},
                {"context":"style","state":"ERROR"},
                {"name":"build","detailsUrl":"https://ci/3","status":"COMPLETED","conclusion":"FAILURE"},
                {"name":"docs","status":"COMPLETED","conclusion":"CANCELLED"},
                {"name":"pack","status":"COMPLETED","conclusion":"FAILURE"},
                {"name":"sixth","status":"COMPLETED","conclusion":"FAILURE"}
            ]}"#,
        );
        let named = failing_contexts(&red);
        assert_eq!(named.len(), FAILING_CHECKS_SHOWN, "capped, not the log");
        let names: Vec<&str> = named.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["build", "unit", "e2e", "style", "docs"],
            "distinct names in rollup order — the nameless one is skipped, the word already says failing"
        );
        assert_eq!(
            named[0].url, "https://ci/1",
            "a re-run does not steal the first run's link"
        );
        assert_eq!(named[1].url, "", "a rollup with no link stays linkless");

        let green = item(
            r#"{"statusCheckRollup":[{"name":"build","status":"COMPLETED","conclusion":"SUCCESS"}]}"#,
        );
        assert!(
            failing_contexts(&green).is_empty(),
            "nothing failed, nothing to name"
        );

        // A queue remembered on disk by an older skein has neither new field, and must not stop
        // parsing over it — that failure mode turns a new field into an empty pane.
        let old: Pr = serde_json::from_value(serde_json::json!({
            "number": 7, "title": "t", "author": "a", "url": "u",
            "head_ref": "f", "head_sha": "s", "base_ref": "main",
            "draft": false, "updated_at": "", "committed_at": "",
            "checks": "failing", "my_review": "none", "review_is_current": false,
            "reasons": [], "lane": "needs-you", "box_name": "b",
        }))
        .expect("a remembered queue from before these fields must stay readable");
        assert!(old.failing_checks.is_empty());
        assert!(!old.snoozed);
    }

    /// SKEIN-142: GitHub's own verdict on the pull request is READ, not just fetched.
    /// `reviewDecision` is the repository's authority on "does this still need somebody" — branch
    /// protection and CODEOWNERS, rules skein cannot see — where `my_review` stays the authority
    /// on "does it need ME". Where the two disagree, the person-level fact wins.
    ///
    /// **What that disagreement IS was corrected by SKEIN-354.** It used to be "the repo is
    /// satisfied but your own approval was left against an older head", and the older head was
    /// skein's inference; it is now "the repo is satisfied but GitHub is asking you again", which
    /// is somebody actually wanting something from you.
    #[test]
    fn githubs_approval_moves_review_work_off_you_unless_github_is_asking_you_again() {
        // Somebody else's approval satisfied the repo: not review work any more — it waits on a
        // merge, not on you.
        let theirs = item(
            r#"{"number":1,"headRefOid":"a","author":{"login":"someone"},"reviewDecision":"APPROVED"}"#,
        );
        assert_eq!(
            build_pr(
                &theirs,
                1,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::Waiting,
            "the repository is satisfied and the queue is for review work"
        );

        // Empty means the repo REQUIRES no review — the queue's whole purpose is repos where
        // review is social rather than enforced, and demoting on silence would empty it there.
        let unenforced = item(
            r#"{"number":2,"headRefOid":"a","author":{"login":"someone"},"reviewDecision":""}"#,
        );
        assert_eq!(
            build_pr(
                &unenforced,
                2,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NeedsYou,
            "no required review is not the same fact as an approved one"
        );

        // The disagreement: the repository is satisfied, and GitHub is asking YOU anyway — which is
        // what a CODEOWNERS re-request on a file you own looks like. The person-level fact wins:
        // being one of the approvals that satisfied a rule is not the same fact as nobody wanting
        // anything from you.
        let asked_again = item(
            r#"{"number":3,"headRefOid":"new","author":{"login":"someone"},
                "reviewDecision":"APPROVED",
                "reviewRequests":[{"name":"me","team":false}],
                "latestReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}"#,
        );
        assert_eq!(
            build_pr(
                &asked_again,
                3,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NeedsYou,
            "GitHub asked you again and a repo-level approval hid the ask"
        );

        // The other two words change nothing.
        for decision in ["CHANGES_REQUESTED", "REVIEW_REQUIRED"] {
            let pr = item(&format!(
                r#"{{"number":4,"headRefOid":"a","author":{{"login":"someone"}},"reviewDecision":"{decision}"}}"#
            ));
            assert_eq!(
                build_pr(
                    &pr,
                    4,
                    "me",
                    "repo",
                    &Reason::Reviewer,
                    &[],
                    &BTreeMap::new()
                )
                .lane,
                Lane::NeedsYou,
                "{decision} keeps the behaviour that always held"
            );
        }
    }

    /// SKEIN-144: set aside *until the head moves*. The snooze names the sha it was taken at, so
    /// the author's next push — not an act, not a timer — is what brings the row back: the entry
    /// stops matching and is ignored.
    #[test]
    fn a_snooze_holds_a_pr_only_at_the_head_it_was_set_aside_at() {
        let held = BTreeMap::from([(3u64, "abc".to_string())]);

        let same = item(r#"{"number":3,"headRefOid":"abc","author":{"login":"someone"}}"#);
        let pr = build_pr(&same, 3, "me", "repo", &Reason::Reviewer, &[], &held);
        assert_eq!(
            pr.lane,
            Lane::Archived,
            "out of Needs you while the head sits"
        );
        assert!(pr.snoozed, "the row can say WHY it is set aside");

        let moved = item(r#"{"number":3,"headRefOid":"def","author":{"login":"someone"}}"#);
        let pr = build_pr(&moved, 3, "me", "repo", &Reason::Reviewer, &[], &held);
        assert_eq!(
            pr.lane,
            Lane::NeedsYou,
            "the push IS the un-snooze — the row returns with no action"
        );
        assert!(!pr.snoozed);

        // "GitHub did not say" must never be what keeps a row hidden: an absent head matches no
        // snooze, even one whose stored sha is somehow empty too.
        let unknown = item(r#"{"number":9,"author":{"login":"someone"}}"#);
        let empty_sha = BTreeMap::from([(9u64, String::new())]);
        let pr = build_pr(
            &unknown,
            9,
            "me",
            "repo",
            &Reason::Reviewer,
            &[],
            &empty_sha,
        );
        assert_eq!(pr.lane, Lane::NeedsYou);

        // Archived outright is the other instrument, and the reason stays distinguishable.
        let pr = build_pr(
            &same,
            3,
            "me",
            "repo",
            &Reason::Reviewer,
            &[3],
            &BTreeMap::new(),
        );
        assert_eq!(pr.lane, Lane::Archived);
        assert!(!pr.snoozed, "archived-forever is not a snooze");
    }

    #[test]
    fn snoozes_are_idempotent_re_aimable_and_cleared_by_hand_with_none() {
        let _home = fresh_home();
        set_snoozed("r", 7, Some("abc")).unwrap();
        set_snoozed("r", 7, Some("abc")).unwrap();
        assert_eq!(snoozed("r"), BTreeMap::from([(7u64, "abc".to_string())]));

        // Snoozing again at a newer head re-aims the hold rather than stacking one.
        set_snoozed("r", 7, Some("def")).unwrap();
        assert_eq!(snoozed("r"), BTreeMap::from([(7u64, "def".to_string())]));

        set_snoozed("r", 7, None).unwrap();
        set_snoozed("r", 7, None).unwrap();
        assert!(snoozed("r").is_empty());

        // An empty sha is refused, not stored: build_pr would never match it, so storing it could
        // only ever be dead weight in the file.
        set_snoozed("r", 9, Some("")).unwrap();
        assert!(snoozed("r").is_empty());

        // Per repo, like the archive.
        set_snoozed("one", 4, Some("s")).unwrap();
        assert!(snoozed("two").is_empty());
    }

    #[test]
    fn the_box_name_is_derived_from_the_head_branch() {
        let v = item(r#"{"number":3,"headRefName":"feature/thing"}"#);
        let pr = build_pr(&v, 3, "me", "acme", &Reason::Author, &[], &BTreeMap::new());
        assert_eq!(pr.box_name, crate::repos::box_name("acme", "feature/thing"));
    }

    // ---- SKEIN-209: the five membership searches travel in ONE GraphQL request ----

    /// One PR node as the batched search returns it — the minimum the parser keys on.
    fn search_node(number: u64) -> String {
        format!(
            r#"{{"number":{number},"title":"pr {number}","url":"https://github.com/acme/x/pull/{number}","isDraft":false,"author":{{"login":"someone"}},"headRefName":"feat-{number}","headRefOid":"sha{number}","updatedAt":"2026-08-1{number}T00:00:00Z","latestReviews":{{"nodes":[]}}}}"#
        )
    }

    /// A GitHub for the batched wire: `/graphql` answers `status` + `graphql_body`, teams answer
    /// one team (`acme/core`) when `teams`, and every request is recorded as `"METHOD path body"`
    /// — the wire is the thing under test, exactly as `fake_github` argues above. `/rate_limit`
    /// answers an unusable `{}` on purpose: learning a real reset is github.rs's own test's job,
    /// and the flat fallback hold is all the queue side needs to prove here.
    fn batched_github(
        teams: bool,
        status: u16,
        graphql_body: String,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        batched_github_pages(teams, vec![(status, graphql_body)])
    }

    /// The same GitHub, answering a DIFFERENT status and body to each successive `/graphql` request
    /// — the first, then the second, and the last one for every request after that.
    ///
    /// Paging asks the same endpoint twice in one refresh and expects two different answers
    /// (SKEIN-280), and a stub with one canned answer cannot tell a refresh that followed a cursor
    /// from one that re-asked the same page.
    fn batched_github_pages(
        teams: bool,
        pages: Vec<(u16, String)>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        batched_github_answering(teams, move |n, _| pages[n.min(pages.len() - 1)].clone())
    }

    /// The same GitHub again, answering from the REQUEST rather than from a script: `answer` is
    /// handed the call's ordinal and its body.
    ///
    /// What the batch-width test needs and the sequence above cannot give it (SKEIN-278): a GitHub
    /// that refuses the wide request **every** time and answers the narrow one every time. A
    /// scripted stub that 504s only once cannot tell "skein remembered the width" from "GitHub
    /// stopped refusing", which is the difference the whole item is about.
    fn batched_github_answering(
        teams: bool,
        answer: impl Fn(usize, &str) -> (u16, String) + Send + 'static,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let served = std::sync::atomic::AtomicUsize::new(0);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder
                    .lock()
                    .unwrap()
                    .push(format!("{method} {path} {body}"));
                let (code, answer) = match path.as_str() {
                    "/graphql" => {
                        let n = served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        answer(n, &body)
                    }
                    "/rate_limit" => (200, "{}".to_string()),
                    p if p.starts_with("/user/teams") => match teams {
                        true => (
                            200,
                            r#"[{"slug":"core","organization":{"login":"acme"}}]"#.to_string(),
                        ),
                        // **What a token without `read:org` actually gets** — a 403, not an empty
                        // list. It used to answer `200 []`, which since SKEIN-262 is a different
                        // fact: an empty list is GitHub saying you are in no teams, and every test
                        // that meant "the scope is missing" was quietly asserting against the
                        // wrong one. `tests/review_queue.rs`'s stub has always answered 403 here.
                        false => (403, r#"{"message":"Requires read:org"}"#.to_string()),
                    },
                    "/user" => (200, r#"{"login":"me"}"#.to_string()),
                    p if p.starts_with("/repos/") => (
                        200,
                        format!(
                            r#"{{"full_name":"{}","default_branch":"main"}}"#,
                            p.trim_start_matches("/repos/")
                        ),
                    ),
                    _ => (200, "{}".to_string()),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// The repo the batched tests refresh. Distinct slugs per test, so the per-process rename and
    /// trunk caches cannot leak one test's answers into another.
    fn batched_repo(slug: &str) -> crate::repos::Repo {
        serde_json::from_value(serde_json::json!({
            "id": slug.rsplit('/').next().unwrap_or(slug),
            "source": format!("https://github.com/{slug}.git"),
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap()
    }

    /// A GitHub that answers one canned body to every request — except on `dies_on`, where it
    /// kills the connection mid-answer. `answer: None` kills every connection. For proving what a
    /// code path does NOT ask for, and what it says when the one thing it does ask for dies.
    fn recording_github(
        answer: Option<&'static str>,
        dies_on: Option<&'static str>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 65536];
                let n = stream.read(&mut buf).unwrap_or(0);
                recorder
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let asked = recorder.lock().unwrap().last().cloned().unwrap_or_default();
                let dead = dies_on.is_some_and(|p| asked.contains(p));
                match answer.filter(|_| !dead) {
                    None => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\nhalf an ans",
                        );
                        let _ = stream.flush();
                    }
                    Some(body) => {
                        let _ = stream.write_all(
                            format!(
                                "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        );
                    }
                }
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// **A post must not inherit a read's failures** (SKEIN-272). The slug a write addresses used
    /// to come out of `queue(repo, false)` — a full refresh past its sixty-second cache, viewer
    /// lookup and five membership searches included — so a GitHub that would not answer a *read*
    /// made a *write* impossible, and said so in the refresh's own words. Reported live: the owner
    /// pressed "post comments" and was told five membership searches were missing.
    ///
    /// The remote is in the checkout. Nothing here needs GitHub to be up.
    #[test]
    fn the_repository_a_post_addresses_is_derived_without_a_refresh() {
        let (base, seen) = recording_github(None, None);
        let _env = wired(&base);
        forget_renames();

        let slug = slug_for_write(&batched_repo("acme/thing"))
            .expect("a GitHub that will not answer must not make a post impossible");

        assert_eq!(slug, "acme/thing");
        let asked = seen.lock().unwrap().clone();
        assert!(
            asked.iter().all(|r| !r.contains("/graphql")),
            "the post asked for a queue refresh: {asked:#?}"
        );
        assert!(
            asked.iter().all(|r| !r.contains("/user/teams")),
            "the post asked who the viewer's teams are: {asked:#?}"
        );
        forget_renames();
    }

    /// The fallback [`head_to_post_against`] uses, now that the post no longer refreshes the
    /// queue to produce one (SKEIN-272). It is what this machine already remembers, read from
    /// disk — and the point is what it must NOT be: the sha the draft was read at. Handing that in
    /// as its own fallback makes "did the branch move" compare a value against itself, nothing
    /// re-anchors, and vetted comments post at line numbers computed against a diff that no longer
    /// exists (SKEIN-230).
    #[test]
    fn the_head_a_post_falls_back_to_is_the_one_this_machine_remembers() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        assert_eq!(
            remembered_head("crit", 11),
            None,
            "a repo nothing is remembered about must say so rather than invent a sha"
        );

        let pr: Pr = serde_json::from_value(serde_json::json!({
            "number": 11, "title": "t", "author": "a", "url": "u",
            "head_ref": "f", "head_sha": "remembered111", "base_ref": "main",
            "draft": false, "updated_at": "", "committed_at": "",
            "checks": "passing", "my_review": "none", "review_is_current": false,
            "reasons": [], "lane": "needs-you", "box_name": "b",
        }))
        .unwrap();
        // Through the same door `review.rs`'s post tests use, so it cannot rot unnoticed.
        remember_for_test(&Queue {
            repo_id: "crit".into(),
            slug: "acme/thing".into(),
            trunk: "main".into(),
            viewer: "me".into(),
            ai: false,
            prs: vec![pr],
            blind_spots: Vec::new(),
            as_of: String::new(),
            fresh: false,
            whole: true,
        });

        assert_eq!(
            remembered_head("crit", 11).as_deref(),
            Some("remembered111"),
            "the post has no second opinion on where the branch was"
        );
        assert_eq!(
            remembered_head("crit", 12),
            None,
            "a pull request nothing is remembered about must not borrow another one's sha"
        );

        // And no network was needed for any of it: SKEIN_GITHUB_API points nowhere at all.
        std::env::remove_var("SKEIN_HOME");
    }

    /// The one GitHub fact a write does need: a renamed repository. A POST is not redirected the
    /// way a GET is, so the canonical name is followed — memoised, and `None` when it cannot be
    /// asked, which is what makes the test above possible.
    #[test]
    fn a_post_addresses_the_repository_under_the_name_it_has_now() {
        let (base, _seen) = recording_github(Some(r#"{"full_name":"acme/renamed"}"#), None);
        let _env = wired(&base);
        forget_renames();

        let slug = slug_for_write(&batched_repo("acme/thing")).expect("the rename resolved");

        assert_eq!(
            slug, "acme/renamed",
            "a review would have been posted to a name the repository no longer has"
        );
        forget_renames();
    }

    /// The recorded `/graphql` requests, whole.
    fn graphql_requests(seen: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|r| r.contains(" /graphql "))
            .cloned()
            .collect()
    }

    /// The 5→1 cut itself: one refresh is ONE `/graphql` request carrying q0..q4 and the shared
    /// fragment — and the parsed queue is what five separate requests produced before. The fixture
    /// is `tests/review_queue.rs`'s "appears once with every reason" case ported to the batched
    /// wire: a PR found by two rules carries both Reasons, in the order the searches are listed.
    #[test]
    fn one_refresh_is_one_graphql_request_carrying_every_membership_rule() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let answer = format!(
            r#"{{"data":{{"q0":{{"nodes":[{one},{seven}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{seven}]}},"q3":{{"nodes":[]}},"q4":{{"nodes":[{nine}]}}}}}}"#,
            one = search_node(1),
            seven = search_node(7),
            nine = search_node(9),
        );
        let (base, seen) = batched_github(true, 200, answer);
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let q = queue(&batched_repo("acme/batch-one"), true).expect("the queue answered");

        let requests = graphql_requests(&seen);
        assert_eq!(
            requests.len(),
            1,
            "five membership rules must cost ONE GraphQL request, got {requests:#?}"
        );
        let sent = &requests[0];
        for alias in ["q0", "q1", "q2", "q3", "q4"] {
            assert!(
                sent.contains(&format!("{alias}: search(query: ${alias}")),
                "alias {alias} is missing from the one request: {sent}"
            );
        }
        assert!(
            sent.contains("fragment PrFields on PullRequest") && sent.contains("...PrFields"),
            "the aliases must share the PR node through one fragment: {sent}"
        );
        for rule in [
            "review-requested:me",
            "reviewed-by:me",
            "author:me",
            "mentions:me",
            "team-review-requested:acme/core",
        ] {
            assert!(
                sent.contains(&format!("repo:acme/batch-one is:pr is:open {rule}")),
                "the `{rule}` search is missing from the variables: {sent}"
            );
        }

        // The same queue five requests built: each alias contributes, a PR found by several
        // aliases appears once with every reason, in search-list order.
        let mut numbers: Vec<u64> = q.prs.iter().map(|p| p.number).collect();
        numbers.sort();
        assert_eq!(
            numbers,
            vec![1, 7, 9],
            "every alias's PRs are in the one queue"
        );
        let seven = q.prs.iter().find(|p| p.number == 7).unwrap();
        assert_eq!(
            seven.reasons,
            vec![Reason::Reviewer, Reason::Author],
            "both memberships kept, in query order"
        );
        assert_eq!(
            q.prs.iter().find(|p| p.number == 9).unwrap().reasons,
            vec![Reason::Team("acme/core".into())]
        );
        assert!(
            q.blind_spots.is_empty(),
            "nothing was hidden: {:?}",
            q.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
    }

    /// GraphQL's partial failure — `data.qN: null` plus an error whose `path` names the alias —
    /// maps back to ITS search's blind spot, and the aliases that answered still fill the queue.
    /// This is exactly what five separate requests gave: partial answers beat none.
    ///
    /// The failed alias is deliberately a MIDDLE one (`q2`, `author:me`): a mapping that pins
    /// every failure on the first alias would pass a q0 fixture by accident, and the wrong-rule
    /// blind spot it produces is precisely the lie this test exists to make loud.
    #[test]
    fn a_failed_alias_is_its_own_blind_spot_and_the_rest_still_answer() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let answer = format!(
            r#"{{"data":{{"q0":{{"nodes":[{five}]}},"q1":{{"nodes":[]}},"q2":null,"q3":{{"nodes":[]}}}},"errors":[{{"message":"HTTP 403: forbidden","path":["q2"]}}]}}"#,
            five = search_node(5),
        );
        let (base, seen) = batched_github(false, 200, answer);
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let q = queue(&batched_repo("acme/batch-partial"), true).expect("the queue answered");

        assert_eq!(graphql_requests(&seen).len(), 1);
        assert!(
            q.blind_spots.iter().any(|b| b
                .contains("the `author:me` query failed, so those PRs are missing")
                && b.contains("403")),
            "the failed alias must name ITS membership rule, with GitHub's reason: {:?}",
            q.blind_spots
        );
        for survivor in ["review-requested:me", "reviewed-by:me", "mentions:me"] {
            assert!(
                !q.blind_spots
                    .iter()
                    .any(|b| b.contains(&format!("`{survivor}` query failed"))),
                "an alias that answered was reported as failed: {:?}",
                q.blind_spots
            );
        }
        assert_eq!(
            q.prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![5],
            "the aliases that answered still contribute"
        );
        assert_eq!(q.prs[0].reasons, vec![Reason::Reviewer]);

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
    }

    /// A whole-request failure — here a 500, live it is just as often the network — is every
    /// membership rule going dark at once, and it is reported as the one failure it is.
    ///
    /// The negative half is the point (SKEIN-258): the per-rule sentence is right for a per-alias
    /// failure and wrong here, where repeating it once per rule turned one dead request into five
    /// alarms on the owner's cold load.
    #[test]
    fn a_dead_batched_request_says_once_that_every_membership_is_missing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, _seen) = batched_github(false, 500, r#"{"message":"boom"}"#.to_string());
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let q = queue(&batched_repo("acme/batch-dead"), true).expect("the queue still answers");

        assert!(q.prs.is_empty());
        assert!(
            q.blind_spots.iter().any(|b| {
                b.contains("GitHub did not answer for acme/batch-dead")
                    && b.contains("membership searches are missing")
                    && b.contains("500")
            }),
            "the refresh's total loss went unreported: {:?}",
            q.blind_spots
        );
        for rule in [
            "review-requested:me",
            "reviewed-by:me",
            "author:me",
            "mentions:me",
        ] {
            assert!(
                !q.blind_spots
                    .iter()
                    .any(|b| b.contains(&format!("the `{rule}` query failed"))),
                "one dead request must not be reported as one broken rule per membership: {:?}",
                q.blind_spots
            );
        }

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
    }

    /// A refresh whose CONNECTION died says so, rather than "GitHub did not answer" (SKEIN-271).
    ///
    /// The two ask different things of whoever reads them. "GitHub did not answer" sends them to
    /// look at GitHub — a token, a rate limit, a refusal — and the connection dying is not GitHub
    /// answering anything; it says the request never completed, so ask again. Skein already has,
    /// once, by the time this line is written, and the sentence says that too.
    #[test]
    fn a_refresh_whose_connection_died_says_so_rather_than_blaming_github() {
        let _g = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) = recording_github(Some(r#"{"login":"me"}"#), Some("/graphql"));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/batch-cut"), true).expect("the queue still answers");

        assert!(q.prs.is_empty());
        let said = q
            .blind_spots
            .iter()
            .find(|b| b.contains("membership searches for acme/batch-cut are missing"))
            .unwrap_or_else(|| {
                panic!(
                    "the refresh's total loss went unreported: {:?}",
                    q.blind_spots
                )
            });
        assert!(
            said.contains("connection to GitHub died"),
            "the reader is sent to look at GitHub for something GitHub never said: {said}"
        );
        assert!(
            !said.contains("GitHub did not answer for"),
            "the two diagnoses must not be the same sentence: {said}"
        );
        assert!(
            said.contains("asked a second time"),
            "a reader deciding whether to press again is not told skein already did: {said}"
        );
        // …and it really did ask twice, rather than only claiming to.
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|r| r.contains("/graphql"))
                .count()
                >= 2,
            "the retry the sentence promises never happened"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// A rate-limited batch is both at once: the refresh's whole loss stated, and the hold engaged
    /// — the next refresh dies at home, never reaching the wire (SKEIN-208's contract, kept through
    /// the merge into one request).
    #[test]
    fn a_rate_limited_batch_engages_the_hold_and_says_the_refresh_is_missing() {
        let _g = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) = batched_github(
            false,
            200,
            r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 123"}]}"#
                .to_string(),
        );
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();

        let first = queue(&batched_repo("acme/batch-limited"), true)
            .expect("a rate-limited refresh still answers, with its blind spots");
        assert!(
            first.blind_spots.iter().any(|b| {
                b.contains("GitHub did not answer for acme/batch-limited")
                    && b.contains("membership searches are missing")
                    && b.contains("rate limiting skein")
            }),
            "a rate-limited batch must say the whole refresh is missing, and why: {:?}",
            first.blind_spots
        );

        // The hold is engaged: the next refresh is refused before the wire — viewer() is the
        // first call a refresh makes, and it never leaves the process.
        let second = queue(&batched_repo("acme/batch-limited"), true)
            .expect_err("a held refresh cannot even identify the viewer");
        assert!(
            second.contains("not calling GitHub"),
            "the refusal says what is happening: {second}"
        );
        assert_eq!(
            graphql_requests(&seen).len(),
            1,
            "the second refresh must never reach the server"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
    }

    /// A check rollup as GitHub sends one: `state` beside a page of `contexts`.
    ///
    /// `state: None` is an answer from before SKEIN-232 asked for the field, which is what every
    /// fixture written against the old shape is — those must keep reading exactly as they did.
    fn rollup_node(state: Option<&str>, total: usize, contexts: &[&str]) -> serde_json::Value {
        let state = match state {
            Some(s) => format!(r#""state":"{s}","#),
            None => String::new(),
        };
        item(&format!(
            r#"{{"number":7,"title":"t","url":"u","isDraft":false,
                "headRefName":"feat","headRefOid":"abc","baseRefName":"main",
                "updatedAt":"2026-08-25T00:00:00Z","author":{{"login":"me"}},
                "latestReviews":{{"nodes":[]}},
                "commits":{{"nodes":[{{"commit":{{"committedDate":"2026-08-25T00:00:00Z",
                  "statusCheckRollup":{{{state}"contexts":{{"totalCount":{total},"nodes":[{}]}}}}
                }}}}]}}}}"#,
            contexts.join(",")
        ))
    }

    /// A hundred green contexts: the page GitHub fills, and the whole of what skein used to see.
    fn a_full_page_of_green() -> Vec<&'static str> {
        vec![r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#; SEARCH_PAGE]
    }

    /// **A red check past the hundredth context must not read as green** (SKEIN-232).
    ///
    /// The queue used to compute the verdict itself, from a `contexts(first: 100)` page, and never
    /// asked GitHub for its own answer over all of them. A matrix build (`os × rust-version ×
    /// feature`) reaches three digits routinely, so the 101st context being red was invisible.
    ///
    /// The consequence is asserted here rather than described, because it is not a wrong dot on a
    /// row: `docs/pr-workflow.md`'s merge train fires on `checks:passing` — the same string, off the
    /// same field ([`crate::prwork`] builds `Facts::checks` from `Pr::checks`) — and its act is
    /// `merge:squash+delete`. A green verdict computed from a page nobody could see all of is a
    /// merged pull request whose CI failed, with the journal recording a clean merge.
    #[test]
    fn a_red_check_past_the_hundredth_context_does_not_read_as_passing() {
        let flat = shape(&rollup_node(Some("FAILURE"), 143, &a_full_page_of_green()));

        assert_eq!(
            rollup(&flat),
            "failing",
            "every context skein can see is green and GitHub says the commit is red — the red is \
             in the 43 it never read, and GitHub's own verdict is the only thing that knows"
        );

        // And the act that verdict guards. The owner's train, as `docs/pr-workflow.md` writes it.
        let train = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"train","serial":true,"matches":["mine"],"steps":[
                 {"when":["label:ci-queue","checks:passing","mergeable","current"],
                  "do":"merge:squash+delete"}]}]}"#,
        )
        .expect("the merge train must be expressible");
        let facts = crate::workflow::Facts {
            approved: true,
            labels: vec!["ci-queue".into()],
            checks: rollup(&flat),
            mergeable: Some(true),
            behind: Some(false),
            base_is_trunk: Some(true),
            mine: true,
            // `ci-queue` is ALL of this pull request's labels, said rather than defaulted:
            // `Facts::default()` is skein having looked nothing up, and on that a `no-label:` may
            // not hold (SKEIN-373). This fixture is about the check rollup, so it must not be
            // silently exercising a truncated label list as well.
            labels_whole: true,
            ..Default::default()
        };
        assert_eq!(
            crate::workflow::next(&train[0], &facts),
            None,
            "the merge train took a step on a pull request whose CI failed — this is the merge, \
             and the branch deletion, that the wrong verdict authorises"
        );

        // The other direction, which is what stops this fix being "say pending and never merge":
        // a rollup GitHub calls green, whose contexts are green, still merges.
        let green = shape(&rollup_node(Some("SUCCESS"), 143, &a_full_page_of_green()));
        assert_eq!(rollup(&green), "passing");
        let facts = crate::workflow::Facts {
            checks: rollup(&green),
            ..facts
        };
        assert!(
            matches!(
                crate::workflow::next(&train[0], &facts),
                Some(crate::workflow::Chosen {
                    act: crate::workflow::Act::Merge(_),
                    ..
                })
            ),
            "a genuinely green pull request must still merge, or the fix has broken the train \
             instead of the bug"
        );
    }

    /// The verdict is the more cautious of the two sources, and an old answer still reads as it did.
    ///
    /// Each line here is a case the single-source walk got wrong or must keep getting right — the
    /// last two are the fixtures from before `state` was asked for, which have no `state` at all
    /// and must be untouched by any of this.
    #[test]
    fn a_check_verdict_never_out_ranks_the_source_that_saw_more() {
        let red = r#"{"status":"COMPLETED","conclusion":"FAILURE"}"#;
        let running = r#"{"status":"IN_PROGRESS"}"#;
        let green = r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#;
        let verdict =
            |state, total, contexts: &[&str]| rollup(&shape(&rollup_node(state, total, contexts)));

        // GitHub's word for a red commit, whichever word it uses, over a page that looks fine.
        assert_eq!(
            verdict(Some("FAILURE"), 143, &a_full_page_of_green()),
            "failing"
        );
        assert_eq!(
            verdict(Some("ERROR"), 143, &a_full_page_of_green()),
            "failing"
        );
        // Still running, over a page that has all finished.
        assert_eq!(
            verdict(Some("PENDING"), 143, &a_full_page_of_green()),
            "pending"
        );
        assert_eq!(
            verdict(Some("EXPECTED"), 143, &a_full_page_of_green()),
            "pending"
        );
        // A word this code does not know is not a merge anybody may take.
        assert_eq!(verdict(Some("SOMETHING_NEW"), 1, &[green]), "pending");
        // And the page out-ranks a green verdict in the other direction: a red or an unfinished
        // context skein can SEE is not overruled by GitHub calling the commit green.
        assert_eq!(verdict(Some("SUCCESS"), 101, &[red]), "failing");
        assert_eq!(verdict(Some("SUCCESS"), 101, &[running]), "pending");
        // Nothing has run: unchanged, and it is why `totalCount` may not simply default to zero.
        assert_eq!(verdict(Some("EXPECTED"), 0, &[]), "none");

        // No `state` — every fixture written before SKEIN-232, and any answer GitHub gives without
        // one. The page is all there is, and it is read exactly as it always was…
        assert_eq!(verdict(None, 1, &[green]), "passing");
        assert_eq!(verdict(None, 2, &[green, red]), "failing");
        assert_eq!(verdict(None, 0, &[]), "none");
        // …except that a page which was CUT OFF has not earned "passing" on its own.
        assert_eq!(
            verdict(None, 143, &a_full_page_of_green()),
            "pending",
            "a walk of 100 of 143 contexts that found nothing wrong knows nothing about the 43"
        );
    }

    /// The queue says which pull request's checks it could not read (SKEIN-232).
    ///
    /// "pending" is the honest verdict for a cut-off list nobody gave a verdict for, and on its own
    /// it is indistinguishable from CI still running — which is a sentence somebody waits on. The
    /// blind spot is the difference, and it fires only where the truncation actually costs the
    /// answer: with GitHub's `state` present the cap costs a NAME and nothing else, and a blind spot
    /// per matrix build would be noise over a verdict that is right.
    #[test]
    fn a_check_list_the_queue_could_not_read_to_the_end_says_so() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");

        let node = |state: Option<&str>| {
            serde_json::to_string(&rollup_node(state, 143, &a_full_page_of_green())).unwrap()
        };
        let answer = |state: Option<&str>| {
            format!(
                r#"{{"data":{{"q0":{{"nodes":[{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                node(state)
            )
        };

        let (base, seen) = batched_github(false, 200, answer(None));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/rollup-cut"), true).expect("the queue answered");

        assert!(
            graphql_requests(&seen)[0]
                .contains("statusCheckRollup { state contexts(first: 100) { totalCount"),
            "the fragment must ask for GitHub's own verdict and the size of the list: {}",
            graphql_requests(&seen)[0]
        );
        assert_eq!(
            q.prs[0].checks, "pending",
            "a cut-off list with no verdict must not read as green"
        );
        assert!(
            q.blind_spots.iter().any(|b| b.contains("#7's checks")
                && b.contains("143")
                && b.contains("no rollup verdict")),
            "the row says `pending` and nothing says why it is not `passing`: {:?}",
            q.blind_spots
        );

        // The same pull request, with GitHub's verdict beside the same cut-off page: the answer is
        // certain, so there is nothing to warn about.
        let (base, _seen) = batched_github(false, 200, answer(Some("SUCCESS")));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/rollup-cut"), true).expect("the queue answered");
        assert_eq!(q.prs[0].checks, "passing");
        assert!(
            !q.blind_spots.iter().any(|b| b.contains("#7's checks")),
            "a verdict GitHub stands behind needs no blind spot beside it: {:?}",
            q.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// **The queue says which pull request had reviewers it could not see** (SKEIN-386).
    ///
    /// The unit test above proves one node parses into a row that knows its review list is short.
    /// This is the half a person meets: whether the QUEUE says so — the same shape as
    /// `a_check_list_the_queue_could_not_read_to_the_end_says_so` and there for the same reason.
    ///
    /// The counter-case is in the same test on purpose. A blind spot on every pull request is
    /// noise, and noise is what stops blind spots being read at all, so the ordinary pull request
    /// whose reviews all arrived must produce no sentence.
    #[test]
    fn a_pull_request_with_more_reviewers_than_the_page_says_how_many_it_lost() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");

        // `read` of `total` reviews, on both connections — which is what a pull request reviewed by
        // more people than the cap looks like on the wire.
        let node = |number: u64, total: u64, read: usize| {
            let nodes = (1..=read)
                .map(|i| {
                    format!(
                        r#"{{"state":"APPROVED","author":{{"login":"reviewer-{i:02}"}},"commit":{{"oid":"sha{number}"}}}}"#
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                r#"{{"number":{number},"title":"reviewed by the whole team","url":"u","isDraft":false,"author":{{"login":"someone"}},"headRefName":"feat-{number}","headRefOid":"sha{number}","updatedAt":"2026-08-26T00:00:00Z","latestReviews":{{"totalCount":{total},"nodes":[{nodes}]}},"latestOpinionatedReviews":{{"totalCount":{total},"nodes":[{nodes}]}}}}"#
            )
        };
        let answer = format!(
            r#"{{"data":{{"q0":{{"nodes":[{cut},{whole}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
            cut = node(31, 31, REVIEWS_FETCHED),
            whole = node(32, 2, 2),
        );

        let (base, seen) = batched_github(false, 200, answer);
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/reviews-cut"), true).expect("the queue answered");
        let row = |n: u64| q.prs.iter().find(|p| p.number == n).expect("the row");

        assert!(
            graphql_requests(&seen)[0].contains(&format!(
                "latestReviews(first: {REVIEWS_FETCHED}) {{ totalCount"
            )),
            "the request itself must ask how many reviews there were, or nothing downstream has \
             anything to say the hole out loud with: {}",
            graphql_requests(&seen)[0]
        );
        assert_eq!(
            (row(31).reviews_total, row(31).reviews_read),
            (Some(31), Some(REVIEWS_FETCHED as u64)),
            "GitHub's count of the reviewers never reached the queue, so nothing downstream can \
             tell this row's `my review` and standing approvals from answers — the defect exactly"
        );

        let said: Vec<&String> = q
            .blind_spots
            .iter()
            .filter(|s| s.contains("#31's reviews"))
            .collect();
        assert_eq!(
            said.len(),
            1,
            "a review list cut off at the page is not said out loud, in a queue whose rule is that \
             a limit skein hit is said out loud: {:?}",
            q.blind_spots
        );
        let spot = said[0];
        assert!(
            spot.contains("31") && spot.contains(&REVIEWS_FETCHED.to_string()),
            "the sentence must carry the size of the hole and not only its existence: {spot}"
        );
        assert!(
            spot.contains("floors"),
            "the blind spot must say what the truncation COSTS — `my review` and the standing \
             approvals on this row are floors from here on, so a reviewer looking at `none` on a \
             pull request they approved has a reason here rather than a mystery: {spot}"
        );

        // The counter-case: two reviews, two arrived, nothing to say.
        assert!(
            row(32).reviews_whole() && row(32).reviews_total == Some(2),
            "the second row's reviews all arrived and it must know it"
        );
        assert!(
            !q.blind_spots.iter().any(|s| s.contains("#32's reviews")),
            "a pull request whose reviews all arrived was reported as short — a blind spot on \
             every row is a blind spot nobody reads: {:?}",
            q.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// **A membership search cut off at its page says how many it could not show** (SKEIN-231).
    ///
    /// The truncation was undetectable by construction: GitHub answers HTTP 200 with exactly a
    /// hundred nodes, and the queue rendered a list that looks complete. That is the rename bug in
    /// a quieter form — there, a stale name matched nothing and the queue was empty; here the queue
    /// is full and merely short, which is harder to notice and just as wrong.
    ///
    /// Three things travel together, and the test insists on all three: the sentence with the
    /// count in it, `whole: false` so nothing downstream reads absence as evidence, and the
    /// archive entry for a pull request past the page surviving the refresh.
    #[test]
    fn a_membership_search_cut_off_at_its_page_says_how_many_it_could_not_show() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");

        // Set aside by hand, and past the page of whatever the searches return: the queue cannot
        // see it, and "cannot see it" must not read as "it closed".
        set_archived("search-cut", 4242, true).expect("archived");

        // Teams listable, so the only thing this queue cannot see is the page — a token that
        // cannot list teams carries its own blind spot and its own `whole: false` (SKEIN-262),
        // which would make every assertion below pass for the wrong reason.
        let answer = |more: bool| {
            format!(
                r#"{{"data":{{"q0":{{"issueCount":143,"pageInfo":{{"hasNextPage":{more}}},"nodes":[{five}]}},
                   "q1":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q2":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q3":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q4":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}}}}}}"#,
                five = search_node(5),
            )
        };

        let (base, seen) = batched_github(true, 200, answer(true));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/search-cut"), true).expect("the queue answered");

        let sent = &graphql_requests(&seen)[0];
        assert!(
            sent.contains("issueCount") && sent.contains("hasNextPage"),
            "the search must ask how many it matched and whether it reached the end: {sent}"
        );
        // The count is what ARRIVED, not the page size (SKEIN-280): the refresh follows the cursor
        // now, so "the first 100" would be a guess about a number the queue already knows. This
        // fixture answers `hasNextPage: true` with no `endCursor` — which is also the assertion
        // that a page nobody can ask for ends the paging instead of being guessed at, since one
        // node came back and one node is what the sentence reports.
        assert!(
            q.blind_spots.iter().any(|b| {
                b.contains(
                "the `review-requested:me` query matched 143 pull requests and skein read 1 of them"
            )
            }),
            "a truncated search must name ITS rule and the size of the hole: {:?}",
            q.blind_spots
        );
        assert_eq!(
            graphql_requests(&seen).len(),
            1,
            "GitHub said there was more and gave nowhere to carry on from, and skein asked again \
             anyway — a page with no cursor is a page nobody can request"
        );
        assert!(
            !q.whole,
            "a queue missing 43 pull requests must not tell anybody it saw them all"
        );
        assert!(
            archived("search-cut").contains(&4242),
            "a set-aside pull request past the page was deleted because a truncated search did \
             not list it — the same erasure SKEIN-229 fixed for an outage"
        );
        // The rules that answered in full are not tarred with it.
        for whole in ["reviewed-by:me", "author:me", "mentions:me"] {
            assert!(
                !q.blind_spots.iter().any(|b| b.contains(whole)),
                "a search that reached the end was reported as truncated: {:?}",
                q.blind_spots
            );
        }

        // The same shape, reaching the end. Nothing is said, `whole` holds, and the prune runs —
        // which is what stops "say it is partial" from becoming "never prune anything".
        let (base, _seen) = batched_github(true, 200, answer(false));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/search-cut"), true).expect("the queue answered");
        assert!(
            !q.blind_spots.iter().any(|b| b.contains("query matched")) && q.whole,
            "a search that saw everything must say nothing about being cut off: {:?}",
            q.blind_spots
        );
        assert!(
            !archived("search-cut").contains(&4242),
            "a queue that saw everything still prunes a set-aside PR that is no longer open"
        );
        // Nothing here can prove what the OTHER reader of this list does with it — see
        // `the_only_other_reader_of_this_list_stands_down_when_it_is_partial`.

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// **A set-aside pull request that only a team review request would list survives a refresh
    /// made without `read:org`** (SKEIN-262).
    ///
    /// SKEIN-229 gated both prunes on `answered` — every membership search skein RAN answered in
    /// full. A team search that was never RUN is a different hole and was still open: without
    /// `read:org` the `for team in &teams` loop adds no search at all, the four personal rules all
    /// answer, `answered` stays true, and the prune deletes the owner's archive entry and snooze
    /// on the evidence of an open set that structurally could not contain the row. Silent, every
    /// three minutes on the badge poll, and permanent on a fleet whose token lacks the scope.
    ///
    /// Both halves, because either alone passes for the wrong reason: with the scope missing the
    /// decisions survive, and with the scope present the very same refresh prunes them. The second
    /// is what makes the first mean "skein declined to prune" rather than "the fixture could not
    /// refresh".
    #[test]
    fn a_set_aside_pr_no_search_could_have_listed_survives_a_refresh_without_read_org() {
        let _g = crate::testutil::env_lock();
        // Right after the env lock, per `github::HoldClear`'s own rule: a test elsewhere in this
        // binary can engage the rate-limit hold, and a held hold refuses every request before it
        // reaches the fixture — which reads here as a request that was never sent.
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");

        // #77's only claim on you is a team review request, so none of the four personal searches
        // will ever return it — which is exactly what makes its absence no evidence at all.
        set_archived("team-blind", 77, true).expect("archived");
        set_snoozed("team-blind", 77, Some("sha77")).expect("snoozed");

        let four_empty =
            r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#;
        let (base, _seen) = batched_github(false, 200, four_empty.to_string());
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let blind = queue(&batched_repo("acme/team-blind"), true).expect("the queue answered");
        assert!(
            blind
                .blind_spots
                .iter()
                .any(|b| b.contains("team review requests are missing")),
            "the fixture is not the one this test is about: {:?}",
            blind.blind_spots
        );
        assert!(
            archived("team-blind").contains(&77),
            "a set-aside pull request was deleted because a search that could not be RUN did not \
             list it — the same erasure SKEIN-229 fixed for a search that ran and failed"
        );
        assert!(
            snoozed("team-blind").contains_key(&77),
            "and the snooze on the same pull request went with it"
        );
        assert!(
            !blind.whole,
            "a queue that never asked about team review requests told everything downstream it \
             had seen every open pull request"
        );

        // The same refresh, with a token that CAN list teams. Every rule that exists was asked,
        // every one answered, nothing came back — so #77 really is closed and both files are
        // pruned. Without this half, deleting the prune entirely would pass the test above.
        let five_empty = r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]},"q4":{"nodes":[]}}}"#;
        let (base, _seen) = batched_github(true, 200, five_empty.to_string());
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let seeing = queue(&batched_repo("acme/team-blind"), true).expect("the queue answered");
        assert!(
            seeing.whole && seeing.blind_spots.is_empty(),
            "a refresh that asked every rule there is reported a hole: {:?}",
            seeing.blind_spots
        );
        assert!(
            archived("team-blind").is_empty() && snoozed("team-blind").is_empty(),
            "an answered refresh stopped pruning — a queue that says it saw everything must still \
             clear decisions about pull requests that are gone"
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// **A repo GitHub will not answer in one request is not asked in one request twice**
    /// (SKEIN-278).
    ///
    /// Reported live on `acme/thing`: the split-in-halves (SKEIN-266) recovered the refresh
    /// exactly as designed, and then did it again ten minutes later, and again — one doomed request
    /// per poll per repo, each carrying GitHub's own 504 latency, each announcing itself in the
    /// fleet's log. The recovery was never the complaint; re-learning it by failing was.
    ///
    /// What is remembered is a width GitHub ANSWERED, never the refusal — the rule
    /// `what_github_said` states for the lookups above it and the pattern SKEIN-281 names — so the
    /// memo expires and [`forget_batch_widths`] clears it. Both halves are asserted here, because
    /// either alone is a bug: the second refresh must not spend the doomed request, and the queue
    /// must SAY that it is being asked narrowly, which the item asks for in as many words ("the
    /// narrowing must be visible ... not a silent adaptation").
    #[test]
    fn a_repo_github_will_not_take_whole_is_not_asked_whole_on_the_next_refresh() {
        let _g = crate::testutil::env_lock();
        // Right after the env lock, per `github::HoldClear`'s own rule: a test elsewhere in this
        // binary can engage the rate-limit hold, and a held hold refuses every request before it
        // reaches the fixture — which reads here as a request that was never sent.
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        forget_batch_widths();

        // A 504 carrying JSON: `edge_refused` reads it as "would not take it", and `edge_shrug`
        // does not, so it costs ONE request rather than github.rs's own retry-once. Everything
        // after it answers.
        let empty = |aliases: usize| {
            let body = (0..aliases)
                .map(|i| {
                    format!(
                        r#""q{i}":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}}"#
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(r#"{{"data":{{{body}}}}}"#)
        };
        // Refused EVERY time it is asked wide, answered every time it is asked narrow — which is
        // what a 504 caused by the repository's own size actually is. A stub that refused once
        // could not tell "skein remembered the width" from "GitHub stopped refusing". Four aliases
        // or more is wide here, so the five-search refresh is refused and its halves (two and
        // three) are not; the answer carries five, so a narrower half reads the first few of it
        // and one body serves every width.
        let too_big = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let refusing = too_big.clone();
        let (base, seen) = batched_github_answering(true, move |_, body| {
            let wide = body.contains("$q3: String!");
            match wide && refusing.load(std::sync::atomic::Ordering::SeqCst) {
                true => (
                    504,
                    r#"{"message":"We couldn't respond to your request in time"}"#.to_string(),
                ),
                false => (200, empty(5)),
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let first = queue(&batched_repo("acme/too-big"), true).expect("the split recovered");
        let after_one = graphql_requests(&seen).len();
        assert_eq!(
            after_one, 3,
            "the first refresh should be the doomed request plus the two halves: {after_one}"
        );

        // The refresh that matters. Nothing about GitHub changed; what changed is that skein was
        // told once and wrote down the width that WORKED.
        let second = queue(&batched_repo("acme/too-big"), true).expect("the queue answered");
        let after_two = graphql_requests(&seen).len() - after_one;
        assert_eq!(
            after_two, 2,
            "the second refresh spent the doomed request again — the repo re-learns by failing on \
             every single poll, which is the whole bug: {after_two} requests"
        );

        // And it is VISIBLE. A repo that has quietly become expensive to refresh looks identical
        // from outside, so the narrowing is said on the queue itself.
        for q in [&first, &second] {
            assert!(
                q.blind_spots
                    .iter()
                    .any(|b| b.contains("membership searches are being asked 3 at a time")),
                "the narrowing is a silent adaptation — nothing the owner can read says this repo \
                 costs more than one request per refresh: {:?}",
                q.blind_spots
            );
        }
        // It is not a completeness claim: every search answered, so the prunes still run.
        assert!(
            second.whole,
            "asking in halves was reported as not having seen everything, which stops every prune"
        );

        // **The memo is a width GitHub answered, and it ends.** Cleared by hand here — the same
        // door `forget_renames` and `forget_trunks` give, and the reason SKEIN-281's pattern does
        // not apply: there is something a person can clear, and it expires on its own besides.
        forget_batch_widths();
        let before = graphql_requests(&seen).len();
        let again = queue(&batched_repo("acme/too-big"), true).expect("the split recovered");
        assert_eq!(
            graphql_requests(&seen).len() - before,
            3,
            "after forgetting the width the refresh did not go back to asking wide — the memo is a \
             narrowing nobody can undo"
        );
        assert!(
            again
                .blind_spots
                .iter()
                .any(|b| b.contains("being asked 3 at a time")),
            "the refusal is still standing and the queue stopped saying so: {:?}",
            again.blind_spots
        );

        // **And when GitHub gets better, the notice goes.** Nothing is pressed here except the
        // memo, which is what the hourly expiry does on its own in a running fleet: one request,
        // no sentence, and the repo is back where it started.
        too_big.store(false, std::sync::atomic::Ordering::SeqCst);
        forget_batch_widths();
        let before = graphql_requests(&seen).len();
        let healed = queue(&batched_repo("acme/too-big"), true).expect("the queue answered");
        assert_eq!(
            graphql_requests(&seen).len() - before,
            1,
            "GitHub took the wide request and the refresh split it anyway"
        );
        assert!(
            !healed.blind_spots.iter().any(|b| b.contains("being asked")),
            "the narrowing notice outlived the narrowing: {:?}",
            healed.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
        forget_batch_widths();
    }

    /// **A membership rule with more pull requests than one page is read to the END** (SKEIN-280).
    ///
    /// SKEIN-231 taught the queue to notice a truncated search and say so. It said so on every
    /// refresh, for ever, because nothing ever asked for the rest: a repo where more than a hundred
    /// pull requests match one rule showed a permanently short queue with a permanent apology
    /// beside it. Noticing is not reading.
    ///
    /// Four facts, and each one fails on its own:
    ///
    ///   * the pull request on page TWO is in the queue — the point of the whole change;
    ///   * the cursor GitHub handed back is the `after` skein sent, so the second request is the
    ///     next page rather than the same page again;
    ///   * only the rule that had more is asked again — the other three are finished and must not
    ///     cost a second request each;
    ///   * and once the last page says `hasNextPage: false` the queue is `whole` with NO blind
    ///     spot, because a search that was followed to the end saw everything there was. That is
    ///     what lets the prunes in `queue_within` and `review::prune` run again.
    #[test]
    fn a_membership_rule_longer_than_one_page_is_followed_to_the_end() {
        let _g = crate::testutil::env_lock();
        // Right after the env lock, per `github::HoldClear`'s own rule: a test elsewhere in this
        // binary can engage the rate-limit hold, and a held hold refuses every request before it
        // reaches the fixture — which reads here as a request that was never sent.
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");

        // Page one: #5 and a cursor. Page two: #6 and the end. The other three rules finish on
        // page one, which is what makes "only the unfinished rule is asked again" observable.
        let page_one = format!(
            r#"{{"data":{{"q0":{{"issueCount":2,"pageInfo":{{"hasNextPage":true,"endCursor":"CUR-2"}},"nodes":[{five}]}},
               "q1":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
               "q2":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
               "q3":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
               "q4":{{"issueCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}}}}}}"#,
            five = search_node(5),
        );
        // The follow-up carries ONE alias, so its answer has one: `q0` is the only rule that was
        // asked again, and the parser reads aliases positionally from what it sent.
        let page_two = format!(
            r#"{{"data":{{"q0":{{"issueCount":2,"pageInfo":{{"hasNextPage":false,"endCursor":"CUR-3"}},"nodes":[{six}]}}}}}}"#,
            six = search_node(6),
        );

        let (base, seen) = batched_github_pages(true, vec![(200, page_one), (200, page_two)]);
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/two-pages"), true).expect("the queue answered");

        let numbers: Vec<u64> = q.prs.iter().map(|p| p.number).collect();
        assert!(
            numbers.contains(&6),
            "the pull request past the first page never reached the queue — the refresh noticed \
             the truncation and did nothing about it: {numbers:?}"
        );
        assert!(
            numbers.contains(&5),
            "the first page was thrown away when the second arrived: {numbers:?}"
        );

        let sent = graphql_requests(&seen);
        assert_eq!(
            sent.len(),
            2,
            "one rule had a second page and the refresh cost {} requests: {sent:?}",
            sent.len()
        );
        assert!(
            sent[1].contains("CUR-2"),
            "the second request did not carry the cursor GitHub gave, so it asked for the same \
             page again: {}",
            sent[1]
        );
        // Only the unfinished rule. The three that reached their end on page one must not be
        // re-asked — that would make paging cost a full batch per page rather than one alias.
        assert!(
            sent[1].contains("review-requested:me") && !sent[1].contains("mentions:me"),
            "a search that had already reached its end was asked again on the next page: {}",
            sent[1]
        );

        // Followed to the end means whole, and whole means silent.
        assert!(
            q.whole,
            "a queue that read every page still says it might be missing pull requests, so \
             nothing downstream will ever prune again"
        );
        assert!(
            !q.blind_spots.iter().any(|b| b.contains("query matched")),
            "the truncation apology outlived the truncation: {:?}",
            q.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }

    /// `review::prune` reads this queue's list, and it may only do so when the list is whole.
    ///
    /// Asserted against the source, the way [`the_badge_reads_through_a_ten_minute_budget`] is and
    /// for the same reason: the call lives in the server binary, behind an HTTP handler and a
    /// detached task, so no test in this module can reach it — and the thing that goes wrong is
    /// the guard being dropped, which a runtime test of the pruning itself would never notice.
    ///
    /// What it costs when it is dropped: every pull request past a truncated search's page is
    /// absent from `open`, so its summary takes the "closed and merged is asked" road and pays a
    /// `pr_is_open` REST call — per file, per tab open, for ever, for pull requests that are alive
    /// and merely past the hundredth (SKEIN-231).
    #[test]
    fn the_only_other_reader_of_this_list_stands_down_when_it_is_partial() {
        let server = std::fs::read_to_string("src/bin/skein-server.rs").expect("the server");
        assert_eq!(
            server.matches("review::prune(").count(),
            1,
            "there is more than one caller now, and only one of them is pinned here"
        );
        let guarded = server
            .split("review::prune(")
            .next()
            .expect("the source before the call");
        // The PROPERTY, not one spelling of it: somewhere before the call, the queues that reach
        // it are filtered on `whole`. Pinned this way round because the exact expression has
        // already moved once — it was `slug.filter(|_| queue.whole)` inline, and is now a filter
        // in the helper that builds the list — and a test that fails on a refactor which KEEPS
        // the guard teaches whoever meets it to delete the test.
        assert!(
            guarded
                .lines()
                .any(|line| line.contains("filter") && line.contains("whole")),
            "the queue's list is pruned against without asking whether it saw everything — a \
             search cut off at its page makes every pull request past the hundredth absent for a \
             reason that has nothing to do with it"
        );
    }

    // ───────────────── resolving a review thread from the panel (SKEIN-305) ─────────────────

    /// A GitHub that answers each connection from a script and records what it was handed.
    ///
    /// `Some(body)` is a 200 carrying that JSON; `None` is the failure this must be tested against
    /// — headers written, then the connection dropped, which is the shape SKEIN-271 met live and
    /// the one where "it ran" and "it did not run" look identical from here. Every request's body
    /// is recorded, so a test reads the mutation that actually went out rather than the one the
    /// source appears to build.
    fn scripted_github(
        script: Vec<Option<&'static str>>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            let mut turn = 0usize;
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                recorder.lock().unwrap().push(format!(
                    "{} {}",
                    request.split_whitespace().nth(1).unwrap_or(""),
                    String::from_utf8_lossy(&body)
                ));
                let answer = script.get(turn).copied().flatten();
                turn += 1;
                match answer {
                    Some(json) => {
                        let _ = stream.write_all(
                            format!(
                                "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                                json.len()
                            )
                            .as_bytes(),
                        );
                    }
                    // Headers, then nothing — the stream dies where the answer should have been.
                    None => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 X\r\nContent-Length: 128\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// The one GraphQL request each direction sends, read off the wire (SKEIN-305).
    ///
    /// Both halves, because a resolve that sends `unresolveReviewThread` and an unresolve that
    /// sends `resolveReviewThread` are the same one-word mistake, and the pane's undo is the place
    /// it would be met. The thread id travels as a **variable** rather than interpolated into the
    /// query, so this also pins that: a node id spliced into the mutation text is an injection and
    /// a syntax error waiting on the first id with a quote in it.
    #[test]
    fn resolving_and_unresolving_send_the_mutation_github_names() {
        let (api, seen) = scripted_github(vec![
            Some(
                r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#,
            ),
            Some(
                r#"{"data":{"unresolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":false}}}}"#,
            ),
        ]);
        let _wired = wired(&api);
        resolve_review_thread("PRRT_1").expect("GitHub said the thread is resolved");
        unresolve_review_thread("PRRT_1").expect("GitHub said the thread is open again");

        let sent = seen.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            2,
            "one request each, and no lookup beside it: {sent:?}"
        );
        for (i, (field, other)) in [
            ("resolveReviewThread", "unresolveReviewThread"),
            ("unresolveReviewThread", "resolveReviewThread"),
        ]
        .iter()
        .enumerate()
        {
            let (path, body) = sent[i].split_once(' ').expect("path and body");
            assert_eq!(
                path, "/graphql",
                "the mutation did not go to GraphQL: {}",
                sent[i]
            );
            let body: serde_json::Value = serde_json::from_str(body).expect("a JSON request");
            let query = body["query"].as_str().unwrap_or_default();
            assert!(
                query.contains(&format!(" {field}(input: {{threadId: $id}})")),
                "the {field} mutation is not what went out: {query}"
            );
            // The leading space is load-bearing: `resolveReviewThread` is a substring of
            // `unresolveReviewThread`, so a bare `contains` cannot tell the two apart in the
            // direction that matters — which is exactly the mistake being tested for.
            assert!(
                !query.contains(&format!(" {other}(")),
                "the two directions send the same mutation: {query}"
            );
            assert_eq!(
                body["variables"]["id"], "PRRT_1",
                "the thread id must travel as a variable, not spliced into the query: {body}"
            );
        }
    }

    /// **A refusal is an error, not a closed undo window** (SKEIN-305).
    ///
    /// Two ways GitHub says no, and both used to be the same `Ok(())` if the answer were dropped
    /// on the floor: a GraphQL `errors` entry, and a 200 whose thread comes back in the state it
    /// started in. The pane draws a receipt and starts an eight-second countdown on `ok`, so a
    /// swallowed failure is a thread the owner believes they resolved and a window that closes
    /// over it.
    #[test]
    fn a_resolve_github_did_not_perform_is_reported_rather_than_swallowed() {
        let (api, _seen) = scripted_github(vec![
            Some(r#"{"errors":[{"message":"Could not resolve to a node with the global id"}]}"#),
            Some(
                r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":false}}}}"#,
            ),
        ]);
        let _wired = wired(&api);
        let why =
            resolve_review_thread("PRRT_1").expect_err("a GraphQL error is not a resolved thread");
        assert!(
            why.contains("global id"),
            "GitHub's own reason did not reach the caller: {why}"
        );
        let why = resolve_review_thread("PRRT_1")
            .expect_err("a thread GitHub reports as still open was not resolved");
        assert!(
            why.contains("still open"),
            "a write that did not take was reported as one that did: {why}"
        );
        // And nothing is sent at all when there is no thread to name — a request GitHub would
        // answer with a schema complaint that reads as a skein bug.
        assert!(resolve_review_thread("  ").is_err());
    }

    /// **A mutation whose connection dies is never sent twice** (SKEIN-271, SKEIN-305).
    ///
    /// This is the routing test: [`crate::github::graphql_partial`] asks a dead connection again,
    /// [`crate::github::graphql`] does not, and a resolve sent twice is a write the owner did not
    /// ask for — the second one lands on a thread somebody may have reopened in between. Counted
    /// on the wire rather than read out of the source, so it holds however the call is spelled.
    #[test]
    fn a_resolve_whose_connection_dies_is_never_sent_twice() {
        let (api, seen) = scripted_github(vec![
            None,
            Some(
                r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#,
            ),
        ]);
        let _wired = wired(&api);
        let why = resolve_review_thread("PRRT_1")
            .expect_err("a dead connection on a mutation is an error, not a retry");
        assert!(!why.is_empty());
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "the resolve reached GitHub twice — the retry belongs to reads only, and this is a \
             write: {:?}",
            seen.lock().unwrap()
        );
    }
}
