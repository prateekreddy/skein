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
    /// The remote this repo lives at. **Always a git URL** — a local path is refused at
    /// registration (see [`add_repo`]), because skein runs inside the fleet sandbox and no checkout
    /// on the host is reachable from there.
    ///
    /// There was a `source_tree` beside this, the user's own checkout, kept for the one question a
    /// mirror cannot answer: what a project keeps OUT of git. In the fleet that answer was already
    /// unreachable — `seed_shared_paths` warned on every launch that the files had not arrived — so
    /// the field and its machinery are gone rather than left as a thing that only ever apologised.
    pub source: String,
    pub store: String, // host shared `.claude` store
    /// May skein read this repo's pull requests without being asked, with nobody watching?
    ///
    /// **Off unless it is switched on, per repo.** The owner's rule: "only ones where I mark the
    /// automatic reading enabled". Every other setting here describes how a repo is worked; this one
    /// decides whether skein spends model calls on it while nobody is looking, so a registry entry
    /// added for an unrelated reason cannot start costing money.
    #[serde(default)]
    pub read_prs: bool,
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
    ///
    /// **A repo registered now starts OFF** (see `add`), and the serde default stays TRUE on
    /// purpose: the two answer different questions. Absent from the file means the repo predates
    /// the field, when every queue was on — so reading it as off would switch off a queue somebody
    /// has been using, on upgrade, without being asked. What a new repo starts as is a choice about
    /// spending; what an old file means is a fact about the past.
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
///
/// Lossy on purpose, and only safe because every caller of it is a **reader**: a fleet whose
/// `repos.json` will not parse shows an empty board, which is wrong but is not destructive. The
/// writers ask [`read_repos_or_why`] instead — see [`update_repos`] for what happens when they do
/// not.
fn read_repos_uncached() -> Vec<Repo> {
    match read_repos_or_why() {
        Ok(repos) => repos,
        Err(why) => {
            // Once per process, for `load_config`'s reason: this is on the path of nearly every
            // request, and a line per call buries the one line that matters under thousands of
            // copies of itself.
            static TOLD: std::sync::Once = std::sync::Once::new();
            TOLD.call_once(|| {
                eprintln!(
                    "skein: cannot read your repo list ({why}) — every surface will show no repos \
                     until that file parses, and skein will refuse to write over it. Fix or move \
                     the file."
                );
            });
            Vec::new()
        }
    }
}

