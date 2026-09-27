//! `sandboxes.json`, and the stores it lives in.
//!
//! A box writes its own entry — branch, working directory, last seen, and the turn state the probe
//! hook reports. skein only reads them, which is why a registry that has gone missing or become
//! unparseable degrades rather than fails: the board's source of record is [`crate::sbx`], and this
//! carries the one datum the sandbox cannot.
//!
//! **There is more than one store, and that is the whole subtlety.** Each managed repo has its own
//! `.claude` store, mounted into that repo's boxes — so a box's signals must be read from ITS repo's
//! store, not from a single global one. Reading only the legacy global registry is what made the
//! board show a box's launch branch forever after an in-box checkout. Aggregate views span every
//! store; per-box reads pick the right one and fall back.

use crate::mailbox::sandboxes_in;
use crate::repos::{load_repos, repo_for_box};
use crate::sbx::Liveness;
use crate::util::ago;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

pub fn locate_registry() -> Result<PathBuf, String> {
    if let Ok(p) = env::var("SKEIN_REGISTRY") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Ok(p) = env::var("SKEIN_SHARED") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p).join("sandboxes.json"));
        }
    }
    // **Nothing is derived from the cwd in-fleet.** The last arm asks `git` where the *process's*
    // working directory sits and puts a store beside it. On the host that is a person standing in
    // their checkout, and the answer is theirs. In-fleet it is not a person at all: the server is
    // started under `tmux new-session` with no `-c`, so it keeps whatever cwd bootstrap ran in —
    // and a cwd inside the fleet is somebody's *box tree*. The path that comes back is one nobody
    // configured, and `all_stores` hands it to `ensure_store`, which CREATES it: a shadow store,
    // built successfully, in the wrong place, indistinguishable from the real one.
    //
    // So a store is named or it does not exist. This is the arm SKEIN-476 is about, and it is
    // unconditional now (SKEIN-576): the fall-through below derived a store from the working
    // directory when skein ran on a host, and there is no host left for that to be right in.
    Err(
        "no store is configured — set $SKEIN_REGISTRY or $SKEIN_SHARED. (skein does not guess \
         one from its working directory: it would be inside somebody's box tree.)"
            .into(),
    )
}

/// Where `locate_registry` got its answer. Worth saying out loud when the lookup fails, because
/// "no store" and "the wrong store" read identically to somebody who does not know which variable
/// was consulted.
///
/// There used to be a third answer: with neither variable set the path was derived from the
/// *current checkout*, so running from a second clone silently looked for a store beside that
/// clone. That arm went with the host (SKEIN-576) — the cwd of a skein inside the fleet is
/// whatever bootstrap ran in, which is inside somebody's box tree — so the only remaining reading
/// of "neither is set" is that nothing chose a store, and the answer says exactly that.
pub fn registry_origin() -> &'static str {
    let set = |k: &str| env::var(k).map(|v| !v.is_empty()).unwrap_or(false);
    if set("SKEIN_REGISTRY") {
        "$SKEIN_REGISTRY"
    } else if set("SKEIN_SHARED") {
        "$SKEIN_SHARED"
    } else {
        "nothing — skein does not derive a store from its working directory, which in the fleet \
         would be inside somebody's box tree, and neither $SKEIN_REGISTRY nor $SKEIN_SHARED is set"
    }
}

pub fn load_registry() -> Result<(BTreeMap<String, Sandbox>, PathBuf), String> {
    let path = locate_registry()?;
    let data = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let value = parse_registry(&data).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    let boxes: BTreeMap<String, Sandbox> =
        serde_json::from_value(value).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    Ok((boxes, path))
}

