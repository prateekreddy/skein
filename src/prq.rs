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
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Which lane a PR sits in. Derived on every fetch, never stored — the only lane skein has an
/// opinion about is [`Lane::Archived`], and even that is cleared the moment GitHub stops calling
/// the PR open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    /// Open, yours, and you have not submitted a decision on the *current* head commit.
    NeedsYou,
    /// You have decided on the current head (approved or requested changes); it is not merged.
    Waiting,
    /// You have set it aside by hand — it is open, but not going to move for reasons skein has no
    /// way to know.
    Archived,
}

/// Why a PR is in your queue. Kept as a list rather than one value because a PR is routinely more
/// than one of these at once, and collapsing them would break the filter you actually asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// You opened it.
    Author,
    /// Your review was requested, personally.
    Reviewer,
    /// You were mentioned in the body or a comment.
    Mentioned,
    /// A team you belong to was asked to review — invisible to the personal query, see [`viewer`].
    Team(String),
}

/// One PR in the queue.
///
/// Fields are pulled defensively from `gh`'s JSON: a field this version of `gh` does not emit
/// degrades that one value, rather than dropping the PR. A PR you never saw is the failure mode
/// that costs something; a PR with an unknown check state is merely less useful.
#[derive(Debug, Clone, Serialize)]
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
    /// "passing" | "pending" | "failing" | "none".
    pub checks: String,
    /// "approved" | "changes-requested" | "commented" | "none" — *your* last review.
    pub my_review: String,
    /// Was that review submitted against the current head? False after new commits land, which is
    /// what returns an approved PR to [`Lane::NeedsYou`].
    pub review_is_current: bool,
    pub reasons: Vec<Reason>,
    pub lane: Lane,
    /// The deterministic box name for this branch — whether or not one exists yet.
    pub box_name: String,
}

/// A repo's queue, plus an honest account of what could not be looked at.
#[derive(Debug, Clone, Serialize)]
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
}

/// The `gh` binary, overridable so tests can stub GitHub without a network or a login.
pub(crate) fn gh_bin() -> String {
    std::env::var("SKEIN_GH_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "gh".into())
}

/// Where the token skein hands `gh` came from, in the order it is looked for.
///
/// The point of the order is that **`gh auth login` is the last resort, not the requirement.** A
/// fleet on the PAT path has already given skein a token; making it also authenticate `gh` asks for
/// a second credential to do a job the first one covers — and on Linux that second credential lives
/// in the login keyring, so it asks for a password too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhToken {
    /// `$GH_TOKEN` / `$GITHUB_TOKEN` — already in the environment, so nothing is read and `gh`
    /// inherits it untouched.
    Environment,
    /// The read token stored in Settings. A user's own PAT, which is what a review queue needs: it
    /// answers "who are you" and can see the repositories its owner can.
    ReadToken,
    /// A per-repo write token, used because it is also a user's PAT and no read token was stored.
    /// Narrower than a read token — what it cannot see is reported as a blind spot rather than
    /// quietly missing from the queue.
    WritePat,
    /// `gh auth token` — the account path. The only one that can reach the system keyring.
    GhLogin,
    /// Nothing to hand it. `gh` will try its own store, and if that is empty the queue says so
    /// rather than reporting an empty queue.
    None,
}

impl GhToken {
    pub fn label(self) -> &'static str {
        match self {
            GhToken::Environment => "$GH_TOKEN",
            GhToken::ReadToken => "the read token in Settings",
            GhToken::WritePat => "a repository write token you stored",
            GhToken::GhLogin => "`gh auth login`",
            GhToken::None => "nothing — `gh` is on its own",
        }
    }
}

