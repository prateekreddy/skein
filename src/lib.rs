//! skein core — read the shared sbx registry and derive fleet views.
//!
//! This is the single source of truth shared by the CLI (`skein`) and the server
//! (`skein-server`). It owns no state the sandboxes don't already write; it only reads
//! `sandboxes.json` and derives status. See ARCHITECTURE.md.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Load a local `.env` (searched from the cwd upward) so the registry/repo paths and `*_CMD`
/// templates needn't be passed on every invocation. Variables already set in the real
/// environment win — dotenv never overrides — so a command-line `VAR=… skein …` still takes
/// precedence. A missing file is fine and silent; a *malformed* file is reported on stderr
/// rather than silently dropping every line after the bad one (which once made a quoting slip
/// look like a "command not found"). The binaries call this once at startup.
pub fn load_dotenv() {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("skein: ignoring malformed .env — {e}"),
    }
}

/// Diff summary a box reports for its branch-vs-base work (written by box-diff.sh).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct DiffStat {
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub ins: u32,
    #[serde(default)]
    pub del: u32,
}

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
    /// Set by the box status hook (box-status.sh); usually empty until a box reports.
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
    /// the registered repo this box belongs to (`<repo>-<branch>`), empty if it matches none.
    /// Lets the cockpit group rows by repo once more than one is managed.
    #[serde(default)]
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
    /// one-line gist of the box's last reported signal (the inbox headline) — the blocking
    /// prompt when it's waiting on you, else the first line of its last message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    /// what the box is doing *right now* — the in-progress TodoWrite item (or journal `next`).
    /// The peripheral "what's happening in the other tabs" signal; shown subtly on every row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// why the turn ended (the fork-detector) — lets the inbox label & batch the trivial asks.
    pub pause: Pause,
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

/// A box name is a registry key / vmid — never a path or a shell token. Reject anything that
/// could escape the store dir on a filesystem join (`..`, separators, NUL). The server validates
/// every `:name` route with this, and the path-touching lib fns guard with it too so the check
/// can't be bypassed by a non-HTTP caller.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && !name.contains(['/', '\\', '\0'])
}

/// Write `bytes` to `path` atomically: a temp file in the same dir, then rename (POSIX-atomic),
/// so a concurrent reader sees either the old or the new whole file, never a truncated one.
/// `dir` must be `path`'s parent (same filesystem) for the rename to be atomic.
fn write_atomic(path: &Path, dir: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = dir.join(format!(".skein.tmp.{}", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| format!("writing temp: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("renaming into place: {e}")
    })
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
    // Fleet source of record: sbx itself (`sbx ls`). The registry only *enriches* — it carries the
    // one datum sbx can't (the agent turn-state) — and serves as a fallback when sbx can't be
    // consulted or a box is direct-mode (no sbx). Everything else (which boxes exist, their dir,
    // branch, run-state, diff) comes from sbx + host git, so a box no longer has to register itself
    // to be seen. See docs/self-sufficient.md.
    let sbx = fleet_boxes();
    let reg = load_registry().map(|(b, _)| b).unwrap_or_default();
    // skein-server may run *inside* one box; that box is provably up, so keep it live even when sbx
    // can't confirm it. Set $SKEIN_SELF to override the detected vmid.
    let self_box = env::var("SKEIN_SELF")
        .or_else(|_| env::var("SANDBOX_VM_ID"))
        .ok()
        .filter(|s| !s.is_empty());

    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Some(v) = &sbx {
        names.extend(v.iter().map(|b| b.name.clone()));
    }
    names.extend(reg.keys().cloned());

    let mut views: Vec<BoxView> = names
        .into_iter()
        .map(|name| {
            let s = sbx.as_ref().and_then(|v| v.iter().find(|b| b.name == name));
            let r = reg.get(&name);
            // dir/branch: prefer the registry's known-good values (no regression for registered
            // boxes); fall back to sbx workspaces + host git for boxes the registry doesn't know.
            let dir = r
                .map(|x| x.dir.clone())
                .filter(|d| !d.is_empty())
                .or_else(|| s.map(|x| x.dir.clone()).filter(|d| !d.is_empty()))
                .unwrap_or_default();
            // The repo this box belongs to (if any), used for grouping + branch fallback.
            let repo = repo_for_box(&name);
            let branch = r
                .map(|x| x.branch.clone())
                .filter(|b| !b.is_empty() && b != "?")
                .or_else(|| git_branch_for(&dir))
                .or_else(|| repo.as_ref().map(|rp| branch_from_box(&name, rp)))
                .unwrap_or_default();
            // Reuse the registry-derived state logic; status (turn-state) is the registry's specific
            // datum, lastSeen is only a fallback when sbx liveness is absent.
            let sb = Sandbox {
                branch: branch.clone(),
                dir: dir.clone(),
                last_seen: r.map(|x| x.last_seen.clone()).unwrap_or_default(),
                // turn-state from skein's own probe; the registry's status is a transitional fallback.
                status: current_status(&name)
                    .or_else(|| r.map(|x| x.status.clone()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default(),
            };
            let live = s.and_then(|x| x.live);
            let (mut state, mut tier) = sb.state_with(live);
            // Self-box stays live when sbx can't confirm it (e.g. skein running outside sbx).
            if live.is_none()
                && sb.status.is_empty()
                && self_box.as_deref() == Some(name.as_str())
                && tier > 3
            {
                state = "live".into();
                tier = 3;
            }
            // The narrative signal (box-session.sh): a cheap per-box file read, no model call.
            // Headline = the blocking prompt when waiting on you, else the gist of the last
            // message; pause classifies *why* it stopped so the inbox can rank and batch.
            let sig = session_signal(&name);
            let blocked = state == "needs-input";
            let signal_text = sig.as_ref().map(|s| {
                if s.kind == "notification" && !s.prompt.trim().is_empty() {
                    s.prompt.clone()
                } else {
                    s.last_message.clone()
                }
            });
            // The live "what's it doing now" signal (box-task.sh / journal `next`).
            let task = current_task(&name);
            let mut headline = signal_text.as_deref().and_then(first_line);
            // When the signal is absent or just the generic "waiting for your input", surface the
            // current task instead — so even a tier-0 needs-input row says what it was working on.
            if headline.as_deref().is_none_or(is_generic_wait) {
                if let Some(t) = task.clone() {
                    headline = Some(t);
                }
            }
            let pause = if tier == 3 {
                Pause::None // still working — nothing owed
            } else {
                classify_message(signal_text.as_deref().unwrap_or(""), blocked)
            };
            BoxView {
                name: name.clone(),
                state,
                tier,
                branch,
                age: sb.age(),
                dir: shorten(&dir),
                repo: repo.map(|rp| rp.id).unwrap_or_default(),
                diff: host_diffstat(&name, &dir),
                headline,
                task,
                pause,
            }
        })
        .collect();
    views.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then(a.pause.rank().cmp(&b.pause.rank()))
            .then(a.name.cmp(&b.name))
    });
    Ok(views)
}

/// What sbx itself reports about a box's run state (from `sbx ls`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Liveness {
    Running,
    Stopped,
}

/// A box as sbx itself sees it (`sbx ls --json`) — the source of record for the fleet, so a box
/// shows up because it's a sandbox sbx knows about, not because it wrote itself into a registry.
#[derive(Clone, Debug)]
pub struct SbxBox {
    pub name: String,
    /// "claude" | "codex" | … — the per-runtime seam for the (later) turn-state adapter.
    pub agent: String,
    /// Run state; `None` if sbx reports an unrecognised status (caller falls back to lastSeen).
    pub live: Option<Liveness>,
    /// The repo workspace (the shared `.claude` store mount is excluded). May be empty.
    pub dir: String,
}

/// Enumerate the fleet from sbx. `None` when sbx can't be consulted (not installed, errored, or
/// unparseable) — callers then fall back to the registry. Override with `$SKEIN_LS_CMD` (run via
/// `sh -c`; must emit the `sbx ls --json` shape).
pub fn fleet_boxes() -> Option<Vec<SbxBox>> {
    let output = match env::var("SKEIN_LS_CMD").ok().filter(|s| !s.is_empty()) {
        Some(c) => Command::new("sh").arg("-c").arg(c).output(),
        None => Command::new("sbx").args(["ls", "--json"]).output(),
    }
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let boxes = parse_boxes(&String::from_utf8_lossy(&output.stdout));
    (!boxes.is_empty()).then_some(boxes)
}

/// One box's run-state from sbx — a single-box view of [`fleet_boxes`].
fn box_liveness(name: &str) -> Option<Liveness> {
    fleet_boxes()?
        .into_iter()
        .find(|b| b.name == name)
        .and_then(|b| b.live)
}

const LS_NAME_KEYS: &[&str] = &[
    "name", "Name", "NAME", "sandbox", "SANDBOX", "vmId", "vmid", "VmId", "id", "ID",
];
const LS_STATUS_KEYS: &[&str] = &["status", "Status", "STATUS", "state", "State"];
const LS_AGENT_KEYS: &[&str] = &["agent", "Agent", "AGENT"];
const LS_WS_KEYS: &[&str] = &[
    "workspaces",
    "Workspaces",
    "workspace",
    "Workspace",
    "WORKSPACE",
];

/// Parse `sbx ls --json` defensively: tolerate NDJSON (one object per line — a common Docker-CLI
/// `--json` shape) or a single array / `{sandboxes:[..]}` / `{name:{..}}` document, and varied key
/// casings. Boxes without a (valid) name are skipped.
fn parse_boxes(json: &str) -> Vec<SbxBox> {
    use serde_json::Value;
    // NDJSON first: each non-empty line an object. If that yields <2 objects it isn't NDJSON, so
    // parse the whole payload as one document instead.
    let mut entries: Vec<Value> = json
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
        .collect();
    if entries.len() < 2 {
        if let Ok(v) = serde_json::from_str::<Value>(json) {
            entries = collect_ls_entries(v);
        }
    }
    let mut out = Vec::new();
    for e in entries {
        let obj = match e.as_object() {
            Some(o) => o,
            None => continue,
        };
        let name = match LS_NAME_KEYS
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
        {
            Some(n) if valid_name(n) => n.to_string(),
            _ => continue,
        };
        let agent = LS_AGENT_KEYS
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let live = LS_STATUS_KEYS
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .and_then(|s| {
                if s.eq_ignore_ascii_case("running") {
                    Some(Liveness::Running)
                } else if s.eq_ignore_ascii_case("stopped") {
                    Some(Liveness::Stopped)
                } else {
                    None
                }
            });
        let dir = LS_WS_KEYS
            .iter()
            .find_map(|k| obj.get(*k))
            .map(repo_workspace)
            .unwrap_or_default();
        out.push(SbxBox {
            name,
            agent,
            live,
            dir,
        });
    }
    out
}