/// Parse the registry JSON, self-healing one corruption we've seen in the wild: a stray leading `{}`
/// that an interrupted/legacy writer left before the real object, which serde rejects as "trailing
/// characters". Strict parse first — a valid file is never touched; only on failure do we strip a
/// leading bare `{}` and re-add the object's opening brace, so the cockpit recovers instead of going
/// blank (and `delist_box`'s rewrite then persists the clean version).
pub(crate) fn parse_registry(data: &str) -> Result<serde_json::Value, String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
        return Ok(v);
    }
    let rest = data.trim_start().strip_prefix("{}").map(str::trim_start);
    if let Some(rest) = rest {
        if rest.starts_with('"') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&format!("{{{rest}")) {
                return Ok(v);
            }
        }
    }
    // surface the original strict error
    serde_json::from_str::<serde_json::Value>(data).map_err(|e| e.to_string())
}

/// The shared store directory (parent of `sandboxes.json`).
pub(crate) fn store_dir() -> Option<PathBuf> {
    locate_registry().ok()?.parent().map(|p| p.to_path_buf())
}

/// The store to read a *specific box's* per-box signals from. Each managed repo has its own store
/// (`~/.skein/repos/<id>/store/.claude`, mounted into its boxes), so turn-state / task / session /
/// journal for a box must come from ITS repo's store — not a single global one. Falls back to
/// `store_dir()` for boxes that match no registered repo (the legacy single-repo path).
///
/// **A repo whose store is not there says so.** That fallback is the one worth hearing about: the
/// box HAS a repo, the repo names a store, and the store is missing or is not a directory — so
/// every per-box read for it silently answers out of the legacy store instead, which is a different
/// box's data or none. Once per box per process, for `load_config`'s reason: this is on the path of
/// every board row on every tick, and a line per call buries itself.
pub(crate) fn store_for_box(name: &str) -> Option<PathBuf> {
    if let Some(repo) = repo_for_box(name) {
        let p = PathBuf::from(&repo.store);
        if p.is_dir() {
            return Some(p);
        }
        if said_once_about(name) {
            eprintln!(
                "skein: {name}'s repo ({}) names a store at {} that is not there — reading its \
                 signals from {} instead, which is not where that box writes them. Fix the store \
                 path with `skein add`, or restore the directory.",
                repo.id,
                p.display(),
                store_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| "nowhere".into()),
            );
        }
    }
    store_dir()
}

/// True the first time this process is asked about `subject`. Keyed by subject rather than a bare
/// `Once`, because one broken repo store must not silence the next one. Callers that are not
/// talking about a box prefix their key (`store:<repo id>`), and a box name cannot hold a `:`
/// ([`crate::util::valid_name`]), so the two namespaces cannot collide and silence each other.
fn said_once_about(subject: &str) -> bool {
    static SAID: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    SAID.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(subject.to_string())
}

/// Every distinct store skein reads from: each managed repo's store plus the legacy `store_dir()`.
/// Boxes write signals/mailbox into their own repo store, so aggregate views must span all of them.
///
/// **A repo whose `store` is not an absolute path contributes nothing here**, and says so once.
/// This list is not only read from: [`crate::probes::ensure_probe_all`] scaffolds every entry and
/// [`crate::mailbox::relay_cross_project_mail`] copies messages into them, so a `PathBuf::from("")`
/// — what a `repos.json` entry with `"store": ""` yields — made every one of those land in whatever
/// directory the process was standing in. [`crate::kit::ensure_store`] refuses such a path too, and
/// the two are not redundant: that stops the scaffold, this stops the reads and the mail delivery,
/// which never went through it (SKEIN-551).
pub(crate) fn all_stores() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = load_repos()
        .into_iter()
        .filter_map(|r| {
            let p = PathBuf::from(&r.store);
            if p.is_absolute() {
                return Some(p);
            }
            if said_once_about(&format!("store:{}", r.id)) {
                eprintln!(
                    "skein: repo {} names its store {:?}, which is not an absolute path — skein \
                     drops it rather than resolving it against its own working directory, so that \
                     repo's boxes contribute no signals and no mail until it is given a real one \
                     with `skein add --store <path>`.",
                    r.id, r.store
                );
            }
            None
        })
        .collect();
    if let Some(d) = store_dir() {
        out.push(d);
    }
    out.sort();
    out.dedup();
    out
}