/// The repo list, or why it could not be read — the distinction the writers cannot do without.
///
/// **"Not there" and "there and unreadable" are different answers**, and reading the second as the
/// first is how one `skein add` deletes a fleet: `add_repo` loads the list, pushes one repo onto
/// what it was handed, and writes the result back. Handed an empty list for a `repos.json` that is
/// merely *unparseable*, that write replaces every other managed repo — store path, review-queue
/// setting, tracker connection — with a single new entry, and reports the add as successful
/// (SKEIN-347). The file is the only copy.
///
/// So a missing file is `Ok(no repos)`, which is what a fresh install genuinely is, and anything
/// else — a read error, a parse error, the zero-length file a crash between
/// [`crate::util::write_atomic`]'s write and its rename used to leave behind — is `Err`.
fn read_repos_or_why() -> Result<Vec<Repo>, String> {
    let path = repos_json();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // Nobody has added a repo yet. The one case where empty is the truth.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    serde_json::from_str::<Vec<Repo>>(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// Say whether skein may read this repo's pull requests unattended.
///
/// Its own function rather than a general "update this repo" one: this is the only field that
/// decides whether skein spends money on its own, and a route that could set it as a side effect of
/// editing something else is a route that turns it on by accident.
pub fn set_read_prs(id: &str, on: bool) -> Result<(), String> {
    let mut repos = load_repos();
    let Some(repo) = repos.iter_mut().find(|r| r.id == id) else {
        return Err(format!("no repo called {id:?}"));
    };
    repo.read_prs = on;
    save_repos(&repos)?;
    // The list is cached for a second; without this, switching reading on and then asking what is
    // on reports the old answer, which reads as the switch not working.
    *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(())
}

/// Persist the repo list to `~/.skein/repos.json` (pretty, atomic).
///
/// **Refuses over a file skein cannot read**, exactly as `config::save_config` does and for the
/// same reason one line up from it: the caller has just been handed an empty list by
/// [`load_repos`] for a file that is unparseable rather than absent, so writing that list back
/// replaces every repo in it with nothing. `set_read_prs` is a load-modify-save over the whole
/// list, and one dropdown would have been enough.
pub fn save_repos(repos: &[Repo]) -> Result<(), String> {
    crate::util::with_lock(&repos_lock(), || {
        read_repos_or_why().map_err(unreadable_refusal)?;
        write_repos(repos)
    })
}

/// What skein says instead of destroying the repo list, in both places that could.
///
/// One wording rather than two, because the two paths differ only in which call is about to lose
/// the file: what the person needs to hear is the same either way, and the sentence that mattered
/// in `config::save_config` is the one naming what a save would cost.
fn unreadable_refusal(why: String) -> String {
    format!(
        "not saving over a repo list skein cannot read ({why}). Saving now would replace every \
         repo in that file — store paths, review-queue settings, tracker connections — with a \
         default nobody chose, and that file is the only copy. Fix or move it, then try again."
    )
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
///
/// **And the read itself may refuse.** A lost update was the failure this function was written for;
/// the larger one is the same sentence with a different subject. `add_repo` pushes one repo onto
/// what the read handed it, so a read that answers "no repos" for a `repos.json` it merely could
/// not *parse* turns one add into a delete of every other repo — and returns Ok (SKEIN-347). The
/// window is not hypothetical: [`crate::util::write_atomic`] had no fsync, and a crash after its
/// rename leaves a zero-length file, which is unparseable.
pub fn update_repos<T>(f: impl FnOnce(&mut Vec<Repo>) -> Result<T, String>) -> Result<T, String> {
    crate::util::with_lock(&repos_lock(), || {
        // Straight off disk, not through the micro-cache: the cache exists to spare a per-tick read
        // and is exactly the wrong thing here, where the point is to see what another writer just
        // put down. And `_or_why`, not `_uncached`: a writer is the one caller that may not read
        // "unreadable" as "empty".
        let mut current = read_repos_or_why().map_err(unreadable_refusal)?;
        let out = f(&mut current)?;
        write_repos(&current)?;
        Ok(out)
    })
}

/// Record that a repository was renamed on GitHub, rewriting the name wherever skein spells it.
///
/// A repository NAME is not an identifier, and skein stored one as if it were. Everything downstream
/// is keyed on it — the review queue's searches, `gitgate`'s per-repo write credentials, what a box
/// is allowed to push to — so following the rename in one place and not the others would trade a
/// silent empty queue for a silent credential scoped to a repository nobody can name any more.
///
/// **The id is untouched, deliberately.** `Repo::id` is what box names, box roots, store paths and
/// placement records are built from; renaming it would rename running boxes and orphan their state,
/// for a change GitHub made to a label. `source` is the URL, and the URL is the thing that moved.
///
/// Substring replacement on the URL rather than reconstruction, because skein does not own the
/// shape: `git@github.com:o/n.git`, `https://github.com/o/n`, and `https://github.com/o/n.git` are
/// all in use, and rebuilding one would quietly change a repo's transport along with its name.
pub fn follow_rename(id: &str, was: &str, now: &str) -> Result<(), String> {
    if was.eq_ignore_ascii_case(now) {
        return Ok(());
    }
    if now.split('/').count() != 2 || now.split('/').any(|part| part.is_empty()) {
        return Err(format!("{now:?} is not an owner/name"));
    }
    update_repos(|repos| {
        let repo = repos
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no repo with id {id:?}"))?;
        if repo.source.contains(was) {
            repo.source = repo.source.replace(was, now);
        }
        Ok(())
    })?;
    // The mirror's origin too, so a fetch stops relying on GitHub's redirect. Best-effort: the name
    // above is what the queue needed, and a mirror that keeps working through the redirect is not a
    // reason to fail the rename that just fixed it.
    let _ = repoint_mirror(id);
    eprintln!("skein: {was} is now {now}; recorded");
    Ok(())
}

/// Point a mirror's `origin` at the URL now recorded for the repo — which `follow_rename` has
/// just rewritten, so this reads it back rather than being told the name twice.
fn repoint_mirror(id: &str) -> Result<(), String> {
    let Some(repo) = load_repos().into_iter().find(|r| r.id == id) else {
        return Err(format!("no repo with id {id:?}"));
    };
    let mirror = mirror_path(id);
    if !mirror.exists() {
        return Ok(());
    }
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&mirror)
        .args(["remote", "set-url", "origin", &repo.source]);
    let out = bounded_output(&mut command, "git remote set-url", Duration::from_secs(20))
        .map_err(|e| e.to_string())?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
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
///
/// **The checkout is asked last, and that ordering is the fix for SKEIN-468.** It used to be asked
/// *first* whenever `source_tree` was non-empty, which was right on a host and is wrong in the
/// fleet, where the checkout is the one thing that is never there: `git -C <missing dir>` exits 128,
/// this returned `None`, and `None` here is not a quiet degradation. [`crate::fleet::clone_script`]
/// emits no `git remote set-url origin` for an empty upstream, so the box's `origin` stayed the bare
/// mirror — which does not even refuse a push, it accepts it into a repository nobody pulls from —
/// and [`crate::gitgate::repo_slug`] found no slug, so that box got no write token either. Four of
/// nine repos on the live fleet were in exactly that state.
///
/// The mirror sits between them because it is often the only place the real answer survives: a
/// repo whose `source` is a host path has had its mirror repointed at the remote it actually
/// fetches from, and that URL is on the volume where this can read it. It is taken **only when it
/// is a git URL**, which is what keeps the paragraph above true — for an adopted repo whose mirror
/// still points at the checkout it was cloned from, the mirror's `origin` is a path, this skips it,
/// and the checkout's own `origin` answers as before.
pub fn repo_origin_url(repo: &Repo) -> Option<String> {
    if is_git_url(&repo.source) {
        return Some(repo.source.trim().to_string());
    }
    let mirror = mirror_path(&repo.id);
    // Read, never made: this is a question about a repo, and a caller asking it has not asked for a
    // 300-second clone. A repo with no mirror yet still has its checkout to answer from.
    if mirror_is_made(&mirror) {
        if let Some(url) =
            remote_origin_url(&mirror.to_string_lossy()).filter(|url| is_git_url(url))
        {
            return Some(url);
        }
    }
    // There was a third source here — the user's own checkout — and it is gone with local-path
    // repos. The two above are the whole answer now, and both are things skein owns.
    None
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
    // **One clone between all callers, or neither of them gets one.**
    //
    // Two readers that both find no mirror both ran `git clone --mirror` into the same directory,
    // and git refuses the second — `fatal: cannot copy .../templates/description: File exists` —
    // so BOTH failed and the repo was left with no mirror at all. That is not a transient: every
    // caller reads `None` as "this repo has nothing to say" rather than "I could not look", so the
    // PR brief silently stopped attributing ownership and the notes pane offered no modules. And
    // `review::summarise` caches its answer against the head sha, so one race froze a wrong summary
    // for the life of that commit. Found by `tests/ui/review.mjs`, which read as flaky because a
    // later call, alone, made the mirror and the checks after it passed.
    //
    // The half-made-mirror repair below made it sharper rather than safer: on a second attempt one
    // caller `remove_dir_all`s the directory another is mid-clone into.
    //
    // The lock file sits BESIDE the mirror, not in it, because the repair deletes the directory.
    let guard = mirror
        .parent()
        .ok_or_else(|| format!("{} has no parent to lock", mirror.display()))?
        .join(".mirror.lock");
    crate::util::with_lock(&guard, || clone_mirror(repo, &mirror))
}

/// The clone itself, with the lock in [`ensure_mirror`] already held.
fn clone_mirror(repo: &Repo, mirror: &Path) -> Result<PathBuf, String> {
    // Asked again under the lock. The caller that held it may have been making exactly this mirror,
    // and cloning over a finished one is the collision this function exists to stop.
    if mirror_is_made(mirror) {
        return Ok(mirror.to_path_buf());
    }
    // The remote, and only ever the remote. This used to prefer an adopted repo's checkout because
    // cloning from it was a local copy rather than a trip over the network; there is no checkout to
    // prefer any more, and `source` is a URL by construction (see [`add_repo`]).
    let from = repo.source.trim();
    if from.is_empty() {
        return Err(format!("{} has nothing to mirror from", repo.id));
    }
    // A half-made mirror from an interrupted clone: git refuses to clone into a non-empty directory,
    // so it would fail here for ever. Nothing in it is anybody's only copy — it is objects that
    // exist in the checkout it was made from.
    if mirror.exists() {
        fs::remove_dir_all(mirror).map_err(|e| format!("clearing a half-made mirror: {e}"))?;
    }
    fs::create_dir_all(mirror.parent().unwrap()).map_err(|e| format!("mkdir: {e}"))?;
    let mut command = Command::new("git");
    command.args(["clone", "--mirror", from]).arg(mirror);
    let out = bounded_output(&mut command, "git clone --mirror", Duration::from_secs(300))?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(mirror);
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
        .arg(mirror)
        .args(["remote", "set-url", "origin", &origin]);
    let _ = bounded_output(&mut set, "git remote set-url", Duration::from_secs(10));
    // **Pack it, once, before any box ever clones from it** (SKEIN-406).
    //
    // `from` is usually a working CHECKOUT, and cloning over git's local transport copies that
    // checkout's object store as it stands — loose objects and all. A checkout somebody has been
    // committing in for months carries thousands of them, and every box that clones this mirror
    // then copies every one, across the sandbox boundary, for ever.
    //
    // Measured on this repository (2026-08-26): the mirror arrived at 88 MB — 4,394 loose objects
    // holding 85 MB of it, against 1 MB actually in a pack. One `git gc` took 1.4s and left 6 MB.
    // A clone from it went from 154 ms and 88 MB of `.git` to 36 ms and 6 MB: four times faster,
    // 93% less data, and 82 MB off the volume permanently.
    //
    // **This is the fix instead of `--shared` or `--reference`, and the measurement is why.** On a
    // packed mirror `--shared` saves nothing at all (36 ms either way) while making every box's
    // objects depend on this directory surviving — and `ensure_mirror` DELETES it above to repair a
    // half-made clone. `--reference … --dissociate` measured 68 ms, nearly twice a plain clone.
    // Neither risk buys anything once the loose objects are gone.
    //
    // Best-effort: a mirror that could not be packed is a slower mirror, not a broken one, and
    // failing the clone over housekeeping would turn a working repo into no repo.
    let mut pack = Command::new("git");
    pack.arg("-C").arg(mirror).args(["gc", "--quiet"]);
    let _ = bounded_output(&mut pack, "git gc", Duration::from_secs(300));
    Ok(mirror.to_path_buf())
}

/// Fetch the mirror from its origin, pruning **branches and tags** the origin no longer has.
///
/// `--prune` matters more here than in a checkout: a mirror keeps every branch, so without it a
/// branch deleted upstream a year ago is still offered to every box that clones from this.
///
/// **What prune may reach is the whole of SKEIN-466, and it is spelled out on the command line
/// rather than left to the mirror's config.** A `git clone --mirror` configures `+refs/*:refs/*`,
/// and `git remote update --prune` under that refspec deletes *every* ref the origin lacks — which
/// for a mirror repointed from a checkout at a real remote is `refs/sandboxes/*`, `refs/stash` and
/// `refs/remotes/*`, none of which any origin carries. That is not hypothetical: repointing skein's
/// own mirror on 2026-08-28 deleted four refs, and `refs/stash` (`5c3a2fe`, a WIP from 2026-08-05)
/// was reachable from nothing else. `gc --auto` runs below, so those objects were on a countdown
/// rather than merely unreferenced.
///
/// Refspecs given here decide what prune considers, so the narrow pair keeps the reason `--prune`
/// is here — a branch genuinely deleted upstream still goes, and so does a deleted tag — while a
/// ref outside `refs/heads/` and `refs/tags/` is no longer prune's to delete. Verified against git
/// directly before the change: with these refspecs a deleted branch and a deleted tag are pruned
/// and `refs/sandboxes/x`, `refs/stash` and `refs/remotes/foo/bar` survive; with
/// `remote update --prune` the same three are deleted.
///
/// Errors are returned rather than swallowed, and the callers decide. A box created while the
/// network is down should still be created — from a mirror that is a day old — and a `skein pull`
/// that could not reach the remote should say so.
pub fn fetch_mirror(repo: &Repo) -> Result<(), String> {
    let mirror = ensure_mirror(repo)?;
    let mut command = Command::new("git");
    command.arg("-C").arg(&mirror).args([
        "fetch",
        "--prune",
        "origin",
        "+refs/heads/*:refs/heads/*",
        "+refs/tags/*:refs/tags/*",
    ]);
    let out = bounded_output(&mut command, "git fetch --prune", Duration::from_secs(300))?;
    if !out.status.success() {
        return Err(format!(
            "fetching {}: {}",
            repo.id,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Keep it packed as it grows (SKEIN-406). `--auto` rather than a plain `gc`: it is a no-op
    // below git's own threshold, so an idle repo pays nothing and a busy one is tidied without
    // skein deciding how often that should be. The one-off cost is at creation, above, where the
    // mirror inherits a checkout's whole loose object store in a single copy.
    //
    // After the fetch and not before, and never in a way that can fail it: what a caller asked for
    // is the new commits, and a repo that could not be packed still has them.
    let mut pack = Command::new("git");
    pack.arg("-C")
        .arg(&mirror)
        .args(["gc", "--auto", "--quiet"]);
    let _ = bounded_output(&mut pack, "git gc --auto", Duration::from_secs(120));
    Ok(())
}

/// Bring ONE pull request's head into the mirror — the ref `fetch_mirror` deliberately does not ask
/// for.
///
/// **A pull request from a fork is in no `refs/heads/*` of the base repository.** Its commits live
/// in the contributor's own repository, and the only place the base repo can serve them from is
/// `refs/pull/<n>/head`, which GitHub maintains and [`fetch_mirror`]'s refspec —
/// `+refs/heads/*` and `+refs/tags/*` — does not fetch. So a fork's pull request could not be stood
/// up at all: `review::stand_the_change_up` checked the head out, failed, fetched, failed again,
/// and answered "nothing here". Correct rather than wrong — a reviewer handed the base branch and
/// told it is the change is the worst outcome that path has — but it meant the reviewer read a
/// diff where it could have read a tree.
///
/// **One ref, on demand, and never in the mirror's own refspec.** Adding `+refs/pull/*` to
/// [`fetch_mirror`] would drag every pull request ever opened into every repo's mirror on every
/// fetch, for ever; the owner's `gadget-demo` alone is past 700. This asks for the one pull
/// request something is about to read, and only when its head is not already reachable — which for
/// a same-repo pull request it always is, because that branch IS in `refs/heads/*`.
///
/// Fetched into the mirror rather than into the reader's checkout on purpose: the mirror is the one
/// place in skein that talks to the remote and the one place its credentials are arranged, so a
/// second door onto GitHub would be a second thing to authenticate and to get wrong. The checkout
/// then takes it from the mirror, which is the hop it already makes for everything else.
pub fn fetch_pull_head(repo: &Repo, number: u64) -> Result<String, String> {
    let mirror = ensure_mirror(repo)?;
    let refspec = pull_head_ref(number);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&mirror)
        .args(["fetch", "--quiet", "origin"])
        .arg(format!("+{refspec}:{refspec}"));
    let out = bounded_output(
        &mut command,
        "git fetch pull head",
        Duration::from_secs(300),
    )?;
    if !out.status.success() {
        return Err(format!(
            "fetching {} of {}: {}",
            refspec,
            repo.id,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(refspec)
}

/// Where GitHub keeps a pull request's head, whoever opened it.
///
/// Its own function because two files name this ref and a ref spelled two ways is a ref that works
/// in one of them: the mirror fetches it here, and the reader fetches the same string out of the
/// mirror.
pub fn pull_head_ref(number: u64) -> String {
    format!("refs/pull/{number}/head")
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
    /// The repo's tree, or `None` when it could not be read — with the reason kept only in the
    /// server log.
    ///
    /// A convenience for callers whose behaviour is already absence-shaped: they do less, safely,
    /// whichever of the two `None` means. A caller that must *tell* "this repo has nothing" from
    /// "I could not look" reads [`Tree::open_telling`] instead — those are different sentences on
    /// screen, and only one of them may be cached (SKEIN-117).
    pub fn open(repo: &Repo) -> Option<Tree> {
        Tree::open_telling(repo)
            .map_err(|why| eprintln!("skein: reading {}: {why}", repo.id))
            .ok()
    }

    /// The repo's tree, or the reason it could not be read.
    ///
    /// [`crate::health::Level`]'s register, applied to reading a repo. `Ok` is *checked*: the
    /// mirror answered, so an absence found through it — no CODEOWNERS, no such file, no modules
    /// — is the repo's own answer and may be acted on and remembered. `Err` is *could not be
    /// checked*: nothing about the repo was learned, and the honest response to "I could not
    /// tell" is to say so and wait — render it as "skein could not read this repo", never as
    /// "nothing there", and never store a decision made while blind. `Unknown` may never drive a
    /// doer.
    pub fn open_telling(repo: &Repo) -> Result<Tree, String> {
        let mirror = ensure_mirror(repo)?;
        // A mirror whose HEAD resolves to nothing answers nothing, and every call below would
        // fail one at a time rather than once here. An empty repository and a mirror that did
        // not survive its clone both land in this probe, and neither can support "the repo has
        // no X" — so both read as could-not-read, the direction that widens attention rather
        // than narrowing it.
        let tree = Tree { mirror };
        if tree.git(&["rev-parse", "--verify", "HEAD"]).is_none() {
            return Err(format!(
                "the mirror at {} has no readable HEAD — an empty repository, or a mirror that \
                 did not survive its clone",
                tree.mirror.display()
            ));
        }
        Ok(tree)
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
    //
    // A recorded checkout that is not *there* is neither (SKEIN-472). It read as the first case and
    // told the owner to run `git remote add origin` in a directory the fleet cannot open, which is
    // advice that cannot be followed and hides the one place that can be: the mirror.
    let where_to_fix = mirror_path(&repo.id).to_string_lossy().into_owned();
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

    // **A repo is a remote, and a path is not one.** Adopting a local checkout is no longer
    // supported: skein runs inside the fleet sandbox, where no host checkout is reachable at all, so
    // a path-registered repo could not be fetched, could not seed the gitignored files that were its
    // only remaining reason to exist, and differed from a URL repo in nothing a box could observe.
    //
    // Refused rather than resolved. Reading `git remote get-url origin` out of the directory and
    // registering THAT would be skein silently substituting something for what a person typed — and
    // a `source` that disagreed with what its mirror fetches is exactly the state that took a repo's
    // fetch down while its clones went on working, invisibly, until somebody looked.
    if !is_git_url(source) {
        return Err(format!(
            "{source} is a path, and skein registers repos by remote. skein runs inside the fleet \
             sandbox and cannot reach a checkout on your machine, so a path-registered repo has \
             nothing to fetch from.\n  Give the remote instead — `git -C {source} remote get-url \
             origin` prints it."
        ));
    }
    // An SSH URL needs a key in the host agent for the clone the mirror is about to make.
    if is_ssh_url(source) {
        let _ = ensure_ssh_key();
    }

    ensure_kit()?;
    ensure_store(&store)?;
    let _ = ensure_gh_secret(); // best-effort; private clones/PRs need it, but absence isn't fatal

    let repo = Repo {
        id: id.clone(),
        source: source.to_string(),
        store: store.to_string_lossy().into_owned(),
        // A repo skein has just been told about reads nothing on its own until somebody says so.
        read_prs: false,
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
        // **Off for a repo skein has just met.** Every repo with the queue on costs one batched
        // GraphQL request per refresh, five membership searches inside it, on the badge's cadence —
        // and a fleet of eight repos spends all of that to answer a question the owner asked about
        // one. Measured, not supposed: the owner's fleet had eight on, exceeded GitHub's rate limit
        // for their user, and the queue they actually watch came back empty because of it.
        //
        // On was the right default for the first repo anybody registers and wrong by the third, and
        // the cost of the two mistakes is not symmetric: a queue switched off is one dropdown away
        // and says so on the repo's own row, while a queue switched on quietly spends somebody's
        // rate limit on pull requests that are none of their business.
        //
        // Note what this does NOT change: an existing `repos.json` that never wrote the field keeps
        // reading as ON (see the field's serde default). A fleet that has been working must not
        // have its queue turned off by an upgrade — that would be skein deciding, silently, that
        // the thing you were watching yesterday is not worth watching today.
        review_queue: false,
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
    // That is the whole of the job. There used to be a second half here — fast-forward the user's
    // own checkout — which only ever applied to an adopted repo and could only run on a host. The
    // mirror is what every box clones from, and updating it is what `pull` means.
    Ok("Mirror updated.".into())
}

/// The token the host's `gh` is logged in with, or `None` if it has none.
///
/// Extracted so it is not only the seeding's. `ensure_gh_secret` reads this to put a credential in
/// front of every box — so a fleet whose boxes push as you has, by construction, a `gh` login on the
/// host that can also answer "who are you" to GitHub. The review queue asked for a token four
/// separate ways and not this one, and then reported "nothing here names a user" on a machine where
/// skein had just used that very login to seed the fleet.
///
/// **Bounded, and worth being last.** `gh` keeps its token in the system keyring on a modern Linux,
/// so this can unlock one — which is exactly why startup skips it once the secret is seeded. Every
/// caller should try the sources that cost nothing first and reach this only when they would
/// otherwise have no credential at all.
pub fn gh_cli_token() -> Option<String> {
    let mut command = Command::new("gh");
    command.args(["auth", "token"]);
    let out = bounded_output(&mut command, "gh auth token", Duration::from_secs(15)).ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
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
    // In-fleet the seeding cannot be done, and saying so is the whole of what this arm is for.
    //
    // **After every "nothing to do" above it**, which is where it was not when SKEIN-104 wrote it.
    // A fleet seeded on the host before the move carries `gh-secret-seeded` across, the secret is
    // already in sbx's store, and its boxes push perfectly well — refusing there would report a
    // credential problem to a fleet that has none. The same for a fleet that scopes, and for one
    // that turned seeding off. The refusal belongs where work would actually start.
    //
    // Both halves are the host's: `gh auth token` reads the host's login, and `sbx secret set`
    // writes the host's keyring — `skein doctor` already says only one of the three credential
    // sources can reach it. Neither exists in the sandbox. Left as a refusal rather than a silent
    // `Ok(())`, because seeding is how boxes get a credential at all: succeeding quietly here means
    // discovering it as a 403 inside a box some minutes later, which is the shape
    // `docs/delivery.md` §5 is a list of.
    if crate::deployment::in_fleet() {
        return Err(
            "the fleet's GitHub secret is seeded from the host: `gh auth token` reads the host's \
             login and `sbx secret set` writes the host's keyring, and neither is reachable from \
             inside the sandbox. Seed it from a skein on the host, scope credentials per repo \
             instead (Settings → GitHub & keys), or set SKEIN_NO_GH_SECRET=1 if this fleet gets its \
             credentials another way"
                .into(),
        );
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

    /// **A repo is a remote, and a path is refused rather than resolved.**
    ///
    /// skein runs inside the fleet sandbox, where no checkout on the host is reachable — so a
    /// path-registered repo has nothing to fetch from, cannot seed the gitignored files that were
    /// its last remaining purpose, and differs from a URL repo in nothing a box can observe.
    ///
    /// Refused and NOT resolved. Reading `git remote get-url origin` out of the directory and
    /// registering that would be skein silently substituting something for what a person typed, and
    /// a `source` disagreeing with what its mirror fetches is the exact state that took a repo's
    /// fetch down while its clones went on working — invisible until somebody looked (2026-08-30).
    ///
    /// The message has to carry the way forward, or it is a refusal a person cannot act on.
    #[test]
    fn a_repo_registered_from_a_path_is_refused_and_told_what_to_pass_instead() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");
        let checkout = tempdir();
        origin_repo(&checkout);

        let why = add_repo(
            &checkout.to_string_lossy(),
            Some("proj"),
            Some("claude"),
            None,
        )
        .expect_err("a path must not register");
        assert!(
            why.contains("registers repos by remote") && why.contains("remote get-url origin"),
            "the refusal does not say what to pass instead, so it cannot be acted on: {why}"
        );
        assert!(
            load_repos().is_empty(),
            "the repo was refused and registered anyway"
        );

        // Non-vacuity, WITHOUT touching the network: a URL gets past the path check and fails
        // later, at the clone. What matters is which check rejected it — a bare `is_err()` here
        // would pass just as well if `add_repo` refused everything.
        let later = add_repo(
            "https://github.com/acme/thing.git",
            Some("thing"),
            Some("claude"),
            None,
        )
        .expect_err("no such repository exists to clone");
        assert!(
            !later.contains("registers repos by remote"),
            "a URL was rejected by the path check, so the refusal above proves nothing: {later}"
        );

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }

    /// Register a repo whose upstream is a directory on this disk.
    ///
    /// [`add_repo`] refuses a path — a repo is a remote now — and these tests are about the MIRROR,
    /// which needs an upstream that exists without reaching the network. So they write the record
    /// and make the mirror directly, which is all `add_repo` did for them anyway.
    fn registered(id: &str, from: &Path) -> Repo {
        let store = skein_home().join("repos").join(id).join("store/.claude");
        let repo: Repo = serde_json::from_value(serde_json::json!({
            "id": id,
            "source": from.to_string_lossy(),
            "store": store.to_string_lossy(),
            "agent": "claude",
        }))
        .unwrap();
        crate::kit::ensure_store(&store).unwrap();
        ensure_mirror(&repo).unwrap();
        update_repos(|repos| {
            repos.retain(|r| r.id != id);
            repos.push(repo.clone());
            Ok(repo.clone())
        })
        .unwrap()
    }

    /// A repo registered now does not start polling GitHub, and an older file that never wrote the
    /// field keeps the queue it has been running with.
    ///
    /// The two are deliberately different answers, and conflating them is how an upgrade turns off
    /// something somebody was watching: what a NEW repo starts as is a choice about spending, and
    /// what an ABSENT field means is a fact about a file written when every queue was on.
    #[test]
    fn a_new_repo_starts_without_a_queue_and_an_old_file_keeps_its_own() {
        let old: Repo = serde_json::from_value(serde_json::json!({
            "id": "written-before-the-field",
            "source": "https://github.com/acme/thing.git",
            "source_tree": "",
            "store": "",
        }))
        .unwrap();
        assert!(
            old.review_queue,
            "an upgrade switched off a queue that had been running, without asking"
        );

        let explicit: Repo = serde_json::from_value(serde_json::json!({
            "id": "said-so",
            "source": "https://github.com/acme/thing.git",
            "source_tree": "",
            "store": "",
            "review_queue": false,
        }))
        .unwrap();
        assert!(!explicit.review_queue, "a deliberate off was not honoured");
    }
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
                                read_prs: false,
                                id: format!("{who}{n}"),
                                source: String::new(),
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
                read_prs: false,
                id: "web".into(),
                source: "s".into(),
                store: "/s".into(),
                agent: "claude".into(),
                plane_project: String::new(),
                sync_connection: String::new(),
                review_queue: true,
                sync_gateway_url: String::new(),
            },
            Repo {
                read_prs: false,
                id: "web-api".into(),
                source: "s".into(),
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
            read_prs: false,
            id: "thing".into(),
            source: "s".into(),
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
            read_prs: false,
            id: "web".into(),
            source: "/src/web".into(),
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

    /// **The mirror a box clones from is PACKED** (SKEIN-406), and that is the whole of why a box
    /// creation stopped copying a repository's history byte by byte.
    ///
    /// A mirror is made with `git clone --mirror` from a working CHECKOUT, over git's local
    /// transport, which copies that checkout's object store exactly as it stands — loose objects
    /// and all. A checkout somebody has been committing in for months carries thousands, and every
    /// box that clones the mirror then copies every one of them across the sandbox boundary.
    ///
    /// Measured on this repository (2026-08-26): the mirror arrived at 88 MB, 4,394 loose objects
    /// holding 85 MB of it against 1 MB in a pack. One `git gc` took 1.4s and left 6 MB, and a
    /// clone from it went from 154 ms / 88 MB to 36 ms / 6 MB.
    ///
    /// The counter-half matters as much: on a packed mirror `--shared` measured 36 ms against a
    /// plain clone's 36 ms — no saving at all, in exchange for every box's objects depending on
    /// this directory, which `ensure_mirror` deletes to repair a half-made clone. So this asserts
    /// the loose objects are gone, NOT that a flag is present.
    #[test]
    fn the_mirror_a_box_clones_from_carries_no_loose_objects() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        origin_repo(&checkout);
        // Commits made one at a time, which is how a person makes them and how the loose objects
        // that cost the box its clone accumulate. Committing once would pack nothing and prove
        // nothing.
        for i in 0..12 {
            fs::write(checkout.join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
            git(&checkout, &["add", "-A"]);
            git(&checkout, &["commit", "-m", &format!("c{i}")]);
        }
        assert!(
            loose_objects(&checkout.join(".git")) > 0,
            "the fixture checkout has no loose objects, so it cannot show that the mirror \
             inherited any — the thing under test never happens"
        );

        registered("proj", &checkout);

        let mirror = mirror_path("proj");
        assert_eq!(
            loose_objects(&mirror),
            0,
            "the mirror carries loose objects inherited from the checkout it was made from, so \
             every box that clones it copies each one across the sandbox boundary — measured at \
             85MB of a 88MB mirror on the skein repo itself"
        );
        // And it is still a mirror afterwards: packing must not cost the refs a box clones.
        assert!(
            mirror_is_made(&mirror),
            "packing the mirror left something that is no longer a mirror"
        );
        assert!(
            !git(&mirror, &["rev-parse", "HEAD"]).is_empty(),
            "the packed mirror has no HEAD to clone"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// How many objects are sitting loose in a git directory, asked of git rather than counted by
    /// walking `objects/` — the layout is git's to change and the question is not.
    fn loose_objects(gitdir: &Path) -> u64 {
        git(gitdir, &["count-objects", "-v"])
            .lines()
            .find_map(|l| l.strip_prefix("count: "))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_default()
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
        let repo = registered("proj", &checkout);

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
            crate::util::resolved(&remote_origin_url(&mirror.to_string_lossy()).unwrap()),
            crate::util::resolved(&checkout.to_string_lossy()),
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

    /// Two readers of a repo with no mirror yet get one mirror between them, not neither.
    ///
    /// Both used to run `git clone --mirror` into the same directory and git refused the second —
    /// `fatal: cannot copy .../templates/description: File exists` — so **both** failed and the repo
    /// was left with nothing. That is not a transient, because of what the callers do with `None`:
    /// `review::ownership` returns `(vec![], 0)`, which its own doc says is deliberately
    /// indistinguishable from "this repo has no CODEOWNERS", and `moduledocs::modules` returns an
    /// empty list. So a PR brief quietly stopped saying which half of a change was yours. And
    /// `review::summarise` caches against the head sha, so one race froze that answer for the life
    /// of the commit.
    ///
    /// **What this test can and cannot see.** It asserts the outcome — every caller gets a usable
    /// mirror — because "both failed" IS the bug, and against the unlocked code it fails reliably at
    /// this width. It cannot prove only one clone RAN; a version that happened to serialise would
    /// pass. Counting the clones would mean putting a wrapper `git` on `$PATH`, which is
    /// process-global and would break every sibling test that shells out — the same trap
    /// `a_missing_tool_is_one_fault_and_not_five` records. The re-check under the lock is what makes
    /// the count one, and it is one line above this comment's subject.
    #[test]
    fn two_readers_of_a_new_repo_make_one_mirror_between_them() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        origin_repo(&checkout);
        // Registered WITHOUT going through `add_repo`, which would make the mirror as a side effect
        // and leave nothing for the racing readers to do.
        let repo = Repo {
            read_prs: false,
            id: "proj".into(),
            source: checkout.to_string_lossy().to_string(),
            store: home.join("store").to_string_lossy().to_string(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        assert!(
            !mirror_is_made(&mirror_path("proj")),
            "the fixture already has a mirror, so this races nothing"
        );

        let outcomes: Vec<Result<PathBuf, String>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8).map(|_| s.spawn(|| ensure_mirror(&repo))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let failed: Vec<&String> = outcomes.iter().filter_map(|r| r.as_ref().err()).collect();
        assert!(
            failed.is_empty(),
            "{} of 8 concurrent readers were left with no mirror: {failed:?}",
            failed.len()
        );
        // And what they were handed is a mirror rather than a directory that exists.
        let mirror = mirror_path("proj");
        assert!(mirror_is_made(&mirror), "no mirror at {}", mirror.display());
        assert!(
            Tree::open(&repo).is_some_and(|t| t.read("tracked.txt").is_some()),
            "the mirror every caller got cannot answer for the tree"
        );

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A repo that cannot be read says why, distinguishably from a repo with nothing to say.
    ///
    /// `Tree::open` folds both into `None`, and every caller used to read that as "nothing there":
    /// `review::ownership` answered "you own none of this" and `moduledocs::modules` answered
    /// "this repo has no modules" about a repo skein could not look at (SKEIN-117). The telling
    /// form is the seam those callers now stand on, so what it promises is pinned here: an
    /// unreadable repo is `Err` with a reason, and a readable one whose files are then absent is
    /// `Ok` — an absence found *through* the tree, which is the repo's own answer.
    #[test]
    fn a_repo_that_cannot_be_read_is_told_apart_from_one_with_nothing_to_say() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        let repo = Repo {
            read_prs: false,
            id: "proj".into(),
            source: checkout.to_string_lossy().to_string(),
            store: home.join("store").to_string_lossy().to_string(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };

        // No mirror, and none can be made: could-not-read, with the reason carried out.
        let why = match Tree::open_telling(&repo) {
            Err(why) => why,
            Ok(_) => panic!("a repo with nothing to mirror from was reported as readable"),
        };
        assert!(!why.is_empty(), "could-not-read must say why");
        assert!(
            Tree::open(&repo).is_none(),
            "the convenience form stays absence-shaped"
        );

        // The same repo, now readable: `Ok`, and a file it does not have is an absence found
        // through the tree — the repo's own answer, not a failure to look.
        origin_repo(&checkout);
        // The mirror is what `Tree` reads, so making it is what makes the repo readable — this used
        // to also assign a `source_tree`, back when a checkout was a thing a repo could have.
        crate::repos::ensure_mirror(&repo).unwrap();
        fetch_mirror(&repo).unwrap();
        let tree = Tree::open_telling(&repo).expect("a made mirror must open");
        assert!(tree.read("tracked.txt").is_some());
        assert!(tree.read("no-such-file").is_none());

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
        let repo = registered("proj", &checkout);
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

        let repo = registered("proj", &upstream);

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
    /// The mirror's `origin` is where the MIRROR fetches from; `repo_origin_url` is where the REPO
    /// lives, and a box pushes to the second. They were told apart because an adopted repo's mirror
    /// fetched from a checkout whose own origin was the remote — three hops. There are no adopted
    /// repos now, and the two are still not the same thing: the mirror is a local directory and the
    /// repo is a URL, so a box that pushed to "where the mirror fetches from" would push into
    /// skein's own copy on the volume and reach nobody.
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
        // The mirror is made from the checkout so nothing here touches the network; the repo's own
        // source is the remote, which is what every repo is now.
        registered("skein", &checkout);
        let repo = update_repos(|repos| {
            let r = repos.iter_mut().find(|r| r.id == "skein").unwrap();
            r.source = "git@github.com:acme/skein.git".into();
            Ok(r.clone())
        })
        .unwrap();

        assert_eq!(
            crate::util::resolved(
                &remote_origin_url(&mirror_path("skein").to_string_lossy()).unwrap()
            ),
            crate::util::resolved(&checkout.to_string_lossy()),
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

    /// A repo whose recorded checkout is **gone** still knows where its boxes push (SKEIN-468).
    ///
    /// This is `gadget-demo` on the live fleet, reproduced: adopted from a host path months ago,
    /// that path unreachable now that skein runs inside the sandbox, and its mirror repointed at the
    /// real remote — the right answer sitting on the volume while `repo_origin_url` asked the dead
    /// directory and returned `None`. What `None` costs is asserted here rather than described:
    /// [`crate::fleet::clone_script`] emits no `git remote set-url origin` for an empty upstream, so
    /// the box's `origin` stays the bare mirror and its pushes land where nobody pulls from, and
    /// [`crate::gitgate::repo_slug`] finds no slug, so the box is given no write token.
    #[test]
    fn a_repo_whose_checkout_is_gone_pushes_to_the_remote_its_mirror_names() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        origin_repo(&checkout);
        let repo = registered("demo", &checkout);
        // The repair somebody already did by hand on the volume: the mirror fetches from the remote
        // the repo really lives at, because the checkout it was made from is not there any more.
        let mirror = mirror_path("demo");
        let url = "https://github.com/acme/thing.git";
        git(&mirror, &["remote", "set-url", "origin", url]);
        // And the directory the mirror was made from goes away, which is every repo's state now:
        // there is no checkout anywhere, and the mirror is the only thing that can answer.
        fs::remove_dir_all(&checkout).unwrap();

        assert_eq!(
            repo_origin_url(&repo).as_deref(),
            Some(url),
            "the answer was on disk in the mirror and a directory that is not there shadowed it"
        );
        assert_eq!(
            crate::gitgate::repo_slug(&repo).as_deref(),
            Some("acme/thing"),
            "no slug means no own-repo write token, so the box cannot push at all"
        );
        let script = crate::fleet::clone_script(
            "demo-main",
            &crate::fleet::clone_source(&repo),
            "master",
            "main",
            &repo_origin_url(&repo).unwrap_or_default(),
        );
        assert!(
            script.contains(&format!("remote set-url origin '{url}'")),
            "the box would come up pushing into skein's own mirror:\n{script}"
        );

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }

    /// Pruning a mirror drops a branch deleted upstream and **nothing else** (SKEIN-466).
    ///
    /// Both halves are the test. `--prune` is there so a branch deleted upstream a year ago stops
    /// being offered to every box, and under a mirror's `+refs/*:refs/*` it also deleted every ref
    /// no origin carries: `refs/sandboxes/*`, `refs/stash`, `refs/remotes/*`. That happened —
    /// repointing skein's own mirror deleted four refs, one of them a stash reachable from nothing
    /// else — and `gc --auto` runs right after the fetch, so it is a countdown rather than a
    /// dangling ref.
    #[test]
    fn a_mirror_fetch_prunes_a_deleted_branch_and_nothing_else() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let checkout = tempdir();
        origin_repo(&checkout);
        git(&checkout, &["branch", "doomed"]);
        let repo = registered("proj", &checkout);
        let mirror = mirror_path("proj");
        let head = git(&mirror, &["rev-parse", "HEAD"]);

        // The three shapes of ref that live only in a mirror. A ref nobody planted is a ref whose
        // loss cannot be noticed, which is how this went unseen until it cost a stash.
        let kept = ["refs/sandboxes/x", "refs/stash", "refs/remotes/foo/bar"];
        for r in kept {
            git(&mirror, &["update-ref", r, &head]);
        }
        assert!(
            git(&checkout, &["branch", "--list", "doomed"]).contains("doomed"),
            "the branch this test prunes was never there to prune"
        );
        git(&checkout, &["branch", "-D", "doomed"]);

        fetch_mirror(&repo).unwrap();

        assert_eq!(
            git(&mirror, &["branch", "--list", "doomed"]),
            "",
            "a branch deleted upstream is still offered to every box that clones this"
        );
        // Listed rather than resolved one at a time: `rev-parse` on a deleted ref fails, and the
        // failure would be the helper's rather than this assertion's — the sabotage that proves
        // this test can fail has to land on the sentence that explains it.
        let refs = git(
            &mirror,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        for r in kept {
            assert!(
                refs.lines().any(|l| l == format!("{r} {head}")),
                "{r} exists in no origin, so pruning against one deleted it — and the objects are \
                 then on gc's countdown, not merely unreferenced. What is left:\n{refs}"
            );
        }

        std::env::remove_var("SKEIN_NO_GH_SECRET");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The credential seeding is host-side on both halves, and refuses rather than succeeding.
    ///
    /// `gh auth token` reads the host's login; `sbx secret set` writes the host's keyring. Neither
    /// exists in the sandbox. Returning `Ok(())` would be the tempting shape — nothing to do here —
    /// and it is wrong: seeding is how boxes get a credential at all, so a quiet success is
    /// discovered as a 403 inside a box some minutes later, three layers from its cause.
    #[test]
    fn seeding_the_fleet_credential_says_it_is_the_hosts_job_rather_than_quietly_not_doing_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env::remove_var("SKEIN_NO_GH_SECRET");
        let mut cfg = crate::config::load_config();
        cfg.seed_gh_secret = true;
        crate::config::save_config(&cfg).unwrap();

        env::set_var(crate::deployment::IN_FLEET, "1");
        let why = ensure_gh_secret()
            .expect_err("seeding reported success from a machine that cannot reach the keyring");
        assert!(
            why.contains("host"),
            "the refusal does not say whose job it is: {why}"
        );
        // Three ways forward, because there are three: do it from the host, scope per repo instead,
        // or say this fleet gets credentials another way.
        for way in ["skein on the host", "Settings", "SKEIN_NO_GH_SECRET"] {
            assert!(why.contains(way), "the refusal omits {way:?}: {why}");
        }

        // And the switch that turns seeding off is still read first — a fleet that has said it does
        // not want this must not be told about a deployment problem it does not have.
        env::set_var("SKEIN_NO_GH_SECRET", "1");
        assert!(ensure_gh_secret().is_ok());

        env::remove_var("SKEIN_NO_GH_SECRET");
        env::remove_var(crate::deployment::IN_FLEET);
        env::remove_var("SKEIN_HOME");
    }

    /// **An unreadable `repos.json` is not an empty one, and a write must not turn it into one.**
    ///
    /// SKEIN-347. `add_repo` pushes one repo onto whatever the read handed it and writes the whole
    /// list back (`repos.push(repo.clone())`). The read answered `T::default()` for a file that was
    /// missing *and* for one that merely would not parse, so a single `skein add` against a fleet
    /// whose repo list had been corrupted replaced every other repo in it — store paths, review
    /// queue settings, tracker connections — with the one being added, and returned Ok.
    ///
    /// The corruption is not invented for the test: a zero-length file is what a crash between
    /// `write_atomic`'s write and its rename leaves on ext4, and zero bytes are unparseable JSON.
    ///
    /// Asserted on the BYTES on disk rather than on the returned error, because the error is the
    /// nice half — the half that matters is that the file somebody's fleet is described by is still
    /// there afterwards.
    #[test]
    fn a_repo_list_skein_cannot_read_is_never_written_over_by_an_add() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        let established = |id: &str| -> Repo {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "source": format!("https://github.com/acme/{id}.git"),
                "source_tree": "",
                "store": format!("/store/{id}"),
            }))
            .unwrap()
        };
        save_repos(&[established("alpha"), established("beta")]).unwrap();

        // The crash artifact, exactly: present, zero-length, unparseable.
        let path = repos_json();
        std::fs::write(&path, b"").unwrap();

        let added = update_repos(|repos| {
            repos.push(established("gamma"));
            Ok(())
        });
        assert!(
            added.is_err(),
            "adding a repo over an unreadable list reported success"
        );
        let why = added.unwrap_err();
        assert!(
            why.contains("cannot read"),
            "the refusal has to say the list could not be read, not just that something failed: {why}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "the unreadable repo list was replaced by the add"
        );

        // The same for a file that is present and holds something that is not this list at all —
        // half a JSON document, the other shape a torn write leaves.
        std::fs::write(&path, b"[{\"id\":\"alpha\"").unwrap();
        assert!(
            update_repos(|repos| {
                repos.push(established("gamma"));
                Ok(())
            })
            .is_err(),
            "a half-written repo list was accepted as an empty one"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[{\"id\":\"alpha\"",
            "the half-written repo list was replaced by the add"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// The other half of the same distinction: **a repo list nobody has written yet is empty, and
    /// that is a fact.**
    ///
    /// Written beside the refusal because the refusal is one line away from breaking every fresh
    /// install — `skein add` on a machine that has never had a repo reads a file that is not there,
    /// and if that were treated as unreadable nobody could ever add their first repo.
    #[test]
    fn a_repo_list_nobody_has_written_yet_still_takes_the_first_add() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        assert!(
            !repos_json().exists(),
            "the fixture already has a repo list"
        );

        update_repos(|repos| {
            repos.push(
                serde_json::from_value(serde_json::json!({
                    "id": "first", "source": "", "source_tree": "", "store": "",
                }))
                .unwrap(),
            );
            Ok(())
        })
        .expect("the first repo of a fresh install could not be added");
        assert_eq!(
            load_repos().into_iter().map(|r| r.id).collect::<Vec<_>>(),
            vec!["first".to_string()]
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// `save_repos` refuses too, because `set_read_prs` is a load-modify-save over the whole list.
    ///
    /// `update_repos` is the locked path and the one `add_repo` takes; this is the unlocked shape
    /// beside it — `load_repos()`, change a field, `save_repos(&repos)` — and it reaches the same
    /// file. Fixing only the first would leave the second able to write a default over a file it
    /// could not read, which is the whole class.
    #[test]
    fn saving_a_whole_repo_list_refuses_over_one_skein_cannot_read() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        std::fs::create_dir_all(home.as_ref() as &std::path::Path).unwrap();
        std::fs::write(repos_json(), b"not json at all").unwrap();

        let saved = save_repos(&[serde_json::from_value(serde_json::json!({
            "id": "whatever", "source": "", "source_tree": "", "store": "",
        }))
        .unwrap()]);
        assert!(
            saved.is_err(),
            "a whole-list save went over an unreadable file"
        );
        assert_eq!(
            std::fs::read_to_string(repos_json()).unwrap(),
            "not json at all",
            "the unreadable repo list was replaced by a save"
        );

        std::env::remove_var("SKEIN_HOME");
    }
}
