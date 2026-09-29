//! The repo list on disk (`~/.skein/repos.json`): reading it, cached and not, writing it only
//! under its lock, and every setting changed through it — per-pull-request triggers, reading,
//! reviewer switches, the peer network, a rename followed, a repo removed.

use super::*;

/// 1s micro-cache over `repos.json`: a single `load_views` pass consults the repo list dozens of
/// times per box (store_for_box, turn_state, current_task, …) and each SSE tick repeats that
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

/// Where a repo's per-pull-request trigger overrides live.
///
/// Beside the mirror rather than in the store, for the reason every other engine record here is:
/// a store is mounted live into every box of the repo, and what wakes skein on somebody's pull
/// request is not a box's business.
fn pr_triggers_path(id: &str) -> std::path::PathBuf {
    crate::config::skein_home()
        .join("repos")
        .join(id)
        .join("pr-triggers.json")
}

/// **Which triggers govern THIS pull request** — `docs/pr-review.md` §10's "overridable per pull
/// request", which until now only the workflow assignment was.
///
/// Three states, and they are the three [`crate::prwork::assign`] already established for the
/// workflow, because a person needs to be able to say the same three things about triggers:
///
/// * **no entry** — the repo's own `auto_review_on` governs, which is the ordinary case;
/// * **a non-empty list** — this pull request wakes on these words and not the repo's;
/// * **an empty list** — this pull request wakes on nothing. Not "no opinion": with a repo-wide
///   set sweeping every pull request in it, "leave this one alone" is a thing somebody has to be
///   able to say, and it is the state an empty name means for the workflow.
///
/// An unreadable file reads as no entry, which is the repo's set — the same fail-toward-the-rules
/// direction `read_assigned` takes, and the one a person can see and correct.
pub fn triggers_for(repo: &Repo, number: u64) -> Vec<String> {
    match pr_triggers(&repo.id).remove(&number.to_string()) {
        Some(words) => words,
        None => repo.auto_review_on.clone(),
    }
}