/// Pick the repo workspace from a box's `workspaces` value: the path that isn't the shared `.claude`
/// store. Accepts an array or a single string; empty if none.
fn repo_workspace(ws: &serde_json::Value) -> String {
    use serde_json::Value;
    let paths: Vec<&str> = match ws {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        Value::String(s) => vec![s.as_str()],
        _ => vec![],
    };
    paths
        .iter()
        .find(|p| !p.trim_end_matches('/').ends_with("/.claude"))
        .or_else(|| paths.first())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The current branch of the git working tree at `dir`, read host-side — so skein can show a box's
/// branch without the registry. None if `dir` isn't a repo or HEAD is detached.
fn git_branch_for(dir: &str) -> Option<String> {
    if dir.is_empty() {
        return None;
    }
    let out = Command::new("git")
        .args(["-C", dir, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!b.is_empty() && b != "HEAD").then_some(b)
}

/// Reduce a single `sbx ls --json` document to a flat list of per-box objects, covering an array,
/// an `{key: [..]}` wrapper, or a `{name: {..}}` map (the box name is injected as `name`).
fn collect_ls_entries(v: serde_json::Value) -> Vec<serde_json::Value> {
    use serde_json::Value;
    match v {
        Value::Array(a) => a,
        Value::Object(o) => {
            if let Some(arr) = o.values().find_map(Value::as_array) {
                return arr.clone();
            }
            o.into_iter()
                .filter_map(|(k, mut val)| match val {
                    Value::Object(ref mut m) => {
                        m.insert("name".into(), Value::String(k));
                        Some(val)
                    }
                    _ => None,
                })
                .collect()
        }
        _ => vec![],
    }
}

/// The shared store directory (parent of `sandboxes.json`).
fn store_dir() -> Option<PathBuf> {
    locate_registry().ok()?.parent().map(|p| p.to_path_buf())
}

/// The store to read a *specific box's* per-box signals from. Each managed repo has its own store
/// (`~/.skein/repos/<id>/store/.claude`, mounted into its boxes), so turn-state / task / session /
/// journal for a box must come from ITS repo's store — not a single global one. Falls back to
/// `store_dir()` for boxes that match no registered repo (the legacy single-repo path).
fn store_for_box(name: &str) -> Option<PathBuf> {
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
fn all_stores() -> Vec<PathBuf> {
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

/// Wrap a string for safe single-quoting in a POSIX shell. Used to quote every value substituted
/// into a `*_CMD` template before it reaches `sh -c`, so a branch/box name can't inject commands.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ───────────────────────────── skein-owned repo registry ─────────────────────────────
//
// skein is no longer single-repo. `~/.skein/repos.json` lists every repo skein manages; each box
// is `<repo-id>-<branch>` and maps back to its repo by id-prefix. This is skein's OWN config —
// distinct from the per-box `sandboxes.json` we dropped — and it's what makes "add a repo URL and
// go" work without the repo shipping anything for skein.

/// One managed repo. `work` is the host clone (for host-side git/diff + as the `sbx run` workspace);
/// `store` is the host `.claude` skein provisions and mounts into every box for that repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub source: String, // git URL or local path the repo was added from
    pub work: String,   // host working clone
    pub store: String,  // host shared `.claude` store
    #[serde(default = "default_agent")]
    pub agent: String, // "claude" (codex later)
}

fn default_agent() -> String {
    "claude".into()
}

/// skein's home dir (`$SKEIN_HOME`, else `~/.skein`): holds `repos.json`, the embedded `kit/`, and
/// (for URL-added repos) `repos/<id>/{work,store}`.
pub fn skein_home() -> PathBuf {
    if let Some(h) = env::var_os("SKEIN_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(h);
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".skein")
}

fn repos_json() -> PathBuf {
    skein_home().join("repos.json")
}

/// skein's app settings (`~/.skein/config.json`) — the toggles the cockpit exposes. Every field has a
/// serde default so old/partial files keep working as new settings are added. Matching `$SKEIN_*` env
/// vars still override these at runtime (env wins) for headless/CI use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Have the kit `apt-get install tmux` in a box when it's missing (so the agent gets a shared
    /// session and reconnects re-attach the same terminal). Off ⇒ the box uses whatever's installed.
    #[serde(default = "default_true")]
    pub install_tmux: bool,
    /// Seed the host `gh` token into sbx (global) at startup so boxes can fetch/push/open PRs.
    /// Off is the UI equivalent of `$SKEIN_NO_GH_SECRET`.
    #[serde(default = "default_true")]
    pub seed_gh_secret: bool,
    /// Overwrite an already-set sbx `github` secret with the current token (refresh on rotation).
    /// On is the UI equivalent of `$SKEIN_FORCE_GH_SECRET`.
    #[serde(default)]
    pub force_gh_secret: bool,
    /// Default agent for newly-added repos / boxes (the per-runtime seam). `claude` for now.
    #[serde(default = "default_agent")]
    pub default_agent: String,
    /// Base branch for `gh pr create` / merge when a repo doesn't specify one. Empty ⇒ repo default.
    /// UI equivalent of `$SKEIN_BASE`.
    #[serde(default)]
    pub base_branch: String,
    /// Confirm before a destructive **Destroy** (clone-mode boxes lose unpushed commits). The cockpit
    /// reads this to decide whether to prompt.
    #[serde(default = "default_true")]
    pub confirm_destroy: bool,
    /// Path to a private SSH key (host) to load into the host ssh-agent so sbx forwards it into boxes
    /// for SSH git push (`git@…`/`ssh://` remotes). Empty ⇒ rely on whatever's already in the agent.
    /// `$SKEIN_SSH_KEY` overrides. The key never enters a box — only the agent socket is forwarded.
    #[serde(default)]
    pub ssh_key: String,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            install_tmux: true,
            seed_gh_secret: true,
            force_gh_secret: false,
            default_agent: default_agent(),
            base_branch: String::new(),
            confirm_destroy: true,
            ssh_key: String::new(),
        }
    }
}

