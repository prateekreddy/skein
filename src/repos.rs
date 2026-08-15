//! The repos skein manages (`~/.skein/repos.json`) and everything derived from one.
//!
//! skein is not single-repo: each box is `<repo-id>-<branch>` and maps back to its repo by
//! id-prefix, longest id winning. This is skein's OWN registration of a repo — distinct from the
//! sbx registry of sandboxes — and it is what makes "add a repo URL and go" work without the repo
//! shipping anything for skein.

use crate::config::*;
use crate::runtime::*;
use crate::util::*;
use crate::{
    ensure_kit, ensure_store, fleet_boxes, git_branch_for, load_connections, lookup_dir,
    plane_project_id, registry_entry_for_box, valid_name,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One managed repo. `work` is the host clone (for host-side git/diff + as the `sbx run` workspace);
/// `store` is the host `.claude` skein provisions and mounts into every box for that repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub source: String, // git URL or local path the repo was added from
    pub work: String,   // host working clone
    pub store: String,  // host shared `.claude` store
    #[serde(default = "default_agent")]
    pub agent: String, // runtime adapter id (see `supported_runtimes`)
    /// The Plane project this repo's work is tracked in — a project URL or a bare uuid, kept
    /// verbatim so the cockpit can link to the board. Per-repo because a project is what an agent
    /// token binds to; empty ⇒ this repo's boxes get a tracker token with no default project, and
    /// must name a project on every call.
    #[serde(default)]
    pub plane_project: String,
    /// Which [`SyncConnection`] this repo's boxes claim work through, by id. Empty ⇒ not tracked.
    /// A *selection*, not a URL: a gateway and the personal token that mints tokens at it are one
    /// thing, and a repo pointed at gateway B while the host holds only gateway A's token is a
    /// setting that can only be right by accident. Per-repo because a gateway is a backlog — two
    /// products in different Plane instances cannot share a claim namespace.
    #[serde(default)]
    pub sync_connection: String,
    /// Does this repo have a review queue, and may the badge poll it?
    ///
    /// **On by default**, and separate from whether summaries are allowed: this is about *this*
    /// repo, not about AI. A repo you have registered only to run boxes in — a fork, a scratch
    /// clone, somebody else's project you read — has pull requests that are none of your business,
    /// and polling it every few minutes to say so would spend `gh` calls to produce a zero.
    ///
    /// A repo with no GitHub remote is skipped whether or not this is set: it cannot have a queue.
    #[serde(default = "crate::config::default_true")]
    pub review_queue: bool,
    /// Superseded by [`Repo::sync_connection`]; read once by the migration, then cleared. Kept so
    /// a `repos.json` written before connections existed still parses.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sync_gateway_url: String,
}

/// 1s micro-cache over `repos.json`: a single `load_views` pass consults the repo list dozens of
/// times per box (store_for_box, current_status, current_task, …) and each SSE tick repeats that
/// per open browser tab — hundreds of disk reads every 2s on a busy fleet, all returning the same
/// bytes. Cleared by `save_repos` so a mutation is visible immediately.
pub(crate) static REPOS_CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<Repo>)>> =
    std::sync::Mutex::new(None);

/// Every repo skein manages (empty if none added yet / file absent or malformed). Micro-cached —
/// see REPOS_CACHE.
pub fn load_repos() -> Vec<Repo> {
    // cfg!(test): unit tests point $SKEIN_HOME at per-test temp dirs and run in parallel — a
    // process-wide cache would leak one test's repo list into the next. Prod (server/CLI) keeps it.
    if !cfg!(test) {
        let cache = REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, repos)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(1) {
                return repos.clone();
            }
        }
    }
    let repos = fs::read_to_string(repos_json())
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Repo>>(&t).ok())
        .unwrap_or_default();
    *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((std::time::Instant::now(), repos.clone()));
    repos
}