/// The token every `gh` skein runs is handed, resolved **at most once per process**.
///
/// Two problems, one answer. The first is the keyring: `gh` keeps its token there on a modern Linux,
/// so every invocation is a libsecret read, and a locked login keyring answers each with an unlock
/// dialog. This module runs four per repo and the cockpit polls it every three minutes.
///
/// The second is that `gh auth login` was effectively mandatory. skein offers three credential
/// paths — a GitHub App, a per-repo PAT, this account token — and the first two are about what a
/// *box* pushes with. The review queue runs on the *host*, always went through `gh`, and `gh` only
/// knew its own login. So someone who deliberately chose a PAT was still told to authenticate `gh`,
/// which is a second credential for a job the first one covers.
///
/// A stored read token is a user's own PAT and is exactly what the queue needs, so it is preferred
/// over the account login. `gh` accepts any token through `GH_TOKEN` and never consults its keyring
/// when one is set — so on the PAT path `gh` needs to be installed and never needs to be logged in.
fn gh_source() -> (GhToken, Option<String>) {
    let mut slot = match GH_TOKEN.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.get_or_insert_with(|| {
        // Exported already ⇒ the child inherits it and there is nothing to add or to read.
        if ["GH_TOKEN", "GITHUB_TOKEN"]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
        {
            return (GhToken::Environment, None);
        }
        // The credential the user actually chose, before the one they were being made to acquire.
        if let Some(pat) = crate::gitgate::read_pat() {
            return (GhToken::ReadToken, Some(pat));
        }
        // And failing that, any write PAT they stored. It belongs to a person, so it can say who
        // that person is — which is the whole of what this needs. A fleet on the PAT path has
        // already given skein a usable credential; making it acquire another is the thing being
        // fixed, not a smaller version of it.
        if let Some(pat) = crate::gitgate::any_user_pat() {
            return (GhToken::WritePat, Some(pat));
        }
        // Last: ask `gh`. This is the one read that can reach the keyring, and it happens lazily —
        // when something already wanted to run `gh` — so it never adds a prompt that was not coming.
        let mut command = std::process::Command::new(gh_bin());
        command.args(["auth", "token"]);
        match crate::util::output_with_timeout(&mut command, std::time::Duration::from_secs(20))
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|t| !t.is_empty())
        {
            Some(token) => (GhToken::GhLogin, Some(token)),
            None => (GhToken::None, None),
        }
    })
    .clone()
}

/// Which credential the host's GitHub calls are running on, for the places that report it.
pub fn gh_token_source() -> GhToken {
    gh_source().0
}

fn gh_child_env() -> Vec<(String, String)> {
    match gh_source().1 {
        Some(token) => vec![("GH_TOKEN".to_string(), token)],
        None => Vec::new(),
    }
}

/// The one resolution, remembered. A `Mutex<Option<_>>` rather than a `OnceLock` so a test can
/// forget it; the outer `Option` is "have we looked yet".
static GH_TOKEN: std::sync::Mutex<Option<(GhToken, Option<String>)>> = std::sync::Mutex::new(None);

/// Forget it, so the next `gh` resolves again. Tests only — in a run, resolving twice is the prompt
/// this exists to prevent.
#[cfg(test)]
pub(crate) fn forget_gh_token() {
    if let Ok(mut slot) = GH_TOKEN.lock() {
        *slot = None;
    }
}

/// Run `gh` the way every caller always should have: one process, one inherited token.
pub(crate) fn run_gh(args: &[&str]) -> Result<(String, String, i32), String> {
    run_gh_for(args, std::time::Duration::from_secs(30))
}

