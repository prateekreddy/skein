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
use crate::util::bounded_output;
use crate::Sandbox;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

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
    let mut command = Command::new("git");
    command.args(["rev-parse", "--show-toplevel"]);
    if let Ok(out) = bounded_output(&mut command, "git rev-parse", Duration::from_secs(5)) {
        if out.status.success() {
            let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Some(parent) = PathBuf::from(&top).parent() {
                return Ok(parent
                    .join("skein-shared")
                    .join(".claude")
                    .join("sandboxes.json"));
            }
        }
    }
    Err("can't locate sandboxes.json — set $SKEIN_REGISTRY or $SKEIN_SHARED".into())
}

/// Where `locate_registry` got its answer. Worth saying out loud when the lookup fails: with neither
/// variable set the path is derived from the *current checkout*, so running from a second clone
/// silently looks for a store beside that clone and reports a missing registry — which reads as
/// "your registry is broken" when it means "you are standing somewhere else".
pub fn registry_origin() -> &'static str {
    let set = |k: &str| env::var(k).map(|v| !v.is_empty()).unwrap_or(false);
    if set("SKEIN_REGISTRY") {
        "$SKEIN_REGISTRY"
    } else if set("SKEIN_SHARED") {
        "$SKEIN_SHARED"
    } else {
        "derived from this checkout (no $SKEIN_REGISTRY/$SKEIN_SHARED) — it follows your cwd, \
         so a second clone looks for a store beside itself"
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
pub(crate) fn store_for_box(name: &str) -> Option<PathBuf> {
    if let Some(repo) = repo_for_box(name) {
        let p = PathBuf::from(&repo.store);
        if p.is_dir() {
            return Some(p);
        }
    }
    store_dir()
}

/// Every distinct store skein reads from: each managed repo's store plus the legacy `store_dir()`.
/// Boxes write signals/mailbox into their own repo store, so aggregate views must span all of them.
pub(crate) fn all_stores() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = load_repos()
        .into_iter()
        .map(|r| PathBuf::from(r.store))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::{branch_of, save_repos, write_launch_spec_for_agent, Repo, REPOS_CACHE};
    use crate::testutil::*;

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
            id: "demo".into(),
            source: work.display().to_string(),
            work: work.display().to_string(),
            store: store.display().to_string(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
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

    // A missing registry is reported the same way whether it was configured or guessed, and the two
    // want opposite responses: fix the path, or go stand in the right checkout.
    #[test]
    fn a_registry_says_whether_it_was_configured_or_guessed_from_the_cwd() {
        let _g = env_lock();
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_SHARED");
        assert!(registry_origin().contains("follows your cwd"));
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
}
