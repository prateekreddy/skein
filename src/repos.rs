//! The repos skein manages (`~/.skein/repos.json`) and everything derived from one.
//!
//! skein is not single-repo: each box is `<repo-id>-<branch>` and maps back to its repo by
//! id-prefix, longest id winning. This is skein's OWN registration of a repo — distinct from the
//! sbx registry of sandboxes — and it is what makes "add a repo URL and go" work without the repo
//! shipping anything for skein.

use crate::config::*;
use crate::kit::{ensure_kit, ensure_store};
use crate::registry::registry_entry_for_box;
use crate::runtime::*;
use crate::sbx::lookup_dir;
use crate::sbx::{fleet_boxes, git_branch_for};
use crate::tracking::{load_connections, plane_project_id};
use crate::util::valid_name;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One managed repo: a **mirror** on the volume, a **store** every box of it reads, and — only when
/// it was adopted from a local path — the **source tree** it was adopted from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub source: String, // git URL or local path the repo was added from
    /// The user's own checkout, for a repo adopted from a local path. **Empty for a repo
    /// registered from a URL**, and that emptiness is the point of the field's name.
    ///
    /// It was called `work` and it meant two different things depending on how the repo was
    /// registered: somebody's working tree in one case, and a second full checkout skein cloned
    /// onto the volume and then never worked in, in the other. Every reader had to know which it
    /// was holding, and none of them said so.
    ///
    /// A URL repo has no source tree because it does not need one. What skein asks a repo — what
    /// does it contain, what is its origin, what is its default branch — the mirror answers, and
    /// answers about the committed state rather than about whatever is checked out. The one
    /// question only a checkout can answer is what a project keeps **out** of git
    /// ([`crate::kit::seed_shared_paths`]), and a repo cloned from a URL has never had those files
    /// anywhere: they were never committed, so no clone of it ever carried them.
    #[serde(default, alias = "work")]
    pub source_tree: String,
    pub store: String, // host shared `.claude` store
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
    let repos = read_repos_uncached();
    *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((std::time::Instant::now(), repos.clone()));
    repos
}

/// The repo list straight off disk. For the writers, which must see what another writer just put
/// down rather than what this process read a moment ago.
fn read_repos_uncached() -> Vec<Repo> {
    fs::read_to_string(repos_json())
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Repo>>(&t).ok())
        .unwrap_or_default()
}

/// Persist the repo list to `~/.skein/repos.json` (pretty, atomic).
pub fn save_repos(repos: &[Repo]) -> Result<(), String> {
    crate::util::with_lock(&repos_lock(), || write_repos(repos))
}

/// Where the repo-list lock lives. Beside the file it guards.
fn repos_lock() -> std::path::PathBuf {
    skein_home().join(".repos.lock")
}

/// Write the repo list with the lock **already held**. Never call this without it.
fn write_repos(repos: &[Repo]) -> Result<(), String> {
    *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None; // mutation → drop the micro-cache
    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(repos).map_err(|e| e.to_string())?;
    write_atomic(&repos_json(), &home, &bytes)
}

/// Change the repo list: read, apply, write — all under one lock.
///
/// Worth more here than for the settings, because every mutation of this file is a read-modify-write
/// over the **whole list**. Losing one is not losing a field, it is losing a repository: adding one
/// while another tab renames a second, and the new repo is simply gone.
///
/// The read happens inside the lock. `f` may refuse by returning `Err`, and nothing is written then
/// — which is what `set_repo_settings` needs, since a half-applied update across two fields is
/// worse than a refusal.
pub fn update_repos<T>(f: impl FnOnce(&mut Vec<Repo>) -> Result<T, String>) -> Result<T, String> {
    crate::util::with_lock(&repos_lock(), || {
        // Straight off disk, not through the micro-cache: the cache exists to spare a per-tick read
        // and is exactly the wrong thing here, where the point is to see what another writer just
        // put down.
        let mut current = read_repos_uncached();
        let out = f(&mut current)?;
        write_repos(&current)?;
        Ok(out)
    })
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
    update_repos(|repos| {
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
        Ok(repo.clone())
    })
}

/// Unregister a repo from `repos.json` by id. Returns the removed `Repo`. Does NOT delete the working
/// clone or store on disk (they may hold unpushed work / a clone-mode box's only copy) — only skein's
/// registration is removed; report the paths so the user can delete them deliberately.
pub fn remove_repo(id: &str) -> Result<Repo, String> {
    update_repos(|repos| {
        let pos = repos
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| format!("no repo with id {id:?}"))?;
        Ok(repos.remove(pos))
    })
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

/// Where this repo lives **upstream** — the repository a box pushes to.
///
/// Deliberately not the mirror's `origin`, and the difference is the whole reason this function
/// exists. A mirror's origin is where the *mirror* fetches from, which for an adopted repo is the
/// checkout on this machine. That is the right answer to "where does the mirror get its commits"
/// and the wrong answer to "where does this repo live", and reading one for the other tells a box
/// to push into a path on the host.
///
/// So: the URL for a repo registered from one, and the checkout's own `origin` for a repo adopted
/// from a path. Being adopted says nothing about whether a repo has a remote — skein's own is
/// adopted in place and its origin is `git@github.com:owner/name`.
pub fn repo_origin_url(repo: &Repo) -> Option<String> {
    match repo.source_tree.trim() {
        "" => is_git_url(&repo.source).then(|| repo.source.trim().to_string()),
        tree => remote_origin_url(tree),
    }
}