/// Ensure the configured SSH key is loaded in the host ssh-agent, so sbx forwards it into boxes for
/// SSH git push. `$SKEIN_SSH_KEY` overrides the config. No key configured ⇒ no-op (the agent's
/// existing keys, if any, are forwarded as-is). The key itself never enters a box — only the agent
/// socket is forwarded (docs.docker.com/ai/sandboxes/security/credentials). Best-effort: returns Err
/// (logged by callers) but never panics. Idempotent — `ssh-add` of an already-loaded key is a no-op.
pub fn ensure_ssh_key() -> Result<(), String> {
    let key = env::var("SKEIN_SSH_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| load_config().ssh_key);
    let key = key.trim();
    if key.is_empty() {
        return Ok(());
    }
    let expanded = expand_tilde(key);
    if !Path::new(&expanded).exists() {
        return Err(format!("ssh key not found: {expanded}"));
    }
    let out = Command::new("ssh-add")
        .arg(&expanded)
        .output()
        .map_err(|e| format!("ssh-add: {e} (is an ssh-agent running? $SSH_AUTH_SOCK)"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ssh-add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Expand a leading `~/` to `$HOME` (ssh-add doesn't do shell tilde expansion when called directly).
fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

fn config_json() -> PathBuf {
    skein_home().join("config.json")
}

/// Load skein's app settings (defaults if the file is absent/malformed).
pub fn load_config() -> Config {
    fs::read_to_string(config_json())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Persist skein's app settings to `~/.skein/config.json`.
pub fn save_config(c: &Config) -> Result<(), String> {
    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    write_atomic(&config_json(), &home, &bytes)
}

/// Every repo skein manages (empty if none added yet / file absent or malformed).
pub fn load_repos() -> Vec<Repo> {
    fs::read_to_string(repos_json())
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Repo>>(&t).ok())
        .unwrap_or_default()
}

/// Persist the repo list to `~/.skein/repos.json` (pretty, atomic).
pub fn save_repos(repos: &[Repo]) -> Result<(), String> {
    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(repos).map_err(|e| e.to_string())?;
    write_atomic(&repos_json(), &home, &bytes)
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

/// The branch *slug* encoded in a box name (`<id>-<branch-slug>` → `<branch-slug>`). This is the
/// sanitized form (no `/`), used only as a fallback when the real branch isn't otherwise known — the
/// authoritative branch (which may contain `/`, e.g. `feat/auth`) is carried in the launch spec and
/// recovered host-side from git. See [`box_name`].
pub fn branch_from_box(name: &str, repo: &Repo) -> String {
    name.strip_prefix(&format!("{}-", repo.id))
        .unwrap_or(name)
        .to_string()
}

/// Sbx sandbox names can't carry every branch character (notably `/`), so the box name is a *slug* of
/// the branch: anything outside `[A-Za-z0-9._-]` becomes `-`, runs collapse, ends trimmed. The real
/// branch (`feat/auth`) is preserved separately (launch spec → `git checkout`); only the *name* is
/// slugged (`<repo>-feat-auth`). Same branch ⇒ same name (stable), so reconnect/lookup are consistent.
pub fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// The sbx box name for a repo + branch: `<repo-id>-<branch-slug>`.
pub fn box_name(repo_id: &str, branch: &str) -> String {
    format!("{}-{}", repo_id, slug(branch))
}

/// Is `source` a git URL (clone it) versus a local path (use in place)?
fn is_git_url(source: &str) -> bool {
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
fn is_ssh_url(s: &str) -> bool {
    s.starts_with("git@") || s.starts_with("ssh://")
}

/// The `origin` URL of the clone at `work`, if any.
fn remote_origin_url(work: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", work, "remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// A heads-up if a managed repo's `origin` is SSH: in-box push then depends on the host SSH agent
/// (sbx forwards `SSH_AUTH_SOCK`), so it works only when that agent has the key loaded — else switch
/// to HTTPS. `None` for HTTPS / no origin. Surfaced by `skein add` + the cockpit so it's known
/// up-front. Not an error; SSH is supported, just host-agent-dependent.
pub fn ssh_remote_warning(work: &str) -> Option<String> {
    let url = remote_origin_url(work)?;
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

/// Host component of an SSH git URL (for the network-policy hint). `None` if unparseable.
fn host_of(url: &str) -> Option<&str> {
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.split(':').next();
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map(|(_, h)| h).unwrap_or(rest);
        return rest.split(['/', ':']).next();
    }
    None
}

/// Best-effort `git@github.com:org/repo.git` / `ssh://git@host/org/repo.git` → `https://host/org/repo.git`.
/// Returns `None` for shapes we don't recognise (caller just omits the suggestion).
fn ssh_to_https(url: &str) -> Option<String> {
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
fn repo_id_from_source(source: &str) -> String {
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
pub fn add_repo(source: &str, id: Option<&str>, agent: Option<&str>) -> Result<Repo, String> {
    let id = id
        .map(|s| s.to_string())
        .unwrap_or_else(|| repo_id_from_source(source));
    if id.is_empty() {
        return Err("could not derive a repo id — pass one explicitly".into());
    }
    let home = skein_home();
    let store = home.join("repos").join(&id).join("store").join(".claude");

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
            let out = Command::new("git")
                .args(["clone", source])
                .arg(&work)
                .output()
                .map_err(|e| format!("git clone: {e}"))?;
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
    };
    let mut repos = load_repos();
    repos.retain(|r| r.id != id); // replace any existing entry with the same id
    repos.push(repo.clone());
    repos.sort_by(|a, b| a.id.cmp(&b.id));
    save_repos(&repos)?;
    Ok(repo)
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
    let token = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|e| format!("gh auth token: {e}"))?;
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
    let out = Command::new("sbx")
        .args(&args)
        .output()
        .map_err(|e| format!("sbx secret set: {e}"))?;
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

const KIT_SPEC_YAML: &str = include_str!("kit/spec.yaml");

/// Install skein's own sbx kit into `~/.skein/kit/spec.yaml` so native launch can `--kit` it without
/// the repo shipping a kit. Embedded via `include_str!`; rewritten each call (idempotent).
pub fn ensure_kit() -> Result<PathBuf, String> {
    let kit = skein_home().join("kit");
    fs::create_dir_all(&kit).map_err(|e| format!("mkdir {}: {e}", kit.display()))?;
    let spec = kit.join("spec.yaml");
    fs::write(&spec, KIT_SPEC_YAML).map_err(|e| format!("write {}: {e}", spec.display()))?;
    Ok(kit)
}

/// Provision a shared store from scratch at `store` (a `.claude` dir): the dirs skein + the probe
/// need, plus the turn-state probe itself. Idempotent. Lets a brand-new repo work with no existing
/// `.claude` and no repo store-template.
pub fn ensure_store(store: &Path) -> Result<(), String> {
    for d in ["mailbox", "status", "tasks", "skein/launch", "skein/bin"] {
        let p = store.join(d);
        fs::create_dir_all(&p).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
    }
    ensure_probe_in(store)
}

/// Record, for box `name`, what its kit startup needs (branch + agent) at
/// `<store>/skein/launch/<name>.json`. The kit finds this file (the store is mounted) and checks out
/// the branch — our env-free channel into the box, since `sbx run --env` is unconfirmed.
fn write_launch_spec(name: &str, branch: &str, repo: &Repo) -> Result<(), String> {
    let dir = Path::new(&repo.store).join("skein").join("launch");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "branch": branch,
        "agent": repo.agent,
        "install_tmux": load_config().install_tmux,
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(format!("{name}.json")), &dir, &bytes)
}

/// The host shell command that launches a new box for `branch`. Override with
/// $SKEIN_LAUNCH_CMD (a template; `{branch}` is substituted); default assumes
/// `setup-sandbox.sh` is on PATH.
pub fn launch_command(name: &str, branch: &str) -> String {
    if let Ok(t) = env::var("SKEIN_LAUNCH_CMD") {
        if !t.is_empty() {
            return t
                .replace("{branch}", &sh_quote(branch))
                .replace("{name}", &sh_quote(name));
        }
    }
    native_launch_command(name, branch)
}

/// skein's own launch command, used when `$SKEIN_LAUNCH_CMD` is unset — so a box can be created
/// without the repo shipping a `setup-sandbox.sh`. Faithful to that script's launch line:
///   `sbx run --clone [--kit <kit>] --name <name> <agent> . <store>`
/// The box's bootstrap derives the branch from the name (`thing-<branch>` → `<branch>`) and checks
/// it out, so no branch arg is needed. `agent` (`$SKEIN_AGENT`, default `claude`) is the per-runtime
/// seam; `kit` (`$SKEIN_KIT`, resolved under `$SKEIN_REPO`) wires the shared store into the clone and
/// runs the bootstrap; `store` (`$SKEIN_STORE`, else the store skein already reads) is mounted so the
/// kit can link it. Runs with cwd `$SKEIN_REPO`, so `.` is the repo workspace.
fn native_launch_command(name: &str, branch: &str) -> String {
    // Repo-managed path: if the box belongs to a registered repo, build entirely from `repos.json`
    // + skein's own kit — no `SKEIN_REPO`/`SKEIN_KIT` env, no repo-side script.
    if let Some(repo) = repo_for_box(name) {
        return repo_launch_command(name, &repo, branch);
    }
    let agent = env::var("SKEIN_AGENT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".into());
    let mut parts: Vec<String> = vec!["sbx".into(), "run".into(), "--clone".into()];
    if let Some(kit) = env::var("SKEIN_KIT").ok().filter(|s| !s.is_empty()) {
        parts.push("--kit".into());
        parts.push(sh_quote(&resolve_under_repo(&kit)));
    }
    parts.push("--name".into());
    parts.push(sh_quote(name));
    parts.push(sh_quote(&agent)); // sbx agent positional
    parts.push(".".into()); // the repo workspace (cwd is $SKEIN_REPO)
    if let Some(store) = launch_store() {
        parts.push(sh_quote(&store));
    }
    parts.join(" ")
}

/// Launch line for a registered repo, built from `repos.json` + skein's embedded kit:
///   `sbx run --clone --kit <home>/kit --name <id>-<branch> <wrapper> <work> <store>`
/// The agent positional is a registered sbx agent **name** (`sbx run` only accepts the built-in set:
/// claude, codex, …; each has its own image, so it can't be a path or a wrapper command). It's the
/// repo's `agent` (`$SKEIN_AGENT` overrides). `<work>` is the host clone; `<store>` is mounted at its
/// host path so the kit links it in. The kit checks out the branch (from the launch spec) before the
/// agent starts. Reconnect is sbx-native — `sbx run --name <box>` re-attaches (see [`attach_argv`]),
/// so no in-box session wrapper is needed. Side effect: writes the launch spec + ensures kit/store
/// (best-effort; a failure only logs, the command still builds).
fn repo_launch_command(name: &str, repo: &Repo, branch: &str) -> String {
    // The real branch (may contain `/`, e.g. feat/auth) comes from the caller; the box *name* is its
    // slug. Fall back to the name's slug only if the caller didn't pass one (e.g. a bare relaunch).
    let branch = if branch.trim().is_empty() {
        branch_from_box(name, repo)
    } else {
        branch.trim().to_string()
    };
    if let Err(e) = ensure_kit() {
        eprintln!("skein: ensure_kit: {e}");
    }
    if let Err(e) = ensure_store(Path::new(&repo.store)) {
        eprintln!("skein: ensure_store: {e}");
    }
    if let Err(e) = write_launch_spec(name, &branch, repo) {
        eprintln!("skein: write_launch_spec: {e}");
    }
    let kit = skein_home().join("kit");
    let agent = env::var("SKEIN_AGENT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| (!repo.agent.is_empty()).then(|| repo.agent.clone()))
        .unwrap_or_else(|| "claude".into());
    [
        "sbx".to_string(),
        "run".into(),
        "--clone".into(),
        "--kit".into(),
        sh_quote(&kit.to_string_lossy()),
        "--name".into(),
        sh_quote(name),
        sh_quote(&agent), // registered sbx agent name (claude | codex | …)
        sh_quote(&repo.work),
        sh_quote(&repo.store),
    ]
    .join(" ")
}

/// Resolve a possibly-relative path against `$SKEIN_REPO` (the dir launches run in), so a relative
/// `$SKEIN_KIT` behaves like a relative `$SKEIN_LAUNCH_CMD`.
fn resolve_under_repo(p: &str) -> String {
    if Path::new(p).is_absolute() {
        return p.to_string();
    }
    match env::var("SKEIN_REPO").ok().filter(|s| !s.is_empty()) {
        Some(repo) => Path::new(&repo).join(p).to_string_lossy().into_owned(),
        None => p.to_string(),
    }
}

/// The shared store to mount into a launched box: `$SKEIN_STORE`, else the store skein already reads
/// (parent of `sandboxes.json`). `None` ⇒ omit the mount.
fn launch_store() -> Option<String> {
    if let Some(s) = env::var("SKEIN_STORE").ok().filter(|s| !s.is_empty()) {
        return Some(s);
    }
    store_dir().map(|p| p.to_string_lossy().into_owned())
}

/// The box's branch, from the registry.
pub fn branch_of(name: &str) -> Option<String> {
    // registry first (no subprocess); else read it host-side from the box's workspace.
    if let Ok((boxes, _)) = load_registry() {
        if let Some(b) = boxes
            .get(name)
            .map(|b| b.branch.clone())
            .filter(|b| !b.is_empty() && b != "?")
        {
            return Some(b);
        }
    }
    git_branch_for(&lookup_dir(name)?)
}

/// Run a program in the repo dir ($SKEIN_REPO, else cwd); returns (stdout, stderr, exit-code).
fn run_capture(prog: &str, args: &[&str]) -> Result<(String, String, i32), String> {
    let mut c = Command::new(prog);
    c.args(args);
    if let Ok(repo) = env::var("SKEIN_REPO") {
        if !repo.is_empty() {
            c.current_dir(repo);
        }
    }
    let out = c
        .output()
        .map_err(|e| format!("{prog} not runnable: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    ))
}

fn run_shell(cmd: &str) -> Result<(String, String, i32), String> {
    run_capture("sh", &["-c", cmd])
}

/// Save a pasted image into the box and return its in-box path. The agent runs *inside* the sandbox
/// (and can't see the user's clipboard), so the bytes are streamed through `sbx exec -i <box> sh -c
/// 'cat > <path>'` to land at `/tmp/skein-paste-<unique>.<ext>`, which the agent can then read. Only
/// `-i` (no `-t`) so the binary isn't mangled by a pty. `ext` is sanitised to a short alnum suffix.
pub fn save_pasted_image(name: &str, ext: &str, bytes: &[u8]) -> Result<String, String> {
    use std::io::Write as _;
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    if bytes.is_empty() {
        return Err("empty image".into());
    }
    let ext: String = ext
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(5)
        .collect();
    let ext = if ext.is_empty() {
        "png".into()
    } else {
        ext.to_ascii_lowercase()
    };
    // unique-enough: millis-since-epoch + a process-local counter (no collisions within a run).
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = format!("/tmp/skein-paste-{ms}-{n}.{ext}");
    let inner = format!("cat > {}", sh_quote(&path));
    let mut child = Command::new("sbx")
        .args(["exec", "-i", name, "sh", "-c", &inner])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("sbx exec not runnable: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("no stdin pipe")?
        .write_all(bytes)
        .map_err(|e| format!("writing image to box: {e}"))?; // ChildStdin drops here → EOF for cat
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "sbx exec failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(path)
}

/// Merge-readiness for a box: does a PR exist, its state, and CI checks. All host-side via `gh`.
#[derive(Debug, Default, Serialize)]
pub struct ShipStatus {
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub pr_state: Option<String>,
    /// "passing" | "pending" | "failing" | "none"
    pub checks: Option<String>,
    pub checks_text: Option<String>,
}

pub fn ship_status(name: &str) -> ShipStatus {
    let mut s = ShipStatus::default();
    let branch = match branch_of(name) {
        Some(b) => b,
        None => return s,
    };
    s.branch = Some(branch.clone());
    if let Ok((out, _, 0)) = run_capture("gh", &["pr", "view", &branch, "--json", "url,state"]) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) {
            s.pr_url = v.get("url").and_then(|x| x.as_str()).map(String::from);
            s.pr_state = v.get("state").and_then(|x| x.as_str()).map(String::from);
        }
    }
    if s.pr_url.is_some() {
        if let Ok((out, err, code)) = run_capture("gh", &["pr", "checks", &branch]) {
            s.checks = Some(
                match code {
                    0 => "passing",
                    8 => "pending",
                    _ => "failing",
                }
                .into(),
            );
            let text = if out.trim().is_empty() { err } else { out };
            s.checks_text = Some(text.lines().take(10).collect::<Vec<_>>().join("\n"));
        }
    }
    s
}

/// Open (or report) a PR for the box's branch. Override the command with $SKEIN_PR_CMD
/// (`{branch}`/`{name}` substituted); default `gh pr create --head <branch> --fill`
/// (+ `--base $SKEIN_BASE` if set). Returns the PR URL on success.
pub fn create_pr(name: &str) -> Result<String, String> {
    let branch = branch_of(name).ok_or("box has no branch in the registry")?;
    let (out, err, code) = match env::var("SKEIN_PR_CMD") {
        Ok(c) if !c.is_empty() => run_shell(
            &c.replace("{branch}", &sh_quote(&branch))
                .replace("{name}", &sh_quote(name)),
        )?,
        _ => {
            let mut args = vec!["pr", "create", "--head", &branch, "--fill"];
            // base branch: $SKEIN_BASE wins (headless), else the cockpit setting, else repo default.
            let base = env::var("SKEIN_BASE")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| load_config().base_branch);
            if !base.is_empty() {
                args.push("--base");
                args.push(&base);
            }
            run_capture("gh", &args)?
        }
    };
    if code == 0 {
        let url = out
            .split_whitespace()
            .chain(err.split_whitespace())
            .find(|w| w.contains("github.com") && w.contains("/pull/"))
            .map(String::from)
            .unwrap_or_else(|| out.trim().to_string());
        Ok(url)
    } else {
        let msg = if err.trim().is_empty() { out } else { err };
        Err(msg.trim().to_string())
    }
}

/// Merge the box's PR (the step that finishes the loop). Override with $SKEIN_MERGE_CMD
/// (`{branch}`/`{name}` substituted); default `gh pr merge <branch> <method>` where method is
/// $SKEIN_MERGE_METHOD (default `--squash`). Returns a short status line on success.
pub fn merge_pr(name: &str) -> Result<String, String> {
    let branch = branch_of(name).ok_or("box has no branch in the registry")?;
    let (out, err, code) = match env::var("SKEIN_MERGE_CMD") {
        Ok(c) if !c.is_empty() => run_shell(
            &c.replace("{branch}", &sh_quote(&branch))
                .replace("{name}", &sh_quote(name)),
        )?,
        _ => {
            let method = env::var("SKEIN_MERGE_METHOD")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "--squash".into());
            run_capture("gh", &["pr", "merge", &branch, &method])?
        }
    };
    if code == 0 {
        let msg = out.trim();
        Ok(if msg.is_empty() {
            "merged".into()
        } else {
            msg.to_string()
        })
    } else {
        let msg = if err.trim().is_empty() { out } else { err };
        Err(msg.trim().to_string())
    }
}

/// Resume a paused box headlessly — the one-click "continue" primitive (step 6). Sends `prompt` as
/// the box's next turn via a headless `claude --continue --print`, fire-and-forget: the agent runs
/// inside its box and reports progress back through its own hooks (working → waiting/done), so the
/// inbox updates over SSE without skein waiting on the (possibly minutes-long) run. Override the
/// command with $SKEIN_RESUME_CMD (`{name}`/`{prompt}` substituted; both shell-quoted).
///
/// This is NOT silent auto-continue: it only ever runs from an explicit human click, and the cockpit
/// limits *batch* use to boxes the fork-detector tagged a trivial "proceed?" — a real fork or a
/// permission prompt is never auto-resumed. The agent keeps its own "ask when in doubt" instinct, so
/// if a resumed turn hits genuine uncertainty it pauses again and returns to the inbox.
pub fn resume_box(name: &str, prompt: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let p = if prompt.trim().is_empty() {
        "Yes, please proceed."
    } else {
        prompt
    };
    let inner = match env::var("SKEIN_RESUME_CMD") {
        Ok(c) if !c.is_empty() => c
            .replace("{name}", &sh_quote(name))
            .replace("{prompt}", &sh_quote(p)),
        _ => format!(
            "sbx run --name {} -- --continue --print {}",
            sh_quote(name),
            sh_quote(p)
        ),
    };
    // fire-and-forget: nohup + `&` so the agent runs detached; the inner `sh` returns at once (and is
    // reaped here, no zombie) while the grandchild agent keeps running, reparented away from skein.
    let (_o, err, code) = run_shell(&format!("nohup {inner} >/dev/null 2>&1 &"))?;
    if code == 0 {
        Ok(())
    } else {
        Err(if err.trim().is_empty() {
            "resume failed to launch".into()
        } else {
            err.trim().to_string()
        })
    }
}

// ---------- step 7: rationed, lazy AI enrichment (subscription `claude -p`, no API key) ----------

/// skein runs *inside* an `sbx run` box where `claude` is logged in on the subscription, so every AI
/// call rides the SAME rate-limit window as the fleet doing the real work. AI is therefore OFF unless
/// you opt in with `$SKEIN_AI=on`, and even then it is lazy (on demand only), cached per turn-end, and
/// never a per-tick fleet sweep. The governing rule: AI may only *add* scrutiny, never remove it.
pub fn ai_enabled() -> bool {
    matches!(
        env::var("SKEIN_AI").ok().as_deref(),
        Some("on" | "1" | "true" | "yes")
    )
}

/// One-shot headless Haiku over the subscription: `claude -p --model <haiku>`. Returns trimmed
/// stdout, or None when AI is disabled / `claude` is absent / the call fails or times out — every
/// caller treats None as "fall back to the free deterministic path". `$SKEIN_CLAUDE_BIN` and
/// `$SKEIN_AI_MODEL` override the binary and model (and let tests stub the call).
fn claude_oneshot(prompt: &str) -> Option<String> {
    if !ai_enabled() {
        return None;
    }
    let bin = env::var("SKEIN_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".into());
    let model = env::var("SKEIN_AI_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude-haiku-4-5".into());
    // `timeout` guards a hung subprocess; if it isn't on PATH, fall back to an unguarded call.
    let run = |guarded: bool| {
        if guarded {
            Command::new("timeout")
                .args(["30", &bin, "-p", "--model", &model, prompt])
                .output()
        } else {
            Command::new(&bin)
                .args(["-p", "--model", &model, prompt])
                .output()
        }
    };
    let out = run(true).or_else(|_| run(false)).ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Memoize an AI result by a turn-end-scoped key, so repeat views of the same paused box don't
/// re-spend tokens; a new signal (new key) recomputes. Negatives are cached too — a flaky/None
/// answer shouldn't be retried on every poll within the same turn.
fn ai_cached(key: &str, compute: impl FnOnce() -> Option<String>) -> Option<String> {
    use std::sync::OnceLock;
    type Cache = std::collections::HashMap<String, Option<String>>;
    static CACHE: OnceLock<std::sync::Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(Cache::new()));
    if let Ok(m) = cache.lock() {
        if let Some(v) = m.get(key) {
            return v.clone();
        }
    }
    let v = compute();
    if let Ok(mut m) = cache.lock() {
        m.insert(key.to_string(), v.clone());
    }
    v
}

/// The text a box last reported (the blocking prompt when waiting on you, else its last message).
fn signal_text(sig: &SessionSignal) -> &str {
    if sig.kind == "notification" {
        &sig.prompt
    } else {
        &sig.last_message
    }
}

/// A one-line AI narration of what a box last did or is asking — the lazy fallback for the digest
/// when the box keeps no journal. One rationed Haiku call, cached per turn-end; None when AI is off
/// or unavailable (the digest then just shows commits + the raw last message). On demand only.
pub fn narrate(name: &str) -> Option<String> {
    if !valid_name(name) || !ai_enabled() {
        return None;
    }
    let sig = session_signal(name)?;
    let text = signal_text(&sig);
    if text.trim().is_empty() {
        return None;
    }
    let key = format!("narrate:{name}:{}", sig.ts);
    let prompt = format!(
        "Summarise in ONE plain sentence (max 20 words) what this autonomous coding agent just did \
         or is asking. No preamble, no quotes — just the sentence.\n\nAgent message:\n{}",
        text.chars().take(2000).collect::<String>()
    );
    ai_cached(&key, || claude_oneshot(&prompt))
}

/// Conservative AI safety check for batch-resume: given a box the heuristic tagged a trivial
/// "proceed?", ask Haiku whether it is actually a real decision the human should make. Returns
/// `Some(true)` = HOLD it back, unless Haiku affirmatively says ROUTINE — so a flaky, garbled, or
/// absent answer errs toward asking you, never toward auto-continuing. `None` means AI is off (the
/// caller then trusts the heuristic verdict). AI can only *add* a hold here, never grant a continue
/// the heuristic wouldn't already allow.
fn ai_says_hold(name: &str) -> Option<bool> {
    if !ai_enabled() {
        return None;
    }
    let sig = session_signal(name)?;
    let text = signal_text(&sig);
    if text.trim().is_empty() {
        return None;
    }
    let key = format!("gate:{name}:{}", sig.ts);
    let prompt = format!(
        "An autonomous coding agent ended its turn with the message below. Reply with ONE word only: \
         ROUTINE if it is merely asking permission to continue with obvious, safe next steps; or \
         DECISION if it is asking the human to make a real choice or judgement the agent should not \
         make alone.\n\nMessage:\n{}",
        text.chars().take(2000).collect::<String>()
    );
    let ans = ai_cached(&key, || claude_oneshot(&prompt))?;
    // err toward HOLD: only an explicit ROUTINE clears a box for auto-continue
    Some(!ans.to_uppercase().contains("ROUTINE"))
}

/// Resume several paused boxes in one gesture (step 6). When AI is enabled (step 7), each box is run
/// past [`ai_says_hold`] first; any the model flags as a genuine decision are HELD (not auto-resumed)
/// and returned so the cockpit can route you to them. Returns (resumed, held). With AI off this
/// resumes every (valid) box — the heuristic already restricted the set to `proceed`.
pub fn resume_batch(names: &[String]) -> (Vec<String>, Vec<String>) {
    let mut resumed = Vec::new();
    let mut held = Vec::new();
    for name in names {
        if !valid_name(name) {
            continue;
        }
        if ai_says_hold(name) == Some(true) {
            held.push(name.clone());
            continue;
        }
        if resume_box(name, "").is_ok() {
            resumed.push(name.clone());
        }
    }
    (resumed, held)
}

/// The host shell command that stops a running box — halts the sandbox so it stops consuming compute,
/// while keeping it so you can resume it later (e.g. via attach). Override with $SKEIN_STOP_CMD;
/// `{name}` is substituted and shell-quoted. Default `sbx stop {name}`. Non-destructive — no commits
/// are lost; the box simply goes stale in the registry until resumed.
pub fn stop_command(name: &str) -> String {
    if let Ok(t) = env::var("SKEIN_STOP_CMD") {
        if !t.is_empty() {
            return t.replace("{name}", &sh_quote(name));
        }
    }
    format!("sbx stop {}", sh_quote(name))
}

/// Stop a box: run `stop_command` to halt the running sandbox. The box stays listed (it goes stale
/// until resumed) — this only frees the compute, it does not delist or destroy.
pub fn stop_box(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let (_out, err, code) = run_shell(&stop_command(name))?;
    if code != 0 {
        return Err(format!("stop failed (exit {code}): {}", err.trim()));
    }
    Ok(())
}

/// Delist a box from the cockpit: remove its registry entry and append it to `<store>/history.jsonl`.
/// Used after `destroy_box` tears the sandbox down, so a removed sandbox doesn't linger as stale.
/// Touches only skein's own records, never the sandbox.
fn delist_box(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    use fs2::FileExt;
    use std::io::Write as _;
    let path = locate_registry()?;
    let store = path
        .parent()
        .ok_or("registry has no parent dir")?
        .to_path_buf();

    // Share the box hooks' flock discipline (sandbox-bootstrap.sh): an exclusive advisory lock on
    // <store>/.sandboxes.lock held across the whole read-modify-write, then an atomic temp+rename
    // so a concurrent heartbeat can't be lost mid-write and a reader can't see a truncated file.
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(store.join(".sandboxes.lock"))
        .map_err(|e| format!("lock open: {e}"))?;
    lock.lock_exclusive().map_err(|e| format!("lock: {e}"))?;

    let result = (|| {
        let data = fs::read_to_string(&path).map_err(|e| format!("reading registry: {e}"))?;
        let mut v: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| format!("parsing registry: {e}"))?;
        let obj = v.as_object_mut().ok_or("registry is not a JSON object")?;
        let mut removed = obj.remove(name).ok_or("no such box in the registry")?;

        if let Some(m) = removed.as_object_mut() {
            m.insert("name".into(), serde_json::Value::String(name.into()));
            m.insert(
                "archivedAt".into(),
                serde_json::Value::String(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            );
        }
        // history is append-only — don't rewrite the whole log (racy + O(n)).
        let hist = store.join("history.jsonl");
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&hist) {
            let _ = writeln!(f, "{removed}");
        }
        let pretty = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        write_atomic(&path, &store, pretty.as_bytes())
    })();
    let _ = lock.unlock();
    result?;
    Ok(())
}

/// The host shell command that tears a box down — kills *and* removes the sandbox, reclaiming the
/// resources it consumed. Override with $SKEIN_DESTROY_CMD; `{name}` is
/// substituted and shell-quoted. Default `sbx rm -f {name}` — `-f` skips sbx's interactive
/// clone-removal confirmation (skein runs non-interactively, so without it `sbx rm` aborts with
/// exit 1). DESTRUCTIVE: in clone mode this removes the sandbox's clone, so any commits made in the
/// box that were never pushed/fetched are lost.
pub fn destroy_command(name: &str) -> String {
    if let Ok(t) = env::var("SKEIN_DESTROY_CMD") {
        if !t.is_empty() {
            return t.replace("{name}", &sh_quote(name));
        }
    }
    format!("sbx rm -f {}", sh_quote(name))
}

/// Destroy a box: tear the sandbox down via `destroy_command`, then delist it. The teardown must
/// succeed before we delist, so a failed `sbx rm` leaves the box on the board to retry rather than
/// orphaning a still-running sandbox you can no longer see. Destructive — see `destroy_command`.
pub fn destroy_box(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let (_out, err, code) = run_shell(&destroy_command(name))?;
    if code != 0 {
        return Err(format!("teardown failed (exit {code}): {}", err.trim()));
    }
    delist_box(name)
}

/// Read the full branch-vs-base patch a box wrote to `<store>/diffs/<name>.patch`.
/// (Boxes report their own diff because `sbx run` can't exec an arbitrary command in them.)
pub fn read_diff(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    // Prefer a fresh diff computed host-side when the box's dir is a git repo *on this
    // host* (direct mode); fall back to the patch the box reported (clone mode, where the
    // dir is an in-box path this host can't see). Computed on demand only — never per tick.
    if let Some(dir) = lookup_dir(name) {
        if let Some(p) = git_diff_for(&dir) {
            return Some(p);
        }
    }
    let reg = locate_registry().ok()?;
    let path = reg.parent()?.join("diffs").join(format!("{name}.patch"));
    fs::read_to_string(path).ok()
}

fn git_ok(dir: &str, args: &[&str]) -> bool {
    let mut a = vec!["-C", dir];
    a.extend_from_slice(args);
    Command::new("git")
        .args(&a)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The git ref the branch-vs-base diff is measured against, or None if `dir` isn't a git repo
/// here. Base = the first of origin/main|origin/master|main|master that resolves; we diff against
/// the merge-base→working-tree. With no common ancestor it falls back to `HEAD` (uncommitted only)
/// rather than exploding into an unrelated-history diff. Shared by the full diff and the shortstat.
fn git_range(dir: &str) -> Option<String> {
    if !Path::new(dir).join(".git").exists() {
        return None;
    }
    let base = ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|b| git_ok(dir, &["rev-parse", "--verify", "-q", b]));
    let merge_base = base.and_then(|b| {
        let o = Command::new("git")
            .args(["-C", dir, "merge-base", "HEAD", b])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !s.is_empty()).then_some(s)
    });
    Some(merge_base.unwrap_or_else(|| "HEAD".into()))
}

/// The full branch-vs-base patch for a working tree at `dir`. Output is capped so a huge patch
/// can't wedge the browser. None if `dir` isn't a git repo here (clone mode → reported patch).
fn git_diff_for(dir: &str) -> Option<String> {
    let range = git_range(dir)?;
    let out = Command::new("git")
        .args(["-C", dir, "diff", &range])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut patch = String::from_utf8_lossy(&out.stdout).to_string();
    const CAP: usize = 2_000_000;
    if patch.len() > CAP {
        patch.truncate(CAP);
        patch.push_str("\n\n# … diff truncated by skein (too large to render) …\n");
    }
    Some(patch)
}

/// The host-side branch-vs-base shortstat for `dir` (same range as [`git_diff_for`]), so the
/// fleet's diff± badge matches the diff *pane* for direct-mode boxes instead of drifting from the
/// box-reported number. None if not a host git repo or there are no changes.
fn git_diffstat_for(dir: &str) -> Option<DiffStat> {
    let range = git_range(dir)?;
    let out = Command::new("git")
        .args(["-C", dir, "diff", "--shortstat", &range])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // e.g. " 3 files changed, 12 insertions(+), 4 deletions(-)"
    let s = String::from_utf8_lossy(&out.stdout);
    let num = |kw: &str| {
        s.split(',')
            .find(|p| p.contains(kw))
            .and_then(|p| p.split_whitespace().next())
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let stat = DiffStat {
        files: num("file"),
        ins: num("insertion"),
        del: num("deletion"),
    };
    (stat.files != 0 || stat.ins != 0 || stat.del != 0).then_some(stat)
}

/// Host-computed diff± for the badge, cached briefly so the per-tick fleet stream doesn't fork a
/// `git` per box every poll. Clone-mode boxes (no host `.git`) cost only a `stat`, not a fork.
fn host_diffstat(name: &str, dir: &str) -> Option<DiffStat> {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    type Cache = std::collections::HashMap<String, (Instant, Option<DiffStat>)>;
    static CACHE: OnceLock<std::sync::Mutex<Cache>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(8);
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(Cache::new()));
    if let Ok(map) = cache.lock() {
        if let Some((t, v)) = map.get(name) {
            if t.elapsed() < TTL {
                return v.clone();
            }
        }
    }
    let v = git_diffstat_for(dir);
    if let Ok(mut map) = cache.lock() {
        map.insert(name.to_string(), (Instant::now(), v.clone()));
    }
    v
}

/// Look up a box's clone root (the `dir` it registered) by name.
pub fn lookup_dir(name: &str) -> Option<String> {
    // registry first (no subprocess); else the box's workspace from sbx, for boxes the registry
    // doesn't know about (sbx-only / not-yet-registered).
    if let Ok((boxes, _)) = load_registry() {
        if let Some(d) = boxes
            .get(name)
            .map(|b| b.dir.clone())
            .filter(|d| !d.is_empty())
        {
            return Some(d);
        }
    }
    fleet_boxes()?
        .into_iter()
        .find(|b| b.name == name)
        .map(|b| b.dir)
        .filter(|d| !d.is_empty())
}

// ---------- step 9: collision radar — warn before two boxes' work overwrites the same file ----------

/// The files a box changed on its branch (host-side `git diff --name-only`, or parsed from the
/// box-reported patch in clone mode where this host can't see the box's `.git`).
pub fn changed_files(name: &str) -> Vec<String> {
    if !valid_name(name) {
        return vec![];
    }
    if let Some(dir) = lookup_dir(name) {
        if let Some(range) = git_range(&dir) {
            if let Ok(out) = Command::new("git")
                .args(["-C", &dir, "diff", "--name-only", &range])
                .output()
            {
                if out.status.success() {
                    let v: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !v.is_empty() {
                        return v;
                    }
                }
            }
        }
    }
    // clone-mode fallback: pull the file list out of the patch the box reported.
    let mut files = Vec::new();
    if let Some(reg) = locate_registry()
        .ok()
        .and_then(|r| r.parent().map(|p| p.to_path_buf()))
    {
        if let Ok(patch) = fs::read_to_string(reg.join("diffs").join(format!("{name}.patch"))) {
            for line in patch.lines() {
                if let Some(rest) = line.strip_prefix("+++ b/") {
                    let f = rest.trim();
                    if !f.is_empty() && f != "/dev/null" {
                        files.push(f.to_string());
                    }
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

/// One file that more than one box has touched — a merge collision waiting to happen.
#[derive(Debug, Clone, Serialize)]
pub struct Collision {
    pub file: String,
    pub boxes: Vec<String>,
}

/// Files changed by two or more boxes at once, so you can reconcile before they fight at merge.
/// Cached briefly (forks a `git` per box) so the cockpit can poll it without hammering the host.
pub fn collisions() -> Vec<Collision> {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<std::sync::Mutex<(Instant, Vec<Collision>)>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(8);
    if let Some(lock) = CACHE.get() {
        if let Ok(g) = lock.lock() {
            if g.0.elapsed() < TTL {
                return g.1.clone();
            }
        }
    }
    let computed = compute_collisions();
    let lock = CACHE.get_or_init(|| std::sync::Mutex::new((Instant::now(), Vec::new())));
    if let Ok(mut g) = lock.lock() {
        *g = (Instant::now(), computed.clone());
    }
    computed
}

fn compute_collisions() -> Vec<Collision> {
    let boxes = match load_registry() {
        Ok((b, _)) => b,
        Err(_) => return vec![],
    };
    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in boxes.keys() {
        for f in changed_files(name) {
            by_file.entry(f).or_default().push(name.clone());
        }
    }
    let mut out: Vec<Collision> = by_file
        .into_iter()
        .filter_map(|(file, mut bs)| {
            bs.sort();
            bs.dedup();
            (bs.len() > 1).then_some(Collision { file, boxes: bs })
        })
        .collect();
    // most-contended files first, then alphabetical
    out.sort_by(|a, b| b.boxes.len().cmp(&a.boxes.len()).then(a.file.cmp(&b.file)));
    out
}

/// The full `sbx` argv that reconnects to box `name` (the leading program is `sbx`; this returns only
/// its arguments). `_dir` is unused for the default invocation but kept so the `{dir}` override stays
/// uniform.
///
/// `sbx run --name <box>` re-attaches to the persistent sandbox and starts an agent session reading
/// the agent from its spec (per `sbx run --help`). Each session is *fresh*, so to make a prior
/// conversation show up we pass the agent's own resume flag — `claude --continue`. The `--` is
/// load-bearing twice: `sbx` (a cobra CLI) would otherwise eat `--continue` as its own flag, and it
/// marks where sbx args end and the agent's begin. The resume flag is per-agent (the runtime seam):
/// claude → `--continue`; unknown agents get a bare re-attach. Override wholesale with
/// `$SKEIN_ATTACH_CMD`. (NB: sbx has no live-process attach — `--continue` resumes the transcript in
/// a new session; a *single shared live* PTY would need the agent running inside tmux, which the
/// registered-agent / per-agent-image model doesn't currently allow. See docs/self-sufficient.md.)
pub fn attach_argv(name: &str, _dir: &str) -> Vec<String> {
    let agent = repo_for_box(name)
        .map(|r| r.agent)
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "claude".into());
    let mut v = vec!["run".into(), "--name".into(), name.into()];
    v.extend(resume_args(&agent));
    v
}

/// The agent's "resume my last session" args, appended after `--` on re-attach (the per-runtime seam).
/// claude resumes with `--continue`; add other agents' flags here as they're supported. Empty ⇒ the
/// agent has no resume flag (bare re-attach).
fn resume_args(agent: &str) -> Vec<String> {
    match agent {
        "claude" => vec!["--".into(), "--continue".into()],
        // codex/others: fill in their resume flag when supported; bare re-attach until then.
        _ => vec![],
    }
}

/// `sbx` argv for an interactive *shell* in the box — a plain terminal to run commands in, separate
/// from the agent session. Uses a persistent `skein-shell` tmux session when tmux is present (so this
/// terminal survives reconnects), and falls back to a plain login shell when it isn't — so it never
/// breaks on an image without tmux. Override the whole command with $SKEIN_SHELL_CMD (`sh -c`).
pub fn shell_argv(name: &str) -> Vec<String> {
    vec![
        "exec".into(),
        "-it".into(),
        name.into(),
        "bash".into(),
        "-lc".into(),
        "command -v tmux >/dev/null 2>&1 && exec tmux new-session -A -s skein-shell || exec bash -li"
            .into(),
    ]
}

/// The narrative signal a box writes on each turn-end (box-session.sh): the last assistant
/// message (Stop) or the prompt it's blocked on (Notification). The free digest source —
/// the agent already wrote the words, so reading them costs no model tokens.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SessionSignal {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub kind: String, // "stop" | "notification"
    #[serde(default, rename = "lastMessage")]
    pub last_message: String,
    #[serde(default)]
    pub prompt: String,
}

/// Read `<store>/sessions/<name>.json` — the last narrative signal the box reported.
pub fn session_signal(name: &str) -> Option<SessionSignal> {
    if !valid_name(name) {
        return None;
    }
    let path = store_for_box(name)?
        .join("sessions")
        .join(format!("{name}.json"));
    let txt = fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// The box's *current task* — what it's doing right now, for the fleet's peripheral view. Prefers
/// the live signal (`box-task.sh` writes the in-progress TodoWrite item to `<store>/tasks/<name>.json`)
/// and falls back to the `next …` clause of the box's most recent journal line. No model call.
pub fn current_task(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    if let Some(p) = store_for_box(name).map(|d| d.join("tasks").join(format!("{name}.json"))) {
        if let Ok(txt) = fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                if let Some(t) = v.get("task").and_then(|t| t.as_str()).map(str::trim) {
                    if !t.is_empty() {
                        return first_line(t);
                    }
                }
            }
        }
    }
    journal_next(name)
}

/// The agent turn-state skein's own probe (box-status.sh) records for a box, from
/// `<store>/status/<name>.json` — the skein-owned replacement for the registry's `status` field.
pub fn current_status(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    let p = store_for_box(name)?
        .join("status")
        .join(format!("{name}.json"));
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(p).ok()?).ok()?;
    v.get("status")
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------- the turn-state probe (skein-owned, installed into the shared store) ----------
// skein ships these hook scripts and wires them into the store's settings.json, so a box reports
// working/waiting/needs-input + its current task without the *repo* providing anything. The store is
// linked into every box by the kit, so every box's Claude loads these hooks. See docs/self-sufficient.md.
const PROBE_STATUS_SH: &str = include_str!("probe/box-status.sh");
const PROBE_TASK_SH: &str = include_str!("probe/box-task.sh");
// Box-side path of the installed scripts (the store is linked at `<clone>/.claude`).
const PROBE_STATUS_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-status.sh";
const PROBE_TASK_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-task.sh";

/// Install skein's turn-state probe into the shared store: write the hook scripts to
/// `<store>/skein/bin/` and merge their hook wiring into `<store>/settings.json` (additive +
/// idempotent — the repo's own hooks are preserved, re-runs don't duplicate). The store is mounted
/// into every box, so this is how skein gets working/waiting/needs-input + task for any box without
/// the repo shipping a thing. Best-effort: returns Err but never panics.
pub fn ensure_probe() -> Result<(), String> {
    let store = store_dir().ok_or("no shared store to install the probe into")?;
    ensure_probe_in(&store)
}

/// Install/refresh the probe in *every* store skein reads — each managed repo's plus `store_dir()` —
/// so multi-repo fleets all report turn-state. Best-effort: errors are collected, not fatal.
pub fn ensure_probe_all() -> Result<(), String> {
    let mut errs = Vec::new();
    for store in all_stores() {
        if let Err(e) = ensure_probe_in(&store) {
            errs.push(format!("{}: {e}", store.display()));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

/// `ensure_probe` against a specific store dir (the store skein reads, or a freshly-provisioned one).
pub fn ensure_probe_in(store: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let bin = store.join("skein").join("bin");
    fs::create_dir_all(&bin).map_err(|e| format!("mkdir {}: {e}", bin.display()))?;
    for (file, body) in [
        ("box-status.sh", PROBE_STATUS_SH),
        ("box-task.sh", PROBE_TASK_SH),
    ] {
        let p = bin.join(file);
        fs::write(&p, body).map_err(|e| format!("write {}: {e}", p.display()))?;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o755));
    }
    let settings = store.join("settings.json");
    let current: serde_json::Value = fs::read_to_string(&settings)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let merged = settings_with_probe(&current);
    let bytes = serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?;
    write_atomic(&settings, store, &bytes)
}

/// Add skein's probe hooks to a `settings.json` value, preserving every existing hook and never
/// duplicating skein's own on a re-run (idempotent). Pure — the testable core of `ensure_probe`.
fn settings_with_probe(existing: &serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Value};
    // (event, command, optional matcher) — status on the turn-boundary events, task on TodoWrite.
    let entries: [(&str, String, Option<&str>); 4] = [
        (
            "UserPromptSubmit",
            format!("{PROBE_STATUS_CMD} working"),
            None,
        ),
        (
            "Notification",
            format!("{PROBE_STATUS_CMD} needs-input"),
            None,
        ),
        ("Stop", format!("{PROBE_STATUS_CMD} waiting"), None),
        ("PostToolUse", PROBE_TASK_CMD.to_string(), Some("TodoWrite")),
    ];
    let mut out = existing.clone();
    if !out.is_object() {
        out = json!({});
    }
    let root = out.as_object_mut().unwrap();
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().unwrap();
    for (event, cmd, matcher) in entries {
        let arr = hooks.entry(event).or_insert_with(|| json!([]));
        if !arr.is_array() {
            *arr = json!([]);
        }
        let arr = arr.as_array_mut().unwrap();
        // idempotent: skip if an entry already wires this exact command.
        let present = arr.iter().any(|e| {
            e.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                hs.iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(cmd.as_str()))
            })
        });
        if present {
            continue;
        }
        let mut entry = json!({ "hooks": [ { "type": "command", "command": cmd } ] });
        if let Some(m) = matcher {
            entry
                .as_object_mut()
                .unwrap()
                .insert("matcher".into(), Value::String(m.to_string()));
        }
        arr.push(entry);
    }
    out
}

/// The `next …` clause of the box's most recent journal line (the ritual is `did … / next … /
/// blocked-on …`) — a free, end-of-turn "what's next" when there's no live task signal.
fn journal_next(name: &str) -> Option<String> {
    let j = read_journal(name)?;
    for line in j.lines().rev() {
        // case-insensitive find of "next"; ASCII fold keeps byte offsets valid in the original line.
        let lower = line.to_ascii_lowercase();
        let Some(i) = lower.find("next") else {
            continue;
        };
        let rest = line[i + 4..].trim_start_matches([':', ' ', '-', '\t', '…']);
        // a clause runs to the next "/" separator, or to an inline "blocked" if not slash-delimited.
        let clause = rest.split('/').next().unwrap_or(rest);
        let clause = match clause.to_ascii_lowercase().find("blocked") {
            Some(b) => &clause[..b],
            None => clause,
        };
        if let Some(h) = first_line(clause.trim()) {
            return Some(h);
        }
    }
    None
}

/// The Notification signal carries only Claude Code's generic "waiting for your input" text — it says
/// a box needs you but not what it was doing. Detect it so the headline can fall back to the task.
fn is_generic_wait(h: &str) -> bool {
    h.to_ascii_lowercase().contains("waiting for your input")
}

/// Recent commit subjects on the box's branch (newest first) — the agent's own changelog,
/// a free, accurate "what was done" with no model call. Bounded; empty if not a host repo.
pub fn recent_commits(name: &str) -> Vec<String> {
    let dir = match lookup_dir(name) {
        Some(d) => d,
        None => return vec![],
    };
    let range = match git_range(&dir) {
        Some(r) => format!("{r}..HEAD"),
        None => return vec![],
    };
    let out = Command::new("git")
        .args(["-C", &dir, "log", "--format=%s", "-n", "20", &range])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![],
    }
}

/// The agent's own turn-end journal (`<dir>/.skein/journal.md`), if it keeps one — the best
/// "what was done" source because it's written with full context (see the CLAUDE.md ritual).
/// Returns the tail (last ~40 lines), capped, or None when the box keeps no journal.
pub fn read_journal(name: &str) -> Option<String> {
    let dir = lookup_dir(name)?;
    let txt = fs::read_to_string(Path::new(&dir).join(".skein").join("journal.md")).ok()?;
    let tail: Vec<&str> = txt.lines().rev().take(40).collect();
    let mut s: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    const CAP: usize = 4000;
    if s.len() > CAP {
        let cut = s.len() - CAP;
        s = format!("…\n{}", &s[cut..]);
    }
    Some(s).filter(|s| !s.trim().is_empty())
}

/// Why a turn ended — the heuristic fork-detector (step 5). A deterministic classification of
/// the agent's last message into what the human owes it, so the inbox can rank and (later) batch
/// only the *trivial* asks. Read-only: this never decides for you, it only routes attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pause {
    /// blocked on a permission/decision (the Notification signal) — most urgent
    NeedsInput,
    /// ended on a trivial "shall I proceed?" — a candidate for one-click batch resolve
    Proceed,
    /// ended on a real question that needs a judgement call — a genuine fork
    Fork,
    /// ended on a statement (work reported, nothing asked) — review at your leisure
    Statement,
    /// nothing to act on (still working, or no signal yet)
    #[default]
    None,
}

impl Pause {
    pub fn as_str(self) -> &'static str {
        match self {
            Pause::NeedsInput => "needs-input",
            Pause::Proceed => "proceed",
            Pause::Fork => "fork",
            Pause::Statement => "statement",
            Pause::None => "none",
        }
    }

    /// Inbox tie-breaker within a status tier: a genuine decision outranks a rote "proceed?",
    /// which outranks a bare statement. Lower = wants your attention sooner.
    pub fn rank(self) -> u8 {
        match self {
            Pause::NeedsInput => 0,
            Pause::Fork => 1,
            Pause::Proceed => 2,
            Pause::Statement => 3,
            Pause::None => 4,
        }
    }
}

/// First non-empty line of `s`, whitespace-collapsed and capped — the inbox headline. None when
/// `s` is blank.
fn first_line(s: &str) -> Option<String> {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    const CAP: usize = 120;
    let out = if collapsed.chars().count() > CAP {
        let mut t: String = collapsed.chars().take(CAP).collect();
        t.push('…');
        t
    } else {
        collapsed
    };
    Some(out).filter(|s| !s.is_empty())
}

/// Classify the last assistant message. `blocked` is true when the box's status is the
/// permission-prompt signal (Notification), which dominates regardless of the text.
pub fn classify_message(msg: &str, blocked: bool) -> Pause {
    if blocked {
        return Pause::NeedsInput;
    }
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        return Pause::None;
    }
    // Only the tail matters — a turn that *ends* on a question is asking; a "?" buried in the
    // middle of a long report is not. Look at the last non-empty line.
    let last_line = trimmed
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(trimmed)
        .trim();
    let ends_question = last_line.ends_with('?');
    let hay = last_line.to_lowercase();
    // Trivial "may I continue" endings — the residual that batch-resolve is for.
    const PROCEED: &[&str] = &[
        "shall i proceed",
        "should i proceed",
        "want me to proceed",
        "shall i continue",
        "should i continue",
        "want me to continue",
        "want me to go ahead",
        "shall i go ahead",
        "should i go ahead",
        "ok to proceed",
        "okay to proceed",
        "proceed?",
        "continue?",
        "go ahead?",
        "want me to start",
        "shall i start",
        "should i start",
        "ready to proceed",
        "let me know if you want me to",
    ];
    if PROCEED.iter().any(|p| hay.contains(p)) {
        return Pause::Proceed;
    }
    if ends_question {
        return Pause::Fork;
    }
    Pause::Statement
}

/// A glanceable "what happened here" for one box — the inbox detail view (steps 1–5), assembled
/// entirely from free sources: the registry state, the host-side diff/commits, the agent's own
/// journal, and its last reported message. No model tokens spent.
#[derive(Debug, Default, Serialize)]
pub struct SessionDigest {
    pub name: String,
    pub branch: String,
    pub state: String,
    pub tier: u8,
    pub age: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
    /// the agent's own changelog (recent commit subjects, newest first)
    pub commits: Vec<String>,
    /// the agent's turn-end journal, if it keeps `.skein/journal.md`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    /// the last assistant message (Stop) — the turn's own sign-off
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    /// the prompt the agent is blocked on (Notification), if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    /// when the box last reported a narrative signal (RFC3339)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_ts: Option<String>,
    /// why the turn ended — drives the inbox ranking and CTA
    pub pause: Pause,
}

/// Assemble the free session digest for a box. Pure reads + a couple of cached `git` calls;
/// never a model call. Returns None for an unknown box name.
pub fn session_digest(name: &str) -> Option<SessionDigest> {
    if !valid_name(name) {
        return None;
    }
    // Registry-independent: dir/branch from sbx + host git, state from sbx liveness + skein's probe,
    // the registry only a fallback. The box must be known to sbx or the registry (else nothing to show).
    let reg = load_registry().ok().and_then(|(b, _)| b.get(name).cloned());
    let live = box_liveness(name);
    let dir = lookup_dir(name).unwrap_or_default();
    if reg.is_none() && live.is_none() && dir.is_empty() {
        return None;
    }
    let branch = branch_of(name).unwrap_or_default();
    let sb = Sandbox {
        branch: branch.clone(),
        dir: dir.clone(),
        last_seen: reg
            .as_ref()
            .map(|r| r.last_seen.clone())
            .unwrap_or_default(),
        status: current_status(name)
            .or_else(|| reg.as_ref().map(|r| r.status.clone()))
            .filter(|s| !s.is_empty())
            .unwrap_or_default(),
    };
    let (state, tier) = sb.state_with(live);
    let blocked = state == "needs-input";

    let sig = session_signal(name);
    let last_message = sig
        .as_ref()
        .map(|s| s.last_message.clone())
        .filter(|m| !m.trim().is_empty());
    let blocked_on = sig
        .as_ref()
        .filter(|s| s.kind == "notification")
        .map(|s| s.prompt.clone())
        .filter(|p| !p.trim().is_empty());
    // classify over whichever text we have (the prompt when blocked, else the last message)
    let class_text = blocked_on
        .clone()
        .or_else(|| last_message.clone())
        .unwrap_or_default();
    let pause = match tier {
        3 => Pause::None, // still working — nothing owed
        _ => classify_message(&class_text, blocked),
    };

    Some(SessionDigest {
        name: name.to_string(),
        branch,
        state,
        tier,
        age: sb.age(),
        diff: host_diffstat(name, &dir),
        commits: recent_commits(name),
        journal: read_journal(name),
        last_message,
        blocked_on,
        signal_ts: sig.map(|s| s.ts).filter(|t| !t.is_empty()),
        pause,
    })
}

pub fn shorten(p: &str) -> String {
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() && p.starts_with(&home) {
            return format!("~{}", &p[home.len()..]);
        }
    }
    p.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    // Env vars are process-global; serialize the tests that read/write them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let d = env::temp_dir().join(format!(
            "skein-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn secs_ago(s: i64) -> String {
        (Utc::now() - chrono::Duration::seconds(s)).to_rfc3339()
    }
    fn sb(status: &str, last_seen: &str) -> Sandbox {
        Sandbox {
            branch: "b".into(),
            dir: "/d".into(),
            last_seen: last_seen.into(),
            status: status.into(),
        }
    }

    #[test]
    fn valid_name_guards_paths() {
        assert!(valid_name("thing-feature"));
        assert!(valid_name("box_123"));
        for bad in ["", "../etc", "a/b", "a\\b", "..", "x..y", "a\0b"] {
            assert!(!valid_name(bad), "should reject {bad:?}");
        }
        assert!(!valid_name(&"x".repeat(200)));
    }

    #[test]
    fn state_prefers_explicit_status() {
        assert_eq!(sb("needs-input", "").state(), ("needs-input".into(), 0));
        assert_eq!(sb("waiting", "").state().1, 1);
        assert_eq!(sb("done", "").state().1, 2);
        assert_eq!(sb("working", "").state().1, 3);
        assert_eq!(sb("compiling", "").state(), ("compiling".into(), 3)); // passthrough
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

    fn by_name<'a>(v: &'a [SbxBox], n: &str) -> &'a SbxBox {
        v.iter().find(|b| b.name == n).expect("box present")
    }

    #[test]
    fn parse_boxes_tolerates_shapes() {
        // NDJSON (Docker-CLI --json), mixed casing, an unknown status, and a stopped box.
        let nd = r#"{"name":"a","status":"running"}
{"SANDBOX":"b","STATUS":"stopped"}
{"name":"c","status":"paused"}"#;
        let v = parse_boxes(nd);
        assert_eq!(by_name(&v, "a").live, Some(Liveness::Running));
        assert_eq!(by_name(&v, "b").live, Some(Liveness::Stopped));
        assert_eq!(by_name(&v, "c").live, None); // unknown status → box still listed, falls back

        // A single JSON array document with a workspace path.
        let arr = r#"[{"name":"x","state":"running","workspace":"/repo/x"}]"#;
        assert_eq!(by_name(&parse_boxes(arr), "x").dir, "/repo/x");

        // A name-keyed object map: {name: {..}}.
        let obj = r#"{"z":{"status":"running"}}"#;
        assert_eq!(
            by_name(&parse_boxes(obj), "z").live,
            Some(Liveness::Running)
        );

        // Garbage / empty → no boxes, so fleet_boxes() returns None and the caller falls back.
        assert!(parse_boxes("not json").is_empty());
        assert!(parse_boxes("[]").is_empty());
        // Invalid names are skipped.
        assert!(parse_boxes(r#"[{"name":"../escape","status":"running"}]"#).is_empty());
    }

    #[test]
    fn parse_boxes_real_sbx_schema() {
        // The actual `sbx ls --json` shape (captured from the host): a {"sandboxes":[...]} wrapper,
        // lowercase name/status/agent, and a workspaces array (repo + shared .claude store).
        let real = r#"{
          "sandboxes": [
            { "name": "claude-agent-memory-consolidation", "id": "59eb", "agent": "claude",
              "status": "stopped", "workspaces": ["/x/agent-memory-consolidation"] },
            { "name": "thing-feat-calender", "id": "2b78", "agent": "claude", "status": "running",
              "ports": [{"host_ip":"127.0.0.1","host_port":49161,"sandbox_port":9418,"protocol":"tcp"}],
              "workspaces": ["/x/gadget-demo", "/x/skein-shared/.claude"] },
            { "name": "thing-master", "id": "f1bf", "agent": "claude", "status": "running",
              "workspaces": ["/x/thing"] }
          ]
        }"#;
        let v = parse_boxes(real);
        assert_eq!(v.len(), 3);
        let calender = by_name(&v, "thing-feat-calender");
        assert_eq!(calender.live, Some(Liveness::Running));
        assert_eq!(calender.agent, "claude");
        // the repo workspace is chosen, the shared .claude store is excluded.
        assert_eq!(calender.dir, "/x/gadget-demo");
        assert_eq!(
            by_name(&v, "claude-agent-memory-consolidation").live,
            Some(Liveness::Stopped)
        );
        assert_eq!(by_name(&v, "thing-master").dir, "/x/thing");
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
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let repos = vec![
            Repo {
                id: "web".into(),
                source: "s".into(),
                work: "/w".into(),
                store: "/s".into(),
                agent: "claude".into(),
            },
            Repo {
                id: "web-api".into(),
                source: "s".into(),
                work: "/w".into(),
                store: "/s".into(),
                agent: "claude".into(),
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
    fn repo_launch_command_uses_skein_kit_and_wrapper() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::remove_var("SKEIN_AGENT");
        let store = home.join("st").join(".claude");
        let repo = Repo {
            id: "thing".into(),
            source: "s".into(),
            work: "/work/thing".into(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
        };
        // box name is the slug `thing-feat-auth`; the REAL branch (with the slash) is feat/auth.
        let cmd = repo_launch_command("thing-feat-auth", &repo, "feat/auth");
        assert!(cmd.contains("sbx run --clone --kit"));
        assert!(cmd.contains("kit'") || cmd.contains("/kit"));
        assert!(cmd.contains("--name 'thing-feat-auth'"));
        assert!(cmd.contains("'claude'")); // registered sbx agent name as the positional
        assert!(cmd.contains("'/work/thing'"));
        // the launch spec carries the real branch (feat/auth) for the kit to check out — not the slug
        let spec = store
            .join("skein")
            .join("launch")
            .join("thing-feat-auth.json");
        let txt = fs::read_to_string(&spec).unwrap();
        assert!(txt.contains("\"branch\": \"feat/auth\""), "spec was: {txt}");
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
    fn ensure_store_and_kit_provision_layout() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("repos").join("x").join("store").join(".claude");
        ensure_store(&store).unwrap();
        for d in ["mailbox", "status", "tasks", "skein/launch", "skein/bin"] {
            assert!(store.join(d).is_dir(), "missing {d}");
        }
        // probe scripts + settings landed in the fresh store
        assert!(store.join("skein/bin/box-status.sh").is_file());
        assert!(store.join("settings.json").is_file());
        let kit = ensure_kit().unwrap();
        assert!(kit.join("spec.yaml").is_file());
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn settings_with_probe_is_additive_and_idempotent() {
        // an existing project settings.json with its own UserPromptSubmit + PreToolUse hooks.
        let existing = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "slice-gate.sh" } ] } ],
                "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "commit-guard.sh" } ] } ]
            },
            "statusLine": { "type": "command", "command": "statusline.sh" }
        });
        let merged = settings_with_probe(&existing);
        // existing hooks are preserved …
        assert_eq!(merged["statusLine"]["command"], "statusline.sh");
        let ups = merged["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert!(ups
            .iter()
            .any(|e| e["hooks"][0]["command"] == "slice-gate.sh"));
        // … and skein's are added.
        assert!(ups.iter().any(|e| e["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("box-status.sh working")));
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 1);
        let post = merged["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post[0]["matcher"], "TodoWrite");

        // idempotent: re-running adds nothing.
        let again = settings_with_probe(&merged);
        assert_eq!(
            again["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            ups.len()
        );
        assert_eq!(again["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn settings_with_probe_from_empty() {
        let merged = settings_with_probe(&serde_json::json!({}));
        for ev in ["UserPromptSubmit", "Notification", "Stop", "PostToolUse"] {
            assert_eq!(
                merged["hooks"][ev].as_array().unwrap().len(),
                1,
                "missing {ev}"
            );
        }
    }

    #[test]
    fn native_launch_command_builds_sbx_run() {
        let _g = ENV_LOCK.lock().unwrap();
        env::remove_var("SKEIN_LAUNCH_CMD");
        env::set_var("SKEIN_KIT", "/abs/kit");
        env::set_var("SKEIN_AGENT", "claude");
        env::set_var("SKEIN_STORE", "/abs/store");
        assert_eq!(
            launch_command("thing-feat-x", "feat-x"),
            "sbx run --clone --kit '/abs/kit' --name 'thing-feat-x' 'claude' . '/abs/store'"
        );
        // agent override is the per-runtime seam.
        env::set_var("SKEIN_AGENT", "codex");
        assert!(launch_command("thing-x", "x").contains(" 'codex' . "));
        // explicit SKEIN_LAUNCH_CMD still wins, with {branch}/{name} substituted + shell-quoted.
        env::set_var("SKEIN_LAUNCH_CMD", "setup.sh {branch} {name}");
        assert_eq!(launch_command("thing-x", "x"), "setup.sh 'x' 'thing-x'");
        for v in [
            "SKEIN_LAUNCH_CMD",
            "SKEIN_KIT",
            "SKEIN_AGENT",
            "SKEIN_STORE",
        ] {
            env::remove_var(v);
        }
    }

    #[test]
    fn age_buckets() {
        assert!(sb("", &secs_ago(5)).age().ends_with("s ago"));
        assert!(sb("", &secs_ago(120)).age().ends_with("m ago"));
        assert!(sb("", &secs_ago(7200)).age().ends_with("h ago"));
        assert_eq!(sb("", "nope").age(), "?");
    }

    #[test]
    fn sh_quote_escapes() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("x'; rm -rf ~"), "'x'\\''; rm -rf ~'");
    }

    #[test]
    fn shorten_replaces_home() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("HOME", "/home/me");
        assert_eq!(shorten("/home/me/work/x"), "~/work/x");
        assert_eq!(shorten("/other/x"), "/other/x");
    }

    #[test]
    fn load_views_promotes_only_the_self_box_when_quiet() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"thing-self":{{"branch":"s","dir":"/d","lastSeen":"{}","status":""}},
                    "thing-other":{{"branch":"o","dir":"/d","lastSeen":"{}","status":""}}}}"#,
                secs_ago(7200),
                secs_ago(7200)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_SELF", "thing-self");

        let v = load_views().unwrap();
        let self_v = v.iter().find(|b| b.name == "thing-self").unwrap();
        let other_v = v.iter().find(|b| b.name == "thing-other").unwrap();
        assert_eq!(self_v.state, "live"); // promoted despite a 2h-old lastSeen
        assert_eq!(other_v.state, "stale"); // a peer is never promoted

        env::remove_var("SKEIN_SELF");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn delist_box_removes_records_and_guards() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":"done"},
               "thing-y":{"branch":"y","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        delist_box("thing-x").unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        assert!(after.get("thing-x").is_none());
        assert!(after.get("thing-y").is_some()); // didn't clobber the rest
        let hist = fs::read_to_string(dir.join("history.jsonl")).unwrap();
        assert!(hist.contains("thing-x") && hist.contains("archivedAt"));
        assert!(dir.join(".sandboxes.lock").exists()); // shares the hooks' flock file
        assert!(delist_box("thing-x").is_err()); // already gone
        assert!(delist_box("../escape").is_err()); // name guard

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn stop_box_runs_command_without_delisting() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        let marker = dir.join("stopped");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        // a failed stop surfaces an error and changes nothing.
        env::set_var("SKEIN_STOP_CMD", "false");
        assert!(stop_box("thing-x").is_err());

        // a successful stop runs the command but leaves the box listed (stop ≠ delist).
        env::set_var(
            "SKEIN_STOP_CMD",
            format!("touch {}", sh_quote(marker.to_str().unwrap())),
        );
        stop_box("thing-x").unwrap();
        assert!(marker.exists());
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        assert!(after.get("thing-x").is_some()); // still listed

        assert!(stop_box("../escape").is_err()); // name guard

        env::remove_var("SKEIN_STOP_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn destroy_box_runs_teardown_then_delists() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        let marker = dir.join("torn-down");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""},
               "thing-y":{"branch":"y","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        // failed teardown must NOT delist — the box stays on the board to retry.
        env::set_var("SKEIN_DESTROY_CMD", "false");
        assert!(destroy_box("thing-x").is_err());
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        assert!(after.get("thing-x").is_some()); // still listed

        // successful teardown runs the command, then delists.
        env::set_var(
            "SKEIN_DESTROY_CMD",
            format!("touch {}", sh_quote(marker.to_str().unwrap())),
        );
        destroy_box("thing-x").unwrap();
        assert!(marker.exists()); // teardown ran
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        assert!(after.get("thing-x").is_none()); // delisted
        assert!(after.get("thing-y").is_some());

        // name guard runs before any teardown command.
        assert!(destroy_box("../escape").is_err());

        env::remove_var("SKEIN_DESTROY_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn git_diff_for_handles_repo_and_nonrepo() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // git not available in this environment
        }
        let dir = tempdir();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["-C", d];
            a.extend_from_slice(args);
            assert!(Command::new("git")
                .args(&a)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        fs::write(dir.join("a.txt"), "hello\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "x"]);
        fs::write(dir.join("a.txt"), "hello world\n").unwrap();
        let patch = git_diff_for(d).expect("a repo with changes yields a patch");
        assert!(patch.contains("hello world"));
        let stat = git_diffstat_for(d).expect("a repo with changes yields a shortstat");
        assert!(stat.files >= 1 && stat.ins + stat.del >= 1);

        let empty = tempdir(); // not a git repo → None, never explodes
        assert!(git_diff_for(empty.to_str().unwrap()).is_none());
        assert!(git_diffstat_for(empty.to_str().unwrap()).is_none());
    }

    #[test]
    fn classify_message_routes_pauses() {
        // a permission prompt dominates regardless of text
        assert_eq!(classify_message("anything", true), Pause::NeedsInput);
        // trivial "may I continue" endings → batch-resolvable
        assert_eq!(
            classify_message("Done with step 1.\nShall I proceed to step 2?", false),
            Pause::Proceed
        );
        assert_eq!(
            classify_message("Want me to continue?", false),
            Pause::Proceed
        );
        assert_eq!(
            classify_message("Tests pass. Should I go ahead and merge?", false),
            Pause::Proceed
        );
        // a real question that isn't a rote proceed → a genuine fork
        assert_eq!(
            classify_message("Two schemas are possible. Which one do you want?", false),
            Pause::Fork
        );
        // a report with a '?' earlier but a statement ending → not asking
        assert_eq!(
            classify_message("Is the cache stale? I checked and refreshed it.", false),
            Pause::Statement
        );
        // plain sign-off
        assert_eq!(
            classify_message("All done; pushed the branch.", false),
            Pause::Statement
        );
        assert_eq!(classify_message("   ", false), Pause::None);
    }

    #[test]
    fn session_signal_reads_store_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("sessions")).unwrap();
        fs::write(
            dir.join("sessions").join("thing-x.json"),
            r#"{"ts":"2026-06-28T00:00:00Z","kind":"stop","lastMessage":"hi"}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        let s = session_signal("thing-x").expect("signal present");
        assert_eq!(s.kind, "stop");
        assert_eq!(s.last_message, "hi");
        assert!(session_signal("thing-missing").is_none());
        assert!(session_signal("../escape").is_none());

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn shell_and_attach_argv_differ() {
        let _g = ENV_LOCK.lock().unwrap();
        // empty home ⇒ repo_for_box finds nothing ⇒ the default agent (claude) → `--continue`.
        env::set_var("SKEIN_HOME", tempdir());
        // attach re-attaches and resumes the agent's session (claude → `--continue`) so a prior
        // session shows up; the `--` separates sbx args from the agent's.
        assert_eq!(
            attach_argv("thing-x", "/d"),
            ["run", "--name", "thing-x", "--", "--continue"]
        );
        // a non-claude agent with no resume flag gets a bare re-attach.
        assert_eq!(resume_args("shell"), Vec::<String>::new());
        // shell prefers a persistent tmux session but falls back to a plain shell when tmux is absent.
        let sh = shell_argv("thing-x");
        assert_eq!(&sh[..3], ["exec", "-it", "thing-x"]);
        assert!(sh
            .last()
            .unwrap()
            .contains("tmux new-session -A -s skein-shell"));
        assert!(sh.last().unwrap().contains("exec bash -li"));
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn save_pasted_image_guards_before_spawning() {
        // both reject before any sbx exec — so the test never shells out.
        assert!(save_pasted_image("../escape", "png", b"x").is_err()); // name guard
        assert!(save_pasted_image("thing-x", "png", b"").is_err()); // empty image
    }

    #[test]
    fn current_task_prefers_live_then_journal() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let work = dir.join("work");
        fs::create_dir_all(work.join(".skein")).unwrap();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"thing-x":{{"branch":"x","dir":"{}","lastSeen":"","status":""}}}}"#,
                work.display()
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        // journal-only fallback: the `next …` clause of the last line, stopping at the blocked-on part.
        fs::write(
            work.join(".skein").join("journal.md"),
            "did: scaffolded api / next: wire the reducer / blocked-on: nothing\n",
        )
        .unwrap();
        assert_eq!(
            current_task("thing-x").as_deref(),
            Some("wire the reducer")
        );

        // the live task signal wins over the journal.
        fs::create_dir_all(dir.join("tasks")).unwrap();
        fs::write(
            dir.join("tasks").join("thing-x.json"),
            r#"{"ts":"2026-06-29T00:00:00Z","task":"Running the tests"}"#,
        )
        .unwrap();
        assert_eq!(
            current_task("thing-x").as_deref(),
            Some("Running the tests")
        );

        // an empty live task falls back to the journal again.
        fs::write(
            dir.join("tasks").join("thing-x.json"),
            r#"{"ts":"2026-06-29T00:00:00Z","task":""}"#,
        )
        .unwrap();
        assert_eq!(
            current_task("thing-x").as_deref(),
            Some("wire the reducer")
        );

        assert!(current_task("../escape").is_none()); // name guard

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn resume_box_guards_name_and_launches() {
        let _g = ENV_LOCK.lock().unwrap();
        assert!(resume_box("../escape", "go").is_err()); // name guard
                                                         // a stub command stands in for `sbx run …` so the test never spawns a real agent; the
                                                         // fire-and-forget wrapper returns Ok once it has *launched*, regardless of agent outcome.
        env::set_var("SKEIN_RESUME_CMD", "true {name} {prompt}");
        env::remove_var("SKEIN_REPO");
        assert!(resume_box("thing-x", "").is_ok());
        env::remove_var("SKEIN_RESUME_CMD");
    }

    // A stub standing in for the `claude` CLI: it reads the prompt (its last arg) and echoes a
    // canned reply, so the AI paths are exercised without a real model call.
    #[cfg(unix)]
    fn write_claude_stub(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("claude-stub.sh");
        fs::write(
            &p,
            "#!/bin/sh\nfor last; do :; done\ncase \"$last\" in\n  *'wire it up'*) echo ROUTINE ;;\n  *'which database'*) echo DECISION ;;\n  *Summarise*) echo 'It wired up the parser.' ;;\n  *) echo '?' ;;\nesac\n",
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn write_session(dir: &Path, name: &str, last_message: &str) {
        fs::create_dir_all(dir.join("sessions")).unwrap();
        let body = serde_json::json!({"ts":"2026-06-29T00:00:00Z","kind":"stop","lastMessage":last_message});
        fs::write(
            dir.join("sessions").join(format!("{name}.json")),
            body.to_string(),
        )
        .unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn narrate_uses_stubbed_claude_and_respects_kill_switch() {
        if Command::new("sh").arg("-c").arg("true").output().is_err() {
            return;
        }
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-n":{"branch":"x","dir":"/d","lastSeen":"","status":"waiting"}}"#,
        )
        .unwrap();
        write_session(&dir, "thing-n", "Refactored the parser module today.");
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_CLAUDE_BIN", write_claude_stub(&dir));

        env::remove_var("SKEIN_AI"); // kill switch: off → no spend, None
        assert_eq!(narrate("thing-n"), None);

        env::set_var("SKEIN_AI", "on");
        assert_eq!(
            narrate("thing-n").as_deref(),
            Some("It wired up the parser.")
        );

        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_CLAUDE_BIN");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    #[cfg(unix)]
    fn resume_batch_holds_real_decisions_when_ai_on() {
        if Command::new("sh").arg("-c").arg("true").output().is_err() {
            return;
        }
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"box-route":{"branch":"a","dir":"/d","lastSeen":"","status":"waiting"},
               "box-decide":{"branch":"b","dir":"/d","lastSeen":"","status":"waiting"}}"#,
        )
        .unwrap();
        write_session(&dir, "box-route", "Parser done — shall I wire it up next?");
        write_session(&dir, "box-decide", "Stuck: which database should I target?");
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_CLAUDE_BIN", write_claude_stub(&dir));
        env::set_var("SKEIN_RESUME_CMD", "true {name} {prompt}"); // don't spawn a real agent
        env::remove_var("SKEIN_REPO");

        env::set_var("SKEIN_AI", "on");
        let (resumed, held) = resume_batch(&["box-route".to_string(), "box-decide".to_string()]);
        assert_eq!(resumed, vec!["box-route".to_string()]); // ROUTINE → continued
        assert_eq!(held, vec!["box-decide".to_string()]); // DECISION → held for the human

        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_CLAUDE_BIN");
        env::remove_var("SKEIN_RESUME_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn collisions_flag_files_touched_by_multiple_boxes() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        // dirs aren't git repos here → changed_files falls back to parsing the reported patches
        fs::write(
            &reg,
            r#"{"box-a":{"branch":"a","dir":"/nope-a","lastSeen":"","status":""},
               "box-b":{"branch":"b","dir":"/nope-b","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("diffs")).unwrap();
        fs::write(
            dir.join("diffs").join("box-a.patch"),
            "+++ b/src/shared.rs\n+++ b/src/only_a.rs\n",
        )
        .unwrap();
        fs::write(
            dir.join("diffs").join("box-b.patch"),
            "+++ b/src/shared.rs\n+++ b/src/only_b.rs\n",
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        assert_eq!(
            changed_files("box-a"),
            vec!["src/only_a.rs", "src/shared.rs"]
        );
        let cols = compute_collisions();
        assert_eq!(cols.len(), 1, "only the shared file collides");
        assert_eq!(cols[0].file, "src/shared.rs");
        assert_eq!(
            cols[0].boxes,
            vec!["box-a".to_string(), "box-b".to_string()]
        );

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn index_html_is_well_formed() {
        // The whole UI is one include_str!'d file; a missing close tag silently blanks the page.
        let html = include_str!("web/index.html");
        assert_eq!(
            html.matches("<script").count(),
            html.matches("</script>").count(),
            "unbalanced <script> tags"
        );
        assert!(html.trim_end().ends_with("</html>"));
        assert!(html.contains("id=\"fleet\""));
        assert!(html.contains("/vendor/xterm.js")); // vendored, not CDN
        assert!(!html.contains("cdn.jsdelivr"));
    }
}
