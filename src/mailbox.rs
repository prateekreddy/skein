//! Messages between boxes, delivered at a turn boundary.
//!
//! A file per message in the shared store, so delivery survives a restart on either side and
//! neither box has to be running when the other writes.

use crate::util::*;
use crate::{all_stores, load_repos, store_for_box};
use crate::{parse_registry, Sandbox};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One cross-box message in the shared `mailbox/` (written by mailbox.sh or skein).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String, // a vmid, or "broadcast"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub ts: String,
    #[serde(default, rename = "seenBy")]
    pub seen_by: Vec<String>,
    /// Host-only bookkeeping: destination store paths this message has already been relayed to by
    /// `relay_cross_project_mail`, so a repeat sweep doesn't re-deliver it. Empty for anything a box
    /// itself wrote and skein hasn't touched yet.
    #[serde(default, rename = "relayedTo")]
    pub relayed_to: Vec<String>,
    /// Set on a relayed *copy* to the source repo id (or store path, if unmanaged) so the receiving
    /// side can show provenance across a project boundary. Empty for a box's own local messages.
    #[serde(default, rename = "originProject")]
    pub origin_project: String,
}

/// All cross-box messages, newest first — aggregated across every repo's store (boxes post into their
/// own repo's `<store>/mailbox/`, so a single store would miss other repos' messages).
pub fn load_mailbox() -> Vec<Message> {
    let mut out = Vec::new();
    for store in all_stores() {
        let dir = store.join("mailbox");
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(txt) = fs::read_to_string(&p) {
                    if let Ok(m) = serde_json::from_str::<Message>(&txt) {
                        out.push(m);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    out
}

/// Post a message (from `skein`) in the shape mailbox.sh writes so each box's `inbox` picks it up.
/// `to` is a vmid or "broadcast". Routed to the right store: a specific box → its repo's store; a
/// broadcast → every store (so boxes of every repo see it).
pub fn send_message(to: &str, kind: &str, body: &str) -> Result<(), String> {
    let targets: Vec<PathBuf> = if to == "broadcast" || to.is_empty() {
        all_stores()
    } else {
        vec![store_for_box(to).ok_or("can't locate a store for that box")?]
    };
    if targets.is_empty() {
        return Err("can't locate the shared store".into());
    }
    let now = Utc::now();
    let msg = Message {
        from: "skein".into(),
        to: to.into(),
        kind: if kind.is_empty() {
            "note".into()
        } else {
            kind.into()
        },
        branch: String::new(),
        body: body.into(),
        ts: now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        seen_by: vec![],
        relayed_to: vec![],
        origin_project: String::new(),
    };
    let id = format!("{}-skein", now.timestamp_nanos_opt().unwrap_or(0));
    let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    let mut wrote = false;
    let mut last_err = String::new();
    for store in targets {
        let dir = store.join("mailbox");
        if let Err(e) = fs::create_dir_all(&dir) {
            last_err = format!("mailbox dir: {e}");
            continue;
        }
        match fs::write(dir.join(format!("{id}.json")), &json) {
            Ok(()) => wrote = true,
            Err(e) => last_err = format!("write: {e}"),
        }
    }
    if wrote {
        Ok(())
    } else {
        Err(last_err)
    }
}

/// Read one store's box registry (`sandboxes.json`) as a map, ignoring any read/parse error
/// (fail-soft — a store with no registry, or a stale one, just yields no matches).
pub(crate) fn sandboxes_in(store: &Path) -> BTreeMap<String, Sandbox> {
    fs::read_to_string(store.join("sandboxes.json"))
        .ok()
        .and_then(|data| parse_registry(&data).ok())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Sweep every managed store's mailbox for box-authored messages addressed across a project
/// boundary — `to: "all-projects"`, `to: "project:<repo-id>"`, or a bare vmid that belongs to a
/// DIFFERENT project's registry than the one the message was found in — and copy them into the
/// destination store(s)' mailbox, rewriting `to` to whatever that destination's own local delivery
/// already matches (`broadcast`, or the specific vmid unchanged). This is the one hop a box itself
/// cannot make: each box mounts only its own project's store (separate microVM kernels), so
/// cross-project delivery can only happen host-side, where skein already reads every managed store
/// (`all_stores`, `load_mailbox`, `send_message`).
///
/// Idempotent: marks the origin message's `relayedTo` with every destination store it has already
/// copied into (locked the same way `mailbox.sh` locks a message file to mark `seenBy`), so a repeat
/// sweep never re-delivers the same message twice. Host-only — never called from inside a box.
/// Best-effort: a single unreadable/unwritable message is skipped, not fatal to the sweep.
pub fn relay_cross_project_mail() -> Result<(), String> {
    use fs2::FileExt;
    let stores = all_stores();
    if stores.len() < 2 {
        return Ok(()); // nothing to relay across when there's only one (or zero) managed project
    }
    let repos = load_repos();
    let mut errs = Vec::new();

    for origin in &stores {
        let mailbox_dir = origin.join("mailbox");
        let Ok(rd) = fs::read_dir(&mailbox_dir) else {
            continue;
        };
        let origin_id = repos
            .iter()
            .find(|r| Path::new(&r.store) == origin.as_path())
            .map(|r| r.id.clone())
            .unwrap_or_else(|| origin.display().to_string());
        let origin_registry = sandboxes_in(origin);

        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(txt) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(msg) = serde_json::from_str::<Message>(&txt) else {
                continue;
            };

            // Resolve destination stores this message hasn't already reached.
            let mut targets: Vec<PathBuf> = Vec::new();
            if msg.to == "all-projects" {
                targets.extend(stores.iter().filter(|s| *s != origin).cloned());
            } else if let Some(id) = msg.to.strip_prefix("project:") {
                if let Some(r) = repos.iter().find(|r| r.id == id) {
                    let p = PathBuf::from(&r.store);
                    if p != *origin {
                        targets.push(p);
                    }
                }
                // an unresolved project id (not added yet) is left unmarked — retried next sweep.
            } else if !msg.to.is_empty()
                && msg.to != "broadcast"
                && !origin_registry.contains_key(&msg.to)
            {
                // A bare vmid the ORIGIN project's own registry doesn't know — it may belong to
                // another one (a box addressing a specific sibling in a different project).
                for other in stores.iter().filter(|s| *s != origin) {
                    if sandboxes_in(other).contains_key(&msg.to) {
                        targets.push(other.clone());
                    }
                }
            }
            targets.retain(|t| {
                let t_str = t.display().to_string();
                !msg.relayed_to.contains(&t_str)
            });
            if targets.is_empty() {
                continue;
            }

            // "all-projects" and "project:<id>" are both fan-out-to-a-whole-project addresses —
            // the relayed copy in the destination project must read as THAT project's own
            // broadcast (mailbox.sh's local match only ever recognizes "broadcast"/"all-projects"/
            // its own vmid; a literal "project:b" would never locally match any box in project b).
            // A bare cross-project vmid address is left as-is so it still targets that one box.
            let new_to = if msg.to == "all-projects" || msg.to.starts_with("project:") {
                "broadcast".to_string()
            } else {
                msg.to.clone()
            };
            let mut relayed_now: Vec<String> = Vec::new();
            for target in &targets {
                let dest_dir = target.join("mailbox");
                if let Err(e) = fs::create_dir_all(&dest_dir) {
                    errs.push(format!("{}: mkdir: {e}", target.display()));
                    continue;
                }
                let copy = Message {
                    from: msg.from.clone(),
                    to: new_to.clone(),
                    kind: msg.kind.clone(),
                    branch: msg.branch.clone(),
                    body: msg.body.clone(),
                    ts: msg.ts.clone(),
                    seen_by: vec![],
                    relayed_to: vec![],
                    origin_project: origin_id.clone(),
                };
                let Ok(json) = serde_json::to_string(&copy) else {
                    continue;
                };
                // The origin filename (timestamp-vmid-pid) is already unique per message, and each
                // target writes into its own store's mailbox dir, so "-relay" alone can't collide.
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("msg");
                match fs::write(dest_dir.join(format!("{stem}-relay.json")), &json) {
                    Ok(()) => relayed_now.push(target.display().to_string()),
                    Err(e) => errs.push(format!("{}: write: {e}", target.display())),
                }
            }
            if relayed_now.is_empty() {
                continue;
            }

            // Mark the origin message's relayedTo, locked the same way mailbox.sh locks a message
            // file to mark seenBy — so a box's concurrent seenBy update can't race this update.
            let lock_path = PathBuf::from(format!("{}.lock", path.display()));
            let lock = match fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
            {
                Ok(f) => f,
                Err(e) => {
                    errs.push(format!("{}: lock open: {e}", path.display()));
                    continue;
                }
            };
            if lock.lock_exclusive().is_err() {
                errs.push(format!("{}: lock", path.display()));
                continue;
            }
            let result = (|| -> Result<(), String> {
                let txt = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let mut cur: Message = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
                for t in &relayed_now {
                    if !cur.relayed_to.contains(t) {
                        cur.relayed_to.push(t.clone());
                    }
                }
                let bytes = serde_json::to_vec_pretty(&cur).map_err(|e| e.to_string())?;
                write_atomic(&path, &mailbox_dir, &bytes)
            })();
            let _ = lock.unlock();
            let _ = fs::remove_file(&lock_path);
            if let Err(e) = result {
                errs.push(format!("{}: {e}", path.display()));
            }
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}