/// Persist the repo list to `~/.skein/repos.json` (pretty, atomic).
pub fn save_repos(repos: &[Repo]) -> Result<(), String> {
    *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None; // mutation → drop the micro-cache
    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(repos).map_err(|e| e.to_string())?;
    write_atomic(&repos_json(), &home, &bytes)
}

/// Update a repo's own settings. Every field is optional: `None` leaves it alone, `Some("")` clears
/// it back to the global default. One function — and one route — rather than one per field, because
/// there are three of these now and a fourth would have been a fourth copy of the same lookup.
///
/// Validation happens before anything is written: a half-applied update across two fields is worse
/// than a refusal. A mistyped Plane project is refused rather than stored, because it would
/// otherwise surface as a token that authenticates and then 403s on the agent's first write.
pub fn set_repo_settings(
    id: &str,
    plane_project: Option<&str>,
    sync_connection: Option<&str>,
    review_queue: Option<bool>,
) -> Result<Repo, String> {
    if let Some(project) = plane_project.map(str::trim) {
        if !project.is_empty() && plane_project_id(project).is_none() {
            return Err(
                "that isn't a Plane project — paste the project URL, or the uuid from it".into(),
            );
        }
    }
    // A selection that names nothing would read on screen as "tracked" and behave as "not tracked",
    // which is the silent-wrong-result this whole surface exists to avoid.
    if let Some(conn) = sync_connection.map(str::trim) {
        if !conn.is_empty() && !load_connections().iter().any(|c| c.id == conn) {
            return Err(format!("no work-tracking connection called {conn:?}"));
        }
    }
    let mut repos = load_repos();
    let repo = repos
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no repo with id {id:?}"))?;
    if let Some(v) = plane_project {
        repo.plane_project = v.trim().to_string();
    }
    if let Some(v) = sync_connection {
        repo.sync_connection = v.trim().to_string();
        repo.sync_gateway_url.clear(); // the selection is now the whole answer
    }
    if let Some(v) = review_queue {
        repo.review_queue = v;
    }
    let updated = repo.clone();
    save_repos(&repos)?;
    Ok(updated)
}

/// Unregister a repo from `repos.json` by id. Returns the removed `Repo`. Does NOT delete the working
/// clone or store on disk (they may hold unpushed work / a clone-mode box's only copy) — only skein's
/// registration is removed; report the paths so the user can delete them deliberately.
pub fn remove_repo(id: &str) -> Result<Repo, String> {
    let mut repos = load_repos();
    let pos = repos
        .iter()
        .position(|r| r.id == id)
        .ok_or_else(|| format!("no repo with id {id:?}"))?;
    let removed = repos.remove(pos);
    save_repos(&repos)?;
    Ok(removed)
}

/// The repo a box belongs to: the registered repo whose id is the box-name prefix (`<id>-<branch>`).
/// Longest id wins, so `web` and `web-api` are unambiguous.
pub fn repo_for_box(name: &str) -> Option<Repo> {
    load_repos()
        .into_iter()
        .filter(|r| name == r.id || name.starts_with(&format!("{}-", r.id)))
        .max_by_key(|r| r.id.len())
}

