//! What a review queue is made of: the row, the lane, and why the row is yours.
//!
//! Every type here is serialised straight to the cockpit, so a field added is a field the pane can
//! read — and a field removed is a pane that silently stops drawing something. [`Pr`] is the
//! object; [`Lane`] and [`Reason`] are the two answers about it that skein derives on every fetch
//! and never stores.

use super::*;

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
    /// Who opened it — the FIRST comment's author. Empty when GitHub did not say, which is the
    /// same rule every other field here follows: absence stays absent.
    #[serde(default)]
    pub author: String,
    /// Where the thread lives on GitHub — the first comment's permalink, which is what a
    /// `PullRequestReviewThread` has instead of a url of its own.
    #[serde(default)]
    pub url: String,
    /// Who wrote the thread's **last** comment, and when — `latest: comments(last: 1)`.
    ///
    /// [`ReviewThread::author`] is the thread's opening comment and therefore whose finding it is.
    /// This is the other end of the same connection, and the pair is what makes *"somebody answered
    /// one of your findings"* answerable at all: `author == you` and `last_author != you` is a
    /// reply to you, and `last_at` says whether it came after you spoke.
    ///
    /// Both empty on a thread of one comment, which is a finding nobody has answered — GitHub
    /// returns the same node at both ends, and [`replied_to`] compares the authors rather than
    /// counting, so a one-comment thread cannot read as a reply to itself.
    ///
    /// **Deserialised and not serialised.** `ReviewThread` is also read back out of `queue.json`,
    /// so the pair has to survive a `Deserialize`; but they are read once, by [`Pr::replied_to`] at
    /// parse time, and nothing reads them off the cache afterwards.
    #[serde(default, skip_serializing)]
    pub last_author: String,
    #[serde(default, skip_serializing)]
    pub last_at: String,
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
/// Fields are pulled defensively out of [`PrNode`]: a field GitHub did not send degrades that one
/// value, rather than dropping the PR. A PR you never saw is the failure mode that costs something;
/// a PR with an unknown check state is merely less useful. That is a property of the type now
/// rather than of each reader — every field in `PrNode` defaults, and `null` is read as absence.
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
    /// pull request rather than shipping one — but on a repo where you are the author and somebody
    /// else reviews, it is the ordinary case, which makes it a gap rather than a design.
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
    /// approval off you. Measured on one live queue on 2026-08-26 this was false on all 26 rows
    /// — including the two the viewer had approved themselves — because GitHub reports the commit
    /// a review was left against and branches move. The rule instead: "approved should come only
    /// if my review status on the PR is approved rn, if I approved and then some file I own
    /// changed, so github asks me to review again then it should show that." That is
    /// [`Pr::my_review`] and [`Pr::my_review_requested`], both of them GitHub's own answers.
    ///
    /// What it is still for is everything that is about the CODE rather than about you: the "new
    /// commits" mark, the clock the your-move lane sorts on, and `review::worth_reading` deciding
    /// that a reading of an older commit is stale. Evidence the head moved, not a verdict on your
    /// review.
    pub review_is_current: bool,
    /// **When you last said anything on this pull request** — `submittedAt` on your latest review,
    /// RFC 3339. Empty when you have not reviewed, or when GitHub did not say.
    ///
    /// Carried rather than derived because nothing else in this struct can date your verdict, and
    /// "did somebody answer me" is a comparison against a time. `Default` is empty, which
    /// [`Pr::replied_to`] reads as *cannot tell* rather than as the beginning of time — a queue
    /// remembered on disk by a skein from before this field existed lands there, and dating your
    /// review to 1970 would make every comment on the pull request look like an answer to you.
    ///
    /// **Not serialised**, which `tests/queue_field_readers.rs` is what settled: it is an INPUT to
    /// [`Pr::replied_to`], consumed at parse time where the login is in scope, and the answer is
    /// what rides the cache. A field that round-trips with no reader on the other side is the thing
    /// that gate exists to refuse.
    #[serde(default, skip_serializing)]
    pub my_review_at: String,
    /// **Has somebody answered one of your findings since you left it?** — [`Pr::replied_to`]'s
    /// answer, computed where the viewer's login is in scope and carried like [`Pr::my_review`].
    ///
    /// A field rather than a call, because the one caller that needs it — `review`'s trigger
    /// adapter — is handed a `Pr` and no identity, and threading one down to it would give the
    /// scope question an opinion about who skein is.
    #[serde(default)]
    pub replied_to_me: Option<bool>,
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
    /// **Newest, and the query now agrees** (SKEIN-340). This sentence was true of the doc and
    /// false of the wire for as long as both existed: [`PR_FRAGMENT`] asked `reviewThreads(first:
    /// …)`, so a pull request with more threads than the cap handed its reader the OLDEST ten —
    /// which on a pull request that has been through a round of review are the ten already
    /// resolved. [`REVIEW_THREADS_FETCHED`] carries what that cost and why the newest end is the
    /// right one; `the_newest_review_threads_are_fetched_not_the_oldest` is what stops the two
    /// drifting apart again, because it reads this doc comment and the query together.
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

    /// Did skein see every review thread? The same shape as [`Pr::labels_whole`], one connection
    /// along — `reviewThreads(last: REVIEW_THREADS_FETCHED)` with its `totalCount` beside it.
    pub fn review_threads_whole(&self) -> bool {
        match self.review_threads_total {
            Some(total) => total as usize <= self.review_threads.len(),
            None => true,
        }
    }

    /// **Has somebody answered one of your findings since you left it?** — `docs/pr-review.md`
    /// §10's `reply` trigger, the one row of that table that could not be computed at all.
    ///
    /// A finding is a review thread you opened, so this is: a thread whose FIRST comment is yours,
    /// whose LAST comment is somebody else's, written after your own latest review. All three
    /// clauses earn their place — the first is what makes it *your* finding rather than any thread,
    /// the second is what makes it an answer rather than your own follow-up, and the third is what
    /// stops a conversation you have already read waking you every two minutes for ever.
    ///
    /// # Three-valued, and asymmetric on purpose
    ///
    /// `Some(true)` is a sighting: one thread is proof, whatever the connection did to the rest.
    /// `Some(false)` is a claim about replies that did NOT arrive, and §7b's rule applies to it in
    /// full — it needs the whole thread list AND a time to compare against. `None` everywhere else:
    /// threads truncated, or no `my_review_at`, which is both "you have not reviewed" and "this
    /// queue was remembered by a skein that never asked for the timestamp".
    ///
    /// **The direction that matters is the one this closes.** §7d's live report was a *false calm*:
    /// in a stacked workflow the fix lands on a descendant branch, the pull request's own head
    /// never moves, and every head-derived trigger stays silent while resolved work sits waiting.
    /// Nothing prompts you to re-check a pull request nothing has told you about.
    pub fn replied_to(&self, viewer: &str) -> Option<bool> {
        if viewer.is_empty() || self.my_review_at.is_empty() {
            return None;
        }
        let mine = |who: &str| who.eq_ignore_ascii_case(viewer);
        let answered = self.review_threads.iter().any(|t| {
            mine(&t.author)
                && !t.last_author.is_empty()
                && !mine(&t.last_author)
                // RFC 3339 from one source, so a lexicographic compare IS a chronological one —
                // both are GitHub's own `Z`-suffixed UTC. Parsing them to compare would add a
                // dependency and a failure mode to a comparison that is already exact.
                && t.last_at.as_str() > self.my_review_at.as_str()
        });
        if answered {
            return Some(true);
        }
        // A "no" about a list that was cut is not a no.
        self.review_threads_whole().then_some(false)
    }
}

/// A placeholder pull request for a test to build on, with the fields nobody can guess supplied.
///
/// **Why this is here rather than in each test module.** `Pr` is built by hand in four fixtures
/// across `src/review/` and `src/queue.rs`, every one of them exhaustive — so adding a field to
/// it broke three files that had no opinion about the field (SKEIN-301). The fixtures in
/// `src/prwork/` never broke, because they build theirs through `serde_json::from_value` and the
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
        my_review_at: String::new(),
        replied_to_me: None,
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

/// How long a pull request must go without a commit before skein reads it unasked.
///
/// A branch somebody is actively pushing to is the worst thing to spend a reading on: the reading
/// describes a commit that is about to stop being the head, and the next poll spends another. The
/// owner asked for an hour, which is also about the shortest gap that reliably means "they have
/// stopped for now" rather than "they are between commits".
pub const SETTLE: Duration = Duration::from_secs(60 * 60);

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
    use crate::prq::fixtures::node_of;
    use crate::prq::node::build_pr;

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
                &node_of(&node(*n, at)),
                *n,
                "me",
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
}