/// Aggregate every project registry. Managed boxes report their *current* branch into their own
/// mounted store; consulting only the legacy global registry made the board fall back to the launch
/// branch forever after an in-box checkout. For duplicate legacy entries, the newest lastSeen wins.
pub(crate) fn all_sandboxes() -> BTreeMap<String, Sandbox> {
    let mut boxes: BTreeMap<String, Sandbox> = BTreeMap::new();
    for store in all_stores() {
        for (name, sandbox) in sandboxes_in(&store) {
            let replace = boxes
                .get(&name)
                .is_none_or(|current| sandbox.last_seen >= current.last_seen);
            if replace {
                boxes.insert(name, sandbox);
            }
        }
    }
    boxes
}

pub(crate) fn registry_entry_for_box(name: &str) -> Option<Sandbox> {
    store_for_box(name)
        .and_then(|store| sandboxes_in(&store).remove(name))
        .or_else(|| all_sandboxes().remove(name))
}

/// One entry in the shared `sandboxes.json` registry written by sandbox-bootstrap.sh.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Sandbox {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default, rename = "lastSeen")]
    pub last_seen: String,
    /// The box's turn state, as the reader worked it out — [`crate::signals::turn_state`], filled in
    /// by the board and the session digest before they ask [`Sandbox::state`].
    ///
    /// **Never read from `sandboxes.json`** (SKEIN-1201). This used to be the registry's own
    /// `status` key, and the board and the digest fell back to it when the box had reported nothing.
    /// Nothing writes that key: the two shell writers of the registry (`box-status.sh` and
    /// `sandbox-bootstrap.sh`) merge in `branch`, `dir`, `lastSeen` and `started`, and a box's turn
    /// state lives under `status/`. So the fallback could only ever show a value some older skein
    /// left behind, and it is skipped when the file is read.
    #[serde(skip)]
    pub status: String,
}

impl Sandbox {
    /// Human label + sort/colour tier, ordered "who needs me first" (lower = more urgent):
    ///   0 error       (the turn died on an API error — most urgent) / needs-input (a decision blocks it)
    ///   1 waiting     (turn ended — your move)
    ///   2 done        (task finished — review / merge)
    ///   3 working     (in flight — leave it alone) / compacting / `live` when no explicit status
    ///   4 ended       (session terminated) / idle        5 stale / unknown
    /// Prefers the explicit status the box's hooks write; falls back to liveness
    /// derived from `lastSeen` when no box has reported a status yet.
    pub fn state(&self) -> (String, u8) {
        match self.status.as_str() {
            "error" => return ("error".into(), 0),
            "needs-input" | "needs-decision" | "blocked" => return ("needs-input".into(), 0),
            "waiting" => return ("waiting".into(), 1),
            "done" => return ("done".into(), 2),
            "working" | "running" => return ("working".into(), 3),
            "compacting" => return ("compacting".into(), 3),
            "ended" => return ("ended".into(), 4),
            "" => {} // derive from lastSeen below
            other => return (other.to_string(), 3),
        }
        match self.age_secs() {
            Some(s) if s < 120 => ("live".into(), 3),
            Some(s) if s < 1800 => ("idle".into(), 4),
            Some(_) => ("stale".into(), 5),
            None => ("unknown".into(), 5),
        }
    }

    pub fn age_secs(&self) -> Option<i64> {
        let t = DateTime::parse_from_rfc3339(&self.last_seen).ok()?;
        Some((Utc::now() - t.with_timezone(&Utc)).num_seconds())
    }

    pub fn age(&self) -> String {
        self.age_secs().map(ago).unwrap_or_else(|| "?".into())
    }