/// Every per-pull-request trigger override this repo has, with an unreadable file read as none.
pub fn pr_triggers(id: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    crate::util::read_json_or_why(&pr_triggers_path(id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Put a trigger set on one pull request, or take it off.
///
/// `None` forgets the entry and lets the repo's set speak again; `Some(vec![])` is the deliberate
/// "wake on nothing". **Refuses over a file it could not read**, for [`crate::prwork::assign`]'s
/// reason: this is a read-modify-write over every override in the repo, and an unparseable file
/// read as empty would turn setting one pull request's triggers into silently clearing the rest.
pub fn set_pr_triggers(id: &str, number: u64, words: Option<Vec<String>>) -> Result<(), String> {
    let path = pr_triggers_path(id);
    let mut all: std::collections::BTreeMap<String, Vec<String>> =
        match crate::util::read_json_or_why(&path) {
            Ok(found) => found.unwrap_or_default(),
            Err(why) => {
                return Err(format!(
                    "{id}'s per-pull-request triggers could not be read ({why}), so this would \
                     have replaced every one of them with a single entry. Nothing has been changed."
                ))
            }
        };
    match words {
        Some(w) => all.insert(number.to_string(), w),
        None => all.remove(&number.to_string()),
    };
    let Some(dir) = path.parent() else {
        return Err("no directory for the trigger overrides".into());
    };
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let body = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, dir, &body)
}

/// Turn this repo's unattended pull-request reading on or off.
///
/// Its own function rather than a general "update this repo" one: this is the only field that
/// decides whether skein spends money on its own, and a route that could set it as a side effect of
/// editing something else is a route that turns it on by accident.
///
/// **Through [`update_repos`], because one field of one repo is still a whole-list write.** This
/// was `load_repos()` → change the field → `save_repos(&repos)`: the read was outside the lock
/// `save_repos` takes, and the list it read came from the 1s micro-cache (`REPOS_CACHE`), so the
/// snapshot could already be a second stale before any lock existed.
/// Anything another writer did in that window — a `skein add`, a rename, another tab's switch —
/// was written back out of the snapshot and gone. `update_repos` reads straight off disk with the
/// lock already held, which is the whole reason it exists (see its doc), and its `write_repos`
/// drops the micro-cache, so asking what is on straight afterwards gets the new answer.
pub fn set_read_prs(id: &str, on: bool) -> Result<(), String> {
    update_repos(|repos| {
        let Some(repo) = repos.iter_mut().find(|r| r.id == id) else {
            return Err(format!("no repo called {id:?}"));
        };
        repo.read_prs = on;
        Ok(())
    })
}

/// Persist the repo list to `~/.skein/repos.json` (pretty, atomic).
///
/// **Refuses over a file skein cannot read**, exactly as `config::save_config` does and for the
/// same reason one line up from it: the caller has just been handed an empty list by
/// [`load_repos`] for a file that is unparseable rather than absent, so writing that list back
/// replaces every repo in it with nothing. One dropdown would have been enough.
///
/// The refusal is not the same guarantee as [`update_repos`]'s, and the difference is the reason
/// nothing new should call this: the caller's *read* happened outside the lock, so a whole-list
/// save still overwrites whatever another writer put down in between. `set_read_prs` was that
/// shape and is not any more; the one production caller left is
/// `tracking::migrate_legacy_sync_config`, which snapshots the list before it writes tokens and
/// `connections.json` and saves it back after.
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

/// The reviewer's two switches, as a struct rather than two more positional `Option`s.
///
/// [`set_repo_settings`] already carried three, and three is where a positional list stops being
/// readable — `(None, Some("own"), None)` says nothing about which field is which, and this module's
/// own discipline is that a write must never carry a field somebody did not mean to change. Named
/// fields are that rule in a type. The three above want the same treatment one day; these two did
/// not have to wait for it.
#[derive(Debug, Clone, Default)]
pub struct ReviewerSettings {
    /// May the engine act on this repo at all — `docs/pr-review.md` §10, layer 3.
    pub auto_review: Option<bool>,
    /// How far it may go unattended, as the word a [`Ceiling`] spells. **Validated strictly here**,
    /// unlike the read path, and the asymmetry is the point: reading an unknown ceiling narrows to
    /// `Ceiling::None` so a downgrade cannot widen what skein does unattended, but doing that on a
    /// WRITE would take "approve", store "none", and report the field saved. A settings surface
    /// that lies about what it stored is worse than one that refuses.
    pub ceiling: Option<String>,
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
    reviewer: ReviewerSettings,
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
    // Read before anything is written, so a word skein does not know refuses the whole request
    // rather than half-applying it beside a Plane project that did land.
    let ceiling = match reviewer.ceiling.as_deref().map(str::trim) {
        None => None,
        Some(word) => Some(
            serde_json::from_value::<Ceiling>(serde_json::Value::String(word.to_string()))
                .map_err(|_| {
                    format!(
                        "{word:?} is not a ceiling — it is one of none, comment, changes, approve"
                    )
                })?,
        ),
    };
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
        if let Some(v) = reviewer.auto_review {
            repo.auto_review = v;
        }
        if let Some(v) = &ceiling {
            repo.auto_review_ceiling = *v;
        }
        Ok(repo.clone())
    })
}

/// Set the branch a new box of this repo starts from ([`Repo::base_branch`]). Empty clears it,
/// which means the remote's own default.
///
/// Refused only for what cannot be a branch at all. Whether the remote HAS it is asked where it
/// matters, at the clone ([`crate::fleet::base_branch`]), because a branch can be created or
/// deleted after this is saved and a check made now would be stale by then.
pub fn set_base_branch(id: &str, branch: &str) -> Result<Repo, String> {
    let branch = branch.trim();
    if !branch.is_empty() && !could_be_a_branch(branch) {
        return Err(format!(
            "{branch:?} cannot be a branch name — leave it blank to start from the remote's default"
        ));
    }
    update_repos(|repos| {
        let repo = repos
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no repo with id {id:?}"))?;
        repo.base_branch = branch.to_string();
        Ok(repo.clone())
    })
}

/// What `git check-ref-format --branch` would refuse, for the cases a person can type: spaces,
/// `..`, the characters git reserves, a leading `-` that `git` would read as an option.
fn could_be_a_branch(branch: &str) -> bool {
    !branch.starts_with('-')
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.ends_with('.')
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("@{")
        && !branch
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
}

