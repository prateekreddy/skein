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
        (GhToken::None, None)
    })
    .clone()
}

/// Which credential the host's GitHub calls are running on, for the places that report it.
pub fn host_token_source() -> GhToken {
    host_credential().0
}

/// The token itself, or the sentence to show instead of an empty queue.
fn host_token() -> Result<String, String> {
    host_credential().1.ok_or_else(|| {
        "no GitHub token: the review queue reads pull requests as you, and nothing here names a \
         user. Export GH_TOKEN, or add a read token in Settings → GitHub & keys. A GitHub App \
         cannot do this one — an installation token is not a person."
            .to_string()
    })
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

// ───────────────────────────── fetching ─────────────────────────────

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
        headRefName headRefOid baseRefName reviewDecision
        author { login }
        latestReviews(first: 30) { nodes { state author { login } commit { oid } } }
        commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
          ... on CheckRun { status conclusion }
          ... on StatusContext { state }
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
    if let Some(map) = out.as_object_mut() {
        map.insert("latestReviews".into(), reviews);
        map.insert("statusCheckRollup".into(), checks);
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

/// A pull request's diff, as a diff — the media type is the whole of what `gh pr diff` did.
pub fn pr_diff_text(slug: &str, number: u64) -> Result<String, String> {
    crate::github::get_text(
        &format!("/repos/{slug}/pulls/{number}"),
        &host_token()?,
        "application/vnd.github.diff",
    )
}

/// The paths a pull request touches.
///
/// Its own endpoint rather than parsing the diff for `+++` lines: a rename, a binary file and a
/// mode-only change are all files GitHub names here and none of them appear the way a parser would
/// expect. One page of 100 — a review over that many files is not one this tool is helping with.
pub fn pr_files(slug: &str, number: u64) -> Result<Vec<String>, String> {
    Ok(crate::github::get_json(
        &format!("/repos/{slug}/pulls/{number}/files?per_page=100"),
        &host_token()?,
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

    /// The host uses the credential you already gave it, and never asks for another.
    ///
    /// This replaces a test about `gh`'s keyring, because the keyring is no longer reachable from
    /// here: the queue talks to the API with a token skein holds. What survives is the property
    /// that mattered — one credential the user chose, doing every job it is capable of — and it is
    /// now asserted on the wire rather than on a subprocess's environment.
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