    /// State, refined by what sbx itself reports about the box's run state (`fleet_boxes`).
    /// `live` is this box's entry from that map:
    ///   - `Some(Running)`: the sandbox is up. An explicit agent turn-status still wins (it's more
    ///     specific); otherwise the box is `live` — *never* aged to `idle`/`stale`. This is the fix
    ///     for "goes idle while still working": liveness is "is the sandbox running", which sbx
    ///     knows directly, not "did a hook fire in the last 120s".
    ///   - `Some(Stopped)`: halted — show stale regardless of a now-meaningless registry status.
    ///   - `None`: sbx couldn't be consulted, or doesn't list this box (e.g. a direct-mode box) —
    ///     fall back to the `lastSeen`-derived `state()`.
    pub fn state_with(&self, live: Option<Liveness>) -> (String, u8) {
        match live {
            Some(Liveness::Running) if self.status.is_empty() => ("live".into(), 3),
            Some(Liveness::Running) => self.state(), // explicit agent turn-status wins
            Some(Liveness::Stopped) => ("stale".into(), 5),
            None => self.state(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::{branch_of, save_repos, write_launch_spec_for_agent, Repo, REPOS_CACHE};
    use crate::testutil::*;

    /// A `status` left in `sandboxes.json` by an older skein is not a turn state. Nothing writes the
    /// key now, so a value there is as old as that skein, and a board that showed it would say
    /// `waiting` about a box that has not reported in months.
    ///
    /// **What would make this fail:** reading the key again (`#[serde(default)]` on
    /// [`Sandbox::status`] in place of `skip`).
    #[test]
    fn a_status_left_in_the_registry_is_not_read() {
        let sb: Sandbox =
            serde_json::from_str(r#"{"branch":"b","dir":"/d","lastSeen":"","status":"waiting"}"#)
                .unwrap();
        assert_eq!(
            sb.status, "",
            "the registry's `status` key was read as the box's turn state"
        );
    }

    #[test]
    fn managed_registry_current_branch_beats_launch_branch() {
        let _g = env_lock();
        let root = tempdir();
        let home = root.join("home");
        let work = root.join("work");
        let store = root.join("store/.claude");
        let legacy = root.join("legacy/.claude");
        for dir in [&home, &work, &store, &legacy] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::create_dir_all(work.join(".git")).unwrap();
        fs::write(legacy.join("sandboxes.json"), "{}").unwrap();
        fs::write(
            store.join("sandboxes.json"),
            r#"{"demo-task":{"branch":"feat/current","dir":"/box/work","lastSeen":"2026-07-13T12:00:00Z"}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", legacy.join("sandboxes.json"));
        let repo = Repo {
            read_prs: false,
            id: "demo".into(),
            source: work.display().to_string(),
            store: store.display().to_string(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };
        save_repos(std::slice::from_ref(&repo)).unwrap();
        write_launch_spec_for_agent("demo-task", "feat/started", &repo, "claude").unwrap();

        assert_eq!(branch_of("demo-task").as_deref(), Some("feat/current"));
        assert_eq!(
            all_sandboxes()
                .get("demo-task")
                .map(|box_| box_.branch.as_str()),
            Some("feat/current")
        );

        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_REGISTRY");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    // A missing registry names the variable that chose it, because that is the thing to change.
    //
    // It used to have a third answer — "derived from this checkout, so it follows your cwd" — and
    // that arm went with the host (SKEIN-576): the cwd of a skein inside the fleet is whatever
    // bootstrap ran in, which is inside somebody's box tree. So the two answers left are the two
    // variables, and the absence of both is its own answer rather than a guess.
    #[test]
    fn a_registry_says_which_variable_named_it_or_that_nothing_did() {
        let _g = env_lock();
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_SHARED");
        assert!(
            registry_origin().contains("does not derive"),
            "with neither variable set the origin has to say nothing chose the store — a reader \
             told it was derived goes looking for a checkout to stand in: {}",
            registry_origin()
        );
        env::set_var("SKEIN_SHARED", "/somewhere");
        assert_eq!(registry_origin(), "$SKEIN_SHARED");
        // an explicitly set registry wins, and is named as the thing to change
        env::set_var("SKEIN_REGISTRY", "/somewhere/sandboxes.json");
        assert_eq!(registry_origin(), "$SKEIN_REGISTRY");
        // set-but-empty is not set — same rule locate_registry follows
        env::set_var("SKEIN_REGISTRY", "");
        assert_eq!(registry_origin(), "$SKEIN_SHARED");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_SHARED");
    }

    /// **The other half of SKEIN-551, and the half `kit::ensure_store`'s guard does not cover.**
    ///
    /// A `repos.json` entry with `"store": ""` yielded `PathBuf::from("")` here, and this list is
    /// acted on by more than the scaffolder: `probes::ensure_probe_all` creates every entry,
    /// `mailbox::load_mailbox` reads `<store>/mailbox` out of each, and
    /// `mailbox::relay_cross_project_mail` *copies messages into* them. Every one of those resolved
    /// against whatever directory the process was standing in. Refusing inside `ensure_store` stops
    /// the scaffold and none of the rest, which is why both changes exist.
    ///
    /// The relative-but-not-empty repo is not a second case: it is the same resolution against the
    /// same cwd, reachable by hand through `skein add --store some/dir`.
    #[test]
    fn a_repo_whose_store_is_not_absolute_contributes_no_store() {
        let _g = env_lock();
        let root = tempdir();
        let home = root.join("home");
        let real = root.join("real/.claude");
        let legacy = root.join("legacy/.claude");
        for dir in [&home, &real, &legacy] {
            fs::create_dir_all(dir).unwrap();
        }
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", root.join("fleet"));
        env::set_var("SKEIN_REGISTRY", legacy.join("sandboxes.json"));
        env::remove_var("SKEIN_SHARED");
        save_repos(&[
            Repo {
                id: "blank".into(),
                store: String::new(),
                ..Default::default()
            },
            Repo {
                id: "relative".into(),
                store: "store/.claude".into(),
                ..Default::default()
            },
            Repo {
                id: "real".into(),
                store: real.display().to_string(),
                ..Default::default()
            },
        ])
        .unwrap();
        let stores = all_stores();
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_REGISTRY");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;

        // Sorted by `all_stores` itself, and `legacy` sorts before `real` under the shared root.
        assert_eq!(
            stores,
            vec![legacy, real],
            "only the store that names an absolute path, and the configured registry's own \
             directory, may be handed to the scaffolder and the mail relay"
        );
    }

    /// An unconfigured store is an error — never a path guessed from the cwd.
    ///
    /// `locate_registry`'s last arm used to ask `git` where the process's working directory sits
    /// and put a store beside it. The server is started under `tmux new-session` with no `-c`, so
    /// its cwd is whatever bootstrap ran in — and in the fleet that is inside somebody's box tree.
    /// The invented path does not merely fail to be read: `all_stores` feeds it to `ensure_store`,
    /// which creates the whole tree, so a shadow store in the wrong place looks exactly like
    /// success.
    ///
    /// That arm had a second half — a person standing in their own checkout on the host — and it
    /// went with the host (SKEIN-576). What is asserted here is unchanged for the deployment that
    /// remains: this was already the in-fleet answer before the flag collapsed, so nothing a
    /// person could do stopped working. The test process's own cwd IS a git checkout, which is
    /// what makes the refusal below evidence rather than an accident of where it ran.
    ///
    /// **What would make this fail**: putting the `git rev-parse --show-toplevel` fall-through
    /// back in `locate_registry`. It would answer from this process's cwd and `expect_err` would
    /// panic on the `Ok`.
    #[test]
    fn a_store_is_named_or_it_does_not_exist() {
        let _g = env_lock();
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_SHARED");

        let why = locate_registry().expect_err(
            "skein derived a store from its own working directory, which is inside somebody's \
             box tree — and something downstream will now CREATE it",
        );
        assert!(
            why.contains("SKEIN_REGISTRY") && why.contains("SKEIN_SHARED"),
            "the refusal does not name what to set: {why}"
        );
        assert!(
            registry_origin().contains("does not derive"),
            "and the origin still claims a cwd derivation that no longer happens: {}",
            registry_origin()
        );
    }

    /// The store fallback speaks once per box, not once per board tick.
    ///
    /// A repo whose store is missing sends every per-box read to the legacy store instead — a
    /// different box's data, or none — and that used to be silent. Saying it is only useful if it
    /// is readable: `store_for_box` runs for every row of every 2s board tick, so an unconditional
    /// line would bury itself thousands deep, and the box name has to be the key or one broken
    /// repo silences the next.
    #[test]
    fn a_store_that_is_not_there_is_reported_once_per_box() {
        assert!(said_once_about("web-main"), "the first sighting must speak");
        assert!(
            !said_once_about("web-main"),
            "a second sighting of the same box repeated itself — this runs once per row per tick"
        );
        assert!(
            said_once_about("web-api"),
            "a different box was silenced by the first one's warning"
        );
    }

    #[test]
    fn parse_registry_heals_stray_leading_empty_object() {
        // the corruption seen in the wild: a leading `{}` before the real object's body.
        let corrupt = "{}\n \"thing-x\": {\n  \"branch\": \"x\"\n }\n}";
        assert!(serde_json::from_str::<serde_json::Value>(corrupt).is_err()); // serde rejects it
        let v = parse_registry(corrupt).expect("self-heals");
        assert_eq!(v["thing-x"]["branch"], "x");
        // a valid registry is returned untouched.
        let ok = r#"{"a":{"branch":"b"}}"#;
        assert_eq!(parse_registry(ok).unwrap()["a"]["branch"], "b");
    }

    #[test]
    fn state_prefers_explicit_status() {
        assert_eq!(sb("needs-input", "").state(), ("needs-input".into(), 0));
        assert_eq!(sb("waiting", "").state().1, 1);
        assert_eq!(sb("done", "").state().1, 2);
        assert_eq!(sb("working", "").state().1, 3);
        assert_eq!(sb("compiling", "").state(), ("compiling".into(), 3)); // passthrough
                                                                          // the richer lifecycle states
        assert_eq!(sb("error", "").state(), ("error".into(), 0)); // most urgent
        assert_eq!(sb("blocked", "").state(), ("needs-input".into(), 0)); // permission → needs you
        assert_eq!(sb("compacting", "").state(), ("compacting".into(), 3)); // busy, not stuck
        assert_eq!(sb("ended", "").state(), ("ended".into(), 4)); // distinct from stale
    }

    #[test]
    fn state_derives_liveness_from_last_seen() {
        assert_eq!(sb("", &secs_ago(10)).state().0, "live");
        assert_eq!(sb("", &secs_ago(600)).state().0, "idle");
        assert_eq!(sb("", &secs_ago(7200)).state().0, "stale");
        assert_eq!(sb("", "not-a-date").state().0, "unknown");
    }

    #[test]
    fn state_with_sbx_liveness() {
        // A running sandbox with no hook status is LIVE even if lastSeen is ancient — the fix.
        assert_eq!(
            sb("", &secs_ago(99999)).state_with(Some(Liveness::Running)),
            ("live".into(), 3)
        );
        // An explicit agent turn-status still wins over the generic "live".
        assert_eq!(
            sb("needs-input", &secs_ago(99999)).state_with(Some(Liveness::Running)),
            ("needs-input".into(), 0)
        );
        // Stopped → stale regardless of a stale "working" left in the registry.
        assert_eq!(
            sb("working", &secs_ago(5)).state_with(Some(Liveness::Stopped)),
            ("stale".into(), 5)
        );
        // No sbx info → behaves exactly like the lastSeen-derived state().
        assert_eq!(
            sb("", &secs_ago(600)).state_with(None),
            sb("", &secs_ago(600)).state()
        );
    }
}
