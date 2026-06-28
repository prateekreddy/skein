//! skein core — read the shared sbx registry and derive fleet views.
//!
//! This is the single source of truth shared by the CLI (`skein`) and the server
//! (`skein-server`). It owns no state the sandboxes don't already write; it only reads
//! `sandboxes.json` and derives status. See ARCHITECTURE.md.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// One entry in the shared `sandboxes.json` registry written by sandbox-bootstrap.sh.
#[derive(Debug, Default, Deserialize)]
pub struct Sandbox {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default, rename = "lastSeen")]
    pub last_seen: String,
    /// Set by the box status hook (roadmap phase 2); usually empty today.
    #[serde(default)]
    pub status: String,
}

/// A registry entry enriched for display — what the CLI table and the web API both render.
#[derive(Debug, Serialize)]
pub struct BoxView {
    pub name: String,
    pub state: String,
    /// 0 waiting/done-attention, sorted up; higher = quieter. See [`Sandbox::state`].
    pub tier: u8,
    pub branch: String,
    pub age: String,
    pub dir: String,
}

impl Sandbox {
    /// Human label + sort/colour tier, ordered "who needs me first" (lower = more urgent):
    ///   0 needs-input (a decision/permission is blocking the agent)
    ///   1 waiting     (turn ended — your move)
    ///   2 done        (task finished — review / merge)
    ///   3 working     (in flight — leave it alone) / `live` when no explicit status
    ///   4 idle        5 stale / unknown
    /// Prefers the explicit status the box's hooks write; falls back to liveness
    /// derived from `lastSeen` when no box has reported a status yet.
    pub fn state(&self) -> (String, u8) {
        match self.status.as_str() {
            "needs-input" | "needs-decision" | "blocked" => return ("needs-input".into(), 0),
            "waiting" => return ("waiting".into(), 1),
            "done" => return ("done".into(), 2),
            "working" | "running" => return ("working".into(), 3),
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

    fn age_secs(&self) -> Option<i64> {
        let t = DateTime::parse_from_rfc3339(&self.last_seen).ok()?;
        Some((Utc::now() - t.with_timezone(&Utc)).num_seconds())
    }

    pub fn age(&self) -> String {
        match self.age_secs() {
            None => "?".into(),
            Some(s) if s < 60 => format!("{s}s ago"),
            Some(s) if s < 3600 => format!("{}m ago", s / 60),
            Some(s) if s < 86400 => format!("{}h ago", s / 3600),
            Some(s) => format!("{}d ago", s / 86400),
        }
    }
}

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
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
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

pub fn load_registry() -> Result<(BTreeMap<String, Sandbox>, PathBuf), String> {
    let path = locate_registry()?;
    let data = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let boxes: BTreeMap<String, Sandbox> =
        serde_json::from_str(&data).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    Ok((boxes, path))
}

/// The fleet, enriched and sorted "who needs me first" (tier asc, then name).
pub fn load_views() -> Result<Vec<BoxView>, String> {
    let (boxes, _) = load_registry()?;
    let mut views: Vec<BoxView> = boxes
        .iter()
        .map(|(name, b)| {
            let (state, tier) = b.state();
            BoxView {
                name: name.clone(),
                state,
                tier,
                branch: b.branch.clone(),
                age: b.age(),
                dir: shorten(&b.dir),
            }
        })
        .collect();
    views.sort_by(|a, b| a.tier.cmp(&b.tier).then(a.name.cmp(&b.name)));
    Ok(views)
}

/// Look up a box's clone root (the `dir` it registered) by name.
pub fn lookup_dir(name: &str) -> Option<String> {
    let (boxes, _) = load_registry().ok()?;
    boxes
        .get(name)
        .map(|b| b.dir.clone())
        .filter(|d| !d.is_empty())
}

/// Wrap a string for safe use inside single quotes in a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The in-box script that *continues* the agent session (vs. starting a new one).
///
/// It runs the agent inside a tmux session named `skein`, created on first attach and
/// reused on every later one (`new-session -A` = attach-if-exists-else-create). So a
/// browser refresh, a `skein attach`, or a server restart all reconnect to the SAME
/// live session — tmux lives in the box and survives the client going away. The session
/// runs `claude --continue` to resume the conversation; if tmux or claude is missing it
/// degrades to a plain login shell rather than failing. `dir` is the box's clone root.
pub fn attach_inner_script(dir: &str) -> String {
    let cd = if dir.is_empty() {
        String::new()
    } else {
        format!("cd {} 2>/dev/null; ", sh_quote(dir))
    };
    let agent = "claude --continue 2>/dev/null || claude . 2>/dev/null || exec bash -l";
    format!(
        "{cd}if command -v tmux >/dev/null 2>&1; then \
           exec tmux new-session -A -s skein {}; \
         else {agent}; fi",
        sh_quote(agent),
    )
}

/// The full `sbx` argv that reconnects to box `name` rooted at `dir`.
/// (The leading program is `sbx`; this returns only its arguments.)
///
/// The `--` is load-bearing: `sbx` is a cobra CLI and parses leading-dash tokens
/// (`-lc`) as its OWN flags unless `--` ends flag parsing first.
pub fn attach_argv(name: &str, dir: &str) -> Vec<String> {
    vec![
        "run".into(),
        "--name".into(),
        name.into(),
        "--".into(),
        "bash".into(),
        "-lc".into(),
        attach_inner_script(dir),
    ]
}

pub fn shorten(p: &str) -> String {
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() && p.starts_with(&home) {
            return format!("~{}", &p[home.len()..]);
        }
    }
    p.to_string()
}