/// The branch skein recorded for a box in its repo's launch spec (`<store>/skein/launch/<name>.json`).
/// Host-readable and authoritative for a clone-mode box (whose private clone isn't on the host), so
/// the board can show the box's real branch — including a slashed one the name slug would have flattened.
pub(crate) fn launch_spec_branch(repo: &Repo, name: &str) -> Option<String> {
    launch_spec(repo, name)?
        .get("branch")
        .and_then(|b| b.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

pub(crate) fn launch_spec(repo: &Repo, name: &str) -> Option<serde_json::Value> {
    let p = Path::new(&repo.store)
        .join("skein")
        .join("launch")
        .join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

pub(crate) fn launch_spec_agent(repo: &Repo, name: &str) -> Option<String> {
    launch_spec(repo, name)?
        .get("agent")
        .and_then(|a| a.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// The branch *slug* encoded in a box name (`<id>-<branch-slug>` → `<branch-slug>`). This is the
/// sanitized form (no `/`), used only as a fallback when the real branch isn't otherwise known — the
/// authoritative branch (which may contain `/`, e.g. `feat/auth`) is carried in the launch spec and
/// recovered host-side from git. See [`box_name`].
pub fn branch_from_box(name: &str, repo: &Repo) -> String {
    name.strip_prefix(&format!("{}-", repo.id))
        .unwrap_or(name)
        .to_string()
}

/// The sbx box name for a repo + branch: `<repo-id>-<branch-slug>`.
pub fn box_name(repo_id: &str, branch: &str) -> String {
    format!("{}-{}", repo_id, slug(branch))
}

/// Is `source` a git URL (clone it) versus a local path (use in place)?
pub(crate) fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.ends_with(".git")
}

/// Is this an SSH git remote (`git@host:…` / `ssh://…`)? sbx forwards the host SSH *agent*
/// (`SSH_AUTH_SOCK`) into the box, so SSH push works *iff* the host agent is running with the key
/// loaded; otherwise it'll fail and HTTPS (proxy-injected creds) is the no-setup path. See
/// docs.docker.com/ai/sandboxes/security/credentials.
pub(crate) fn is_ssh_url(s: &str) -> bool {
    s.starts_with("git@") || s.starts_with("ssh://")
}

/// The `origin` URL of the clone at `work`, if any.
pub(crate) fn remote_origin_url(work: &str) -> Option<String> {
    let mut command = Command::new("git");
    command.args(["-C", work, "remote", "get-url", "origin"]);
    let out = bounded_output(&mut command, "git remote", Duration::from_secs(5)).ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// A heads-up about a managed repo's push path, surfaced by `skein add` + the cockpit so it's known
/// up-front (not an error — both cases are workable). Two cases warn: a repo with **no `origin`
/// remote** (common when adopting a local folder never pushed) — a box can't push or open a PR until
/// one exists; and an **SSH `origin`** — in-box push then leans on the host SSH agent (sbx forwards
/// `SSH_AUTH_SOCK`), so it works only when that agent has the key loaded, else switch to HTTPS.
/// `None` for an HTTPS origin (the no-setup happy path; a URL clone always lands here).
pub fn remote_warning(work: &str) -> Option<String> {
    let Some(url) = remote_origin_url(work) else {
        return Some(format!(
            "this repo has no `origin` remote — a box can't push or open a PR until one exists. Add it on the host:  git -C {work} remote add origin <url>  (HTTPS needs no setup)."
        ));
    };
    if !is_ssh_url(&url) {
        return None;
    }
    // If a key is configured, skein loads it into the agent (which sbx forwards) — so it's set up;
    // only note the network-policy caveat. Otherwise spell out the agent requirement + HTTPS fallback.
    let key_configured = env::var("SKEIN_SSH_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| Some(load_config().ssh_key).filter(|s| !s.is_empty()))
        .is_some();
    if key_configured {
        return Some(format!(
            "origin is an SSH remote ({url}). skein loads your configured key into the ssh-agent (sbx forwards it into boxes), so push should work — just ensure the sandbox network policy allows {}.",
            host_of(&url).unwrap_or("the git host")
        ));
    }
    let mut msg = format!(
        "origin is an SSH remote ({url}). In-box push uses your host's forwarded SSH agent, so it works only if a key is loaded — set one in Settings (skein will `ssh-add` it), or it must already be in your agent."
    );
    if let Some(h) = ssh_to_https(&url) {
        msg.push_str(&format!(
            " For a no-setup path, switch to HTTPS:  git -C {work} remote set-url origin {h}"
        ));
    }
    Some(msg)
}

/// Best-effort `git@github.com:org/repo.git` / `ssh://git@host/org/repo.git` → `https://host/org/repo.git`.
/// Returns `None` for shapes we don't recognise (caller just omits the suggestion).
pub(crate) fn ssh_to_https(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return Some(format!("https://{host}/{path}"));
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        return Some(format!("https://{rest}"));
    }
    None
}

/// Derive a repo id from a source: the last path/URL component, minus a trailing `.git`.
pub(crate) fn repo_id_from_source(source: &str) -> String {
    let last = source
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(source);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// Add a repo to skein: clone it (URL) or adopt it in place (local path), provision its shared store
/// and skein's kit, seed gh auth, and record it in `repos.json`. Returns the stored `Repo`. This is
/// the whole `skein add <url|path>` flow; the box launch then needs nothing from the repo.
pub fn add_repo(
    source: &str,
    id: Option<&str>,
    agent: Option<&str>,
    store: Option<&str>,
) -> Result<Repo, String> {
    if let Some(runtime) = agent.map(str::trim).filter(|value| !value.is_empty()) {
        if !valid_runtime(runtime) {
            return Err(format!(
                "unsupported runtime {runtime:?}; available: {}",
                supported_runtimes()
                    .iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let id = id
        .map(|s| s.to_string())
        .unwrap_or_else(|| repo_id_from_source(source));
    if id.is_empty() {
        return Err("could not derive a repo id — pass one explicitly".into());
    }
    let home = skein_home();
    // The repo's shared-data folder (its `.claude` store), shared live across all the repo's boxes —
    // cross-box memory/mailbox/skills/statusline. The caller may point it at an existing rich store
    // (e.g. thing's `skein-shared/.claude`); otherwise skein manages one under its home. Either way
    // `ensure_store` is idempotent (adds the probe, seeds only what's absent), so an existing store is
    // adopted, not clobbered.
    let store = match store.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(expand_tilde(s)),
        None => home.join("repos").join(&id).join("store").join(".claude"),
    };

    let work = if is_git_url(source) {
        // Clone the URL into skein's managed area.
        let work = home.join("repos").join(&id).join("work");
        if work.join(".git").is_dir() {
            // already cloned — leave it (the user can pull); just (re)register.
        } else {
            // An SSH URL needs a key in the host agent for the clone itself; load it first.
            if is_ssh_url(source) {
                let _ = ensure_ssh_key();
            }
            fs::create_dir_all(work.parent().unwrap()).map_err(|e| format!("mkdir: {e}"))?;
            let mut command = Command::new("git");
            command.args(["clone", source]).arg(&work);
            let out = bounded_output(&mut command, "git clone", Duration::from_secs(300))?;
            if !out.status.success() {
                return Err(format!(
                    "git clone failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        work
    } else {
        // Local path: use it in place.
        let p = PathBuf::from(source);
        let p = p.canonicalize().unwrap_or(p);
        if !p.join(".git").exists() {
            return Err(format!("{} is not a git repo", p.display()));
        }
        p
    };

    ensure_kit()?;
    ensure_store(&store)?;
    let _ = ensure_gh_secret(); // best-effort; private clones/PRs need it, but absence isn't fatal

    let repo = Repo {
        id: id.clone(),
        source: source.to_string(),
        work: work.to_string_lossy().into_owned(),
        store: store.to_string_lossy().into_owned(),
        agent: agent
            .map(|s| s.to_string())
            .unwrap_or_else(|| load_config().default_agent),
        plane_project: String::new(),
        // One connection ⇒ adopt it, so a single-tracker fleet needs no ceremony per repo. Two or
        // more ⇒ leave it unset: which backlog this repo belongs to is not skein's guess to make,
        // and a wrong one mints a real credential against the wrong Plane.
        sync_connection: match load_connections().as_slice() {
            [only] => only.id.clone(),
            _ => String::new(),
        },
        review_queue: true,
        sync_gateway_url: String::new(),
    };
    let mut repos = load_repos();
    repos.retain(|r| r.id != id); // replace any existing entry with the same id
    repos.push(repo.clone());
    repos.sort_by(|a, b| a.id.cmp(&b.id));
    save_repos(&repos)?;
    Ok(repo)
}

/// Pull the latest code into a managed repo's working clone (`git -C <work> pull --ff-only`), so the
/// next box branches from current upstream. Fast-forward-only on purpose: skein never merges or
/// rebases on the user's behalf, so a diverged or dirty clone fails loudly rather than silently
/// rewriting their tree. Returns git's own summary on success. `None` repo id ⇒ error.
///
/// How much this matters depends on how the repo was registered, and the difference is worth knowing:
///
/// * **Registered from a URL** — boxes clone from the remote, so this only keeps the host clone's
///   `origin/HEAD` current, which is where [`crate::fleet::base_branch`] reads the base from.
/// * **Adopted in place** (a path, no URL) — there is no remote for a box to clone from, so
///   `fleet::clone_source` hands boxes the host clone itself. Its freshness *is* what a new
///   box starts from, and a stale clone means every new box starts stale with nothing to say so.
///
/// Which is why this did not go away when boxes stopped cloning from the host: for adopted repos it
/// became more load-bearing, not less.
pub fn pull_repo(id: &str) -> Result<String, String> {
    let repo = load_repos()
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no repo with id {id:?}"))?;
    if remote_origin_url(&repo.work).is_none() {
        return Err("this repo has no `origin` remote to pull from".into());
    }
    let mut command = Command::new("git");
    command.args(["-C", &repo.work, "pull", "--ff-only"]);
    let out = bounded_output(&mut command, "git pull", Duration::from_secs(120))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(if err.is_empty() {
            "git pull failed (the clone may have diverged or have local changes)".into()
        } else {
            err.to_string()
        });
    }
    let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if summary.is_empty() {
        "Already up to date.".into()
    } else {
        summary
    })
}

/// Seed the host's GitHub token into sbx globally so every box can fetch/push/open PRs:
/// `sbx secret set -g github -t "$(gh auth token)"`. Best-effort; skip with $SKEIN_NO_GH_SECRET.
/// Done once (global) rather than per-box, sidestepping the "box must exist first" timing.
///
/// Idempotent: sbx refuses to overwrite an existing secret without `-f`, so an already-seeded token
/// is treated as success (the boxes can already push) — not an error. Set $SKEIN_FORCE_GH_SECRET to
/// pass `-f` and refresh the token (e.g. after `gh auth refresh` / rotation).
pub fn ensure_gh_secret() -> Result<(), String> {
    let cfg = load_config();
    // env wins over the UI setting (headless/CI); either can disable seeding.
    if env::var_os("SKEIN_NO_GH_SECRET").is_some() || !cfg.seed_gh_secret {
        return Ok(());
    }
    let mut token_command = Command::new("gh");
    token_command.args(["auth", "token"]);
    let token = bounded_output(&mut token_command, "gh auth token", Duration::from_secs(15))?;
    if !token.status.success() {
        return Err("gh auth token failed (run `gh auth login` on the host)".into());
    }
    let token = String::from_utf8_lossy(&token.stdout).trim().to_string();
    if token.is_empty() {
        return Err("gh auth token was empty".into());
    }
    let force = env::var_os("SKEIN_FORCE_GH_SECRET").is_some() || cfg.force_gh_secret;
    let mut args = vec!["secret", "set", "-g", "github", "-t", &token];
    if force {
        args.push("-f");
    }
    let mut secret_command = Command::new("sbx");
    secret_command.args(&args);
    let out = bounded_output(
        &mut secret_command,
        "sbx secret set",
        Duration::from_secs(30),
    )?;
    if out.status.success() {
        return Ok(());
    }
    // Not forcing + the secret is already there → boxes can already push; that's success, not failure.
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !force && stderr.contains("already exists") {
        return Ok(());
    }
    Err(format!("sbx secret set failed: {}", stderr.trim()))
}

// ───────────────────────────── kit / store provisioning ─────────────────────────────

/// Record, for box `name`, what its kit startup needs (branch + agent) at
/// `<store>/skein/launch/<name>.json`. The kit finds this file (the store is mounted) and checks out
/// the branch — our env-free channel into the box, since `sbx run --env` is unconfirmed.
pub(crate) fn write_launch_spec_for_agent(
    name: &str,
    branch: &str,
    repo: &Repo,
    agent: &str,
) -> Result<(), String> {
    let dir = Path::new(&repo.store).join("skein").join("launch");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "branch": branch,
        "agent": agent,
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(format!("{name}.json")), &dir, &bytes)
}

/// Re-pin an already-created box to a different branch, without relaunching it. For when the agent
/// has moved off the box's recorded branch (e.g. branch-per-slice work) and the kit's startup hook —
/// which re-reads the launch spec on every reconnect — needs to stop re-asserting the stale one on
/// its next reconnect instead of the branch the agent actually wants to be on. This only rewrites the
/// launch spec; it does not touch the box's live working tree, so if the box is currently mid-session
/// on the wrong branch you still need to `git checkout` inside it once, or just reconnect after this.
/// Errs for an unknown box name or one that belongs to no registered repo (the legacy single-repo
/// path derives its branch from the box name and keeps no launch spec to repin).
pub fn repin_branch(name: &str, branch: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let branch = branch.trim();
    if branch.is_empty() {
        return Err("branch is empty".into());
    }
    let repo = repo_for_box(name).ok_or_else(|| format!("no registered repo for box {name}"))?;
    let agent = launch_spec_agent(&repo, name).unwrap_or_else(|| repo.agent.clone());
    write_launch_spec_for_agent(name, branch, &repo, &agent)
}

/// The box's current branch from its project registry, with launch branch only as a fallback.
pub fn branch_of(name: &str) -> Option<String> {
    // SAME cascade as the board (load_views): live registry → launch spec → box-name slug → host git.
    // This feeds *write* actions (gh pr create/merge --head), where the old registry-else-host-git
    // shortcut was dangerous: for a clone-mode box, `lookup_dir` is the SHARED host clone (often
    // sitting on master) — a missing registry branch meant creating/merging a PR for master, not
    // the box's real branch. The board never made that mistake; now the actions can't either.
    if let Some(branch) = registry_entry_for_box(name)
        .map(|box_| box_.branch)
        .filter(|branch| !branch.is_empty() && branch != "?")
    {
        return Some(branch);
    }
    if let Some(rp) = repo_for_box(name) {
        if let Some(b) = launch_spec_branch(&rp, name) {
            return Some(b);
        }
        return Some(branch_from_box(name, &rp));
    }
    git_branch_for(&lookup_dir(name)?)
}

/// Runtime configured for a box. Prefer sbx's live record, then the per-box launch spec (which
/// preserves a New-box override), then the repo default. Unknown/legacy boxes remain Claude for
/// backwards compatibility.
pub fn agent_for_box(name: &str) -> String {
    // Not for a box in the fleet. Migration leaves the old sandbox stopped but still listed under
    // the box's name, so `sbx ls` answers with a record from before the move — which would outrank
    // the launch spec a later takeover wrote. A placed box's spec is the live answer.
    if crate::place::shared_record(name).is_none() {
        if let Some(agent) = fleet_boxes()
            .and_then(|boxes| boxes.into_iter().find(|b| b.name == name))
            .map(|b| b.agent)
            .filter(|a| valid_runtime(a))
        {
            return agent;
        }
    }
    if let Some(repo) = repo_for_box(name) {
        return launch_spec_agent(&repo, name)
            .or_else(|| (!repo.agent.is_empty()).then(|| repo.agent.clone()))
            .unwrap_or_else(default_agent);
    }
    default_agent()
}