/// The `origin` URL of the git directory at `dir`, if any. Works on a bare mirror and on a checkout.
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

/// A repo's **bare mirror** on the volume — `~/.skein/repos/<id>/mirror`.
///
/// The distinction this whole module now turns on: a *mirror* is a remote, and a *checkout* is
/// somebody's working tree. They were the same directory, and the checkout was the one boxes cloned
/// from, which made three separate things true at once and none of them on purpose:
///
///   * a box's clone inherited whatever branch happened to be checked out on the host;
///   * the host's own working tree had to be mounted into the sandbox for a box to clone at all,
///     so every box could read the tree its user works in;
///   * `~/.skein/repos/<id>/work` was a full second checkout on the volume for URL repos — the
///     volume carrying a working tree it never works in.
///
/// Bare, deliberately. A non-bare mirror has a checked-out branch that means nothing and clones
/// inherit, and it would *look* like it solved the gitignored-shared-paths problem below while
/// solving nothing: a clone of any shape carries tracked files only, so the files that block is for
/// are absent from a mirror however it is made. That source is the repo's checkout, it is a
/// different thing from this, and it is named separately now ([`crate::kit::record_repo_source`]).
pub fn mirror_path(id: &str) -> PathBuf {
    skein_home().join("repos").join(id).join("mirror")
}

/// Is there a mirror at `path` — a git repository rather than an empty or half-made directory?
///
/// `HEAD` and `objects/`, because a `git clone --mirror` interrupted partway leaves the directory
/// and some of its contents behind, and a mirror that exists but has no objects fails every clone
/// taken from it with a message about the *box*.
fn mirror_is_made(path: &Path) -> bool {
    path.join("HEAD").is_file() && path.join("objects").is_dir()
}

