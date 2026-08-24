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

/// The repository's current name when it differs from the one skein holds, else `None`.
///
/// Remembered per process like the token beside it: this is a REST round trip and the queue is
/// polled from the board, so asking per refresh would spend a call on an answer that changes about
/// once a year. A lookup that fails is remembered as "no rename" rather than retried on every poll
/// — the cost of being wrong is one stale name until a restart, and the cost of not caching it is a
/// call per repo per poll on every fleet that has no rename at all, which is all of them.
fn renamed_to(slug: &str) -> Option<String> {
    let mut seen = match RENAMES.lock() {
        Ok(seen) => seen,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(known) = seen.get(slug) {
        return known.clone();
    }
    let now = host_token()
        .ok()
        .and_then(|token| crate::github::canonical_repo(slug, &token).ok())
        .filter(|now| !now.eq_ignore_ascii_case(slug));
    seen.insert(slug.to_string(), now.clone());
    now
}

static RENAMES: std::sync::Mutex<std::collections::BTreeMap<String, Option<String>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Forget the resolved names, for tests and for a caller that has just been told one changed.
pub fn forget_renames() {
    if let Ok(mut seen) = RENAMES.lock() {
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
    if !force && !cfg!(test) {
        let cache = QUEUE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, q)) = cache.as_ref().and_then(|m| m.get(&repo.id)) {
            if at.elapsed() < Duration::from_secs(60) {
                return Ok(q.clone());
            }
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

    // One query per membership rule. Three calls rather than one because GitHub's search cannot
    // express the union, and a client-side filter over every open PR would be far more expensive on
    // a busy repo than three narrow searches.
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
    for (search, reason) in searches {
        let items = match search_prs(&slug, &search) {
            Ok(items) => items,
            Err(e) => {
                blind_spots.push(format!(
                    "the `{search}` query failed, so those PRs are missing: {e}"
                ));
                continue;
            }
        };
        for item in items {
            let Some(number) = item.get("number").and_then(|v| v.as_u64()) else {
                continue;
            };
            if let Some(existing) = prs.iter_mut().find(|p| p.number == number) {
                if !existing.reasons.contains(&reason) {
                    existing.reasons.push(reason.clone());
                }
                continue;
            }
            prs.push(build_pr(
                &item,
                number,
                &login,
                &repo.id,
                &reason,
                &archived_numbers,
                &snoozed_shas,
            ));
        }
    }

    newest_first(&mut prs);

    // An archived PR that is no longer open cannot be in this list, so its entry is dead weight.
    // Pruning is safe in the direction that matters: if a PR is ever reopened it comes back
    // unarchived, which is *more* of your attention, not less.
    let open: Vec<u64> = prs.iter().map(|p| p.number).collect();
    if archived_numbers.iter().any(|n| !open.contains(n)) {
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
    if snoozed_shas.iter().any(|(n, sha)| !live(n, sha)) {
        let kept: BTreeMap<u64, String> = snoozed_shas
            .into_iter()
            .filter(|(n, sha)| live(n, sha))
            .collect();
        let _ = write_snoozed(&repo.id, &kept);
    }

    let q = Queue {
        repo_id: repo.id.clone(),
        slug,
        viewer: login,
        ai: crate::review::summaries_enabled(),
        prs,
        blind_spots,
        as_of: chrono::Utc::now().to_rfc3339(),
        fresh: true,
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
    let cache = QUEUE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let (at, q) = cache.as_ref()?.get(repo_id)?;
    (at.elapsed() < Duration::from_secs(60)).then(|| q.clone())
}

/// The query behind the queue. One call per membership rule, and every field the parser needs.
///
/// GraphQL rather than REST, and not as a preference: a pull request's reviews, the commit each was
/// left against, and its check rollup are three more REST calls **per pull request**. This returns
/// all of it for a hundred at once. It is also, underneath, exactly what `gh pr list --json` did —
/// its field names *are* these — which is why [`shape`] below is almost an identity.
const SEARCH_QUERY: &str = r#"
query($q: String!, $n: Int!) {
  search(query: $q, type: ISSUE, first: $n) {
    nodes {
      ... on PullRequest {
        number title url isDraft updatedAt
        headRefName headRefOid baseRefName reviewDecision mergeable
        additions deletions changedFiles
        labels(first: 20) { nodes { name } }
        author { login }
        latestReviews(first: 30) { nodes { state author { login } commit { oid } } }
        commits(last: 1) { nodes { commit { committedDate statusCheckRollup { contexts(first: 100) { nodes {
          ... on CheckRun { name detailsUrl status conclusion }
          ... on StatusContext { context targetUrl state }
        } } } } } }
      }
    }
  }
}"#;

/// One search, returning items in the shape the parser has always read.
fn search_prs(slug: &str, search: &str) -> Result<Vec<serde_json::Value>, String> {
    let token = host_token()?;
    // `is:pr is:open` and the repo are what `gh pr list --repo … --state open` added for us. Spelled
    // out here because the search string is now ours to build rather than gh's.
    let q = format!("repo:{slug} is:pr is:open {search}");
    let data = crate::github::graphql(
        SEARCH_QUERY,
        serde_json::json!({ "q": q, "n": 100 }),
        &token,
    )?;
    let nodes = data
        .get("search")
        .and_then(|s| s.get("nodes"))
        .and_then(|n| n.as_array())
        .cloned()
        .unwrap_or_default();
    // A search that matches an issue rather than a pull request comes back as an empty object — the
    // inline fragment simply does not apply — so those are dropped rather than parsed into a PR
    // with number 0.
    Ok(nodes
        .iter()
        .filter(|node| node.get("number").is_some())
        .map(shape)
        .collect())
}

/// GraphQL's nesting, flattened into the shape `gh --json` produced.
///
/// Two differences, both structural rather than semantic: a GraphQL connection is `{nodes: […]}`
/// where gh gave a bare array, and the check rollup hangs off the last commit rather than off the
/// pull request. Everything else is the same name and the same value, which is what made this port
/// a translation rather than a rewrite — and what lets every test of [`build_pr`],
/// [`my_review_state`] and [`rollup`] keep asserting on the fixtures they always had.
fn shape(node: &serde_json::Value) -> serde_json::Value {
    let mut out = node.clone();
    let reviews = node
        .get("latestReviews")
        .and_then(|r| r.get("nodes"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let checks = node
        .get("commits")
        .and_then(|c| c.get("nodes"))
        .and_then(|n| n.as_array())
        .and_then(|n| n.first())
        .and_then(|c| c.get("commit"))
        .and_then(|c| c.get("statusCheckRollup"))
        .and_then(|r| r.get("contexts"))
        .and_then(|c| c.get("nodes"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
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
fn rollup(item: &serde_json::Value) -> String {
    let Some(checks) = item.get("statusCheckRollup").and_then(|v| v.as_array()) else {
        return "none".into();
    };
    if checks.is_empty() {
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
    if pending { "pending" } else { "passing" }.into()
}

/// WHICH contexts are behind a red rollup — name and detail link, first [`FAILING_CHECKS_SHOWN`]
/// in rollup order, deduplicated by name (SKEIN-153).
///
/// Deduplicated because re-runs of one check arrive as repeated contexts, and a row that says
/// "build, build, build" answers the question worse than one that says "build". A failing context
/// GitHub gave no name for is skipped rather than shown blank: the one-word `checks` verdict
/// still says "failing", so nothing is hidden — there is just no name to show for it.
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
}

/// Take the count for every repo that has a review queue switched on.
///
/// Skips repos with no GitHub remote before touching the network — they cannot have PRs, so asking
/// would be a guaranteed error rather than a real one. Goes through the same 60s per-repo cache as
/// the pane, so opening the queue right after a poll costs nothing.
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
                };
            }
            match queue(&repo, false) {
                Ok(q) => Count {
                    repo_id: repo.id,
                    needs_you: q.prs.iter().filter(|p| p.lane == Lane::NeedsYou).count(),
                    error: String::new(),
                    skipped: String::new(),
                },
                Err(e) => Count {
                    repo_id: repo.id,
                    needs_you: 0,
                    error: e,
                    skipped: String::new(),
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
}

/// Post one review carrying line comments — the vetted output of `crate::review::critique`.
///
/// `head_sha` is sent as `commit_id` and it is load-bearing: the comments were anchored against
/// that commit's diff, and GitHub attaches them to whatever commit this names. The caller has
/// already refused a moved head with a better sentence than GitHub's 422; this is the second lock.
pub fn submit_review_with_comments(
    slug: &str,
    number: u64,
    head_sha: &str,
    verdict: Verdict,
    body: &str,
    comments: &[ReviewComment],
) -> Result<String, String> {
    // A bare approval is a complete statement; anything else with neither words nor comments is a
    // press with nothing behind it.
    if body.trim().is_empty() && comments.is_empty() && verdict != Verdict::Approve {
        return Err("nothing to post — every comment was dropped and the note is empty.".into());
    }
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    let mut payload = serde_json::json!({
        "event": event,
        "commit_id": head_sha,
        "body": body.trim(),
    });
    if !comments.is_empty() {
        payload["comments"] = comments
            .iter()
            .map(|c| {
                serde_json::json!({
                    "path": c.path, "line": c.line, "side": "RIGHT", "body": c.body,
                })
            })
            .collect();
    }
    crate::github::send_json(
        "POST",
        &format!("/repos/{slug}/pulls/{number}/reviews"),
        &host_token()?,
        &payload,
    )?;
    let said = match verdict {
        Verdict::Approve => "approved",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "posted the review",
    };
    Ok(match comments.len() {
        0 => said.into(),
        1 => format!("{said} — with 1 line comment"),
        n => format!("{said} — with {n} line comments"),
    })
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
                        // what GitHub really does and why the queue went quietly empty.
                        let hit =
                            body.contains("acme/new-name") && body.contains("review-requested");
                        (
                            200,
                            match hit {
                                true => r#"{"data":{"search":{"nodes":[{"number":7,"title":"a pull request","url":"u","isDraft":false,"author":{"login":"someone"},"headRefOid":"abc","updatedAt":"2026-08-01T00:00:00Z","latestReviews":{"nodes":[]},"reviewRequests":{"nodes":[]}}]}}}"#.to_string(),
                                false => r#"{"data":{"search":{"nodes":[]}}}"#.to_string(),
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
            SEARCH_QUERY.contains("commit { committedDate"),
            "the head commit's date is read but never requested: {SEARCH_QUERY}"
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
            SEARCH_QUERY.contains("additions deletions changedFiles"),
            "the fields are read but never requested: {SEARCH_QUERY}"
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

    #[test]
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
            SEARCH_QUERY.contains("... on CheckRun { name detailsUrl status conclusion }"),
            "the CheckRun name/link is read but never requested: {SEARCH_QUERY}"
        );
        assert!(
            SEARCH_QUERY.contains("... on StatusContext { context targetUrl state }"),
            "the StatusContext name/link is read but never requested: {SEARCH_QUERY}"
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
}
