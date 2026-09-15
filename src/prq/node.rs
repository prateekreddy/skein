//! How GitHub's answer becomes a [`Pr`].
//!
//! The `Deserialize` twins of everything [`PR_FRAGMENT`] asks for, and [`build_pr`], which reads
//! them defensively: a field GitHub did not send degrades that one value rather than dropping the
//! pull request. A PR you never saw is the failure mode that costs something.

use super::checks::{failing_contexts, rollup};
use super::*;

/// `null` and "absent" are the same absence, and both mean the default.
///
/// GitHub nulls what an ordinary answer fills — a connection's `nodes`, a deleted user's `author`,
/// a `submittedAt` on a review that was never submitted — while a fixture simply leaves the key
/// out. Serde treats those as two different things, one of them an error; [`Pr`]'s doc argues for
/// treating them as one, because a field skein cannot read must cost that field and never the whole
/// pull request.
fn lenient<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One GraphQL connection: a page of `nodes`, and `totalCount` saying how big the list it came from
/// is.
///
/// **The pair is one fact and is kept as one.** Every connection in [`PR_FRAGMENT`] is capped —
/// labels at [`LABELS_FETCHED`], reviews at [`REVIEWS_FETCHED`], threads at
/// [`REVIEW_THREADS_FETCHED`] — so "here are the labels" is only ever half an answer, and the other
/// half is whether that is all of them. A row that cannot say it was cut is a row a workflow reads
/// as complete: `no-label:` holding on a label skein never received (SKEIN-373), a `my_review` of
/// "none" that means "your review sorted past the cap" (SKEIN-386).
///
/// `total_count` is therefore an `Option` and **must not** default to nought. Nought is a claim —
/// "there are none" — and "GitHub did not say" is the opposite of one.
#[derive(Debug, Deserialize)]
// The bound is spelled out because `deserialize_with` on `nodes` stops serde inferring one.
#[serde(
    default,
    rename_all = "camelCase",
    bound(deserialize = "T: Deserialize<'de>")
)]
pub(super) struct Connection<T> {
    pub(super) total_count: Option<u64>,
    #[serde(deserialize_with = "lenient")]
    pub(super) nodes: Vec<T>,
}

impl<T> Default for Connection<T> {
    fn default() -> Self {
        Connection {
            total_count: None,
            nodes: Vec::new(),
        }
    }
}

/// One pull request as [`PR_FRAGMENT`] asks for it — **GitHub's own nesting, deserialised once**.
///
/// This used to be two parses. GraphQL's answer was first reshaped, key by key, into a
/// `serde_json::Value` that imitated what `gh --json` had emitted — connections flattened to bare
/// arrays, the check rollup lifted off the last commit, and four invented `…Total` keys carrying
/// what the flattening would otherwise have dropped — and [`build_pr`] then read *that* by string
/// name. The intermediate bought exactly one thing: fixtures written against `gh` kept working. `gh`
/// itself was removed in `f5b8f29`, so the cost of the reshape was paid on every pull request of
/// every refresh, for ever, to avoid rewriting test fixtures once.
///
/// The fixtures are GraphQL-shaped now and the reshape is gone. What the invented keys carried is
/// [`Connection::total_count`], where it is a field with a type rather than a name a caller has to
/// spell right.
///
/// **Every field defaults.** `number` is the exception that is an `Option` on purpose: a search can
/// match an issue rather than a pull request, the fragment simply does not apply, and GitHub answers
/// with an empty object — so "this node is not a pull request" has to be a value this struct can
/// hold. Everything else follows [`Pr`]'s rule: a field GitHub did not send costs that field and
/// nothing more.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PrNode {
    pub(super) number: Option<u64>,
    #[serde(deserialize_with = "lenient")]
    pub(super) title: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) url: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) is_draft: bool,
    #[serde(deserialize_with = "lenient")]
    pub(super) updated_at: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) head_ref_name: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) head_ref_oid: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) base_ref_name: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) review_decision: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) mergeable: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) merge_state_status: String,
    pub(super) additions: Option<u64>,
    pub(super) deletions: Option<u64>,
    pub(super) changed_files: Option<u64>,
    pub(super) author: Option<Actor>,
    #[serde(deserialize_with = "lenient")]
    pub(super) labels: Connection<Label>,
    #[serde(deserialize_with = "lenient")]
    pub(super) latest_reviews: Connection<ReviewNode>,
    #[serde(deserialize_with = "lenient")]
    pub(super) latest_opinionated_reviews: Connection<ReviewNode>,
    #[serde(deserialize_with = "lenient")]
    pub(super) review_requests: Connection<ReviewRequestNode>,
    #[serde(deserialize_with = "lenient")]
    pub(super) review_threads: Connection<ThreadNode>,
    #[serde(deserialize_with = "lenient")]
    pub(super) comments: Connection<CommentNode>,
    /// `commits(last: 1)` — the head commit, and the only reason the query asks for a commit at
    /// all: its `committedDate` and the check rollup that hangs off it.
    #[serde(deserialize_with = "lenient")]
    pub(super) commits: Connection<CommitNode>,
}

/// A GitHub account, wherever the query asks for one. `login` and nothing else: the fragment never
/// asks for more, and a deleted account arrives as `null` — which is why every holder of one of
/// these holds an `Option`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Actor {
    #[serde(deserialize_with = "lenient")]
    pub(super) login: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Label {
    #[serde(deserialize_with = "lenient")]
    pub(super) name: String,
}

/// One review, from either of the two review connections — they are asked for the same fields.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct ReviewNode {
    #[serde(deserialize_with = "lenient")]
    pub(super) state: String,
    pub(super) author: Option<Actor>,
    #[serde(deserialize_with = "lenient")]
    pub(super) submitted_at: String,
    /// Which commit the review was left against. `None` where GitHub did not say, and that is not
    /// the same as "the head": a review skein cannot place cannot be proved to cover anything, so
    /// [`my_review_state`] reads it as not current and [`standing_approvals`] does not count it.
    pub(super) commit: Option<CommitOid>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct CommitOid {
    #[serde(deserialize_with = "lenient")]
    pub(super) oid: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct ReviewRequestNode {
    pub(super) requested_reviewer: Option<Reviewer>,
}

/// Whoever is being waited on: `... on User { login }` or `... on Team { slug organization { login
/// } }`. A union, so exactly one branch is filled — and a branch this code does not know (GitHub
/// adds types to it) fills neither, which is why both halves are optional and a reviewer that is
/// neither is dropped rather than rendered as an empty name.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Reviewer {
    pub(super) login: Option<String>,
    pub(super) slug: Option<String>,
    pub(super) organization: Option<Actor>,
}