/// [`run_gh`] with the budget named at the call site — a `pr diff` is not a `pr list`.
pub(crate) fn run_gh_for(
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<(String, String, i32), String> {
    crate::util::run_capture_for_env(&gh_bin(), args, timeout, &gh_child_env())
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
    let (out, err, code) = run_gh(&["api", "user", "--jq", ".login"])?;
    if code != 0 {
        let msg = if err.trim().is_empty() { out } else { err };
        // Which credential this ran on, and every way to change it. The queue is about *your*
        // pull requests, so it needs a token that answers "who are you" — and the answer used to be
        // "run `gh auth login`", as if that were the only one. It is the last of three.
        return Err(format!(
            "GitHub could not identify you from {}: {}. The review queue needs a token that names a \
             user — export GH_TOKEN, or add a read token in Settings → GitHub & keys (a PAT of your \
             own, which is also what a fleet on the PAT path already has), or `gh auth login`.",
            gh_token_source().label(),
            msg.trim()
        ));
    }
    let login = out.trim().to_string();
    if login.is_empty() {
        return Err("gh returned an empty login".into());
    }
    let teams = run_gh(&[
        "api",
        "user/teams",
        "--paginate",
        "--jq",
        r#".[] | .organization.login + "/" + .slug"#,
    ])
    .ok()
    .filter(|(_, _, code)| *code == 0)
    .map(|(out, _, _)| {
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default();
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

// ───────────────────────────── fetching ─────────────────────────────

/// The JSON fields asked of `gh pr list`. `latestReviews` is the load-bearing one: it carries the
/// **commit** each review was submitted against, which is what distinguishes "you approved this"
/// from "you approved something three commits ago".
const PR_FIELDS: &str = "number,title,author,url,headRefName,headRefOid,baseRefName,isDraft,updatedAt,reviewDecision,latestReviews,statusCheckRollup";

/// 60s micro-cache **per repo**, for the same reason [`crate::repos::REPOS_CACHE`] exists: the
/// cockpit re-renders far more often than GitHub changes, and each fetch is three network round
/// trips.
///
/// Keyed by repo rather than holding one entry, because the badge poller walks every repo that has
/// the queue switched on. A single slot would let each repo evict the last one and turn a cache
/// into a guaranteed miss — the exact opposite of what it is for.
static QUEUE_CACHE: Mutex<Option<HashMap<String, (Instant, Queue)>>> = Mutex::new(None);

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
    let slug = repo_slug(repo)
        .ok_or("this repo has no GitHub remote, so it has no pull requests to review")?;
    let (login, teams) = viewer()?;
    let mut blind_spots = Vec::new();
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
            ));
        }
    }

    // Newest activity first: the queue is worked from the top, and "changed most recently" is the
    // closest thing to "most likely to still be moving".
    prs.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then(b.number.cmp(&a.number))
    });

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

    let q = Queue {
        repo_id: repo.id.clone(),
        slug,
        viewer: login,
        ai: crate::review::summaries_enabled(),
        prs,
        blind_spots,
    };
    if !cfg!(test) {
        QUEUE_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(repo.id.clone(), (Instant::now(), q.clone()));
    }
    Ok(q)
}

/// One `gh pr list --search` call, returning the raw JSON items.
fn search_prs(slug: &str, search: &str) -> Result<Vec<serde_json::Value>, String> {
    let (out, err, code) = run_capture(
        &gh_bin(),
        &[
            "pr", "list", "--repo", slug, "--state", "open", "--search", search, "--limit", "100",
            "--json", PR_FIELDS,
        ],
    )?;
    if code != 0 {
        let msg = if err.trim().is_empty() { &out } else { &err };
        return Err(msg.trim().to_string());
    }
    serde_json::from_str::<Vec<serde_json::Value>>(&out).map_err(|e| e.to_string())
}