/// Copy an older `config.json`'s fleet-wide base branch into every repo that names none, then
/// clear it there (the owner, 2026-09-27: the branch a new box starts from is per repo).
///
/// Repos first, config second, so an interruption between the two leaves the old value in place
/// and the next run finishes the job: a repo that already has one is never overwritten, so running
/// this twice changes nothing the first run did. Returns how many repos took the value.
pub fn adopt_fleet_base_branch() -> Result<usize, String> {
    let fleet = load_config().base_branch.trim().to_string();
    if fleet.is_empty() {
        return Ok(0);
    }
    let adopted = update_repos(|repos| {
        let mut adopted = 0;
        for repo in repos.iter_mut().filter(|r| r.base_branch.trim().is_empty()) {
            repo.base_branch = fleet.clone();
            adopted += 1;
        }
        Ok(adopted)
    })?;
    update_config(|c| {
        c.base_branch.clear();
        Ok(())
    })?;
    Ok(adopted)
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

/// Turn this repo's boxes on or off the peer network.
///
/// Its own function rather than a fifth argument to [`set_repo_settings`], for the reason
/// [`set_read_prs`] is: this is the only setting here that changes a **mount**, and a route that
/// could flip it as a side effect of saving an unrelated field would change what a repo's boxes can
/// reach without anybody having asked.
///
/// **Nothing here reaches a running box.** The value is read when a box is launched and travels
/// with it from there ([`Repo::peer_messaging`]), so the honest surface after this returns is the
/// board asking for a restart, not a claim that anything changed.
pub fn set_peer_messaging(id: &str, on: bool) -> Result<(), String> {
    update_repos(|repos| {
        let Some(repo) = repos.iter_mut().find(|r| r.id == id) else {
            return Err(format!("no repo called {id:?}"));
        };
        repo.peer_messaging = on;
        Ok(())
    })
}

/// Let Claude Code in this repo's boxes send Anthropic its usage telemetry, or stop it.
///
/// Its own function for the reason [`set_peer_messaging`] is: it changes what a box does at launch,
/// and a route that saved it as a side effect of an unrelated field would change that unasked.
///
/// **Nothing here reaches a running box.** The value is read when a box is launched
/// ([`Repo::anthropic_telemetry`]), so a box started before this returns keeps what it started
/// with until its next start.
pub fn set_anthropic_telemetry(id: &str, on: bool) -> Result<(), String> {
    update_repos(|repos| {
        let Some(repo) = repos.iter_mut().find(|r| r.id == id) else {
            return Err(format!("no repo called {id:?}"));
        };
        repo.anthropic_telemetry = on;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, env_pins, tempdir};

    /// **An older config's fleet-wide base branch becomes each repo's own, once, and never over a
    /// repo that already names one** (the owner, 2026-09-27).
    ///
    /// What would make it fail: the copy overwriting a repo's own value (`trunk` would become
    /// `develop`); the config value left in place, so Settings and the repo card both claim it
    /// (`the fleet-wide value is gone`); or a second run doing anything (`twice is once`).
    #[test]
    fn the_fleet_base_branch_moves_onto_each_repo_that_has_none() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        fs::write(home.join("config.json"), r#"{"base_branch":"develop"}"#).unwrap();
        fs::write(
            home.join("repos.json"),
            serde_json::json!([
                { "id": "plain", "source": "https://example.com/plain.git", "store": "/s", "agent": "claude" },
                { "id": "own", "source": "https://example.com/own.git", "store": "/s", "base_branch": "trunk" },
            ])
            .to_string(),
        )
        .unwrap();
        let bases = || {
            load_repos()
                .into_iter()
                .map(|r| (r.id, r.base_branch))
                .collect::<Vec<_>>()
        };

        assert_eq!(adopt_fleet_base_branch(), Ok(1));
        assert_eq!(
            bases(),
            [
                ("plain".into(), "develop".into()),
                ("own".into(), "trunk".into())
            ],
            "each repo with none takes it, and one with its own keeps it"
        );
        let config = fs::read_to_string(home.join("config.json")).unwrap();
        assert!(
            !config.contains("base_branch"),
            "the fleet-wide value is gone: {config}"
        );

        assert_eq!(adopt_fleet_base_branch(), Ok(0), "twice is once");
        assert_eq!(bases()[0].1, "develop");
    }

    /// **The card's field takes a branch, clears to the remote's default, and refuses what cannot
    /// be a branch** before writing anything.
    ///
    /// What would make it fail: a space or a leading `-` stored and later handed to
    /// `git ls-remote`/`git clone` (`refused`); or empty refused rather than clearing.
    #[test]
    fn a_repos_base_branch_is_set_cleared_and_refused_when_it_cannot_be_one() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        save_repos(&[Repo {
            id: "thing".into(),
            source: "https://example.com/thing.git".into(),
            store: "/s".into(),
            ..Default::default()
        }])
        .unwrap();

        assert_eq!(
            set_base_branch("thing", " feat/x ").unwrap().base_branch,
            "feat/x"
        );
        for bad in ["has space", "-x", "a..b", "x~1"] {
            assert!(set_base_branch("thing", bad).is_err(), "refused: {bad:?}");
        }
        assert_eq!(
            load_repos()[0].base_branch,
            "feat/x",
            "a refusal wrote nothing"
        );
        assert_eq!(set_base_branch("thing", "").unwrap().base_branch, "");
    }

    /// **A pull request may be governed by its own trigger set** — `docs/pr-review.md` §10 says the
    /// triggers are "overridable per pull request", and until now only the workflow assignment was.
    ///
    /// Three states, deliberately the same three the workflow assignment already had, because a
    /// person needs to say the same three things: nothing (the repo's set governs), a list (these
    /// instead), and the EMPTY list (wake on nothing). The third is the one worth a test — reading
    /// an empty override as "no opinion" would silently hand the pull request back to the repo's
    /// set, which is the opposite of what somebody who emptied it asked for, and it is the same
    /// distinction `assign`'s empty name draws.
    ///
    /// **What would make each row fail:** ignoring the override and always answering the repo's
    /// words, which is the mechanism not existing; falling back to the repo on an empty list, which
    /// is the state that cannot be expressed any other way; and forgetting one entry taking the
    /// others with it, which is what a read-modify-write over the file gets wrong.
    #[test]
    fn a_pull_request_can_be_given_its_own_trigger_set() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let repo = Repo {
            id: "demo".into(),
            auto_review_on: vec!["requested".into(), "reply".into()],
            ..Default::default()
        };

        // Nothing said about it: the repo's set, unchanged. The half that must NOT refuse.
        assert_eq!(triggers_for(&repo, 7), vec!["requested", "reply"]);

        set_pr_triggers("demo", 7, Some(vec!["approved-commits".into()])).unwrap();
        assert_eq!(triggers_for(&repo, 7), vec!["approved-commits"]);
        assert_eq!(
            triggers_for(&repo, 8),
            vec!["requested", "reply"],
            "an override on one pull request governed another"
        );

        // The empty set: wake on nothing. Not "no opinion".
        set_pr_triggers("demo", 7, Some(Vec::new())).unwrap();
        assert!(
            triggers_for(&repo, 7).is_empty(),
            "an emptied trigger set fell back to the repo's, so \"leave this one alone\" cannot be \
             said at all"
        );

        // And forgetting it gives the repo its say back, without touching the neighbour.
        set_pr_triggers("demo", 8, Some(vec!["reply".into()])).unwrap();
        set_pr_triggers("demo", 7, None).unwrap();
        assert_eq!(triggers_for(&repo, 7), vec!["requested", "reply"]);
        assert_eq!(
            triggers_for(&repo, 8),
            vec!["reply"],
            "forgetting one override forgot the others too"
        );
    }

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
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
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
                                plane_project: String::new(),
                                sync_connection: String::new(),
                                review_queue: true,
                                sync_gateway_url: String::new(),
                                ..Default::default()
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

    /// **Off is per repo, and it reaches every box of that repo by name.**
    ///
    /// `box_is_on_the_peer_network` is the single input `fleet::session_script` reads to decide
    /// `SKEIN_BOX_PEERS`, so this is the seam between the registry and the mount. A box belonging
    /// to no registered repo stays ON: the ship default, not a safety fallback — a box quietly off
    /// a network its peers believe it is on is the failure the whole switch exists to prevent.
    ///
    /// **What would make this fail:** `box_is_on_the_peer_network` returning a constant, or reading
    /// the wrong repo for a box whose name is a longer id's prefix.
    #[test]
    fn a_repo_switched_off_takes_its_own_boxes_off_the_peer_network_and_no_others() {
        let _g = env_lock();
        let dir = tempdir();
        // Pinned rather than set, and the fleet root is why. For as long as this test existed it
        // set both and put back neither, and the root leaked forward into
        // `a_repo_whose_checkout_is_gone_pushes_to_the_remote_its_mirror_names`, which pins none of
        // its own — so a missing pin and a missing cleanup cancelled out and the pair read as
        // health in every suite run. `alone-check` is what saw it, and no gate could (SKEIN-696).
        //
        // The trailing `remove_var` that first repaired it was not enough either: a failing
        // assertion in the 40 lines below unwinds straight past the last line of the test, so the
        // repair held only while the test passed. `env_pins` restores from `Drop` (SKEIN-701).
        let mut env = env_pins();
        env.set("SKEIN_HOME", &dir);
        env.set("SKEIN_FLEET_ROOT", dir.join("boxes"));
        save_repos(&[
            Repo {
                id: "web".into(),
                store: dir.join("web").to_string_lossy().into_owned(),
                ..Default::default()
            },
            Repo {
                id: "web-api".into(),
                store: dir.join("api").to_string_lossy().into_owned(),
                ..Default::default()
            },
        ])
        .unwrap();

        // The default every box starts under, asserted BEFORE the change — an absence that was
        // never a presence proves nothing about what the switch did.
        assert!(box_is_on_the_peer_network("web-main"));
        assert!(box_is_on_the_peer_network("web-api-main"));

        set_peer_messaging("web", false).unwrap();

        assert!(
            !box_is_on_the_peer_network("web-main"),
            "the switch did not reach a box of the repo it was flipped on"
        );
        assert!(
            box_is_on_the_peer_network("web-api-main"),
            "longest-id-wins: `web-api-main` belongs to `web-api`, which nobody switched off"
        );
        assert!(
            box_is_on_the_peer_network("unregistered-box"),
            "a box of no registered repo is on the network, which is the ship default"
        );
    }

    /// **A ceiling is validated on the way IN, and the read path's leniency does not apply here.**
    ///
    /// Reading an unrecognised ceiling narrows it to `Ceiling::None`, deliberately: a value a newer
    /// skein wrote must never widen what an older one does unattended. Doing the same on a WRITE
    /// would take a person's "approve", store `none`, flash "saved" at them, and leave a repo
    /// behaving as though they had chosen the opposite. A settings surface that lies about what it
    /// stored is worse than one that refuses.
    ///
    /// **What would make this fail:** reusing the lenient reader on this path. The refusal below
    /// becomes an `Ok`, and the assertion that nothing was stored catches what it stored instead.
    #[test]
    fn a_ceiling_the_write_path_does_not_recognise_is_refused_rather_than_narrowed() {
        let _g = env_lock();
        let dir = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &dir);
        save_repos(&[Repo {
            id: "web".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            ..Default::default()
        }])
        .unwrap();

        let nonsense = ReviewerSettings {
            ceiling: Some("everything".into()),
            ..Default::default()
        };
        let refused = set_repo_settings("web", None, None, None, nonsense).unwrap_err();
        assert!(
            refused.contains("not a ceiling") && refused.contains("approve"),
            "the refusal must name the words that ARE ceilings: {refused}"
        );
        assert_eq!(
            load_repos()[0].auto_review_ceiling,
            Ceiling::default(),
            "a refused ceiling was stored anyway, or stored as something narrower"
        );

        // And the whole request is refused, not half-applied: the Plane project rode along and must
        // not have landed beside a ceiling that did not.
        let both = ReviewerSettings {
            ceiling: Some("everything".into()),
            ..Default::default()
        };
        let url =
            "https://plane.example.net/acme/projects/1e2a3b4c-5d6e-4f70-8912-abcdefabcdef/issues";
        assert!(set_repo_settings("web", Some(url), None, None, both).is_err());
        assert_eq!(
            load_repos()[0].plane_project,
            "",
            "a request refused for one field applied another"
        );

        // Every word that IS a ceiling round-trips.
        for word in ["none", "comment", "changes", "approve"] {
            let picked = ReviewerSettings {
                ceiling: Some(word.into()),
                auto_review: Some(true),
            };
            let saved = set_repo_settings("web", None, None, None, picked).unwrap();
            assert_eq!(saved.auto_review_ceiling.spelled(), word);
            assert!(saved.auto_review);
        }
    }

    #[test]
    fn a_repo_refuses_a_project_no_uuid_can_be_read_from() {
        let _g = env_lock();
        let dir = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &dir);
        save_repos(&[Repo {
            read_prs: false,
            id: "web".into(),
            source: "/src/web".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();
        assert!(set_repo_settings(
            "web",
            Some("the backlog one"),
            None,
            None,
            Default::default()
        )
        .is_err());
        assert_eq!(
            load_repos()[0].plane_project,
            "",
            "a refusal stores nothing"
        );
        // The URL is kept verbatim — the uuid is derived, so a board link stays possible.
        let url =
            "https://plane.example.net/acme/projects/1e2a3b4c-5d6e-4f70-8912-abcdefabcdef/issues";
        set_repo_settings("web", Some(url), None, None, Default::default()).unwrap();
        assert_eq!(load_repos()[0].plane_project, url);
        set_repo_settings("web", Some(""), None, None, Default::default()).unwrap();
        assert_eq!(load_repos()[0].plane_project, "", "empty clears it");
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
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

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
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
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
    }

    /// `save_repos` refuses too, because a whole-list save reaches the same file.
    ///
    /// `update_repos` is the locked path and the one `add_repo` takes; this is the unlocked shape
    /// beside it — read the list somewhere, change a field, `save_repos(&repos)`. `set_read_prs`
    /// was written that way and has moved onto `update_repos`;
    /// `tracking::migrate_legacy_sync_config` still has it. Fixing only the first would leave the
    /// second able to write a default over a file it could not read, which is the whole class.
    #[test]
    fn saving_a_whole_repo_list_refuses_over_one_skein_cannot_read() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

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
    }

    /// **Turning pull-request reading on must not delete a repo somebody added while it ran.**
    ///
    /// `set_read_prs` was `load_repos()` → set the field → `save_repos(&repos)`. The lock is
    /// `save_repos`'s, and it is taken *after* the read, so everything another writer put down in
    /// between is written back out of a stale snapshot and gone. In production the snapshot can be
    /// a second staler still, because `load_repos` serves the micro-cache.
    ///
    /// **Not two racing threads and a hope**: the interleaving is *made*, so the test is arithmetic
    /// rather than timing. This thread holds the repo lock; the switch starts under it; the add
    /// happens with the lock still held; only then is it released. The unlocked shape reads before
    /// it ever asks for the lock, so it reads the one-repo list and writes it back over the add —
    /// `beta` is gone. The locked shape blocks at `update_repos`, reads after the add, and keeps
    /// both.
    ///
    /// **What would make this fail:** restore the old body of `set_read_prs`. `beta` disappears and
    /// the first assertion fails. Done, watched fail, restored.
    #[test]
    fn switching_reading_on_does_not_lose_a_repo_added_while_it_ran() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let established = |id: &str| -> Repo {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "source": format!("https://github.com/acme/{id}.git"),
                "source_tree": "",
                "store": format!("/store/{id}"),
            }))
            .unwrap()
        };
        save_repos(&[established("alpha")]).unwrap();

        let switch = crate::util::with_lock(&repos_lock(), || {
            let switch = std::thread::spawn(move || set_read_prs("alpha", true));
            // Long enough that the unlocked shape has certainly taken its snapshot — it reads
            // before asking for any lock — while the locked one is still waiting on this thread.
            std::thread::sleep(std::time::Duration::from_millis(300));
            let mut repos = read_repos_or_why().unwrap();
            repos.push(established("beta"));
            write_repos(&repos).unwrap();
            Ok(switch)
        })
        .unwrap();
        switch
            .join()
            .unwrap()
            .expect("the switch itself failed, so it proves nothing about the add");

        let after = load_repos();
        let ids: Vec<&str> = after.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"beta"),
            "the repo added while the switch ran was written out of existence by it: {ids:?}"
        );
        assert!(
            after
                .iter()
                .find(|r| r.id == "alpha")
                .expect("alpha went missing")
                .read_prs,
            "the switch did not stick"
        );
    }
}