/// One review thread. Its author, timestamp and permalink are its **first comment's** — a
/// `PullRequestReviewThread` carries none of the three itself — and `latest` is the other end of
/// the same connection, asked for under an alias.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct ThreadNode {
    #[serde(deserialize_with = "lenient")]
    pub(super) id: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) is_resolved: bool,
    #[serde(deserialize_with = "lenient")]
    pub(super) comments: Connection<ThreadComment>,
    #[serde(deserialize_with = "lenient")]
    pub(super) latest: Connection<ThreadComment>,
}

/// A comment inside a review thread. **No body**: see [`ReviewThread`] for why the text is not
/// asked for, and what asking would cost.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct ThreadComment {
    pub(super) author: Option<Actor>,
    #[serde(deserialize_with = "lenient")]
    pub(super) url: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) created_at: String,
}

/// A PR-level comment. These *do* carry their bodies — see [`PrComment`].
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct CommentNode {
    pub(super) author: Option<Actor>,
    #[serde(deserialize_with = "lenient")]
    pub(super) body: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) created_at: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct CommitNode {
    pub(super) commit: Option<CommitDetail>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct CommitDetail {
    #[serde(deserialize_with = "lenient")]
    pub(super) committed_date: String,
    pub(super) status_check_rollup: Option<Rollup>,
}

/// The check rollup on the head commit: GitHub's own verdict over **every** context, and a page of
/// the contexts themselves.
///
/// Two sources rather than one, and [`rollup`] takes the more cautious of them (SKEIN-232): the
/// page is a hundred contexts, `state` is the verdict over however many there are. Read from the
/// page alone, a pull request whose 101st context is red reads green — and a merge train acts on
/// that.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Rollup {
    #[serde(deserialize_with = "lenient")]
    pub(super) state: String,
    #[serde(deserialize_with = "lenient")]
    pub(super) contexts: Connection<CheckContext>,
}

/// One context in the rollup — a `CheckRun` or a classic `StatusContext`, in one struct because the
/// query asks for both branches of the union and exactly one of them is filled.
///
/// **Every field is an `Option` and none of them defaults to `""`**, because which branch answered
/// is decided by which fields are *there*: a `CheckRun` names itself `name` and links `detailsUrl`,
/// a `StatusContext` is named by `context` and links `targetUrl`, and [`verdict`] falls from
/// `conclusion` to `state` only when the first is absent. Defaulting these to the empty string would
/// make "GitHub sent no conclusion" indistinguishable from "GitHub sent an empty one", and the
/// fallback would stop happening.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct CheckContext {
    pub(super) name: Option<String>,
    pub(super) details_url: Option<String>,
    pub(super) status: Option<String>,
    pub(super) conclusion: Option<String>,
    pub(super) context: Option<String>,
    pub(super) target_url: Option<String>,
    pub(super) state: Option<String>,
}

/// The head commit's detail — `commits(last: 1)`, so the first of at most one node.
fn head_commit(item: &PrNode) -> Option<&CommitDetail> {
    item.commits.nodes.first().and_then(|c| c.commit.as_ref())
}

/// The check rollup hanging off the head commit, where there is one.
pub(super) fn check_rollup(item: &PrNode) -> Option<&Rollup> {
    head_commit(item).and_then(|c| c.status_check_rollup.as_ref())
}

/// The page of contexts the rollup carried — empty where there is no rollup at all, which is the
/// same emptiness for every reader here and deliberately so: [`rollup`] tells the two apart by
/// [`rollup_total`], not by this.
pub(super) fn contexts(item: &PrNode) -> &[CheckContext] {
    check_rollup(item)
        .map(|r| r.contexts.nodes.as_slice())
        .unwrap_or_default()
}

/// Your review out of one connection, matched on login without regard to case.
///
/// Its own function rather than a closure because two callers want it against different
/// connections and [`my_review_state`] wants it against both in order.
fn my_review<'a>(reviews: &'a Connection<ReviewNode>, login: &str) -> Option<&'a ReviewNode> {
    reviews.nodes.iter().find(|r| {
        r.author
            .as_ref()
            .is_some_and(|a| a.login.eq_ignore_ascii_case(login))
    })
}

/// Everybody GitHub is still waiting on, as the row carries them.
///
/// A reviewer that is neither a `User` nor a `Team` is dropped rather than rendered as an empty
/// name — GitHub adds types to that union, and a blank chip on a row is worse than one fewer.
fn review_requests(item: &PrNode) -> Vec<ReviewRequest> {
    item.review_requests
        .nodes
        .iter()
        .filter_map(|r| {
            let who = r.requested_reviewer.as_ref()?;
            match &who.login {
                Some(login) => Some(ReviewRequest {
                    name: login.clone(),
                    team: false,
                }),
                None => {
                    let slug = who.slug.as_deref()?;
                    let org = who.organization.as_ref()?.login.as_str();
                    Some(ReviewRequest {
                        name: format!("{org}/{slug}"),
                        team: true,
                    })
                }
            }
        })
        .collect()
}

