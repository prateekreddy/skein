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
    #[serde(default)]
    pub labels: Vec<String>,
    /// GitHub's verdict on the pull request as a whole: `APPROVED`, `CHANGES_REQUESTED`,
    /// `REVIEW_REQUIRED`, or empty where the repository asks for no review.
    ///
    /// Distinct from [`Pr::my_review`], which is what YOU last said. A workflow that merges cares
    /// about the repository's answer — being one of six reviewers who approved is not the same fact
    /// as the pull request being approved.
    #[serde(default)]
    pub review_decision: String,
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
    pub my_review: String,
    /// Was that review submitted against the current head? False after new commits land, which is
    /// what returns an approved PR to [`Lane::NeedsYou`].
    pub review_is_current: bool,
    pub reasons: Vec<Reason>,
    pub lane: Lane,
    /// Why an [`Lane::Archived`] row is there: `true` when it was set aside *until the head moves*
    /// (SKEIN-144) rather than archived outright. The lane is deliberately shared — both mean "not
    /// claiming your attention" — but the endings differ (a human act versus the author's next
    /// push), so the page needs to know which story to tell. Defaulted for remembered queues.
    #[serde(default)]
    pub snoozed: bool,
    /// The deterministic box name for this branch — whether or not one exists yet.
    pub box_name: String,
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
fn trunk_of(slug: &str) -> String {
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

/// Your GitHub login and the teams you belong to, as `gh` reports them.
///
/// Teams are best-effort: `gh api user/teams` needs `read:org`, which a perfectly good `gh` login
/// may lack. When it fails the caller records a blind spot instead of quietly returning a queue
/// missing every team-requested review — the one omission that would cost you a merge.
pub fn viewer() -> Result<(String, Vec<String>), String> {
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
    // Best-effort: `read:org` is a scope a perfectly good token may lack, and the caller records a
    // blind spot rather than quietly returning a queue missing every team-requested review.
    let teams = crate::github::get_json("/user/teams?per_page=100", &token)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| {
            let org = t.get("organization")?.get("login")?.as_str()?;
            let slug = t.get("slug")?.as_str()?;
            Some(format!("{org}/{slug}"))
        })
        .collect();
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
    if !cfg!(test) {
        if let Some(young) = unexpired_within(&repo.id, max_age) {
            return Ok(young);
        }
    }
    let stored = repo_slug(repo)
        .ok_or("this repo has no GitHub remote, so it has no pull requests to review")?;
    let (login, teams) = viewer()?;
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
    if teams.is_empty() {
        // Short, and it names the cure. A warning that cannot be acted on is shown on every load
        // forever, and a banner that is always there stops being read — so the fix belongs in the
        // sentence, not in documentation somewhere behind it.
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
    let mut answered = true;
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
        if !found.whole {
            blind_spots.push(match found.matched {
                Some(n) => format!(
                    "the `{search}` query matched {n} pull requests and skein read the first \
                     {SEARCH_PAGE} — the rest are missing from this queue and from its count"
                ),
                None => format!(
                    "the `{search}` query filled its page of {SEARCH_PAGE}, so there are probably \
                     more pull requests it did not reach — they are missing from this queue"
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
            prs.push(build_pr(
                &item,
                number,
                &login,
                &repo.id,
                reason,
                &archived_numbers,
                &snoozed_shas,
            ));
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
    if !cfg!(test) {
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
/// [`queue_within`] deliberately neither caches nor remembers under `cfg!(test)`, so a test's world
/// has no remembered queue in it unless it says so — and "no remembered queue" is a state a real
/// post is almost never in, because the pane must have rendered this repo for a draft to exist at
/// all. A write path's test that wants the state it will actually run in seeds it here.
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
/// Its own function rather than three lines inside [`queue_within`], because that path bypasses
/// the cache under `cfg!(test)` — this is the piece a test can hold, by seeding [`QUEUE_CACHE`]
/// with a back-stamped entry and asking.
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
const PR_FRAGMENT: &str = r#"
fragment PrFields on PullRequest {
  number title url isDraft updatedAt
  headRefName headRefOid baseRefName reviewDecision mergeable mergeStateStatus
  additions deletions changedFiles
  labels(first: 20) { nodes { name } }
  author { login }
  latestReviews(first: 30) { nodes { state author { login } commit { oid } } }
  commits(last: 1) { nodes { commit { committedDate statusCheckRollup { state contexts(first: 100) { totalCount nodes {
    ... on CheckRun { name detailsUrl status conclusion }
    ... on StatusContext { context targetUrl state }
  } } } } } }
}"#;

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
fn batched_query(count: usize) -> String {
    use std::fmt::Write as _;
    let mut vars = String::from("$n: Int!");
    let mut body = String::new();
    for i in 0..count {
        let _ = write!(vars, ", $q{i}: String!");
        let _ = writeln!(
            body,
            "  q{i}: search(query: $q{i}, type: ISSUE, first: $n) {{ issueCount pageInfo {{ \
             hasNextPage }} nodes {{ ...PrFields }} }}"
        );
    }
    format!("query({vars}) {{\n{body}}}\n{PR_FRAGMENT}")
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

/// Every membership search of one refresh, in ONE GraphQL request — five requests per repo per
/// refresh was where nearly all of skein's quota went (SKEIN-209).
///
/// The outer `Result` is the request: an `Err` means nothing was asked or nothing answered, and
/// the caller must report **every** search as missing. The inner ones are per search, in the order
/// given: GraphQL delivers a failed alias as `data.qN: null` plus an `errors` entry whose `path`
/// names the alias, and that mapping is what keeps each failure its own blind spot — four good
/// answers are still four good answers, exactly as they were when each search was its own request.
fn search_prs_all(slug: &str, searches: &[String]) -> Result<Vec<Result<Found, String>>, String> {
    match one_request(slug, searches) {
        Ok(found) => Ok(found),
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
            let (left, right) = searches.split_at(searches.len() / 2);
            let mut out = search_prs_all(slug, left)
                .unwrap_or_else(|e| left.iter().map(|_| Err(e.clone())).collect());
            out.extend(
                search_prs_all(slug, right)
                    .unwrap_or_else(|e| right.iter().map(|_| Err(e.clone())).collect()),
            );
            // Told once, on the answer rather than in the log: a refresh that had to split is a
            // refresh that cost more than it should, and a fleet where that is the normal case
            // wants to know before it meets the rate limit again.
            eprintln!(
                "skein: GitHub would not take {slug}'s {} searches in one request ({why}) — asked                  in two",
                searches.len()
            );
            Ok(out)
        }
        Err(why) => Err(why),
    }
}

/// One batched request, as it has always been — the recursion above is what turns a refusal of the
/// whole batch into halves.
fn one_request(slug: &str, searches: &[String]) -> Result<Vec<Result<Found, String>>, String> {
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
    if let Some(map) = out.as_object_mut() {
        map.insert("labels".into(), serde_json::Value::Array(labels));
        map.insert("latestReviews".into(), reviews);
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
    let lane = if archived_numbers.contains(&number) || snoozed {
        Lane::Archived
    } else if author == login
        || (review_is_current && matches!(my_review.as_str(), "approved" | "changes-requested"))
    {
        Lane::Waiting
    } else if draft || mergeable == Some(false) {
        Lane::NotReady
    } else if review_decision == "APPROVED"
        && !(matches!(my_review.as_str(), "approved" | "changes-requested") && !review_is_current)
    {
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
        //   - Where the two authorities disagree — GitHub says APPROVED but YOUR decision was
        //     left against an older head — the person-level fact wins and the PR returns to you,
        //     the guard above. Skein is right about the person: the repo being satisfied does not
        //     mean you have seen what was pushed after you decided, and hiding that behind a
        //     repo-level fact is how a stale approval merges.
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
        settled: settled(&s("committedDate")),
        review_decision,
        mergeable,
        merge_state: s("mergeStateStatus"),
        additions: item.get("additions").and_then(|v| v.as_u64()),
        deletions: item.get("deletions").and_then(|v| v.as_u64()),
        changed_files: item.get("changedFiles").and_then(|v| v.as_u64()),
        checks,
        failing_checks: failing_contexts(item),
        my_review,
        review_is_current,
        reasons: vec![reason.clone()],
        lane,
        snoozed,
    }
}

/// Your last review on this PR, and whether it was submitted against the current head.
///
/// A `COMMENTED` review is deliberately **not** a decision: leaving a note is not the same as
/// clearing the PR, so it stays in [`Lane::NeedsYou`]. Approving and requesting changes both are
/// decisions — in each case the ball is in the author's court, which is what the lane means.
fn my_review_state(item: &serde_json::Value, login: &str, head_sha: &str) -> (String, bool) {
    let Some(reviews) = item.get("latestReviews").and_then(|v| v.as_array()) else {
        return ("none".into(), false);
    };
    let mine = reviews.iter().find(|r| {
        r.get("author")
            .and_then(|a| a.get("login"))
            .and_then(|v| v.as_str())
            .is_some_and(|l| l.eq_ignore_ascii_case(login))
    });
    let Some(mine) = mine else {
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
    let current = !at.is_empty() && !head_sha.is_empty() && at == head_sha;
    (state.into(), current)
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

/// The RIGHT-side commentable lines of a unified diff: every `+` and context line, with the NEW
/// file's line number and the line's content (marker stripped). This is exactly the set of
/// coordinates GitHub accepts for a `side: "RIGHT"` review comment.
///
/// The counters come from each `@@ -a,b +c,d @@` header's `+c`; `-` lines do not advance the new
/// counter, and a `\ No newline at end of file` marker advances nothing. File identity comes from
/// the `+++ b/...` header. A hunk header that does not parse (the assembled diff's
/// `@@ no patch available @@` placeholder) suspends counting until the next real one, so a file
/// GitHub served only as numbers contributes no false anchors.
fn right_side_lines(diff: &str) -> Vec<(String, u64, String)> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut new_line: u64 = 0;
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            path = None;
            in_hunk = false;
        } else if !in_hunk && line.starts_with("+++ ") {
            let name = line["+++ ".len()..].trim();
            path =
                (name != "/dev/null").then(|| name.strip_prefix("b/").unwrap_or(name).to_string());
        } else if !in_hunk && line.starts_with("--- ") {
            // The old-file header; only the +++ side names what RIGHT comments attach to.
        } else if line.starts_with("@@") {
            // `@@ -a,b +c,d @@` — only `+c` matters here.
            in_hunk = false;
            if let Some(plus) = line.split_whitespace().find(|w| w.starts_with('+')) {
                let start = plus[1..].split(',').next().unwrap_or("");
                if let Ok(n) = start.parse::<u64>() {
                    new_line = n;
                    in_hunk = true;
                }
            }
        } else if in_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                if let Some(p) = &path {
                    out.push((p.clone(), new_line, rest.to_string()));
                }
                new_line += 1;
            } else if line.starts_with('\\') || line.starts_with('-') {
                // `\ No newline…` marks the previous line; `-` lines live only in the old file.
            } else {
                // Context: a leading space, or the entirely empty line git emits for blank context.
                let rest = line.strip_prefix(' ').unwrap_or(line);
                if let Some(p) = &path {
                    out.push((p.clone(), new_line, rest.to_string()));
                }
                new_line += 1;
            }
        }
    }
    out
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
    let lines = right_side_lines(new_diff);
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
pub fn merge(slug: &str, number: u64) -> Result<String, String> {
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
        &serde_json::json!({ "merge_method": method }),
    )?;
    Ok(out
        .get("message")
        .and_then(|m| m.as_str())
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("merged")
        .to_string())
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

    /// The TTL rule the badge rides on: a remembered in-process queue is served only while it is
    /// younger than the caller's age budget.
    ///
    /// Asserted on `unexpired_within` directly, because `queue_within` bypasses the cache under
    /// `cfg!(test)` — seeding [`QUEUE_CACHE`] with a back-stamped entry is the only way the rule
    /// is reachable from a test at all.
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
    /// Asserted against the source, the way `nothing_here_shells_out_to_gh` is, because the
    /// runtime path is unreachable from a test: `queue_within` bypasses the cache under
    /// `cfg!(test)`, so a test that called `counts()` would pass identically on either budget.
    /// What this pins is the call itself — pointing `counts` back at `queue(&repo, false)` is
    /// the regression that rebuilt every repo's queue through a 60s cache every three minutes
    /// per open tab, and it is exactly what this fails on.
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
        struct Undo(std::sync::MutexGuard<'static, ()>);
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
            "the head commit's date is read but never requested: {PR_FRAGMENT}"
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
        // And it is actually ASKED for, same trap as `committedDate` above: everything here works
        // on a node handed to it, so without this the field could be one GitHub was never told to
        // send, and every PR would read as merge-state unknown.
        assert!(
            PR_FRAGMENT.contains("mergeStateStatus"),
            "merge_state is read but never requested: {PR_FRAGMENT}"
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
            "the fields are read but never requested: {PR_FRAGMENT}"
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
            "the CheckRun name/link is read but never requested: {PR_FRAGMENT}"
        );
        assert!(
            PR_FRAGMENT.contains("... on StatusContext { context targetUrl state }"),
            "the StatusContext name/link is read but never requested: {PR_FRAGMENT}"
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
    #[test]
    fn githubs_approval_moves_review_work_off_you_but_never_hides_your_stale_review() {
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

        // The disagreement: GitHub says APPROVED, but YOUR approval was left against an older
        // head. Skein is right about the person — new commits you have not seen return the PR to
        // you, and the repo-level fact must not hide the person-level one.
        let stale_mine = item(
            r#"{"number":3,"headRefOid":"new","author":{"login":"someone"},
                "reviewDecision":"APPROVED",
                "latestReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"old"}}]}"#,
        );
        assert_eq!(
            build_pr(
                &stale_mine,
                3,
                "me",
                "repo",
                &Reason::Reviewer,
                &[],
                &BTreeMap::new()
            )
            .lane,
            Lane::NeedsYou,
            "your approval is outdated — the existing return-to-you rule still wins"
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
                let (code, answer) = match path.as_str() {
                    "/graphql" => (status, graphql_body.clone()),
                    "/rate_limit" => (200, "{}".to_string()),
                    p if p.starts_with("/user/teams") => (
                        200,
                        match teams {
                            true => r#"[{"slug":"core","organization":{"login":"acme"}}]"#.into(),
                            false => "[]".to_string(),
                        },
                    ),
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

        let answer = |more: bool| {
            format!(
                r#"{{"data":{{"q0":{{"issueCount":143,"pageInfo":{{"hasNextPage":{more}}},"nodes":[{five}]}},
                   "q1":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q2":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}},
                   "q3":{{"issueCount":0,"pageInfo":{{"hasNextPage":false}},"nodes":[]}}}}}}"#,
                five = search_node(5),
            )
        };

        let (base, seen) = batched_github(false, 200, answer(true));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/search-cut"), true).expect("the queue answered");

        let sent = &graphql_requests(&seen)[0];
        assert!(
            sent.contains("issueCount") && sent.contains("hasNextPage"),
            "the search must ask how many it matched and whether it reached the end: {sent}"
        );
        assert!(
            q.blind_spots.iter().any(|b| b.contains(
                "the `review-requested:me` query matched 143 pull requests and skein read the \
                 first 100"
            )),
            "a truncated search must name ITS rule and the size of the hole: {:?}",
            q.blind_spots
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
        let (base, _seen) = batched_github(false, 200, answer(false));
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
        assert!(
            guarded.contains("slug.filter(|_| queue.whole)"),
            "the queue's list is pruned against without asking whether it saw everything — a \
             search cut off at its page makes every pull request past the hundredth absent for a \
             reason that has nothing to do with it"
        );
    }
}