/// Make sure this repo has a mirror, cloning one if it has none. Returns its path.
///
/// Cloned from the **checkout**, not from the URL, even for a repo registered from a URL: the
/// checkout is already there and already fetched, so this is a local copy rather than a second trip
/// over the network. `origin` is then pointed at the real URL, so every later fetch goes where it
/// should.
///
/// Idempotent, and the reason it is a function rather than a step in [`add_repo`]: every repo
/// registered before mirrors existed has none, and the alternative to making one on demand is a
/// migration that has to run before anything else works.
pub fn ensure_mirror(repo: &Repo) -> Result<PathBuf, String> {
    let mirror = mirror_path(&repo.id);
    if mirror_is_made(&mirror) {
        return Ok(mirror);
    }
    // The checkout when there is one — already fetched, so this is a local copy rather than a
    // second trip over the network — and the URL when there is not.
    let from = match repo.source_tree.trim() {
        "" => repo.source.trim(),
        tree => tree,
    };
    if from.is_empty() {
        return Err(format!("{} has nothing to mirror from", repo.id));
    }
    // A half-made mirror from an interrupted clone: git refuses to clone into a non-empty directory,
    // so it would fail here for ever. Nothing in it is anybody's only copy — it is objects that
    // exist in the checkout it was made from.
    if mirror.exists() {
        fs::remove_dir_all(&mirror).map_err(|e| format!("clearing a half-made mirror: {e}"))?;
    }
    fs::create_dir_all(mirror.parent().unwrap()).map_err(|e| format!("mkdir: {e}"))?;
    let mut command = Command::new("git");
    command.args(["clone", "--mirror", from]).arg(&mirror);
    let out = bounded_output(&mut command, "git clone --mirror", Duration::from_secs(300))?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(&mirror);
        return Err(format!(
            "mirroring {from}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Point it at where the code really comes from. A URL repo's mirror fetches from the URL; an
    // adopted repo's fetches from the checkout it was made from, which is the only source there is.
    let origin = match is_git_url(&repo.source) {
        true => repo.source.trim().to_string(),
        false => from.to_string(),
    };
    let mut set = Command::new("git");
    set.arg("-C")
        .arg(&mirror)
        .args(["remote", "set-url", "origin", &origin]);
    let _ = bounded_output(&mut set, "git remote set-url", Duration::from_secs(10));
    Ok(mirror)
}

/// Fetch the mirror from its origin, pruning refs the origin no longer has.
///
/// `--prune` matters more here than in a checkout: a mirror keeps every branch, so without it a
/// branch deleted upstream a year ago is still offered to every box that clones from this.
///
/// Errors are returned rather than swallowed, and the callers decide. A box created while the
/// network is down should still be created — from a mirror that is a day old — and a `skein pull`
/// that could not reach the remote should say so.
pub fn fetch_mirror(repo: &Repo) -> Result<(), String> {
    let mirror = ensure_mirror(repo)?;
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&mirror)
        .args(["remote", "update", "--prune"]);
    let out = bounded_output(&mut command, "git remote update", Duration::from_secs(300))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!(
            "fetching {}: {}",
            repo.id,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// A repo's files, read out of its **mirror** rather than off somebody's disk.
///
/// Three host-side features — the diff, the module notes and CODEOWNERS — read `repo.source_tree`
/// directly, which is a working checkout: it has a branch somebody chose, edits nobody committed,
/// and in-fleet it is not reachable at all. What they actually want is "this repo, as committed",
/// and that is what a mirror holds.
///
/// `HEAD` is the ref, and it is the right one without asking anybody: a `--mirror` clone takes the
/// origin's `HEAD`, so this is the remote's own default branch rather than whatever the person at
/// the keyboard has checked out.
///
/// Reading through git rather than off disk changes one thing worth saying: uncommitted work is
/// invisible. For a note about what a module *is*, and for the ownership rules a repo has agreed
/// on, that is the answer that was wanted anyway.
pub struct Tree {
    mirror: PathBuf,
}

impl Tree {
    /// The repo's tree, or `None` when there is no mirror to read and one cannot be made.
    pub fn open(repo: &Repo) -> Option<Tree> {
        let mirror = ensure_mirror(repo)
            .map_err(|why| eprintln!("skein: reading {}: {why}", repo.id))
            .ok()?;
        // A mirror of a repository with no commits at all answers nothing, and every call below
        // would fail one at a time rather than once here.
        let probe = Tree {
            mirror: mirror.clone(),
        };
        probe.git(&["rev-parse", "--verify", "HEAD"])?;
        Some(Tree { mirror })
    }

    fn git(&self, args: &[&str]) -> Option<String> {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.mirror).args(args);
        let out = bounded_output(&mut command, "git", Duration::from_secs(30)).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// One file's contents, or `None` if the tree has no such file.
    pub fn read(&self, path: &str) -> Option<String> {
        self.git(&["show", &format!("HEAD:{path}")])
    }

    /// Every file under `prefix` (the whole tree when it is empty), repo-relative.
    pub fn files(&self, prefix: &str) -> Vec<String> {
        let mut args = vec!["ls-tree", "-r", "--name-only", "-z", "HEAD"];
        if !prefix.is_empty() {
            args.push("--");
            args.push(prefix);
        }
        // NUL-separated: a path with a newline in it is legal in git and would otherwise split into
        // two files that do not exist.
        self.git(&args)
            .unwrap_or_default()
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The directories directly under the tree's root.
    pub fn top_level_dirs(&self) -> Vec<String> {
        self.git(&["ls-tree", "--name-only", "-z", "-d", "HEAD"])
            .unwrap_or_default()
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(|p| p.trim_end_matches('/').to_string())
            .collect()
    }

    /// Is there a directory at this path?
    pub fn is_dir(&self, path: &str) -> bool {
        let path = path.trim_matches('/');
        !path.is_empty()
            && self
                .git(&["ls-tree", "-d", "--name-only", "HEAD", path])
                .map(|out| !out.trim().is_empty())
                .unwrap_or(false)
    }

    /// How many **lines of the tree mention** a symbol.
    ///
    /// Deliberately not "call sites", and the difference is the whole honesty of the number: `git
    /// grep` cannot tell a call from a comment, a string, an unrelated field of the same name, or
    /// the declaration itself. What it can say is how widely the word appears, which is a real
    /// measure of how far a change reaches and is not a claim about calls. §11.1's mock-up says
    /// "call sites"; this says mentions, because that is what can be derived rather than asserted.
    ///
    /// Whole words only (`-w`) and a fixed string (`-F`), so `check` does not match `checked` and a
    /// symbol containing regex punctuation is not a pattern. Short symbols are refused outright: a
    /// two-character name matches everything and the count would be noise wearing a number.
    ///
    /// `None` means *nothing was counted* — the tree could not be searched, or the symbol was not
    /// worth searching for. It never means zero. A caller must render the two differently: "0
    /// mentions" reads as "nothing uses this", which is the opposite of "we did not look".
    pub fn mentions(&self, symbol: &str) -> Option<usize> {
        let symbol = symbol.trim();
        if symbol.len() < 3 || symbol.chars().any(|c| c.is_whitespace()) {
            return None;
        }
        // Its own invocation rather than `git()`, because here a **failure exit is an answer**:
        // `git grep` exits 1 when it matched nothing, and `git()` reports that as "no output" —
        // indistinguishable from a tree it could not read. Anything above 1 is a real error.
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.mirror)
            .args(["grep", "-I", "-F", "-w", "-c", "-e", symbol, "HEAD"]);
        let out = bounded_output(&mut command, "git grep", Duration::from_secs(30)).ok()?;
        match out.status.code() {
            Some(0) => {}
            Some(1) => return Some(0),
            _ => return None,
        }
        // `HEAD:path/to/file:7` — the count is after the last colon, and a path may contain one.
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.rsplit(':').next()?.trim().parse::<usize>().ok())
                .sum(),
        )
    }

    /// The commit that last touched `path`, or empty when git cannot answer.
    pub fn last_commit(&self, path: &str) -> String {
        self.git(&["log", "-1", "--format=%H", "HEAD", "--", path])
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

/// A heads-up about a managed repo's push path, surfaced by `skein add` + the cockpit so it's known
/// up-front (not an error — both cases are workable). Two cases warn: a repo with **no `origin`
/// remote** (common when adopting a local folder never pushed) — a box can't push or open a PR until
/// one exists; and an **SSH `origin`** — in-box push then leans on the host SSH agent (sbx forwards
/// `SSH_AUTH_SOCK`), so it works only when that agent has the key loaded, else switch to HTTPS.
/// `None` for an HTTPS origin (the no-setup happy path; a URL clone always lands here).
pub fn remote_warning(repo: &Repo) -> Option<String> {
    // Where the advice is typed matters, so it names the place the person can actually change: the
    // checkout for an adopted repo, and the mirror for a URL repo — which has no checkout, and
    // whose origin is the URL it was registered with anyway.
    let where_to_fix = match repo.source_tree.trim() {
        "" => mirror_path(&repo.id).to_string_lossy().into_owned(),
        tree => tree.to_string(),
    };
    let work = &where_to_fix;
    let Some(url) = repo_origin_url(repo) else {
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

    // The source tree, and **only** for a repo adopted from a local path. A URL repo used to get a
    // second full checkout on the volume here, cloned from the network and then never worked in;
    // the mirror below is what a URL repo gets instead, and it is the thing boxes and skein both
    // read. See [`Repo::source_tree`].
    let source_tree = match is_git_url(source) {
        true => {
            // An SSH URL needs a key in the host agent for the clone the mirror is about to make.
            if is_ssh_url(source) {
                let _ = ensure_ssh_key();
            }
            PathBuf::new()
        }
        false => {
            let p = PathBuf::from(expand_tilde(source));
            let p = p.canonicalize().unwrap_or(p);
            if !p.join(".git").exists() {
                return Err(format!("{} is not a git repo", p.display()));
            }
            p
        }
    };

    ensure_kit()?;
    ensure_store(&store)?;
    let _ = ensure_gh_secret(); // best-effort; private clones/PRs need it, but absence isn't fatal

    let repo = Repo {
        id: id.clone(),
        source: source.to_string(),
        source_tree: source_tree.to_string_lossy().into_owned(),
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
    // Before registering it: a repo whose boxes cannot clone is a repo that looks added and does
    // not work, and the failure would surface later as a box that never starts. For an adopted repo
    // this is a local copy of the checkout above; for a URL repo it is the one clone that happens,
    // where there used to be two.
    ensure_mirror(&repo)?;
    update_repos(|repos| {
        repos.retain(|r| r.id != id); // replace any existing entry with the same id
        repos.push(repo.clone());
        repos.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(repo.clone())
    })
}

/// Bring a repo up to date: **the mirror always, and the checkout when there is one.**
///
/// The mirror is what every box clones from, so advancing it is the part that changes what a new box
/// starts with. That is the whole of the job for a repo registered from a URL, which has no checkout
/// on this machine at all.
///
/// For a repo adopted from a local path there is a second step, and it is somebody else's tree:
/// `git -C <source_tree> pull --ff-only`, fast-forward only on purpose, because skein never merges
/// or rebases on a person's behalf — a diverged or dirty tree fails loudly rather than being
/// silently rewritten. The mirror has already taken that tree's commits across by then, so the
/// failure costs the boxes nothing, and the message says so rather than reading as a failed pull.
///
/// An adopted repo with **no remote of its own** is not an error and used to be refused as one: the
/// mirror has just been updated from the checkout, which is everything a box needs.
pub fn pull_repo(id: &str) -> Result<String, String> {
    let repo = load_repos()
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no repo with id {id:?}"))?;
    // The mirror first, and it always has somewhere to fetch from: a URL repo's mirror fetches from
    // the URL, an adopted repo's from the checkout it was made from. For a URL repo that is the
    // whole of the job — there is no checkout, and the mirror is what every box clones from.
    fetch_mirror(&repo)?;
    let Some(tree) = Some(repo.source_tree.trim()).filter(|t| !t.is_empty()) else {
        return Ok("Mirror updated.".into());
    };
    // An adopted repo with no remote of its own is not an error, and it used to be refused as one.
    // The mirror has just taken its new commits across, which is the whole of what a box needs.
    if remote_origin_url(tree).is_none() {
        return Ok(format!(
            "Mirror updated from {tree}. That checkout has no `origin` remote, so there is nothing \
             further to pull into it."
        ));
    }
    // And the checkout, for an adopted repo — the user's own, which is why this is fast-forward
    // only: skein never merges or rebases on somebody's behalf, so a diverged or dirty tree fails
    // loudly rather than being silently rewritten.
    let mut command = Command::new("git");
    command.args(["-C", tree, "pull", "--ff-only"]);
    let out = bounded_output(&mut command, "git pull", Duration::from_secs(120))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(format!(
            "the mirror is current; your checkout at {tree} is not: {}",
            match err.is_empty() {
                true => "git pull failed (it may have diverged or have local changes)",
                false => err,
            }
        ));
    }
    let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(match summary.is_empty() {
        true => "Already up to date.".to_string(),
        false => summary,
    })
}

/// Seed the host's GitHub token into sbx globally so every box can fetch/push/open PRs:
/// `sbx secret set -g github -t "$(gh auth token)"`. Best-effort; skip with $SKEIN_NO_GH_SECRET.
/// Done once (global) rather than per-box, sidestepping the "box must exist first" timing.
///
/// Idempotent: sbx refuses to overwrite an existing secret without `-f`, so an already-seeded token
/// is treated as success (the boxes can already push) — not an error. Set $SKEIN_FORCE_GH_SECRET to
/// pass `-f` and refresh the token (e.g. after `gh auth refresh` / rotation).
///
/// **Note on the token's route.** It is passed to `sbx` as a command-line argument, so it is visible
/// in the host's process table for as long as that call runs. [`crate::gitgate::curl_config`] exists
/// precisely to keep the App JWT off a command line, and this is the same class of secret taking the
/// path that one was built to avoid. It is left as-is only because `sbx`'s interface is not skein's
/// to change and no stdin form of `secret set` is documented; the mitigation is below — once tokens
/// can be scoped, this credential stops being seeded at all.
pub fn ensure_gh_secret() -> Result<(), String> {
    let cfg = load_config();
    // env wins over the UI setting (headless/CI); either can disable seeding.
    if env::var_os("SKEIN_NO_GH_SECRET").is_some() || !cfg.seed_gh_secret {
        return Ok(());
    }
    // A fleet that can scope does not get the account token.
    //
    // These were two switches that had to be kept in step by hand: configuring an App scoped every
    // box, and the fleet-wide credential stayed seeded until someone remembered to turn this off
    // separately. Nothing reminded them. A box drops `GH_TOKEN` at startup, so the secret was
    // usually unused — but "usually unused" is not "gone", and it remained in sbx's store, reachable
    // by anything in the sandbox that does not come up through `box-session.sh`.
    //
    // Gated on the same question [`crate::gitgate::box_is_scoped`] asks, so the two cannot disagree:
    // the moment an App or a stored token exists, this stops. Forcing still works, for the fleet
    // that deliberately wants both.
    let force = env::var_os("SKEIN_FORCE_GH_SECRET").is_some() || cfg.force_gh_secret;
    if !force && crate::gitgate::can_issue_write_tokens() {
        return Ok(());
    }
    // Already seeded ⇒ nothing to do, and *nothing to ask*. This is the line that stops a password
    // dialog at every launch.
    //
    // `gh` keeps its token in the system keyring on a modern Linux, so `gh auth token` is a libsecret
    // call — and a locked login keyring answers it with "unlock your login keyring", which on Ubuntu
    // arrived once per `skein-server` start, for ever. What made it indefensible is that after the
    // first seed the answer was *discarded*: the token was fetched, handed to sbx, refused with
    // "already exists", and thrown away. The dialog bought nothing.
    //
    // So the fact is remembered rather than re-proven. Not a heuristic standing in for the truth —
    // sbx told us, and this is its answer written down. The secret is global to sbx rather than to a
    // sandbox, so rebuilding the fleet does not remove it and cannot invalidate this.
    //
    // What *can*: deleting the secret in sbx by hand, or reinstalling sbx. Both are recovered by the
    // same control that has always meant "seed it again" — **Overwrite token on startup**, or
    // `$SKEIN_FORCE_GH_SECRET` — which skips this check and rewrites the marker.
    let seeded = crate::config::skein_home().join("gh-secret-seeded");
    if !force && seeded.exists() {
        return Ok(());
    }
    // The environment first, because it costs nothing. A token already exported here is the same
    // credential `gh` would hand back, and asking `gh` for it would unlock a keyring to learn what
    // this process was already told. Headless and CI setups live here.
    let from_env = ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .filter_map(|k| env::var(k).ok())
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty());
    let token = match from_env {
        Some(t) => t,
        None => {
            let mut token_command = Command::new("gh");
            token_command.args(["auth", "token"]);
            let token =
                bounded_output(&mut token_command, "gh auth token", Duration::from_secs(15))?;
            if !token.status.success() {
                return Err("gh auth token failed (run `gh auth login` on the host)".into());
            }
            let token = String::from_utf8_lossy(&token.stdout).trim().to_string();
            if token.is_empty() {
                return Err("gh auth token was empty".into());
            }
            token
        }
    };
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
        remember_gh_secret(&seeded);
        return Ok(());
    }
    // Not forcing + the secret is already there → boxes can already push; that's success, not failure.
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !force && stderr.contains("already exists") {
        remember_gh_secret(&seeded);
        return Ok(());
    }
    Err(format!("sbx secret set failed: {}", stderr.trim()))
}

/// Write down that the sbx secret is in place, so the next start does not unlock a keyring to
/// rediscover it.
///
/// Deliberately best-effort and silent: failing to record this costs one dialog at the next launch,
/// while an error here would turn a fleet that is working perfectly into a startup complaint. It
/// holds no secret — only a timestamp, so `skein doctor` can say *when* rather than merely *that*.
fn remember_gh_secret(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, format!("{}\n", chrono::Utc::now().to_rfc3339()));
}

/// Has the sbx secret been seeded, as far as skein knows? For `skein doctor`, which reports this
/// rather than making the user infer it from a dialog that stopped appearing.
pub fn gh_secret_seeded() -> Option<String> {
    fs::read_to_string(crate::config::skein_home().join("gh-secret-seeded"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, tempdir};

    /// Two writers adding repos at once, and none of them vanishes.
    ///
    /// Worse here than for the settings: every mutation of this file is a read-modify-write over
    /// the whole list, so a lost update is a lost **repository** rather than a lost field. Adding
    /// one while another tab renames a second, and the new repo is simply not there — no error, no
    /// half-written file, just a registration that never happened.
    ///
    /// Counting, for the reason the settings test spells out: a version where each thread writes
    /// its own distinct thing passes against the unlocked code often enough to be useless.
    #[test]
    fn two_writers_adding_repos_lose_none_of_them() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_repos(&[]).unwrap();

        const EACH: usize = 60;
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let hands: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|who| {
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for n in 0..EACH {
                        update_repos(|repos| {
                            repos.push(Repo {
                                id: format!("{who}{n}"),
                                source: String::new(),
                                source_tree: String::new(),
                                store: String::new(),
                                agent: "claude".into(),
                                plane_project: String::new(),
                                sync_connection: String::new(),
                                review_queue: true,
                                sync_gateway_url: String::new(),
                            });
                            Ok(())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in hands {
            h.join().unwrap();
        }

        let end = read_repos_uncached();
        assert_eq!(
            end.len(),
            EACH * 2,
            "repositories were lost between two writers"
        );
    }

    /// The account token stops being seeded the moment the fleet can scope.
    ///
    /// These were two switches nobody kept in step: configuring an App scoped every box, and this
    /// went on copying a `repo`-scoped user token into the sandbox-wide secret until someone
    /// separately remembered to turn it off. Nothing reminded them, and the credential stayed in
    /// sbx's store — unused by a box that comes up through `box-session.sh`, and perfectly usable by
    /// anything that does not.
    ///
    /// Proven with a `gh` that always fails: with an issuer configured this must return `Ok` having
    /// run nothing at all, and without one it must try, and say why it could not.
    ///
    /// The stub is *prepended* to `$PATH` rather than replacing it. Cargo runs these as threads in
    /// one process, so `$PATH` is shared with every test running alongside — and blanking it broke
    /// an unrelated one that shells out to `sh`. `env_lock` serialises the tests that take it, which
    /// is no help at all to the ones that do not.
    #[test]
    fn a_fleet_that_can_scope_does_not_seed_the_account_token() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let root = home.as_ref() as &std::path::Path;
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("gh"), "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();

        let previous_path = env::var_os("PATH");
        env::set_var("SKEIN_HOME", root);
        env::set_var(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                previous_path.clone().unwrap_or_default().to_string_lossy()
            ),
        );
        env::remove_var("SKEIN_NO_GH_SECRET");
        env::remove_var("SKEIN_FORCE_GH_SECRET");

        // Nobody has chosen the account token, so nothing reaches for it. This is the property that
        // stops startup unlocking a keyring before the user has said which credential path they want.
        assert!(
            ensure_gh_secret().is_ok(),
            "unchosen must mean untouched: with seeding off, `gh` is never invoked"
        );

        // Chosen, and no issuer: now it tries, and fails on the missing `gh` rather than skipping —
        // because with nothing to scope with, the account token is all a box would have.
        let mut chose_it = crate::config::load_config();
        chose_it.seed_gh_secret = true;
        crate::config::save_config(&chose_it).unwrap();
        assert!(
            ensure_gh_secret().is_err(),
            "having picked the fleet-wide credential, seeding it must be attempted"
        );

        crate::gitgate::set_write_credential("mine", "one repo", &["a/one".into()]).unwrap();
        crate::gitgate::set_credential_token("mine", "github_pat_XYZ").unwrap();
        assert!(
            ensure_gh_secret().is_ok(),
            "a fleet that can issue scoped tokens must not copy the account token into the sandbox"
        );

        match previous_path {
            Some(p) => env::set_var("PATH", p),
            None => env::remove_var("PATH"),
        }
    }

    #[test]
    fn repo_id_and_url_detection() {
        assert_eq!(
            repo_id_from_source("https://github.com/acme/gadget-demo.git"),
            "gadget-demo"
        );
        assert_eq!(
            repo_id_from_source("git@github.com:org/My-Repo.git"),
            "My-Repo"
        );
        assert_eq!(repo_id_from_source("/Users/you/work/thing/"), "thing");
        assert!(is_git_url("https://github.com/x/y.git"));
        assert!(is_git_url("git@github.com:x/y.git"));
        assert!(is_git_url("ssh://git@host/x.git"));
        assert!(!is_git_url("/Users/you/work/thing"));
    }

    #[test]
    fn repo_for_box_matches_longest_id_prefix() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let repos = vec![
            Repo {
                id: "web".into(),
                source: "s".into(),
                source_tree: "/w".into(),
                store: "/s".into(),
                agent: "claude".into(),
                plane_project: String::new(),
                sync_connection: String::new(),
                review_queue: true,
                sync_gateway_url: String::new(),
            },
            Repo {
                id: "web-api".into(),
                source: "s".into(),
                source_tree: "/w".into(),
                store: "/s".into(),
                agent: "claude".into(),
                plane_project: String::new(),
                sync_connection: String::new(),
                review_queue: true,
                sync_gateway_url: String::new(),
            },
        ];
        save_repos(&repos).unwrap();
        // longest matching id wins, so "web-api-feat-x" is web-api/feat-x, not web/api-feat-x.
        let r = repo_for_box("web-api-feat-x").unwrap();
        assert_eq!(r.id, "web-api");
        assert_eq!(branch_from_box("web-api-feat-x", &r), "feat-x");
        let r2 = repo_for_box("web-login").unwrap();
        assert_eq!(r2.id, "web");
        assert_eq!(branch_from_box("web-login", &r2), "login");
        assert!(repo_for_box("other-x").is_none());
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn repin_branch_rewrites_launch_spec_without_relaunch() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("st").join(".claude");
        fs::create_dir_all(&store).unwrap();
        let repos = vec![Repo {
            id: "thing".into(),
            source: "s".into(),
            source_tree: "/w".into(),
            store: store.to_string_lossy().to_string(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }];
        save_repos(&repos).unwrap();
        // box created on the wrong branch (its creation branch)…
        write_launch_spec_for_agent("thing-feat-x", "feat-x", &repos[0], "claude").unwrap();
        assert_eq!(
            launch_spec_branch(&repos[0], "thing-feat-x").as_deref(),
            Some("feat-x")
        );
        // …re-pinned to the branch the agent actually moved to, without relaunching.
        repin_branch("thing-feat-x", "feat-y").unwrap();
        assert_eq!(
            launch_spec_branch(&repos[0], "thing-feat-x").as_deref(),
            Some("feat-y")
        );
        // unknown / unregistered box name errs rather than silently no-opping.
        assert!(repin_branch("no-such-box", "main").is_err());
        assert!(repin_branch("thing-feat-x", "").is_err());
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn ssh_url_detection_and_https_conversion() {
        assert!(is_ssh_url("git@github.com:org/repo.git"));
        assert!(is_ssh_url("ssh://git@github.com/org/repo.git"));
        assert!(!is_ssh_url("https://github.com/org/repo.git"));
        assert_eq!(
            ssh_to_https("git@github.com:org/repo.git").as_deref(),
            Some("https://github.com/org/repo.git")
        );
        assert_eq!(
            ssh_to_https("ssh://git@gitlab.com/org/repo.git").as_deref(),
            Some("https://gitlab.com/org/repo.git")
        );
        assert_eq!(host_of("git@github.com:org/repo.git"), Some("github.com"));
        assert_eq!(
            host_of("ssh://git@gitlab.com/org/repo.git"),
            Some("gitlab.com")
        );
    }

    #[test]
    fn slug_and_box_name_handle_slashes() {
        assert_eq!(slug("feat/auth"), "feat-auth");
        assert_eq!(slug("feat/auth/v2"), "feat-auth-v2");
        assert_eq!(slug("user@host~weird"), "user-host-weird");
        assert_eq!(slug("keep.dots_and-dashes"), "keep.dots_and-dashes");
        assert_eq!(slug("/leading/and/trailing/"), "leading-and-trailing");
        assert_eq!(box_name("thing", "feat/auth"), "thing-feat-auth");
    }

    #[test]
    fn a_repo_refuses_a_project_no_uuid_can_be_read_from() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            source_tree: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        assert!(set_repo_settings("web", Some("the backlog one"), None, None).is_err());
        assert_eq!(
            load_repos()[0].plane_project,
            "",
            "a refusal stores nothing"
        );
        // The URL is kept verbatim — the uuid is derived, so a board link stays possible.
        let url =
            "https://plane.example.net/acme/projects/1e2a3b4c-5d6e-4f70-8912-abcdefabcdef/issues";
        set_repo_settings("web", Some(url), None, None).unwrap();
        assert_eq!(load_repos()[0].plane_project, url);
        set_repo_settings("web", Some(""), None, None).unwrap();
        assert_eq!(load_repos()[0].plane_project, "", "empty clears it");
        env::remove_var("SKEIN_HOME");
    }

    /// Run git in `dir`, with an identity, and refuse to continue if it failed.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A git repository with one commit on `master`, at `dir`.
    fn origin_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-b", "master"]);
        fs::write(dir.join("tracked.txt"), "in git\n").unwrap();
        // The point of the mirror/checkout distinction, in one file: this is in the tree and never
        // in any clone taken from it.
        fs::write(dir.join(".gitignore"), "secret.env\n").unwrap();
        fs::write(dir.join("secret.env"), "KEY=1\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", "one"]);
    }

    /// A registered repo has a mirror, the mirror is bare, and it is what a box clones from.
    ///
    /// Bare is the part worth asserting. A non-bare mirror carries a checked-out branch that every
    /// clone inherits and a working tree that is nobody's — and it would read as the safer choice,
    /// because `$mirror/<path>` would then find tracked files and look like it had solved the
    /// gitignored-shared-paths problem it has not touched.
    #[test]
    fn a_repo_is_mirrored_and_the_mirror_is_what_a_box_clones_from() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        origin_repo(&checkout);
        let repo = add_repo(
            &checkout.to_string_lossy(),
            Some("proj"),
            Some("claude"),
            None,
        )
        .unwrap();

        let mirror = mirror_path("proj");
        assert!(mirror_is_made(&mirror), "no mirror at {}", mirror.display());
        assert!(
            !mirror.join(".git").exists(),
            "the mirror has a working tree, so it is a checkout wearing the name"
        );
        assert_eq!(git(&mirror, &["config", "--get", "core.bare"]), "true");
        assert_eq!(
            crate::fleet::clone_source(&repo),
            mirror.to_string_lossy(),
            "a box must clone from the mirror, not from anybody's checkout"
        );
        // And its origin is where the code really comes from, so a fetch goes to the right place.
        assert_eq!(
            remote_origin_url(&mirror.to_string_lossy()).unwrap(),
            checkout.to_string_lossy(),
            "an adopted repo mirrors the checkout it was adopted from"
        );

        // A clone of it carries the tracked file and, by construction, not the gitignored one.
        let clone = tempdir();
        let dst = clone.join("box");
        git(
            &clone,
            &["clone", &mirror.to_string_lossy(), &dst.to_string_lossy()],
        );
        assert_eq!(
            fs::read_to_string(dst.join("tracked.txt")).unwrap(),
            "in git\n"
        );
        assert!(
            !dst.join("secret.env").exists(),
            "a gitignored file cannot come out of a mirror, which is why the surfacing block reads \
             the repo's source tree instead"
        );

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The mirror advances when it is fetched, and not before.
    ///
    /// This is what replaced "every box clones from the host checkout, so the checkout's freshness
    /// is what a box starts from". The staleness did not disappear — it moved somewhere with a name
    /// and a command that advances it.
    #[test]
    fn the_mirror_is_as_fresh_as_its_last_fetch() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        origin_repo(&checkout);
        let repo = add_repo(
            &checkout.to_string_lossy(),
            Some("proj"),
            Some("claude"),
            None,
        )
        .unwrap();
        let mirror = mirror_path("proj");

        git(&checkout, &["branch", "release"]);
        let branches = |()| git(&mirror, &["branch", "--list", "release"]);
        assert_eq!(
            branches(()),
            "",
            "a mirror that had never fetched knew about a branch made after it"
        );
        fetch_mirror(&repo).unwrap();
        assert!(
            branches(()).contains("release"),
            "the fetch did not bring the new branch across"
        );

        // And a mirror deleted underneath skein is remade rather than reported.
        fs::remove_dir_all(&mirror).unwrap();
        assert!(!mirror_is_made(&mirror));
        ensure_mirror(&repo).unwrap();
        assert!(mirror_is_made(&mirror));

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A repo registered from a URL gets a mirror and no checkout.
    ///
    /// It used to get both: `git clone <url>` into `repos/<id>/work`, a full working tree on the
    /// volume that nothing in a box could see and skein only read for host-side conveniences —
    /// every one of which the mirror answers, and answers about the committed state rather than
    /// about whatever branch that clone happened to be left on.
    ///
    /// The URL here is a bare repository on disk, because a path ending in `.git` IS a git URL and
    /// git is content to clone one. That is the same code path a `https://` source takes, without
    /// a test that needs the network to pass.
    #[test]
    fn a_repo_added_from_a_url_gets_a_mirror_and_no_checkout() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let elsewhere = tempdir();
        let checkout = elsewhere.join("built-from");
        origin_repo(&checkout);
        let upstream = elsewhere.join("upstream.git");
        git(
            &elsewhere,
            &[
                "clone",
                "--bare",
                "-q",
                &checkout.to_string_lossy(),
                &upstream.to_string_lossy(),
            ],
        );
        assert!(
            is_git_url(&upstream.to_string_lossy()),
            "the fixture is not exercising the URL path"
        );

        let repo = add_repo(
            &upstream.to_string_lossy(),
            Some("proj"),
            Some("claude"),
            None,
        )
        .unwrap();

        assert_eq!(
            repo.source_tree, "",
            "a URL repo has no checkout on this machine"
        );
        assert!(
            !skein_home().join("repos/proj/work").exists(),
            "the volume still carries a working tree skein never works in"
        );
        let mirror = mirror_path("proj");
        assert!(mirror_is_made(&mirror));
        assert_eq!(git(&mirror, &["config", "--get", "core.bare"]), "true");
        assert_eq!(
            crate::fleet::clone_source(&repo),
            mirror.to_string_lossy(),
            "a box must still have somewhere to clone from"
        );
        // The mirror fetches from the URL, and that is also where a box pushes — for a URL repo the
        // two questions have the same answer, which is exactly why they had to be separated for the
        // adopted case below.
        assert_eq!(
            remote_origin_url(&mirror.to_string_lossy()).unwrap(),
            upstream.to_string_lossy()
        );
        assert_eq!(
            repo_origin_url(&repo).unwrap(),
            upstream.to_string_lossy(),
            "a box would push somewhere other than the repository it came from"
        );

        // A commit made upstream reaches the mirror, with no checkout in between.
        git(&checkout, &["checkout", "-q", "-b", "later"]);
        git(
            &elsewhere,
            &[
                "-C",
                &upstream.to_string_lossy(),
                "fetch",
                "-q",
                &checkout.to_string_lossy(),
                "later:later",
            ],
        );
        fetch_mirror(&repo).unwrap();
        assert!(git(&mirror, &["branch", "--list", "later"]).contains("later"));

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The two "where does this repo live" questions do not have the same answer for an adopted
    /// repo, and reading one for the other tells a box to push into a path on the host.
    ///
    /// The mirror's `origin` is where the MIRROR fetches from — the checkout, for an adopted repo.
    /// `repo_origin_url` is where the REPO lives, which is that checkout's own origin. skein's own
    /// repository is this case: adopted in place, with a GitHub remote.
    #[test]
    fn where_the_mirror_fetches_from_is_not_where_a_box_pushes() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        origin_repo(&checkout);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:acme/skein.git",
            ],
        );
        let repo = add_repo(
            &checkout.to_string_lossy(),
            Some("skein"),
            Some("claude"),
            None,
        )
        .unwrap();

        assert_eq!(
            remote_origin_url(&mirror_path("skein").to_string_lossy()).unwrap(),
            checkout.to_string_lossy(),
            "the mirror must fetch from the checkout it was made from"
        );
        assert_eq!(
            repo_origin_url(&repo).unwrap(),
            "git@github.com:acme/skein.git",
            "a box must push to the repository, not into skein's own mirror"
        );

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }
}