fn build_pr(
    item: &serde_json::Value,
    number: u64,
    login: &str,
    repo_id: &str,
    reason: &Reason,
    archived_numbers: &[u64],
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
    let lane = if archived_numbers.contains(&number) {
        Lane::Archived
    } else if review_is_current && matches!(my_review.as_str(), "approved" | "changes-requested") {
        Lane::Waiting
    } else {
        Lane::NeedsYou
    };
    Pr {
        number,
        title: s("title"),
        author: item
            .get("author")
            .and_then(|a| a.get("login"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        url: s("url"),
        box_name: crate::repos::box_name(repo_id, &head_ref),
        head_ref,
        head_sha,
        base_ref: s("baseRefName"),
        draft: item
            .get("isDraft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        updated_at: s("updatedAt"),
        checks: rollup(item),
        my_review,
        review_is_current,
        reasons: vec![reason.clone()],
        lane,
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
    let (mut failing, mut pending) = (false, false);
    for c in checks {
        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let conclusion = c
            .get("conclusion")
            .and_then(|v| v.as_str())
            .or_else(|| c.get("state").and_then(|v| v.as_str()))
            .unwrap_or("");
        match conclusion {
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
            | "ERROR" => failing = true,
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            _ => {
                if status == "COMPLETED" {
                    failing = true;
                } else {
                    pending = true;
                }
            }
        }
    }
    if failing {
        "failing"
    } else if pending {
        "pending"
    } else {
        "passing"
    }
    .into()
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
    crate::load_repos()
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

impl Verdict {
    fn flag(self) -> &'static str {
        match self {
            Verdict::Approve => "--approve",
            Verdict::RequestChanges => "--request-changes",
            Verdict::Comment => "--comment",
        }
    }
}

/// Submit a review as **you**, via the host's own `gh` login.
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
    let n = number.to_string();
    let mut args = vec!["pr", "review", &n, "--repo", slug, verdict.flag()];
    if !body.is_empty() {
        args.push("--body");
        args.push(body);
    }
    let (out, err, code) = run_gh(&args)?;
    if code != 0 {
        let msg = if err.trim().is_empty() { out } else { err };
        return Err(msg.trim().to_string());
    }
    Ok(match verdict {
        Verdict::Approve => "approved",
        Verdict::RequestChanges => "changes requested",
        Verdict::Comment => "commented",
    }
    .into())
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
    let n = number.to_string();
    let (out, err, code) = run_gh(&["pr", "merge", &n, "--repo", slug, &method])?;
    if code != 0 {
        let msg = if err.trim().is_empty() { out } else { err };
        return Err(msg.trim().to_string());
    }
    let msg = out.trim();
    Ok(if msg.is_empty() {
        "merged".into()
    } else {
        msg.to_string()
    })
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

    /// `gh` is asked for its token **once per run**, not once per call.
    ///
    /// On a modern Linux `gh` keeps its token in the system keyring, so every invocation is a
    /// libsecret read and a locked login keyring answers with an unlock dialog. This module runs four
    /// per repo, the cockpit polls it every three minutes, and each poll spawns fresh processes — a
    /// password prompt every three minutes, for ever, from a feature that is on by default. It was
    /// reported as "the credentials popup still keeps happening", and it was this, not the account
    /// token that had already been made opt-in.
    #[test]
    fn gh_is_asked_for_its_token_once_a_run_and_not_once_a_call() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let log = home.join("gh.log");
        let fake = home.join("gh");
        // Records what it was asked and what it was handed. `auth token` answers with a token, the
        // way a keyring read would after someone typed their password.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s | GH_TOKEN=%s\\n' \"$*\" \"${{GH_TOKEN:-}}\" >> {log}\n\
                 [ \"$1\" = auth ] && echo gho_fromkeyring\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("SKEIN_GH_BIN", &fake);
        // Neither exported: this is the case that has to reach the keyring at all.
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
        forget_gh_token();

        for _ in 0..3 {
            let _ = run_gh(&["api", "user"]);
        }
        assert_eq!(
            gh_token_source(),
            GhToken::GhLogin,
            "with nothing else, gh's own login"
        );
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let asked = calls
            .lines()
            .filter(|l| l.starts_with("auth token"))
            .count();
        assert_eq!(
            asked, 1,
            "one keyring read per run, not one per call:\n{calls}"
        );
        // And every real call carries it, so `gh` never goes to the keyring itself.
        for line in calls.lines().filter(|l| l.starts_with("api ")) {
            assert!(
                line.contains("GH_TOKEN=gho_fromkeyring"),
                "a gh call went out without the token, so it read the keyring itself: {line}"
            );
        }

        // An exported token means the keyring is never touched at all — the fix a user can apply
        // themselves, and the one `skein doctor` points at.
        forget_gh_token();
        std::fs::write(&log, "").unwrap();
        std::env::set_var("GH_TOKEN", "gho_exported");
        let _ = run_gh(&["api", "user"]);
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !calls.contains("auth token"),
            "an exported token still went to the keyring:\n{calls}"
        );

        assert_eq!(gh_token_source(), GhToken::Environment);
        std::env::remove_var("GH_TOKEN");

        // And the answer to "why must I log `gh` in when I chose a PAT": a stored read token is a
        // user's own PAT, which is exactly what a queue about *your* pull requests needs. It is
        // preferred over the account login, so on the PAT path `gh` is installed and never
        // authenticated — and the keyring is never opened.
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", skein_home.as_ref() as &std::path::Path);
        crate::gitgate::set_read_pat("github_pat_mine").unwrap();
        forget_gh_token();
        std::fs::write(&log, "").unwrap();
        let _ = run_gh(&["api", "user"]);
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(gh_token_source(), GhToken::ReadToken);
        assert!(
            !calls.contains("auth token"),
            "a fleet with its own PAT was still made to log `gh` in:\n{calls}"
        );
        assert!(
            calls.contains("GH_TOKEN=github_pat_mine"),
            "the token the user chose was not the one used:\n{calls}"
        );

        // And the sharper case: a fleet that stored only a *write* PAT. That token belongs to a
        // person too, so it can answer "who are you" — one credential the user chose, doing every
        // job it is capable of, instead of a second one being asked for.
        crate::gitgate::set_read_pat("").unwrap();
        crate::gitgate::set_write_credential("mine", "mine", &["me/repo".into()]).unwrap();
        crate::gitgate::set_credential_token("mine", "github_pat_write").unwrap();
        forget_gh_token();
        std::fs::write(&log, "").unwrap();
        let _ = run_gh(&["api", "user"]);
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(gh_token_source(), GhToken::WritePat);
        assert!(
            !calls.contains("auth token"),
            "a fleet with a PAT of its own was still made to log `gh` in:\n{calls}"
        );
        assert!(calls.contains("GH_TOKEN=github_pat_write"), "{calls}");

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_GH_BIN");
        forget_gh_token();
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

        crate::save_repos(&[
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
        let pr = build_pr(&v, 3, "me", "repo", &Reason::Reviewer, &[3]);
        assert_eq!(pr.lane, Lane::Archived);
    }

    #[test]
    fn an_unreviewed_pr_needs_you() {
        let v = item(r#"{"number":3,"headRefOid":"abc","title":"t"}"#);
        let pr = build_pr(&v, 3, "me", "repo", &Reason::Reviewer, &[]);
        assert_eq!(pr.lane, Lane::NeedsYou);
        assert_eq!(pr.checks, "none");
    }

    #[test]
    fn a_decided_pr_waits() {
        let v = item(
            r#"{"number":3,"headRefOid":"abc","latestReviews":[{"author":{"login":"me"},"state":"CHANGES_REQUESTED","commit":{"oid":"abc"}}]}"#,
        );
        let pr = build_pr(&v, 3, "me", "repo", &Reason::Author, &[]);
        assert_eq!(pr.lane, Lane::Waiting);
    }

    #[test]
    fn the_box_name_is_derived_from_the_head_branch() {
        let v = item(r#"{"number":3,"headRefName":"feature/thing"}"#);
        let pr = build_pr(&v, 3, "me", "acme", &Reason::Author, &[]);
        assert_eq!(pr.box_name, crate::repos::box_name("acme", "feature/thing"));
    }
}