/// The threads on a pull request, each folded down to its two ends.
///
/// A thread of one comment returns that comment at *both* ends, which is why [`Pr::replied_to`]
/// compares authors rather than counting comments.
fn review_threads(item: &PrNode) -> Vec<ReviewThread> {
    item.review_threads
        .nodes
        .iter()
        .map(|t| {
            let first = t.comments.nodes.first();
            let last = t.latest.nodes.first();
            let login = |c: Option<&ThreadComment>| {
                c.and_then(|c| c.author.as_ref())
                    .map(|a| a.login.clone())
                    .unwrap_or_default()
            };
            ReviewThread {
                id: t.id.clone(),
                resolved: t.is_resolved,
                author: login(first),
                url: first.map(|c| c.url.clone()).unwrap_or_default(),
                last_author: login(last),
                last_at: last.map(|c| c.created_at.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

/// The PR-level conversation, bodies included — the panel renders these.
fn pr_comments(item: &PrNode) -> Vec<PrComment> {
    item.comments
        .nodes
        .iter()
        .map(|c| PrComment {
            author: c
                .author
                .as_ref()
                .map(|a| a.login.clone())
                .unwrap_or_default(),
            body: c.body.clone(),
            created_at: c.created_at.clone(),
            url: c.url.clone(),
        })
        .collect()
}

pub(super) fn build_pr(
    item: &PrNode,
    number: u64,
    login: &str,
    reason: &Reason,
    archived_numbers: &[u64],
    snoozed_shas: &BTreeMap<u64, String>,
) -> Pr {
    let head_sha = item.head_ref_oid.clone();
    let head_ref = item.head_ref_name.clone();
    let (my_review, review_is_current) = my_review_state(item, login, &head_sha);
    let my_review_at = my_review_submitted_at(item, login);
    // Read off the same two connections, before `head_sha` is moved into the row it describes.
    let standing_approvals = standing_approvals(item, &head_sha);
    // How much of those two connections arrived (SKEIN-386). Both answers above are drawn from a
    // capped list, so the row carries the size of the list beside them — otherwise a `my_review` of
    // "none" reads the same whether nobody asked you or your review sorted past the cap.
    let (reviews_total, reviews_read) = reviews_counted(item);
    // Is GitHub asking YOU, by name, right now? Read off the same [`review_requests`] the roster on
    // the row is drawn from, so the two cannot disagree about who was asked. A TEAM entry is skipped
    // deliberately — see [`Pr::my_review_requested`] for why this is a floor and why the error may
    // only fall towards leaving you alone.
    let review_requests = review_requests(item);
    let my_review_requested = review_requests
        .iter()
        .any(|r| !r.team && r.name.eq_ignore_ascii_case(login));
    let author = item
        .author
        .as_ref()
        .map(|a| a.login.clone())
        .unwrap_or_default();
    let draft = item.is_draft;
    // GitHub's enum, kept as three states rather than two. See the field.
    let mergeable = match item.mergeable.as_str() {
        "MERGEABLE" => Some(true),
        "CONFLICTING" => Some(false),
        _ => None,
    };
    let checks = rollup(item);
    let review_decision = item.review_decision.clone();
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
    // Failing checks are deliberately NOT here. Some fleets run CI only after review — a
    // workflow applies the CI label on approval — so an unreviewed PR being red says nothing
    // about whether it can be reviewed, and treating red as not-ready removed live PRs from the
    // reviewer's view. The dot on the row still says red; the lane says whose move it is.
    //
    // **Your verdict stands until GitHub asks you again** (SKEIN-354). This used to read
    // `review_is_current && …`: skein compared the sha you reviewed with the head that is there
    // now, so a rebase or a typo fix took your approval off you and put the row back in your queue.
    // The requirement, verbatim: "approved should come only if my review status on the PR is
    // approved rn, if I approved and then some file I own changed, so github asks me to review
    // again then it should show that." Both halves of that are GitHub's own answer now —
    // `my_review` from `latestOpinionatedReviews`, `my_review_requested` from `reviewRequests` —
    // and skein infers neither. On the live queue this was measured on, `review_is_current` was
    // false on all 26 rows, so the old rule cleared nothing the reviewer ever did.
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
    let built = Pr {
        number,
        title: item.title.clone(),
        author,
        url: item.url.clone(),
        head_ref,
        head_sha,
        base_ref: item.base_ref_name.clone(),
        draft,
        updated_at: item.updated_at.clone(),
        committed_at: head_commit(item)
            .map(|c| c.committed_date.clone())
            .unwrap_or_default(),
        labels: item.labels.nodes.iter().map(|l| l.name.clone()).collect(),
        labels_total: item.labels.total_count,
        review_decision,
        standing_approvals,
        reviews_total,
        reviews_read,
        mergeable,
        merge_state: item.merge_state_status.clone(),
        additions: item.additions,
        deletions: item.deletions,
        changed_files: item.changed_files,
        checks,
        failing_checks: failing_contexts(item),
        my_review,
        review_is_current,
        my_review_at,
        my_review_requested,
        reasons: vec![reason.clone()],
        lane,
        snoozed,
        // An EMPTY list rather than a failed pull request, where GitHub did not carry these: a
        // schema that moves costs the row its threads and nothing else. Same defensiveness the
        // struct's own doc argues for, and here it is the type's — every field in [`PrNode`]
        // defaults.
        review_threads: review_threads(item),
        review_threads_total: item.review_threads.total_count,
        comments: pr_comments(item),
        comments_total: item.comments.total_count,
        review_requests,
        // Filled below, from the row that has just been built: the answer needs the threads and
        // `my_review_at` together, and both are fields of it.
        replied_to_me: None,
    };
    // **Answered here rather than by the reader**, exactly as `my_review` and `review_is_current`
    // are, and for their reason: this is the one place the viewer's login is in scope. The engine
    // asks this question from `review::triggers_read_from`, which is handed a `Pr` and no identity
    // — so a `replied_to` computed there would have nobody to compare thread authors against.
    Pr {
        replied_to_me: built.replied_to(login),
        ..built
    }
}

/// **When you last said something, as GitHub timestamps it** — `submittedAt`, RFC 3339, empty when
/// GitHub did not say or you have not reviewed.
///
/// Its own reader rather than a third value out of [`my_review_state`], which answers *what* you
/// said and *which commit about*: those two are one question and this is another, asked by exactly
/// one caller ([`Pr::replied_to`]) for exactly one purpose.
///
/// **`latestReviews` and not the opinionated connection**, which is the opposite of the choice
/// `my_review_state` makes and is right for the opposite reason. There the question is "is my
/// verdict standing", so a COMMENTED note must not displace an APPROVED. Here the question is "have
/// I spoken since", and a note you left IS speaking — taking the opinionated one would date you to
/// a verdict from last week and read your own follow-up comment as somebody else's reply.
fn my_review_submitted_at(item: &PrNode, login: &str) -> String {
    my_review(&item.latest_reviews, login)
        .map(|mine| mine.submitted_at.clone())
        .unwrap_or_default()
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
fn my_review_state(item: &PrNode, login: &str, head_sha: &str) -> (String, bool) {
    let Some(mine) = my_review(&item.latest_opinionated_reviews, login)
        .or_else(|| my_review(&item.latest_reviews, login))
    else {
        return ("none".into(), false);
    };
    let state = match mine.state.as_str() {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes-requested",
        "COMMENTED" => "commented",
        _ => "none",
    };
    // No commit on the review means we cannot prove it covers the current head. Treating that as
    // "not current" sends the PR back to Needs you — the over-flag direction, on purpose.
    let at = mine.commit.as_ref().map(|c| c.oid.as_str()).unwrap_or("");
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
///   A connection GitHub did not send arrives as an empty [`Connection`], so "carried one" is
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
fn standing_approvals(item: &PrNode, head_sha: &str) -> Option<u64> {
    if head_sha.is_empty() {
        return None;
    }
    let reviews = [&item.latest_opinionated_reviews, &item.latest_reviews]
        .into_iter()
        .map(|c| c.nodes.as_slice())
        .find(|nodes| !nodes.is_empty())
        .unwrap_or_default();
    Some(
        reviews
            .iter()
            .filter(|r| r.state == "APPROVED")
            .filter(|r| r.commit.as_ref().is_some_and(|c| c.oid == head_sha))
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
fn reviews_counted(item: &PrNode) -> (Option<u64>, Option<u64>) {
    let counted = |c: &Connection<ReviewNode>| Some((c.total_count?, c.nodes.len() as u64));
    let widest = [
        counted(&item.latest_opinionated_reviews),
        counted(&item.latest_reviews),
    ]
    .into_iter()
    .flatten()
    .max_by_key(|(total, read)| total.saturating_sub(*read));
    match widest {
        Some((total, read)) => (Some(total), Some(read)),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prq::checks::{rollup_total, truncated_rollup};
    use crate::prq::fixtures::{
        batched_github, batched_repo, graphql_requests, item, node, node_of,
    };
    use crate::prq::search::{
        LABELS_FETCHED, PR_COMMENTS_FETCHED, PR_FRAGMENT, REVIEWS_FETCHED, REVIEW_REQUESTS_FETCHED,
        REVIEW_THREADS_FETCHED, SEARCH_PAGE,
    };

    /// GitHub's own nesting, read straight into [`PrNode`].
    ///
    /// The two places GraphQL's answer is not flat are the two this has always been about: a review
    /// list is a **connection** (`{totalCount, nodes}`) rather than a bare array, and the check
    /// rollup hangs off `commits(last: 1)` rather than off the pull request. Everything downstream —
    /// lanes, "is your approval current", the check summary — is decided from those two, so this is
    /// the seam where a parse either preserves behaviour or silently changes it.
    ///
    /// It used to assert on an intermediate: the answer was reshaped into an imitation of what
    /// `gh --json` emitted, and this test read `latestReviews` back as a bare array. There is no
    /// intermediate now, so the same claim is made against the fields — and then against the two
    /// answers those fields decide, which is the half that was always the point.
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
        let flat = node_of(&node);

        // The connection is unwrapped to its nodes…
        assert_eq!(
            flat.latest_reviews.nodes.len(),
            1,
            "the review connection did not reach the node: {flat:?}"
        );
        // …and the rollup is found through the head commit rather than on the pull request.
        assert_eq!(
            contexts(&flat).len(),
            1,
            "the check rollup is hidden one commit deeper than this parse looked: {flat:?}"
        );
        // A plain scalar stays a plain scalar, which is most of the fragment.
        assert_eq!(flat.head_ref_oid, "abc");

        // And the readers that decide on those two still say what they always said.
        assert_eq!(
            my_review_state(&flat, "me", "abc"),
            ("approved".into(), true)
        );
        assert_eq!(rollup(&flat), "passing");
    }

    /// **Every `totalCount` GitHub sends reaches the row the cap belongs to.**
    ///
    /// This is what the two-stage parse could get wrong without saying so. It lifted each
    /// connection's `totalCount` onto an invented key of its own — one per connection, five of
    /// them, each a name the reshaper wrote as a string and a reader looked up as a string. A key
    /// written and never read, or read and never written, compiles: the field arrives as `None`,
    /// which this codebase reads as *"GitHub did not say"*, and "did not say" is precisely the
    /// value that silences the blind spot. A dropped label count does not show up as a missing
    /// number on a row — it shows up as [`Pr::labels_whole`] answering *true* about a list that was
    /// cut, which is SKEIN-373 with nothing to warn anybody (and SKEIN-386 one connection along).
    ///
    /// Six counts, six different numbers, so a wire crossed between two of them is visible rather
    /// than merely absent. Fields cannot be typo'd, but they can still be forgotten at the one
    /// place they are read off — so the concrete change that breaks this is writing
    /// `labels_total: None` (or any of its five siblings) in [`build_pr`].
    ///
    /// `reviews_total` is the odd one and deliberately so: the row carries **the widest hole of the
    /// two** review connections, so 33-of-1 beats 22-of-2 and the pair asserted here is the
    /// opinionated connection's.
    #[test]
    fn every_count_github_sends_reaches_the_row_the_cap_belongs_to() {
        let review = |login: &str| {
            serde_json::json!({
                "state": "APPROVED",
                "author": { "login": login },
                "commit": { "oid": "abc" },
            })
        };
        let node = node_of(&serde_json::json!({
            "number": 9, "title": "t", "url": "u", "isDraft": false,
            "updatedAt": "2026-08-30T00:00:00Z",
            "headRefName": "feat", "headRefOid": "abc", "baseRefName": "main",
            "author": { "login": "someone" },
            "labels": { "totalCount": 11, "nodes": [{ "name": "ci" }] },
            "latestReviews": { "totalCount": 22, "nodes": [review("her"), review("him")] },
            "latestOpinionatedReviews": { "totalCount": 33, "nodes": [review("her")] },
            "reviewThreads": { "totalCount": 44, "nodes": [
                { "id": "PRRT_1", "isResolved": false,
                  "comments": { "nodes": [{ "author": { "login": "her" }, "url": "u1" }] },
                  "latest": { "nodes": [{ "author": { "login": "him" },
                                          "createdAt": "2026-08-30T01:00:00Z" }] } }
            ]},
            "comments": { "totalCount": 55, "nodes": [
                { "author": { "login": "her" }, "body": "b",
                  "createdAt": "2026-08-30T02:00:00Z", "url": "u2" }
            ]},
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": {
                "state": "SUCCESS",
                "contexts": { "totalCount": 66, "nodes": [
                    { "status": "COMPLETED", "conclusion": "SUCCESS" }
                ]},
            }}}]},
        }));
        let pr = build_pr(&node, 9, "me", &Reason::Reviewer, &[], &BTreeMap::new());

        assert_eq!(
            (
                pr.labels_total,
                pr.reviews_total,
                pr.reviews_read,
                pr.review_threads_total,
                pr.comments_total,
                rollup_total(&node),
            ),
            (Some(11), Some(33), Some(1), Some(44), Some(55), Some(66)),
            "a count GitHub sent did not reach its row — which reads as `nobody said`, and \
             `nobody said` is what stops the blind spot being written"
        );

        // And the counts are what the row's own "did I see all of it" answers are made of: each
        // one is short here, so every one of them must say so.
        assert!(
            !pr.labels_whole(),
            "11 labels, 1 read, and the row says whole"
        );
        assert!(
            !pr.reviews_whole(),
            "33 reviews, 1 read, and the row says whole"
        );
        assert!(
            !pr.review_threads_whole(),
            "44 threads, 1 read, and the row says whole"
        );
        assert!(truncated_rollup(&node), "66 contexts, 1 read");
    }

    /// **A `null` where GitHub usually sends a value costs that field and nothing else.**
    ///
    /// The rule [`Pr`]'s doc has always stated, now enforced by the type rather than by each
    /// reader. It is the one thing a parse rewrite is most likely to lose: the old readers went
    /// through `Value::get(…).and_then(as_str)`, for which `null` and a missing key are the same
    /// nothing, while a plain serde derive treats `null` on a `String` as an **error** — and an
    /// error here is not a blank field, it is a pull request dropped out of the queue, which is the
    /// failure mode `Pr`'s doc singles out as the one that costs something.
    ///
    /// Every null below is one GitHub really sends: `author` on a deleted account, `reviewDecision`
    /// where no review is required, `submittedAt` on a review that was never submitted, and a
    /// connection's `nodes` beside a `totalCount`.
    ///
    /// The concrete change that breaks it: dropping `deserialize_with = "lenient"` from any of the
    /// fields below. `node_of` unwraps, so the failure lands as a panic naming the fixture.
    #[test]
    fn a_null_costs_the_field_it_is_on_and_never_the_pull_request() {
        let node = node_of(&serde_json::json!({
            "number": 12, "title": null, "url": null, "isDraft": null,
            "updatedAt": null, "headRefName": null, "headRefOid": "abc", "baseRefName": null,
            "reviewDecision": null, "mergeable": null, "mergeStateStatus": null,
            "additions": null, "deletions": null, "changedFiles": null,
            "author": null,
            "labels": { "totalCount": 4, "nodes": null },
            "latestReviews": { "nodes": [
                { "state": "COMMENTED", "author": null, "submittedAt": null, "commit": null }
            ]},
            "reviewThreads": null,
            "comments": null,
            "commits": { "nodes": [{ "commit": { "committedDate": null,
                                                 "statusCheckRollup": null } }] },
        }));
        let pr = build_pr(&node, 12, "me", &Reason::Reviewer, &[], &BTreeMap::new());

        assert_eq!(pr.number, 12, "the pull request survived its own nulls");
        assert_eq!(
            (pr.title.as_str(), pr.author.as_str(), pr.base_ref.as_str()),
            ("", "", ""),
            "a null must read as absence, not as a value"
        );
        assert_eq!(
            (pr.additions, pr.mergeable),
            (None, None),
            "a null number is not nought and a null enum is not a verdict"
        );
        // The count beside a null list still arrives — which is the case that matters, because it
        // is the one that says the list was cut.
        assert_eq!(pr.labels_total, Some(4));
        assert!(pr.labels.is_empty());
        assert!(
            !pr.labels_whole(),
            "4 labels, none read, and the row says whole"
        );
        assert_eq!(
            pr.checks, "none",
            "no rollup at all is `none`, not a colour"
        );
        assert_eq!(pr.committed_at, "");
    }

    /// **The conversation shapes survive the wire, and the one that costs money is not on it**
    /// (SKEIN-301).
    ///
    /// Three things a pull request carries that `review_decision` cannot say: which review threads
    /// are open, what was said on the pull request itself, and **who** still owes an approval.
    /// Each is asserted through the real path — GitHub's nesting, [`PrNode`]'s deserialisation,
    /// [`build_pr`]'s read of it — because every one of those three is a place a field can be
    /// fetched and then dropped, and a dropped field looks exactly like a pull request with
    /// nothing open on it.
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
                                         "url": "https://github.com/acme/t/pull/7#discussion_r1"}]},
                 "latest": {"nodes": [{"author": {"login": "dave"},
                                       "createdAt": "2026-08-19T11:00:00Z"}]}},
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
            &node_of(&node),
            7,
            "me",
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

        // **The threads**, with the id skein needs and with each thread's author and permalink
        // taken from its FIRST comment — a `PullRequestReviewThread` has neither of its own. The
        // timestamp and `isOutdated` went with the thread PANEL: what is left of threads on the
        // page is "N threads unresolved", which reads `resolved` and nothing else.
        assert_eq!(pr.review_threads.len(), 2, "{:?}", pr.review_threads);
        assert_eq!(
            pr.review_threads[0],
            ReviewThread {
                id: "PRRT_1".into(),
                resolved: false,
                author: "bob".into(),
                url: "https://github.com/acme/t/pull/7#discussion_r1".into(),
                // The OTHER end of the same connection, under its `latest:` alias — bob opened the
                // thread and dave answered it. The pair is what makes §10's `reply` trigger
                // answerable: whose finding it is, and who spoke last.
                last_author: "dave".into(),
                last_at: "2026-08-19T11:00:00Z".into(),
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
            PR_FRAGMENT.contains(&format!("reviewThreads(last: {REVIEW_THREADS_FETCHED})"))
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
                &node_of(&node),
                20,
                "me",
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
                &node_of(&node),
                31,
                "me",
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
    /// merge train then held approved work, for ever, on a repo where you are the author and
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
                &node_of(&node),
                7,
                "me",
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
            &node_of(&serde_json::json!({
                "number": 8, "title": "t", "url": "u", "isDraft": false,
                "headRefName": "feat", "baseRefName": "main",
                "author": { "login": "someone" },
                "latestReviews": { "nodes": [review("APPROVED", "alice", HEAD)] },
            })),
            8,
            "me",
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

    /// A pull request says when its HEAD COMMIT landed, not when the pull request was last touched.
    ///
    /// The two are different questions and only one of them is about commits. `updatedAt` moves on
    /// a comment, so a branch nobody has pushed to in days reads as hot the moment somebody
    /// discusses it — backwards for deciding whether a PR has settled enough to be worth reading,
    /// which is what this field exists for.
    ///
    /// Free: `commits(last: 1)` is already fetched for the check rollup, so this is one more field
    /// inside a node skein asks for anyway. Asserted through the whole parse rather than off the
    /// node, because a field that is on the wire and never read off [`PrNode`] is gone by the time
    /// a `Pr` is built, and it would be gone silently.
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

        let shaped = node_of(&node);
        let pr = build_pr(&shaped, 7, "me", &Reason::Author, &[], &BTreeMap::new());
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
            &node_of(&item(
                r#"{"number":8,"title":"t","url":"u","isDraft":false,
                    "headRefName":"feat","headRefOid":"abc","baseRefName":"main",
                    "author":{"login":"someone"},"latestReviews":{"nodes":[]}}"#,
            )),
            8,
            "me",
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
            &node_of(&serde_json::json!({
                "number": 9, "title": "t", "url": "u", "isDraft": false,
                "author": { "login": "someone" }, "headRefName": "f", "headRefOid": "d",
                "baseRefName": "main", "updatedAt": "2026-08-23T12:00:00Z",
                "latestReviews": { "nodes": [] }, "mergeable": "CONFLICTING",
            })),
            9,
            "me",
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
        let bare = node_of(&serde_json::json!({
            "number": 8, "title": "t", "url": "u", "isDraft": false,
            "author": { "login": "someone" }, "headRefName": "f", "headRefOid": "d",
            "baseRefName": "main", "updatedAt": "2026-08-23T12:00:00Z",
            "latestReviews": { "nodes": [] },
        }));
        let bare = build_pr(&bare, 8, "me", &Reason::Author, &[], &BTreeMap::new());
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
        let v = node(
            r#"{"headRefOid":"abc","latestReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"abc"}}]}}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("approved".into(), true));
    }

    #[test]
    fn new_commits_undo_your_approval() {
        let v = node(
            r#"{"latestReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}}"#,
        );
        assert_eq!(my_review_state(&v, "me", "new"), ("approved".into(), false));
    }

    #[test]
    fn a_comment_is_not_a_decision() {
        let v = node(
            r#"{"latestReviews":{"nodes":[{"author":{"login":"me"},"state":"COMMENTED","commit":{"oid":"abc"}}]}}"#,
        );
        let (state, current) = my_review_state(&v, "me", "abc");
        assert_eq!(state, "commented");
        assert!(current);
        // The lane, not the flag, is what matters: commented never reaches Waiting.
        assert!(!matches!(state.as_str(), "approved" | "changes-requested"));
    }

    #[test]
    fn someone_elses_approval_is_not_yours() {
        let v = node(
            r#"{"latestReviews":{"nodes":[{"author":{"login":"her"},"state":"APPROVED","commit":{"oid":"abc"}}]}}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("none".into(), false));
    }

    #[test]
    fn a_review_with_no_commit_is_treated_as_stale() {
        let v =
            node(r#"{"latestReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED"}]}}"#);
        assert_eq!(my_review_state(&v, "me", "abc"), ("approved".into(), false));
    }

    #[test]
    fn login_case_does_not_hide_your_own_review() {
        let v = node(
            r#"{"latestReviews":{"nodes":[{"author":{"login":"Me"},"state":"APPROVED","commit":{"oid":"abc"}}]}}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("approved".into(), true));
    }

    /// **"Somebody answered one of your findings" is three clauses, and each one is load-bearing.**
    ///
    /// §10's `reply` trigger, which shipped in the table unable to fire at all. A finding is a
    /// review thread YOU opened; an answer is a later comment by somebody ELSE; and it has to be
    /// AFTER you last spoke or a conversation you have already read wakes a reading every two
    /// minutes for ever.
    ///
    /// **What would make each row fail:** dropping the opening-author test wakes you for every
    /// thread on the pull request, including ones you never touched. Dropping the last-author test
    /// makes your own follow-up comment read as somebody answering you. Dropping the timestamp
    /// test re-fires on the same answer for the life of the head. And answering `Some(false)` on a
    /// cut thread list is §7b's rule broken — a claim about replies that did not arrive, made from
    /// a list that was truncated.
    #[test]
    fn a_reply_is_a_thread_you_opened_that_somebody_else_spoke_on_after_you() {
        let thread = |author: &str, last: &str, at: &str| ReviewThread {
            id: "t".into(),
            resolved: false,
            author: author.into(),
            url: String::new(),
            last_author: last.into(),
            last_at: at.into(),
        };
        let pr = |threads: Vec<ReviewThread>, total: Option<u64>, mine_at: &str| Pr {
            review_threads: threads,
            review_threads_total: total,
            my_review_at: mine_at.into(),
            ..blank_pr(7, "abc")
        };
        let after = "2026-08-19T11:00:00Z";
        let before = "2026-08-17T09:00:00Z";
        let spoke = "2026-08-18T10:00:00Z";

        assert_eq!(
            pr(vec![thread("me", "dave", after)], None, spoke).replied_to("me"),
            Some(true),
            "an answer to your own finding did not register"
        );
        assert_eq!(
            pr(vec![thread("bob", "dave", after)], None, spoke).replied_to("me"),
            Some(false),
            "a thread you never opened was read as an answer to you"
        );
        assert_eq!(
            pr(vec![thread("me", "me", after)], None, spoke).replied_to("me"),
            Some(false),
            "your own follow-up was read as somebody answering you"
        );
        assert_eq!(
            pr(vec![thread("me", "dave", before)], None, spoke).replied_to("me"),
            Some(false),
            "an answer from before you last spoke would re-fire for the life of the head"
        );

        // **Unknown, three ways, and none of them is a no.**
        assert_eq!(
            pr(vec![thread("me", "dave", before)], Some(9), spoke).replied_to("me"),
            None,
            "a claim about replies that did not arrive, made from a truncated thread list"
        );
        assert_eq!(
            pr(vec![thread("me", "dave", after)], None, "").replied_to("me"),
            None,
            "with no time for your own review there is nothing to compare against"
        );
        assert_eq!(
            pr(vec![thread("me", "dave", after)], None, spoke).replied_to(""),
            None,
            "with no viewer there is nobody for a thread to belong to"
        );
        // A sighting still counts on a truncated list — one thread is proof, whatever the cap did
        // to the rest. This is the asymmetry §7b describes, in one assertion.
        assert_eq!(
            pr(vec![thread("me", "dave", after)], Some(9), spoke).replied_to("me"),
            Some(true),
            "a reply skein SAW was discarded because the list was capped"
        );
    }

    /// SKEIN-354. GraphQL's `latestReviews` is the latest review per author WHATEVER it said, so a
    /// note left after an approval comes back as `COMMENTED` and silently demotes your own verdict
    /// — it reads exactly like never having decided. `latestOpinionatedReviews` exists to exclude
    /// that, and the verdict is read from it.
    ///
    /// Confirmed against GitHub rather than argued from the schema: on `acme/thing` #693 the
    /// two connections disagree — `latestReviews` carries a `COMMENTED` review by the viewer and
    /// `latestOpinionatedReviews` carries nothing of theirs at all.
    ///
    /// **And the rule is symmetric**, which this used to assert in one direction only. Reported
    /// 2026-08-31 by a box whose own review monitor had the bug this test exists to prevent: it
    /// classified from the latest review rather than the latest opinionated one, so leaving an
    /// informational note on a pull request it had already approved bounced that pull request back
    /// onto its board as unfinished. It measured the other direction at the same time — `COMMENTED`
    /// follow-ups on three pull requests it was blocking, all three still reading
    /// `CHANGES_REQUESTED` afterwards — so the mirror below is a measurement rather than an
    /// argument from symmetry.
    ///
    /// Worth having as two rows rather than one: the code path is the same line, but a rule stated
    /// in one direction invites a reader to think commenting *lifts* a block, which would be the
    /// same false alarm pointed at the other verdict.
    #[test]
    fn a_comment_after_your_verdict_does_not_take_the_verdict_away() {
        for (opinionated, expected) in [
            ("APPROVED", "approved"),
            ("CHANGES_REQUESTED", "changes-requested"),
        ] {
            let v = node(&format!(
                r#"{{"headRefOid":"abc",
                "latestReviews":{{"nodes":[{{"author":{{"login":"me"}},"state":"COMMENTED","commit":{{"oid":"abc"}}}}]}},
                "latestOpinionatedReviews":{{"nodes":[{{"author":{{"login":"me"}},"state":"{opinionated}","commit":{{"oid":"abc"}}}}]}}}}"#
            ));
            assert_eq!(
                my_review_state(&v, "me", "abc"),
                (expected.into(), true),
                "a note left after {opinionated} was read as withdrawing it"
            );
        }
    }

    /// The fallback, and the one fact the opinionated connection cannot carry: that you commented.
    /// It matters because "you said something and decided nothing" is a different row from "you
    /// have not looked", and because every fixture written before the second connection existed
    /// must go on reading the way it always did.
    #[test]
    fn a_comment_with_no_verdict_behind_it_still_reads_as_a_comment() {
        let v = node(
            r#"{"headRefOid":"abc",
                "latestReviews":{"nodes":[{"author":{"login":"me"},"state":"COMMENTED","commit":{"oid":"abc"}}]},
                "latestOpinionatedReviews":{"nodes":[]}}"#,
        );
        assert_eq!(my_review_state(&v, "me", "abc"), ("commented".into(), true));
        // An approval GitHub DISMISSED is not opinionated any more, and skein must not remember it:
        // "is my review status approved right now" is the whole question.
        let dismissed = node(
            r#"{"headRefOid":"abc",
                "latestReviews":{"nodes":[{"author":{"login":"me"},"state":"DISMISSED","commit":{"oid":"abc"}}]},
                "latestOpinionatedReviews":{"nodes":[]}}"#,
        );
        assert_eq!(
            my_review_state(&dismissed, "me", "abc"),
            ("none".into(), false)
        );
    }

    /// SKEIN-354, the lane half. The requirement: "approved should come only if my review status
    /// on the PR is approved rn, if I approved and then some file I own changed, so github asks me
    /// to review again then it should show that."
    ///
    /// Measured on one live queue on 2026-08-26 (26 rows): `review_is_current` was false on ALL of
    /// them, including the two the viewer had approved themselves — so the rule this replaces
    /// returned every decided pull request to them on the next push, which is why their approvals
    /// never cleared anything. If this test fails and the change that broke it put
    /// `review_is_current` back into the lane rule, the change is wrong and the rule is right.
    #[test]
    fn your_verdict_stands_until_github_asks_you_again() {
        let build = |json: &str| {
            build_pr(
                &node(json),
                5,
                "me",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new(),
            )
        };
        // Approved, and the branch has moved several commits past what you read.
        let moved = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},
                "latestOpinionatedReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}}"#,
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

        // The counter-case, and the half the requirement asks for by name: a file you own
        // changed, so CODEOWNERS asked you again. The row is yours.
        let again = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},
                "reviewRequests":{"nodes":[{"requestedReviewer":{"login":"ME"}}]},
                "latestOpinionatedReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}}"#,
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
        // not-ready either — a reviewer, on somebody else's conflicted PR: "as far as I am
        // concerned my work there is done".
        let dirty = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},"mergeable":"CONFLICTING",
                "latestOpinionatedReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}}"#,
        );
        assert_eq!(dirty.lane, Lane::Waiting, "their conflict, your decision");

        // A comment is still not a verdict, so a row you only remarked on stays yours.
        let noted = build(
            r#"{"number":5,"headRefOid":"new","author":{"login":"someone"},
                "latestReviews":{"nodes":[{"author":{"login":"me"},"state":"COMMENTED","commit":{"oid":"new"}}]}}"#,
        );
        assert_eq!(noted.lane, Lane::NeedsYou);
    }

    /// Who GitHub is asking, read off the same list the row's roster is drawn from — and a floor
    /// rather than a census. A TEAM you are in arrives as the team, so it cannot be attributed to
    /// you; the error may only fall towards leaving you alone, which is the side this errs to:
    /// "theirs until they ask again".
    #[test]
    fn a_review_request_names_you_or_it_does_not_count() {
        let asked = |json: &str| {
            build_pr(
                &node(json),
                6,
                "me",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new(),
            )
            .my_review_requested
        };
        assert!(asked(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                "reviewRequests":{"nodes":[{"requestedReviewer":{"login":"her"}},{"requestedReviewer":{"login":"me"}}]}}"#
        ));
        assert!(
            asked(
                r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                    "reviewRequests":{"nodes":[{"requestedReviewer":{"login":"Me"}}]}}"#
            ),
            "GitHub's casing of your own login must not hide a request for you"
        );
        assert!(
            !asked(
                r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                    "reviewRequests":{"nodes":[{"requestedReviewer":{"slug":"me","organization":{"login":"acme"}}}]}}"#
            ),
            "a team is not you, however its slug reads"
        );
        assert!(!asked(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},"reviewRequests":{"nodes":[]}}"#
        ));
        assert!(
            !asked(r#"{"number":6,"headRefOid":"a","author":{"login":"someone"}}"#),
            "an answer with no requests in it is not an answer that you were asked"
        );
    }

    #[test]
    fn an_archived_pr_lands_in_the_archived_lane() {
        let v = node(r#"{"number":3,"headRefOid":"abc","title":"t"}"#);
        let pr = build_pr(&v, 3, "me", &Reason::Reviewer, &[3], &BTreeMap::new());
        assert_eq!(pr.lane, Lane::Archived);
    }

    #[test]
    fn an_unreviewed_pr_needs_you() {
        let v = node(r#"{"number":3,"headRefOid":"abc","title":"t"}"#);
        let pr = build_pr(&v, 3, "me", &Reason::Reviewer, &[], &BTreeMap::new());
        assert_eq!(pr.lane, Lane::NeedsYou);
        assert_eq!(pr.checks, "none");
    }

    #[test]
    fn a_decided_pr_waits() {
        let v = node(
            r#"{"number":3,"headRefOid":"abc","latestReviews":{"nodes":[{"author":{"login":"me"},"state":"CHANGES_REQUESTED","commit":{"oid":"abc"}}]}}"#,
        );
        let pr = build_pr(&v, 3, "me", &Reason::Author, &[], &BTreeMap::new());
        assert_eq!(pr.lane, Lane::Waiting);
    }

    /// The done-when fixture from SKEIN-139: a red PR, a draft, a conflicted one, one of yours,
    /// and one genuinely awaiting you — readiness decides the lane, not whether you have acted.
    #[test]
    fn a_lane_says_whose_move_it_is_not_whether_you_acted() {
        // Red is the ORDINARY state of an unreviewed PR here: CI runs only after review (the
        // workflow applies the CI label on approval), so failing checks must not take a PR off
        // the reviewer. The first version of this rule did, and live PRs vanished from the view.
        let red = node(
            r#"{"number":1,"headRefOid":"a","author":{"login":"someone"},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                    {"status":"COMPLETED","conclusion":"FAILURE"}]}}}}]}}"#,
        );
        assert_eq!(
            build_pr(&red, 1, "me", &Reason::Reviewer, &[], &BTreeMap::new()).lane,
            Lane::NeedsYou,
            "failing checks do not excuse the review — on this fleet CI follows review"
        );

        let draft =
            node(r#"{"number":2,"headRefOid":"a","author":{"login":"someone"},"isDraft":true}"#);
        assert_eq!(
            build_pr(&draft, 2, "me", &Reason::Reviewer, &[], &BTreeMap::new()).lane,
            Lane::NotReady,
            "a draft is its author saying it is not finished"
        );

        let conflicted = node(
            r#"{"number":3,"headRefOid":"a","author":{"login":"someone"},"mergeable":"CONFLICTING"}"#,
        );
        assert_eq!(
            build_pr(
                &conflicted,
                3,
                "me",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NotReady,
            "unmergeable: the branch has to move before a review of it means anything"
        );

        // Yours, even red: your problem as an AUTHOR, and this queue is the reviewer's.
        let yours = node(
            r#"{"number":4,"headRefOid":"a","author":{"login":"me"},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                    {"status":"COMPLETED","conclusion":"FAILURE"}]}}}}]}}"#,
        );
        assert_eq!(
            build_pr(&yours, 4, "me", &Reason::Author, &[], &BTreeMap::new()).lane,
            Lane::Waiting,
            "you authored it — the next review is somebody else's to give"
        );

        // UNKNOWN is what GitHub says for a while after every push — it is "not yet computed",
        // never "conflicted", and a freshly pushed PR must not fall out of your lane for it.
        let fresh = node(
            r#"{"number":5,"headRefOid":"a","author":{"login":"someone"},"mergeable":"UNKNOWN"}"#,
        );
        assert_eq!(
            build_pr(&fresh, 5, "me", &Reason::Reviewer, &[], &BTreeMap::new()).lane,
            Lane::NeedsYou,
            "mergeability GitHub has not computed is not a reason to demote"
        );
    }

    /// The reviewer's first question is "can I do this now?" — size, before anything else. The
    /// search answers it in the same call, and absence stays absent: a queue remembered from
    /// before these fields must render nothing rather than claim an empty change.
    #[test]
    fn a_row_can_say_how_big_the_change_is_before_it_is_opened() {
        let sized = node(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                "additions":120,"deletions":18,"changedFiles":6}"#,
        );
        let pr = build_pr(&sized, 6, "me", &Reason::Reviewer, &[], &BTreeMap::new());
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

        let bare = node(r#"{"number":7,"headRefOid":"a","author":{"login":"someone"}}"#);
        let pr = build_pr(&bare, 7, "me", &Reason::Reviewer, &[], &BTreeMap::new());
        assert_eq!(
            (pr.additions, pr.deletions, pr.changed_files),
            (None, None, None),
            "absent size must stay absent — a defaulted 0 claims an empty change"
        );

        let awaiting = node(r#"{"number":5,"headRefOid":"a","author":{"login":"someone"}}"#);
        assert_eq!(
            build_pr(&awaiting, 5, "me", &Reason::Reviewer, &[], &BTreeMap::new()).lane,
            Lane::NeedsYou,
            "green, settled, not yours, undecided: genuinely your move"
        );

        // Pending checks are not failing checks: a PR mid-CI is still yours to start reading.
        let pending = node(
            r#"{"number":6,"headRefOid":"a","author":{"login":"someone"},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                    {"status":"IN_PROGRESS"}]}}}}]}}"#,
        );
        assert_eq!(
            build_pr(&pending, 6, "me", &Reason::Reviewer, &[], &BTreeMap::new()).lane,
            Lane::NeedsYou,
            "pending is not failing — waiting for green to read is a choice, not a gate"
        );
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
        let theirs = node(
            r#"{"number":1,"headRefOid":"a","author":{"login":"someone"},"reviewDecision":"APPROVED"}"#,
        );
        assert_eq!(
            build_pr(&theirs, 1, "me", &Reason::Reviewer, &[], &BTreeMap::new()).lane,
            Lane::Waiting,
            "the repository is satisfied and the queue is for review work"
        );

        // Empty means the repo REQUIRES no review — the queue's whole purpose is repos where
        // review is social rather than enforced, and demoting on silence would empty it there.
        let unenforced = node(
            r#"{"number":2,"headRefOid":"a","author":{"login":"someone"},"reviewDecision":""}"#,
        );
        assert_eq!(
            build_pr(
                &unenforced,
                2,
                "me",
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
        let asked_again = node(
            r#"{"number":3,"headRefOid":"new","author":{"login":"someone"},
                "reviewDecision":"APPROVED",
                "reviewRequests":{"nodes":[{"requestedReviewer":{"login":"me"}}]},
                "latestReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}}"#,
        );
        assert_eq!(
            build_pr(
                &asked_again,
                3,
                "me",
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
            let pr = node(&format!(
                r#"{{"number":4,"headRefOid":"a","author":{{"login":"someone"}},"reviewDecision":"{decision}"}}"#
            ));
            assert_eq!(
                build_pr(&pr, 4, "me", &Reason::Reviewer, &[], &BTreeMap::new()).lane,
                Lane::NeedsYou,
                "{decision} keeps the behaviour that always held"
            );
        }
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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "gho_test");
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
        env.set("SKEIN_GITHUB_API", &base);
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

        forget_host_token();
        forget_renames();
    }
}
