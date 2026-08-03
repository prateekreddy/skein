//! skein core — read the shared sbx registry and derive fleet views.
//!
//! This is the single source of truth shared by the CLI (`skein`) and the server
//! (`skein-server`). It owns no state the sandboxes don't already write; it only reads
//! `sandboxes.json` and derives status. See ARCHITECTURE.md.

mod config;
mod diff;
mod files;
mod runtime;
mod sandbox;
mod signals;
mod tracking;
mod transcript;
mod util;
mod verify;

pub use config::*;
pub use diff::*;
pub use files::*;
pub use runtime::*;
pub use sandbox::*;
pub use signals::*;
pub use tracking::*;
pub use transcript::*;
pub use util::*;
pub use verify::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
#[derive(Debug, Default, Serialize)]
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
    /// Runtime configured for this sandbox (`claude` or `codex`). The cockpit uses this as the
    /// default agent and offers the other runtime as a replacement-box takeover target.
    #[serde(default = "default_agent")]
    pub agent: String,
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
    /// which dialog is blocking, when the box's own screen says one is: `permission` | `question` |
    /// `trust` | `auth`. Empty when nothing blocks, or when no screen observation was available —
    /// each wants a different move from you, so the row names it instead of saying "decision".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blocked_kind: String,
    /// probe wiring health: "" = fine; "never" = the sandbox is Running but no probe has EVER
    /// reported (no heartbeat, no status file) — hooks dark for this box; the cockpit badges it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hook_health: String,
    /// the *other* half's health — whether this box's own screen is being read, and if not why:
    /// "" | "none" | "stale" | "unreadable" | "unsupported". See [`screen_health`]. Without it,
    /// falling back to hook-only turn state looks exactly like everything working.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub screen_health: String,
    /// the last check that ran in this box, and whether the box has worked since — "who needs me"
    /// is only half of triage; "whose work stands up" is the other half. Absent when no check has
    /// ever run here (which is not a failure, and must not read as one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifySummary>,
    /// this repo's store holds work-tracking documents newer than the ones installed from it, so a
    /// re-apply has something to deliver. Repo-scoped and host-side, because that half of the answer
    /// is free; whether *this box's* CLAUDE.md is stale can only be read inside the box, and is not
    /// worth waking one on every snapshot to find out. See [`sync_docs_available`].
    #[serde(default)]
    pub docs_update: bool,
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
fn parse_registry(data: &str) -> Result<serde_json::Value, String> {
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

/// The fleet, enriched and sorted "who needs me first" (tier asc, then name).
pub fn load_views() -> Result<Vec<BoxView>, String> {
    // Fleet source of record: sbx itself (`sbx ls`). The registry only *enriches* — it carries the
    // one datum sbx can't (the agent turn-state) — and serves as a fallback when sbx can't be
    // consulted or a box is direct-mode (no sbx). Everything else (which boxes exist, their dir,
    // branch, run-state, diff) comes from sbx + host git, so a box no longer has to register itself
    // to be seen. See docs/self-sufficient.md.
    let sbx = fleet_boxes();
    let reg = all_sandboxes();
    // skein-server may run *inside* one box; that box is provably up, so keep it live even when sbx
    // can't confirm it. Set $SKEIN_SELF to override the detected vmid.
    let self_box = env::var("SKEIN_SELF")
        .or_else(|_| env::var("SANDBOX_VM_ID"))
        .ok()
        .filter(|s| !s.is_empty());

    // sbx (`sbx ls`) is authoritative for which boxes *exist*; the registry only enriches them. We
    // fall back to the registry to populate the board *only* when sbx can't be consulted (None) — so a
    // destroyed box whose registry entry lingers (e.g. a delist that failed on a corrupt registry) no
    // longer shows up as a stale phantom once sbx confirms it's gone.
    let mut names: BTreeSet<String> = BTreeSet::new();
    match &sbx {
        Some(v) => {
            names.extend(v.iter().map(|b| b.name.clone()));
            // keep the self-box even if sbx didn't list it (skein may be running inside it).
            if let Some(self_name) = &self_box {
                names.insert(self_name.clone());
            }
        }
        None => names.extend(reg.keys().cloned()),
    }

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
            // Runtime resolution mirrors branch resolution: sbx knows what image/agent created the
            // box; the launch spec preserves an explicit per-box override; the repo is the default.
            let agent = s
                .map(|x| x.agent.clone())
                .filter(|a| valid_runtime(a))
                .or_else(|| repo.as_ref().and_then(|rp| launch_spec_agent(rp, &name)))
                .or_else(|| repo.as_ref().map(|rp| rp.agent.clone()))
                .filter(|a| valid_runtime(a))
                .unwrap_or_else(default_agent);
            // Branch resolution, most-authoritative first. Lifecycle probes refresh the per-store
            // registry after an in-box checkout. The launch spec is only the creation fallback; the
            // host clone is a different worktree (often on main) and must never override box state.
            let branch = r
                .map(|x| x.branch.clone())
                .filter(|b| !b.is_empty() && b != "?")
                .or_else(|| repo.as_ref().and_then(|rp| launch_spec_branch(rp, &name)))
                .or_else(|| repo.as_ref().map(|rp| branch_from_box(&name, rp)))
                .or_else(|| git_branch_for(&dir))
                .unwrap_or_default();
            // Reuse the registry-derived state logic; status (turn-state) is the registry's specific
            // datum, lastSeen is only a fallback when sbx liveness is absent.
            // Turn-state: the level observation of the box's own screen, fused with the hook edges
            // (docs/turn-state.md §4.3). With no observation this is exactly the edge signal, so a
            // box running an older probe behaves as it always did.
            let raw_pane = read_pane_raw(&name);
            let pane = raw_pane.clone().filter(pane_is_fresh);
            let level = pane
                .as_ref()
                .map(|obs| (classify_pane(&agent, obs), obs.ts));
            let (fused, blocked_kind) = fuse_status(status_edge(&name), level.clone());
            let sb = Sandbox {
                branch: branch.clone(),
                dir: dir.clone(),
                last_seen: r.map(|x| x.last_seen.clone()).unwrap_or_default(),
                // the registry's own status remains the transitional fallback for unprobed boxes.
                status: fused
                    .or_else(|| r.map(|x| x.status.clone()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default(),
            };
            let live = s.and_then(|x| x.live);
            let (mut state, mut tier) = sb.state_with(live);
            // Cold-start fallback has no authoritative existence/liveness signal. Old outcome files
            // must not resurrect destroyed boxes in "needs you": once the registry heartbeat is
            // stale (or absent), show the record as stale regardless of its sticky error/wait state.
            if sbx.is_none()
                && self_box.as_deref() != Some(name.as_str())
                && sb.age_secs().is_none_or(|seconds| seconds >= 30 * 60)
            {
                state = "stale".into();
                tier = 5;
            }
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
            // The live "what's it doing now" signal (box-task.sh / journal `next`), with the
            // terminal title's activity text as a last resort — Claude Code writes the running tool
            // there ("Run bash command true"), which is fresher and free. Only while the box is
            // actually busy: the title keeps the finished tool's text, so on an idle box it lies.
            let task = current_task(&name).or_else(|| {
                let obs = pane.as_ref()?;
                // Busy *and* a title this observer watched change recently: the text is a live tool
                // description only under both conditions (see PaneObs::title_age). Claude only —
                // Codex's title is the working directory (`⠧ skein`), which names no activity, so
                // reading it as one would put the box's own folder name in the task column.
                let fresh = (0..=TITLE_FRESH_SECS).contains(&obs.title_age);
                (agent == "claude"
                    && fresh
                    && level.as_ref().map(|(s, _)| s) == Some(&Screen::Busy))
                .then(|| title_activity(&obs.title))
                .flatten()
            });
            let mut headline = signal_text.as_deref().and_then(first_line);
            // When the signal is absent or just the generic "waiting for your input", surface the
            // current task instead — so even a tier-0 needs-input row says what it was working on.
            if headline.as_deref().is_none_or(is_generic_wait) {
                if let Some(t) = task.clone() {
                    headline = Some(t);
                }
            }
            // error/ended carry their own reason from the probe (error_type / end reason) — that's the
            // headline that matters for those rows, so it wins over the stale narrative/task signal.
            let is_outcome = state == "error" || state == "ended";
            if is_outcome {
                if let Some(d) = current_status_detail(&name) {
                    headline = Some(d);
                }
            }
            let pause = if tier == 3 || is_outcome {
                Pause::None // still working, or a terminal outcome whose pill speaks for itself
            } else {
                classify_message(signal_text.as_deref().unwrap_or(""), blocked)
            };
            // Hook-health: distinguish never-wired probes from sessions still running an older
            // box-side contract. Silence alone is normal between lifecycle events; revision drift
            // is not, because the old session may emit a payload shape the new host misreads.
            let hook_health = if live == Some(Liveness::Running) {
                let store = store_for_box(&name);
                let dark = store.as_ref().is_none_or(|st| {
                    !st.join("hook-log").join(format!("{name}.jsonl")).exists()
                        && !st.join("status").join(format!("{name}.json")).exists()
                });
                if dark {
                    "never".to_string()
                } else if store.as_ref().is_some_and(|st| probe_is_stale(st, &name)) {
                    "stale".to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            // The other half's health: a box can be perfectly wired for hooks and still be blind to
            // its own screen (no observer, an observer that stopped, a screen we can't parse), which
            // is invisible unless we say it.
            let screen = screen_health(&agent, raw_pane.as_ref(), live == Some(Liveness::Running));
            BoxView {
                name: name.clone(),
                state,
                tier,
                branch,
                age: sb.age(),
                dir: shorten(&dir),
                repo: repo.as_ref().map(|rp| rp.id.clone()).unwrap_or_default(),
                agent,
                diff: read_diffstat_file(&name),
                headline,
                task,
                pause,
                blocked_kind: blocked_kind.to_string(),
                verify: verify_summary(&name),
                hook_health,
                screen_health: screen.to_string(),
                // Two file reads against the repo's store — no box is woken to answer this, which is
                // what makes it affordable on a signal computed for every row on every snapshot.
                docs_update: repo
                    .as_ref()
                    .is_some_and(|rp| sync_docs_available(Path::new(&rp.store))),
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

/// 1.5s micro-cache over `sbx ls`: `load_views` used to re-run it once at the top and then again
/// via `lookup_dir` for every box the registry didn't know (`read_journal` → `lookup_dir` →
/// `fleet_boxes`) — an N-box fleet paid 1+N subprocess spawns per 2s tick, times open browser tabs.
static FLEET_CACHE: std::sync::Mutex<Option<(std::time::Instant, Option<Vec<SbxBox>>)>> =
    std::sync::Mutex::new(None);
static FLEET_LAST_GOOD: std::sync::Mutex<Option<Vec<SbxBox>>> = std::sync::Mutex::new(None);
static FLEET_DEGRADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enumerate the fleet from sbx. `None` when sbx can't be consulted (not installed, errored,
/// unparseable, or hung past the timeout) — callers then fall back to the registry. Override with
/// `$SKEIN_LS_CMD` (run via `sh -c`; must emit the `sbx ls --json` shape). Micro-cached — see
/// FLEET_CACHE.
pub fn fleet_boxes() -> Option<Vec<SbxBox>> {
    // cfg!(test): tests swap $SKEIN_LS_CMD per case and run in parallel — a process-wide cache
    // would serve one test's fleet to another. Prod (server/CLI) keeps it.
    if !cfg!(test) {
        let cache = FLEET_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, boxes)) = cache.as_ref() {
            if at.elapsed() < Duration::from_millis(1500) {
                return boxes.clone();
            }
        }
    }
    let mut cmd = match env::var("SKEIN_LS_CMD").ok().filter(|s| !s.is_empty()) {
        Some(c) => {
            let mut sh = Command::new("sh");
            sh.arg("-c").arg(c);
            sh
        }
        None => {
            let mut sbx = Command::new("sbx");
            sbx.args(["ls", "--json"]);
            sbx
        }
    };
    // Bounded: a wedged sbx daemon used to hang this .output() forever — and with it every
    // fleet-snapshot task, accumulating stuck blocking threads until the board went permanently
    // blank. A timeout degrades to the registry fallback instead.
    let fresh = output_with_timeout(&mut cmd, Duration::from_secs(5))
        .filter(|o| o.status.success())
        .and_then(|o| parse_boxes_checked(&String::from_utf8_lossy(&o.stdout)));
    // Unit tests deliberately swap the command between cases. Keep their calls isolated; exercise
    // last-known-good selection through its pure helper below instead of leaking global state.
    let boxes = if cfg!(test) {
        fresh
    } else {
        let mut last_good = FLEET_LAST_GOOD.lock().unwrap_or_else(|e| e.into_inner());
        let (resolved, degraded) = resolve_fleet(fresh, &mut last_good);
        FLEET_DEGRADED.store(degraded, std::sync::atomic::Ordering::Relaxed);
        resolved
    };
    *FLEET_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((std::time::Instant::now(), boxes.clone()));
    boxes
}

/// A successful response (including an empty fleet) replaces last-known-good. A transport/command/
/// parse failure reuses last-known-good instead of resurrecting stale registry-only boxes.
fn resolve_fleet(
    fresh: Option<Vec<SbxBox>>,
    last_good: &mut Option<Vec<SbxBox>>,
) -> (Option<Vec<SbxBox>>, bool) {
    match fresh {
        Some(boxes) => {
            *last_good = Some(boxes.clone());
            (Some(boxes), false)
        }
        None => (last_good.clone(), true),
    }
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
#[cfg(test)]
fn parse_boxes(json: &str) -> Vec<SbxBox> {
    parse_boxes_checked(json).unwrap_or_default()
}

/// Parse a syntactically valid sbx fleet response. `Some([])` is materially different from `None`:
/// an empty array/map authoritatively says no boxes exist, while `None` means the command output was
/// not a fleet document and callers may use last-known-good/cold-start fallback.
fn parse_boxes_checked(json: &str) -> Option<Vec<SbxBox>> {
    use serde_json::Value;
    let lines = json
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    // Multi-line NDJSON is valid only when every line is an object. Do not silently discard an API
    // error/malformed line and present the remainder as an authoritative fleet.
    let entries = if lines.len() > 1 {
        let ndjson = lines
            .iter()
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .filter(Value::is_object)
            })
            .collect::<Option<Vec<_>>>();
        match ndjson {
            Some(entries) => entries,
            None => collect_ls_entries_checked(serde_json::from_str::<Value>(json).ok()?)?,
        }
    } else {
        collect_ls_entries_checked(serde_json::from_str::<Value>(json).ok()?)?
    };
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
    Some(out)
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
    let mut command = Command::new("git");
    command.args(["-C", dir, "rev-parse", "--abbrev-ref", "HEAD"]);
    let out = bounded_output(&mut command, "git branch", Duration::from_secs(5)).ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!b.is_empty() && b != "HEAD").then_some(b)
}

/// Reduce a single `sbx ls --json` document to a flat list of per-box objects, covering an array,
/// an `{key: [..]}` wrapper, or a `{name: {..}}` map (the box name is injected as `name`).
fn collect_ls_entries_checked(v: serde_json::Value) -> Option<Vec<serde_json::Value>> {
    use serde_json::Value;
    match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => {
            if o.is_empty() {
                return Some(vec![]);
            }
            if LS_NAME_KEYS
                .iter()
                .any(|key| o.get(*key).and_then(Value::as_str).is_some())
            {
                return Some(vec![Value::Object(o)]); // one-line NDJSON with exactly one box
            }
            if let Some(arr) = o.values().find_map(Value::as_array) {
                return Some(arr.clone());
            }
            if !o.values().all(Value::is_object) {
                return None; // e.g. {"error":"API error"} is not an empty fleet
            }
            Some(
                o.into_iter()
                    .filter_map(|(k, mut val)| match val {
                        Value::Object(ref mut m) => {
                            m.insert("name".into(), Value::String(k));
                            Some(val)
                        }
                        _ => None,
                    })
                    .collect(),
            )
        }
        _ => None,
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
fn sandboxes_in(store: &Path) -> BTreeMap<String, Sandbox> {
    fs::read_to_string(store.join("sandboxes.json"))
        .ok()
        .and_then(|data| parse_registry(&data).ok())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Aggregate every project registry. Managed boxes report their *current* branch into their own
/// mounted store; consulting only the legacy global registry made the board fall back to the launch
/// branch forever after an in-box checkout. For duplicate legacy entries, the newest lastSeen wins.
fn all_sandboxes() -> BTreeMap<String, Sandbox> {
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

fn registry_entry_for_box(name: &str) -> Option<Sandbox> {
    store_for_box(name)
        .and_then(|store| sandboxes_in(&store).remove(name))
        .or_else(|| all_sandboxes().remove(name))
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
    /// Command a **verify** runs inside a box of this repo (`cargo test`, `npm test`, a script).
    /// Empty ⇒ fall back to the global default in [`Config::check_command`]. Per-repo because a
    /// fleet spanning a Rust service and a web app has no single right answer.
    #[serde(default)]
    pub check: String,
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
    /// Superseded by [`Repo::sync_connection`]; read once by the migration, then cleared. Kept so
    /// a `repos.json` written before connections existed still parses.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sync_gateway_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub ok: bool,
    pub registry: HealthCheck,
    pub sbx: HealthCheck,
    pub git: HealthCheck,
    pub gh: HealthCheck,
    pub probes: HealthCheck,
    pub mailbox: HealthCheck,
    pub dark_boxes: Vec<String>,
    pub stale_boxes: Vec<String>,
    pub runtimes: Vec<RuntimeInfo>,
}

/// Read-only environment diagnosis for detached server deployments. Unlike startup `eprintln!`,
/// this remains inspectable from the cockpit and makes a missing box-side jq dependency explicit.
pub fn health_report() -> HealthReport {
    let registry = match load_registry() {
        Ok((boxes, path)) => HealthCheck {
            ok: true,
            detail: format!("{} ({} boxes)", path.display(), boxes.len()),
        },
        Err(error) => HealthCheck {
            ok: false,
            detail: error,
        },
    };
    let fleet = fleet_boxes();
    let fleet_degraded = FLEET_DEGRADED.load(std::sync::atomic::Ordering::Relaxed);
    let sbx = HealthCheck {
        ok: program_on_path("sbx") && fleet.is_some() && !fleet_degraded,
        detail: match &fleet {
            Some(boxes) if fleet_degraded => format!(
                "`sbx ls` temporarily unavailable; showing last successful snapshot ({} boxes)",
                boxes.len()
            ),
            Some(boxes) => format!("available ({} boxes)", boxes.len()),
            None if program_on_path("sbx") => "installed, but `sbx ls` failed or timed out".into(),
            None => "not found on PATH".into(),
        },
    };
    let tool = |name: &str, required: bool| HealthCheck {
        ok: program_on_path(name) || !required,
        detail: if program_on_path(name) {
            "available".into()
        } else if required {
            "not found on PATH".into()
        } else {
            "not found (optional)".into()
        },
    };
    let git = tool("git", true);
    let gh = tool("gh", false);

    let repos = load_repos();
    let fleet_names = fleet.as_ref().map(|boxes| {
        boxes
            .iter()
            .map(|box_| box_.name.as_str())
            .collect::<BTreeSet<_>>()
    });
    let mut probe_errors = Vec::new();
    let mut mailbox_errors = Vec::new();
    for repo in &repos {
        let store = Path::new(&repo.store);
        for relative in [
            "skein/probe-revision",
            "skein/runtimes.tsv",
            "skein/bin/box-status.sh",
            "skein/bin/mailbox.sh",
            "skein/bin/shared-home.sh",
            "skein/bin/agent-guide.sh",
            "skein/bin/install-codex-hooks.sh",
        ] {
            if !store.join(relative).is_file() {
                probe_errors.push(format!("{} missing {relative}", repo.id));
            }
        }
        if !store.join("mailbox").is_dir() {
            mailbox_errors.push(format!("{} mailbox directory missing", repo.id));
        }
        if !store.join("shared-home").is_dir() {
            probe_errors.push(format!("{} shared-home directory missing", repo.id));
        }
        let boot_dir = store.join("skein/boot");
        if let Ok(entries) = fs::read_dir(boot_dir) {
            for path in entries.flatten().map(|entry| entry.path()) {
                let box_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("box");
                if fleet_names
                    .as_ref()
                    .is_some_and(|names| !names.contains(box_name))
                {
                    continue;
                }
                let boot = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
                let jq_available = boot.as_ref().and_then(|value| value.get("jq")?.as_bool());
                if jq_available == Some(false) {
                    mailbox_errors.push(format!("{box_name} is missing required jq"));
                }
                let tmux_available = boot.as_ref().and_then(|value| value.get("tmux")?.as_bool());
                if tmux_available == Some(false) {
                    probe_errors.push(format!("{box_name} is missing required tmux"));
                }
                if boot
                    .as_ref()
                    .and_then(|value| value.get("shared_home")?.as_str())
                    .is_some_and(|state| state != "linked")
                {
                    probe_errors.push(format!("{box_name} shared home is unavailable"));
                }
                if boot
                    .as_ref()
                    .and_then(|value| value.get("agent_guide")?.as_str())
                    .is_some_and(|state| state != "installed")
                {
                    probe_errors.push(format!("{box_name} durable agent guidance is unavailable"));
                }
            }
        }
    }
    let mut probes = HealthCheck {
        ok: probe_errors.is_empty(),
        detail: if probe_errors.is_empty() {
            format!("installed for {} managed repos", repos.len())
        } else {
            probe_errors.join("; ")
        },
    };
    let mailbox = HealthCheck {
        ok: mailbox_errors.is_empty(),
        detail: if mailbox_errors.is_empty() {
            "shared stores and required jq available in reporting boxes".into()
        } else {
            mailbox_errors.join("; ")
        },
    };
    let views = load_views().unwrap_or_default();
    let dark_boxes = views
        .iter()
        .filter(|view| view.hook_health == "never")
        .map(|view| view.name.clone())
        .collect::<Vec<_>>();
    let stale_boxes = views
        .into_iter()
        .filter(|view| view.hook_health == "stale")
        .map(|view| view.name)
        .collect::<Vec<_>>();
    if !dark_boxes.is_empty() {
        probes.ok = false;
        probes.detail.push_str(&format!(
            "; no signals from running boxes: {}",
            dark_boxes.join(", ")
        ));
    }
    // Deliberately NOT reported here: a box on hook-only turn state (see `screen_health`) is not
    // unhealthy — it degrades to exactly its pre-observer behaviour. Nagging in the environment
    // banner would be crying wolf; the caveat belongs on the row and tab it applies to.
    let ok = registry.ok && sbx.ok && git.ok && probes.ok && mailbox.ok && stale_boxes.is_empty();
    HealthReport {
        ok,
        registry,
        sbx,
        git,
        gh,
        probes,
        mailbox,
        dark_boxes,
        stale_boxes,
        runtimes: supported_runtimes(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            seed_gh_secret: true,
            force_gh_secret: false,
            default_agent: default_agent(),
            base_branch: String::new(),
            confirm_destroy: true,
            ssh_key: String::new(),
            check_command: String::new(),
            sync_gateway_url: String::new(),
        }
    }
}

/// 1s micro-cache over `repos.json`: a single `load_views` pass consults the repo list dozens of
/// times per box (store_for_box, current_status, current_task, …) and each SSE tick repeats that
/// per open browser tab — hundreds of disk reads every 2s on a busy fleet, all returning the same
/// bytes. Cleared by `save_repos` so a mutation is visible immediately.
static REPOS_CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<Repo>)>> =
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
    check: Option<&str>,
    plane_project: Option<&str>,
    sync_connection: Option<&str>,
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
    if let Some(v) = check {
        repo.check = v.trim().to_string();
    }
    if let Some(v) = plane_project {
        repo.plane_project = v.trim().to_string();
    }
    if let Some(v) = sync_connection {
        repo.sync_connection = v.trim().to_string();
        repo.sync_gateway_url.clear(); // the selection is now the whole answer
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
fn launch_spec_branch(repo: &Repo, name: &str) -> Option<String> {
    launch_spec(repo, name)?
        .get("branch")
        .and_then(|b| b.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn launch_spec(repo: &Repo, name: &str) -> Option<serde_json::Value> {
    let p = Path::new(&repo.store)
        .join("skein")
        .join("launch")
        .join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

fn launch_spec_agent(repo: &Repo, name: &str) -> Option<String> {
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
        check: String::new(), // set later, per repo, in Settings → Repositories
        plane_project: String::new(),
        // One connection ⇒ adopt it, so a single-tracker fleet needs no ceremony per repo. Two or
        // more ⇒ leave it unset: which backlog this repo belongs to is not skein's guess to make,
        // and a wrong one mints a real credential against the wrong Plane.
        sync_connection: match load_connections().as_slice() {
            [only] => only.id.clone(),
            _ => String::new(),
        },
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

/// Documents the shared-store layout for the user — written into a fresh store only when absent.
const STORE_README: &str = include_str!("store/README.md");

/// Provision a repo's shared-data folder at `store` (a `.claude` dir): scaffold the directory
/// structure (only what's missing — never clobbering data the user already put there), install skein's
/// own machinery (turn-state probe + the SessionStart bootstrap, mailbox, and a default status line,
/// all under `skein/bin/`), and wire it into `settings.json`. Idempotent and safe to run on every
/// launch — an empty folder comes up fully working (memory bridge, mailbox, status line), an
/// already-populated one is left intact (machinery refreshed, settings merged additively). The user
/// only optionally fills `memory/` and `skills/` with their own content.
pub fn ensure_store(store: &Path) -> Result<(), String> {
    fs::create_dir_all(store).map_err(|e| format!("mkdir {}: {e}", store.display()))?;
    // skein-owned runtime (skein/, mailbox/, status/, tasks/) + the user-filled content homes
    // (memory/, skills/, hooks/). create_dir_all is idempotent, so existing dirs are untouched.
    for d in [
        "mailbox",
        "status",
        "tasks",
        "journals",
        "telemetry",
        // durable Claude <-> Codex takeover briefs plus one-shot per-runtime pending copies
        "handoffs",
        // narrative signal per box (box-session.sh): the headline / fork-detector / digest source
        "sessions",
        // per-box hook heartbeats (every probe appends one line per firing) — how the cockpit
        // distinguishes "hooks broken" from "box quiet"; see hook_health in load_views
        "hook-log",
        "skein/launch",
        "skein/bin",
        "memory",
        "skills",
        "hooks",
        // RW-surfaced shared paths (shared-paths.txt entries marked `rw`) live here — the store is
        // a genuinely writable host directory, unlike the RO clone-mode source mirror. See
        // sandbox-bootstrap.sh's surfacing loop.
        "shared-rw",
        // Project-scoped durable user workspace, surfaced as $HOME/shared in every box. Real $HOME
        // remains private so credentials, caches, and concurrent runtime state cannot collide.
        "shared-home",
    ] {
        let p = store.join(d);
        fs::create_dir_all(&p).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
    }
    // Document the layout so the user knows what they can optionally add — written only if absent.
    write_if_absent(&store.join("README.md"), STORE_README);
    ensure_probe_in(store)
}

/// Write `body` to `path` only when nothing is there yet — so scaffolding never overwrites the user's
/// own files. Best-effort: a write failure is logged, not fatal.
fn write_if_absent(path: &Path, body: &str) {
    if path.exists() {
        return;
    }
    if let Err(e) = fs::write(path, body) {
        eprintln!("skein: write {}: {e}", path.display());
    }
}

/// Record, for box `name`, what its kit startup needs (branch + agent) at
/// `<store>/skein/launch/<name>.json`. The kit finds this file (the store is mounted) and checks out
/// the branch — our env-free channel into the box, since `sbx run --env` is unconfirmed.
fn write_launch_spec_for_agent(
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
    if let Some(agent) = fleet_boxes()
        .and_then(|boxes| boxes.into_iter().find(|b| b.name == name))
        .map(|b| b.agent)
        .filter(|a| valid_runtime(a))
    {
        return agent;
    }
    if let Some(repo) = repo_for_box(name) {
        return launch_spec_agent(&repo, name)
            .or_else(|| (!repo.agent.is_empty()).then(|| repo.agent.clone()))
            .unwrap_or_else(default_agent);
    }
    default_agent()
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
    let mut command = Command::new(&bin);
    command.args(["-p", "--model", &model, prompt]);
    let out = bounded_output(&mut command, "AI enrichment", Duration::from_secs(30)).ok()?;
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

/// Look up a box's clone root (the `dir` it registered) by name.
pub fn lookup_dir(name: &str) -> Option<String> {
    // registry first (no subprocess); else the box's workspace from sbx, for boxes the registry
    // doesn't know about (sbx-only / not-yet-registered).
    if let Some(dir) = registry_entry_for_box(name)
        .map(|box_| box_.dir)
        .filter(|dir| !dir.is_empty())
    {
        return Some(dir);
    }
    fleet_boxes()?
        .into_iter()
        .find(|b| b.name == name)
        .map(|b| b.dir)
        .filter(|d| !d.is_empty())
}

// ---------- files: in-cockpit browsing of a box's workspace ----------
// The cockpit's answer to "I have to leave skein just to read a doc or check a file": list and
// read the box's HOST-side workspace (`dir` — the same tree diff/session already read), so a dev
// never exits to an editor for a README, a config, or an artifact the agent produced.

/// Write a durable, provider-neutral takeover brief plus a one-shot copy for `target`. Native
/// transcripts remain separate; replacement takeover adds worktree/context artifacts around this
/// core brief. The box-handoff hook injects it with authoritative in-box git status.
pub fn prepare_handoff(name: &str, from: Option<&str>, target: &str) -> Result<PathBuf, String> {
    prepare_handoff_for(name, name, from, target, None, None)
}

/// Replacement-box variant of [`prepare_handoff`]. The source supplies the digest, while the
/// pending filename is addressed to the destination vm id so its first SessionStart consumes it.
fn prepare_handoff_for(
    source_name: &str,
    destination_name: &str,
    from: Option<&str>,
    target: &str,
    native_context: Option<&str>,
    store_override: Option<&Path>,
) -> Result<PathBuf, String> {
    let name = source_name;
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    if !valid_name(destination_name) {
        return Err("invalid destination box name".into());
    }
    if !valid_runtime(target) {
        return Err(format!("unsupported handoff runtime {target:?}"));
    }
    let source = from
        .filter(|a| valid_runtime(a))
        .map(str::to_string)
        .unwrap_or_else(|| agent_for_box(name));
    let digest = session_digest(name).ok_or_else(|| format!("no such box: {name}"))?;
    let store = store_override
        .map(Path::to_path_buf)
        .or_else(|| store_for_box(name))
        .ok_or("can't locate the box's shared store")?;
    let dir = store.join("handoffs");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;

    let mut brief = format!(
        "# Skein cross-agent handoff\n\n- source box: `{name}`\n- destination box: `{destination_name}`\n- from: `{source}`\n- to: `{target}`\n- branch: `{}`\n- state: `{}`\n- prepared: `{}`\n\nContinue the existing work in this replacement sandbox. Its commits and working tree were restored from the source snapshot; inspect them before editing and do not redo completed work. The source box remains intact as rollback.\n",
        digest.branch,
        digest.state,
        Utc::now().to_rfc3339()
    );
    brief.push_str(
        "\n## Git authentication reminder\n\nInspect `git remote -v` before pushing. HTTPS uses Skein's seeded `gh` credentials (`gh auth status`; `gh auth setup-git` if needed). For GitHub SSH, establish host trust once with `mkdir -p ~/.ssh && chmod 700 ~/.ssh && ssh -o StrictHostKeyChecking=accept-new -T git@github.com`, then inspect the forwarded agent with `ssh-add -l`. GitHub's successful-auth/no-shell message exits 1 and means SSH works. Never copy a private key into the box.\n",
    );
    if let Some(task) = current_task(name) {
        brief.push_str(&format!("\n## Active objective\n\n{task}\n"));
    }
    if let Some(blocked) = digest.blocked_on {
        brief.push_str(&format!("\n## Blocked on\n\n{blocked}\n"));
    } else if let Some(last) = digest.last_message {
        brief.push_str(&format!("\n## Previous agent's last message\n\n{last}\n"));
    }
    if let Some(journal) = digest.journal {
        brief.push_str(&format!("\n## Journal tail\n\n{journal}\n"));
    }
    if !digest.commits.is_empty() {
        brief.push_str("\n## Recent branch commits\n");
        for commit in digest.commits.iter().take(20) {
            brief.push_str(&format!("\n- {commit}"));
        }
        brief.push('\n');
    }
    if let Some(d) = digest.diff {
        brief.push_str(&format!(
            "\n## Change summary\n\n{} files, +{} / -{} lines.\n",
            d.files, d.ins, d.del
        ));
    }
    let files = changed_files(name);
    if !files.is_empty() {
        brief.push_str("\nChanged files reported to Skein:\n");
        for file in files.iter().take(120) {
            brief.push_str(&format!("\n- `{file}`"));
        }
        brief.push('\n');
    }

    if let Some(context) = native_context.filter(|text| !text.trim().is_empty()) {
        brief.push_str("\n## Bounded native conversation export\n\nThis is continuity context, not a native resumable session. Provider-specific metadata and tool state may be omitted.\n\n");
        brief.push_str(context.trim());
        brief.push('\n');
    }

    let durable = dir.join(format!("{destination_name}.md"));
    write_atomic(&durable, &dir, brief.as_bytes())?;
    let pending = dir.join(format!("{destination_name}.{target}.pending.md"));
    write_atomic(&pending, &dir, brief.as_bytes())?;
    Ok(pending)
}

// ───────────────────────── cross-runtime takeover ─────────────────────────

/// Result of preparing and launching one immutable source snapshot in a new single-runtime box.
#[derive(Debug, Clone, Serialize)]
pub struct Replacement {
    pub source: String,
    pub target: String,
    pub source_runtime: String,
    pub target_runtime: String,
    pub repo: String,
    pub branch: String,
    pub snapshot: String,
    pub context_exported: bool,
}

/// Resolve custom box names by workspace as well as by the modern `<repo>-<branch>` convention.
/// The store must remain explicit because a takeover copies shared agent state into it.
fn takeover_repo(name: &str) -> Option<Repo> {
    if let Some(repo) = repo_for_box(name) {
        return Some(repo);
    }
    let dir = fleet_boxes()?
        .into_iter()
        .find(|box_| box_.name == name)?
        .dir;
    let wanted = PathBuf::from(expand_tilde(&dir));
    let wanted = wanted.canonicalize().unwrap_or(wanted);
    load_repos().into_iter().find(|repo| {
        let work = PathBuf::from(&repo.work);
        work.canonicalize().unwrap_or(work) == wanted
    })
}

fn replacement_name(repo: &Repo, source: &str, branch: &str, target_runtime: &str) -> String {
    let base = if source == repo.id {
        format!("{}-{target_runtime}", box_name(&repo.id, branch))
    } else if source.starts_with(&format!("{}-", repo.id)) {
        format!("{source}-{target_runtime}")
    } else {
        format!("{}-{source}-{target_runtime}", repo.id)
    };
    let base = slug(&base);
    let existing = fleet_boxes()
        .unwrap_or_default()
        .into_iter()
        .map(|box_| box_.name)
        .collect::<BTreeSet<_>>();
    if !existing.contains(&base) {
        return base;
    }
    for n in 2..10_000 {
        let candidate = format!("{base}-{n}");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", Utc::now().timestamp())
}

/// Render the provider-neutral one-line footer for a box whose runtime needs an adapter. Claude
/// invokes the same renderer natively with its status-line stdin, so it returns `None` here. Codex
/// maps its latest token_count event into that schema and is polled by the cockpit every 30 seconds.
/// The renderer is embedded in the guest command rather than addressed through the shared store:
/// direct-workspace and older boxes may not mount that store, but every managed box has Bash + jq.
pub fn agent_statusline(name: &str) -> Result<Option<String>, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let runtime_id = agent_for_box(name);
    let runtime = runtime_adapter(&runtime_id).ok_or("runtime adapter unavailable")?;
    let Some(input) = runtime.statusline_input else {
        return Ok(None);
    };
    let renderer = sh_quote(STATUSLINE_SH);
    let setup = runtime.interactive_setup;
    let shell = format!(
        r#"set -o pipefail; {setup}; payload="$({input})"; [ -n "$payload" ] || exit 0; printf '%s\n' "$payload" | bash -c {renderer}"#
    );
    let rendered = sbx_guest_output(name, &shell, Duration::from_secs(30))?;
    let rendered = rendered.trim_end_matches(['\r', '\n']).to_string();
    Ok((!rendered.is_empty()).then_some(rendered))
}

/// One top-level entry found while inspecting a box's private home for an explicit shared-home
/// import. Inventory is read-only; excluded entries remain visible with the reason so the safety
/// boundary is reviewable rather than hidden in implementation details.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SharedHomeCandidate {
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub eligible: bool,
    pub reason: String,
}

const SHARED_HOME_INVENTORY: &str = r####"set -o pipefail; command -v jq >/dev/null 2>&1 || { echo 'jq is required for shared-home inventory' >&2; exit 1; }; find "$HOME" -mindepth 1 -maxdepth 1 -print0 2>/dev/null | sort -z | while IFS= read -r -d '' path; do name="${path##*/}"; kind="other"; [ -f "$path" ] && kind="file"; [ -d "$path" ] && kind="directory"; [ -L "$path" ] && kind="symlink"; eligible=true; reason=""; if [ -L "$path" ]; then eligible=false; reason="symlinks are never imported"; elif [ ! -f "$path" ] && [ ! -d "$path" ]; then eligible=false; reason="sockets/devices/FIFOs are never imported"; else case "$name" in .* ) eligible=false; reason="hidden credential/runtime/cache path" ;; workspace|work|project|projects|src|repos|repositories|node_modules|target|build|dist|vendor|venv ) eligible=false; reason="workspace, dependency, or build-output path" ;; esac; fi; if [ "$eligible" = true ] && [ -d "$path" ] && find "$path" -type d -name .git -print -quit 2>/dev/null | grep -q .; then eligible=false; reason="contains a Git repository"; fi; bytes=0; if [ "$eligible" = true ]; then kb="$(du -sk "$path" 2>/dev/null | awk 'NR==1 {print $1}')"; case "$kb" in ''|*[!0-9]*) kb=0 ;; esac; bytes=$((kb * 1024)); fi; jq -cn --arg name "$name" --arg kind "$kind" --argjson bytes "$bytes" --argjson eligible "$eligible" --arg reason "$reason" '{name:$name,kind:$kind,bytes:$bytes,eligible:$eligible,reason:$reason}'; done"####;

/// Inspect a source box's private `$HOME` without copying anything. Hidden state, workspaces,
/// repositories, dependencies/build outputs, symlinks, and special files are explicitly ineligible.
pub fn shared_home_inventory(name: &str) -> Result<Vec<SharedHomeCandidate>, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let repo = repo_for_box(name).ok_or_else(|| format!("no registered repo for box {name}"))?;
    ensure_store(Path::new(&repo.store))?;
    let raw = sbx_guest_output(name, SHARED_HOME_INVENTORY, Duration::from_secs(120))?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|error| format!("invalid inventory response from {name}: {error}"))
        })
        .collect()
}

const SHARED_HOME_IMPORT: &str = r####"
set -euo pipefail
root="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || pwd)"
store="$root/.claude"
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
canonical="$store/shared-home"
[ -d "$canonical" ] && [ -w "$canonical" ] \
  || { echo "shared-home unavailable or not writable: $canonical" >&2; exit 1; }
[ "$#" -gt 0 ] || { echo 'no import entries selected' >&2; exit 1; }

# Structural safety remains strict. Read failures are handled separately by tar below: the user may
# choose a best-effort import, but a symlink/device or destination collision is never safe to guess.
for name in "$@"; do
  src="$HOME/$name"
  [ -f "$src" ] || [ -d "$src" ] || { echo "source entry unavailable: $name" >&2; exit 1; }
  [ ! -L "$src" ] || { echo "source entry became a symlink: $name" >&2; exit 1; }
  [ ! -e "$canonical/$name" ] && [ ! -L "$canonical/$name" ] \
    || { echo "destination already exists: $name" >&2; exit 1; }
  unsafe="$(find "$src" -mindepth 1 \( -type l -o -type s -o -type b -o -type c -o -type p \) -print -quit 2>/dev/null || true)"
  [ -z "$unsafe" ] || { echo "unsafe nested entry blocks import: $unsafe" >&2; exit 1; }
done

stage="$store/.shared-home-import.$(printf '%s' "${SANDBOX_VM_ID:-box}" | tr / -).$$"
mkdir -p "$stage"
trap 'rm -rf "$stage"' EXIT
warnings="$stage/.tar-warnings"

# `--ignore-failed-read` skips only source entries tar cannot stat/read. Capture every warning so the
# import is explicitly best-effort rather than silently claiming parity with the old home.
tar -C "$HOME" -cf - --ignore-failed-read \
  --exclude='*/.*' \
  --exclude='*/node_modules' --exclude='*/node_modules/*' \
  --exclude='*/target' --exclude='*/target/*' \
  --exclude='*/build' --exclude='*/build/*' \
  --exclude='*/dist' --exclude='*/dist/*' \
  --exclude='*/vendor' --exclude='*/vendor/*' \
  --exclude='*/venv' --exclude='*/venv/*' \
  --exclude='*/__pycache__' --exclude='*/__pycache__/*' \
  --exclude='*/credentials' --exclude='*/credentials/*' --exclude='*/credentials.json' \
  --exclude='*/secrets' --exclude='*/secrets/*' --exclude='*/secrets.*' \
  --exclude='*/auth.json' --exclude='*/token.json' \
  --exclude='*/id_rsa*' --exclude='*/id_ed25519*' --exclude='*.pem' --exclude='*.key' \
  -- "$@" 2>"$warnings" | tar -C "$stage" -xf -

imported_list="$stage/.imported"
: >"$imported_list"
imported=0
for name in "$@"; do
  if [ -e "$stage/$name" ]; then
    mv "$stage/$name" "$canonical/$name"
    printf '%s\n' "$name" >>"$imported_list"
    imported=$((imported + 1))
  else
    printf 'skipped entirely (nothing readable): %s\n' "$name" >>"$warnings"
  fi
done
[ "$imported" -gt 0 ] || { cat "$warnings" >&2; echo 'nothing readable was imported' >&2; exit 1; }

mkdir -p "$store/skein/imports"
items="$(jq -Rsc 'split("\n")[:-1]' <"$imported_list")"
warning_text="$(cat "$warnings")"
jq -cn --arg from "${SANDBOX_VM_ID:-unknown}" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson items "$items" --arg warnings "$warning_text" \
  '{from:$from,ts:$ts,items:$items,warnings:$warnings}' \
  > "$store/skein/imports/$(date -u +%Y%m%dT%H%M%SZ)-${SANDBOX_VM_ID:-box}.json"
printf 'imported %s item(s) into %s\n' "$imported" "$canonical"
if [ -s "$warnings" ]; then
  echo 'Skipped source entries:'
  cat "$warnings"
fi
"####;

/// Copy explicitly selected, inventory-approved top-level entries from one box's private home into
/// the repo's canonical shared home. Existing destinations never get merged or overwritten. Nested
/// hidden state, credentials, dependencies, and build outputs remain excluded during the copy.
pub fn import_shared_home(name: &str, selected: &[String]) -> Result<String, String> {
    if selected.is_empty() {
        return Err("choose at least one inventory entry to import".into());
    }
    let inventory = shared_home_inventory(name)?;
    let repo = repo_for_box(name).ok_or_else(|| format!("no registered repo for box {name}"))?;
    let canonical = Path::new(&repo.store).join("shared-home");
    let mut unique = BTreeSet::new();
    for entry in selected {
        if entry.contains(['\n', '\r', '\0']) {
            return Err(format!("unsafe control character in entry name: {entry:?}"));
        }
        if !unique.insert(entry) {
            return Err(format!("duplicate import entry: {entry:?}"));
        }
        let candidate = inventory
            .iter()
            .find(|candidate| candidate.name == *entry)
            .ok_or_else(|| format!("{entry:?} is not a top-level entry in {name}"))?;
        if !candidate.eligible {
            return Err(format!("{entry:?} is excluded: {}", candidate.reason));
        }
        if canonical.join(entry).exists() || canonical.join(entry).is_symlink() {
            return Err(format!(
                "shared-home destination already exists: {entry:?} (nothing was copied)"
            ));
        }
    }
    let selection = selected
        .iter()
        .map(|entry| sh_quote(entry))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!("set -- {selection}; {SHARED_HOME_IMPORT}");
    sbx_guest_output(name, &command, Duration::from_secs(600))
}

/// Stream a possibly-large guest artifact straight to a host file. This avoids base64, Python, and
/// holding a repository bundle in the server's memory.
fn copy_guest_file(name: &str, guest: &str, host: &Path) -> Result<(), String> {
    let parent = host.parent().ok_or("snapshot path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        host.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("artifact")
    ));
    let err = parent.join(format!(
        ".{}.stderr",
        host.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("artifact")
    ));
    let stdout = fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
    let stderr = fs::File::create(&err).map_err(|e| format!("create {}: {e}", err.display()))?;
    let mut child = Command::new("sbx")
        .args(["exec", name, "cat", guest])
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("sbx exec not runnable: {e}"))?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= Duration::from_secs(300) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&tmp);
                let _ = fs::remove_file(&err);
                return Err(format!("copying {guest} exceeded 300s"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for artifact copy: {e}"));
            }
        }
    };
    let stderr = fs::read_to_string(&err).unwrap_or_default();
    let _ = fs::remove_file(&err);
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("copying {guest}: {}", stderr.trim()));
    }
    fs::rename(&tmp, host).map_err(|e| format!("install {}: {e}", host.display()))
}

fn ensure_source_takeover_tools(name: &str) -> Result<(), String> {
    let script = r#"need=""; command -v jq >/dev/null 2>&1 || need="$need jq"; command -v tmux >/dev/null 2>&1 || need="$need tmux"; if [ -n "$need" ]; then command -v apt-get >/dev/null 2>&1 || { echo "missing required tools:$need and no supported package manager" >&2; exit 1; }; waited=0; while ps -eo comm= 2>/dev/null | grep -Eq '^[[:space:]]*(apt|apt-get|dpkg)[[:space:]]*$' && [ "$waited" -lt 240 ]; do sleep 2; waited=$((waited + 2)); done; timeout 120 sudo apt-get install -y -qq $need 2>/dev/null || { timeout 120 sudo apt-get update -qq && timeout 120 sudo apt-get install -y -qq $need; }; sudo rm -rf /var/lib/apt/lists/* 2>/dev/null || true; fi; command -v jq >/dev/null && command -v tmux >/dev/null"#;
    sbx_guest_output(name, script, Duration::from_secs(520)).map(|_| ())
}

fn write_replacement_launch_spec(
    target: &str,
    branch: &str,
    repo: &Repo,
    runtime: &str,
    snapshot_relative: &str,
    source: &str,
) -> Result<(), String> {
    let dir = Path::new(&repo.store).join("skein").join("launch");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "branch": branch,
        "agent": runtime,
        "handoff": { "source": source, "dir": snapshot_relative },
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(format!("{target}.json")), &dir, &bytes)
}

fn copy_tree_additive(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(destination).map_err(|e| format!("mkdir {}: {e}", destination.display()))?;
    for entry in fs::read_dir(source).map_err(|e| format!("read {}: {e}", source.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let to = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree_additive(&entry.path(), &to)?;
        } else if kind.is_file() && !to.exists() {
            fs::copy(entry.path(), &to).map_err(|e| format!("copy {}: {e}", to.display()))?;
        }
        // Do not reproduce symlinks from another sandbox/store: their absolute target is commonly
        // box-specific. The underlying shared files/directories are copied through normal entries.
    }
    Ok(())
}

fn sanitized_user_hooks(value: &serde_json::Value) -> serde_json::Value {
    let mut hooks = value.as_object().cloned().unwrap_or_default();
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(commands) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) {
                commands.retain(|command| {
                    !command
                        .get("command")
                        .and_then(|v| v.as_str())
                        .is_some_and(|command| command.contains("/.claude/skein/bin/"))
                });
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(|v| v.as_array())
                .is_none_or(|commands| !commands.is_empty())
        });
    }
    serde_json::Value::Object(hooks)
}

/// Import only user-owned shared context from a legacy store snapshot. Existing target files and
/// settings win; generated Skein hook commands are removed and regenerated from the current probe.
fn merge_shared_context(snapshot: &Path, target_store: &Path) -> Result<(), String> {
    let archive = snapshot.join("shared-context.tgz");
    if fs::metadata(&archive).map(|m| m.len()).unwrap_or(0) == 0 {
        return Ok(());
    }
    let staging = snapshot.join("shared-context");
    fs::create_dir_all(&staging).map_err(|e| format!("mkdir {}: {e}", staging.display()))?;
    let mut tar = Command::new("tar");
    tar.args(["-xzf"]).arg(&archive).arg("-C").arg(&staging);
    let out = bounded_output(&mut tar, "extract shared context", Duration::from_secs(120))?;
    if !out.status.success() {
        return Err(format!(
            "extracting shared context: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    for directory in ["memory", "skills", "hooks"] {
        copy_tree_additive(&staging.join(directory), &target_store.join(directory))?;
    }
    let source_settings = fs::read_to_string(staging.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    if let Some(mut source) = source_settings {
        let settings_path = target_store.join("settings.json");
        let mut target = fs::read_to_string(&settings_path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if let (Some(source_obj), Some(target_obj)) =
            (source.as_object_mut(), target.as_object_mut())
        {
            let user_hooks = source_obj
                .remove("hooks")
                .map(|hooks| sanitized_user_hooks(&hooks));
            for (key, value) in source_obj.iter() {
                target_obj
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
            if let Some(serde_json::Value::Object(source_hooks)) = user_hooks {
                let target_hooks = target_obj
                    .entry("hooks")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(target_hooks) = target_hooks.as_object_mut() {
                    for (event, groups) in source_hooks {
                        let target_groups = target_hooks
                            .entry(event)
                            .or_insert_with(|| serde_json::json!([]));
                        if let (Some(target_groups), Some(source_groups)) =
                            (target_groups.as_array_mut(), groups.as_array())
                        {
                            for group in source_groups {
                                if !target_groups.contains(group) {
                                    target_groups.push(group.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        let bytes = serde_json::to_vec_pretty(&target).map_err(|e| e.to_string())?;
        write_atomic(&settings_path, target_store, &bytes)?;
    }
    ensure_probe_in(target_store)
}

/// Snapshot one source and prepare a target launch spec. The source sandbox and its native session
/// are not modified beyond installing the mandatory jq/tmux substrate when absent.
pub fn prepare_replacement(source: &str, target_runtime: &str) -> Result<Replacement, String> {
    if !valid_name(source) || !valid_runtime(target_runtime) {
        return Err("invalid source box or target runtime".into());
    }
    let source_runtime = agent_for_box(source);
    if source_runtime == target_runtime {
        return Err(format!(
            "{source} already uses {target_runtime}; use the same-provider tmux restart path"
        ));
    }
    let repo = takeover_repo(source).ok_or_else(|| {
        format!("{source} is not mapped to a managed repo; register its workspace first")
    })?;
    ensure_store(Path::new(&repo.store))?;
    ensure_kit()?;
    ensure_source_takeover_tools(source)?;

    let branch = sbx_guest_output(
        source,
        "git rev-parse --abbrev-ref HEAD",
        Duration::from_secs(30),
    )?
    .trim()
    .to_string();
    let head = sbx_guest_output(source, "git rev-parse HEAD", Duration::from_secs(30))?
        .trim()
        .to_string();
    if branch.is_empty() || branch == "HEAD" || head.is_empty() {
        return Err("source must have an attached branch and at least one commit".into());
    }
    let target = replacement_name(&repo, source, &branch, target_runtime);

    static SNAPSHOT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SNAPSHOT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let run = format!(
        "{}-{}-{seq}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        std::process::id()
    );
    let relative = format!("skein/handoff-snapshots/{target}/{run}");
    let snapshot = Path::new(&repo.store).join(&relative);
    fs::create_dir_all(&snapshot).map_err(|e| format!("mkdir {}: {e}", snapshot.display()))?;
    let guest = format!("/tmp/skein-handoff-{}-{seq}", std::process::id());
    let export = runtime_adapter(&source_runtime)
        .map(|adapter| adapter.context_export)
        .unwrap_or(":");
    let build = format!(
        "set -e; root=\"$(git rev-parse --show-toplevel)\"; rm -rf {guest}; mkdir -p {guest}; git -C \"$root\" bundle create {guest}/repo.bundle HEAD; git -C \"$root\" diff --cached --binary HEAD > {guest}/index.patch; git -C \"$root\" diff --binary > {guest}/worktree.patch; git -C \"$root\" ls-files --others --exclude-standard -z -- . ':(exclude).claude' ':(exclude).claude/**' > {guest}/untracked.list; if [ -s {guest}/untracked.list ]; then tar -C \"$root\" --null -T {guest}/untracked.list -czf {guest}/untracked.tgz; else tar -czf {guest}/untracked.tgz --files-from /dev/null; fi; shared=\"$root/.claude\"; if [ -L \"$shared/skein\" ]; then shared=\"$(dirname \"$(readlink \"$shared/skein\")\")\"; elif [ -L \"$shared\" ]; then shared=\"$(readlink -f \"$shared\")\"; fi; names=\"\"; for item in memory skills hooks settings.json; do [ -e \"$shared/$item\" ] && names=\"$names $item\"; done; if [ -n \"$names\" ]; then tar -C \"$shared\" -czf {guest}/shared-context.tgz $names; else tar -czf {guest}/shared-context.tgz --files-from /dev/null; fi; ({export}) > {guest}/context.md 2>/dev/null || true"
    );
    sbx_guest_output(source, &build, Duration::from_secs(300))?;
    for file in [
        "repo.bundle",
        "index.patch",
        "worktree.patch",
        "untracked.tgz",
        "shared-context.tgz",
        "context.md",
    ] {
        copy_guest_file(source, &format!("{guest}/{file}"), &snapshot.join(file))?;
    }
    let _ = sbx_guest_output(source, &format!("rm -rf {guest}"), Duration::from_secs(30));
    let context = fs::read(snapshot.join("context.md"))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    merge_shared_context(&snapshot, Path::new(&repo.store))?;
    prepare_handoff_for(
        source,
        &target,
        Some(&source_runtime),
        target_runtime,
        Some(&context),
        Some(Path::new(&repo.store)),
    )?;
    write_replacement_launch_spec(&target, &branch, &repo, target_runtime, &relative, source)?;
    let manifest = serde_json::json!({
        "source": source, "target": target, "from": source_runtime.clone(), "to": target_runtime,
        "repo": repo.id.clone(), "branch": branch.clone(), "head": head, "created": Utc::now().to_rfc3339(),
        "artifacts": ["repo.bundle", "index.patch", "worktree.patch", "untracked.tgz", "shared-context.tgz", "context.md"]
    });
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    write_atomic(&snapshot.join("manifest.json"), &snapshot, &bytes)?;
    Ok(Replacement {
        source: source.to_string(),
        target,
        source_runtime,
        target_runtime: target_runtime.to_string(),
        repo: repo.id,
        branch,
        snapshot: snapshot.to_string_lossy().into_owned(),
        context_exported: !context.trim().is_empty(),
    })
}

/// Create the prepared single-runtime box and start its mandatory tmux session detached. The kit
/// restores the snapshot before this returns; any failure leaves the source untouched and the
/// immutable snapshot available for retry/inspection.
pub fn launch_replacement(replacement: &Replacement) -> Result<(), String> {
    let repo = load_repos()
        .into_iter()
        .find(|repo| repo.id == replacement.repo)
        .ok_or_else(|| format!("repo {} is no longer registered", replacement.repo))?;
    let runtime = runtime_adapter(&replacement.target_runtime)
        .ok_or_else(|| "target runtime adapter disappeared".to_string())?;
    let kit = ensure_kit()?;
    let kit_path = kit.to_string_lossy().into_owned();
    let _ = ensure_gh_secret();
    let mut create = Command::new("sbx");
    create.args([
        "create",
        "--clone",
        "--kit",
        &kit_path,
        "--name",
        &replacement.target,
        &replacement.target_runtime,
        &repo.work,
        &repo.store,
    ]);
    let out = bounded_output(
        &mut create,
        "sbx create replacement",
        Duration::from_secs(600),
    )?;
    if !out.status.success() {
        return Err(format!(
            "sbx create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let shell = format!(
        "{INITIAL_SETUP_WAIT}command -v {} >/dev/null 2>&1 || {{ echo 'target runtime is missing' >&2; exit 1; }}; command -v tmux >/dev/null 2>&1 || exit 1; {}; {}; tmux new-session -d -s skein-agent {:?}; {TMUX_CONFIGURE}tmux set-option -t skein-agent @skein-agent-contract {TMUX_AGENT_CONTRACT}",
        runtime.info.executable,
        runtime.interactive_setup,
        runtime.update_before_start,
        guarded_agent_command(runtime.info.id, runtime.interactive_start),
    );
    sbx_guest_output(&replacement.target, &shell, Duration::from_secs(660)).map(|_| ())
}

pub fn replace_box(source: &str, target_runtime: &str) -> Result<Replacement, String> {
    let replacement = prepare_replacement(source, target_runtime)?;
    launch_replacement(&replacement)?;
    Ok(replacement)
}

/// Pop the host's native folder/file picker and return the chosen absolute path (`Ok(None)` if the
/// user cancelled). `kind` is "file" → file picker, anything else → folder picker. skein-server runs
/// on the host, so this is a *real* OS dialog — which means it only works where that host has a GUI
/// (local use, not a headless / `tailscale serve` box, where the user types the path instead).
/// Best-effort across platforms: macOS `osascript`, then Linux `zenity`, then `kdialog`.
pub fn pick_path(kind: &str) -> Result<Option<String>, String> {
    let folder = kind != "file";
    let clean = |p: &str| -> Option<String> {
        let p = p.trim().trim_end_matches('/');
        (!p.is_empty()).then(|| p.to_string())
    };
    // macOS — AppleScript returns a POSIX path; a cancel exits non-zero with "User canceled".
    if cfg!(target_os = "macos") {
        let script = if folder {
            "POSIX path of (choose folder with prompt \"Pick a folder\")"
        } else {
            "POSIX path of (choose file with prompt \"Pick a file\")"
        };
        let out = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .map_err(|e| format!("osascript: {e}"))?;
        if out.status.success() {
            return Ok(clean(&String::from_utf8_lossy(&out.stdout)));
        }
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        if err.contains("cancel") {
            return Ok(None); // user dismissed the dialog
        }
        return Err(format!("native picker failed: {}", err.trim()));
    }
    // Linux — zenity, then kdialog. Both exit non-zero on cancel with empty stdout.
    for (bin, args) in linux_picker_argv(folder) {
        match std::process::Command::new(bin).args(&args).output() {
            Ok(out) if out.status.success() => {
                return Ok(clean(&String::from_utf8_lossy(&out.stdout)));
            }
            Ok(_) => return Ok(None), // present but cancelled
            Err(_) => continue,       // not installed → try the next
        }
    }
    Err("no native folder picker found (install zenity or kdialog, or type the path)".into())
}

fn linux_picker_argv(folder: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    if folder {
        vec![
            ("zenity", vec!["--file-selection", "--directory"]),
            ("kdialog", vec!["--getexistingdirectory", "."]),
        ]
    } else {
        vec![
            ("zenity", vec!["--file-selection"]),
            ("kdialog", vec!["--getopenfilename", "."]),
        ]
    }
}

// ---------- the turn-state probe (skein-owned, installed into the shared store) ----------
// skein ships these hook scripts and wires them into the store's settings.json, so a box reports
// working/waiting/needs-input + its current task without the *repo* providing anything. The store is
// linked into every box by the kit, so every box's Claude loads these hooks. See docs/self-sufficient.md.
/// Wires a box to the `sync` work tracker and installs the discipline for it. Lives in the store
/// rather than the kit on purpose: a kit only reaches boxes created after it changed, and an
/// existing box has to be wireable too. See `sync_provision_box`.
const SYNC_INSTALL_SH: &str = include_str!("store/sync-install.sh");
const SYNC_REFRESH_SH: &str = include_str!("store/sync-refresh.sh");
/// The three documents that installer places, once a box is actually registered: the always-on
/// rules, the memory, and the on-demand skill for Plane's full surface.
const SYNC_BLOCK_MD: &str = include_str!("store/sync/work-tracking.block.md");
const SYNC_MEMORY_MD: &str = include_str!("store/sync/work-tracking.memory.md");
const SYNC_SKILL_MD: &str = include_str!("store/sync/work-tracking.skill.md");

const PROBE_STATUS_SH: &str = include_str!("probe/box-status.sh");
// box-pane.sh: NOT hook-driven. Started detached by the attach command (see agent_attach_argv)
// and it outlives the attach, because the states it exists to catch — a crashed agent, a trust
// prompt before any session exists, a dialog dismissed with esc — are exactly the ones where no
// hook will ever fire. Its output is the level half of turn-state (see read_pane/classify_pane).
const PROBE_PANE_SH: &str = include_str!("probe/box-pane.sh");
const PROBE_TASK_SH: &str = include_str!("probe/box-task.sh");
// box-diff.sh: wired from Stop — writes branch-vs-base patch + shortstat JSON + commit list to
// <store>/diffs/<vmid>.{patch,json,commits} so the host can show them for clone-mode boxes where
// the git repo lives inside the sandbox and is not visible to the host.
const PROBE_DIFF_SH: &str = include_str!("probe/box-diff.sh");
// box-journal.sh: wired from Stop — copies `.skein/journal.md`'s tail to <store>/journals/<vmid>.md,
// the same "host can't see inside the clone" problem box-diff.sh solves, applied to the journal so
// the cockpit's Session tab can actually show it for clone-mode boxes (read_journal reads this first).
const PROBE_JOURNAL_SH: &str = include_str!("probe/box-journal.sh");
// box-token-usage.sh: wired from Stop — appends this turn's token usage (from the transcript the
// Stop hook points at) to <store>/telemetry/<vmid>.jsonl, a durable per-turn log that outlives the
// box, feeding the same cross-run learn-loop as journals/diffs.
const PROBE_TOKEN_USAGE_SH: &str = include_str!("probe/box-token-usage.sh");
// Codex runtime adapter: its UserPromptSubmit prompt is the active objective, while its rollout
// token_count events + PostToolUse hooks provide telemetry.
const PROBE_CODEX_TASK_SH: &str = include_str!("probe/box-codex-task.sh");
const PROBE_CODEX_TELEMETRY_SH: &str = include_str!("probe/box-codex-telemetry.sh");
const PROBE_CODEX_HOOK_SH: &str = include_str!("probe/box-codex-hook.sh");
// Provider-neutral one-shot context bridge used when Claude takes over Codex work or vice versa.
const PROBE_HANDOFF_SH: &str = include_str!("probe/box-handoff.sh");
// box-session.sh writes the narrative signal (<store>/sessions/<vmid>.json) that session_signal()
// reads — the last assistant message at Stop, the blocking prompt at Notification. Without it the
// whole headline / fork-detector / digest pipeline reads a file nothing writes (which is exactly
// what happened for skein's first months: the feature was tested against hand-written fixtures and
// dark in production).
const PROBE_SESSION_SH: &str = include_str!("probe/box-session.sh");
// skein-owned machinery installed alongside the probe so an empty shared folder works end-to-end:
// the SessionStart bootstrap (memory bridge + mailbox inbox + box registration), the mailbox, and a
// default status line. They live in `<store>/skein/bin/` (skein-owned namespace), refreshed each run.
const BOOTSTRAP_SH: &str = include_str!("store/sandbox-bootstrap.sh");
const SHARED_HOME_SH: &str = include_str!("store/shared-home.sh");
const SHARED_HOME_GUIDE: &str = include_str!("store/SHARED-HOME.md");
const AGENT_GUIDE_SH: &str = include_str!("store/agent-guide.sh");
const INSTALL_CODEX_HOOKS_SH: &str = include_str!("store/install-codex-hooks.sh");
const MAILBOX_SH: &str = include_str!("store/mailbox.sh");
const STATUSLINE_SH: &str = include_str!("store/statusline-command.sh");
// Box-side path of the installed scripts (the store is linked at `<clone>/.claude`).
const PROBE_STATUS_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-status.sh";
const PROBE_TASK_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-task.sh";
const PROBE_DIFF_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-diff.sh";
const PROBE_JOURNAL_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-journal.sh";
const PROBE_TOKEN_USAGE_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-token-usage.sh";
const PROBE_HANDOFF_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-handoff.sh";
const PROBE_SESSION_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-session.sh";
const BOOTSTRAP_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/sandbox-bootstrap.sh";
const STATUSLINE_CMD: &str = "bash $CLAUDE_PROJECT_DIR/.claude/skein/bin/statusline-command.sh";
// mailbox.sh hook entries — turn-boundary delivery so mail is re-checked every turn, not just at
// SessionStart. `inbox` (UserPromptSubmit) surfaces unread mail as additional context at the start
// of a turn; `stop-check` (Stop) blocks the stop with exit 2 + stderr if mail arrived mid-turn, so a
// message can never sit unread just because nobody happened to ask. Both mark seenBy on delivery,
// so the same message can't fire twice.
const MAILBOX_INBOX_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/mailbox.sh inbox";
const MAILBOX_STOPCHECK_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/mailbox.sh stop-check";

/// Content-derived revision for lifecycle *wiring* loaded when an agent starts. Probe scripts live
/// on the shared mount and update in place, so hashing their bodies made harmless docs/implementation
/// edits demand agent restarts. Only generated Claude/Codex hook configuration belongs here.
fn probe_revision() -> String {
    let mut hash = 0xcbf29ce484222325u64;
    let claude =
        serde_json::to_vec(&settings_with_probe(&serde_json::json!({}))).unwrap_or_default();
    let codex = serde_json::to_vec(&codex_hooks_with_probe()).unwrap_or_default();
    for body in [&claude, &codex] {
        for byte in body {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

/// Install skein's turn-state probe into the shared store: write the hook scripts to
/// `<store>/skein/bin/` and merge their hook wiring into `<store>/settings.json` (additive +
/// idempotent — the repo's own hooks are preserved, re-runs don't duplicate). The store is mounted
/// into every box, so this is how skein gets working/waiting/needs-input + task for any box without
/// the repo shipping a thing.
///
/// Refreshes *every* store skein reads — each managed repo's plus `store_dir()` — so multi-repo
/// fleets all report turn-state. Best-effort: errors are collected, not fatal.
pub fn ensure_probe_all() -> Result<(), String> {
    let mut errs = Vec::new();
    for store in all_stores() {
        // Existing repos must receive newly-required store directories as Skein evolves, not just
        // refreshed scripts. `ensure_store` is idempotent and delegates back to `ensure_probe_in`.
        if let Err(e) = ensure_store(&store) {
            errs.push(format!("{}: {e}", store.display()));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

/// Install/refresh the probe in one specific store dir (called per-store by `ensure_probe_all`, and
/// on a freshly-provisioned store).
pub fn ensure_probe_in(store: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let bin = store.join("skein").join("bin");
    fs::create_dir_all(&bin).map_err(|e| format!("mkdir {}: {e}", bin.display()))?;
    for (file, body) in [
        ("box-status.sh", PROBE_STATUS_SH),
        ("box-pane.sh", PROBE_PANE_SH),
        ("box-task.sh", PROBE_TASK_SH),
        ("box-diff.sh", PROBE_DIFF_SH),
        ("box-journal.sh", PROBE_JOURNAL_SH),
        ("box-token-usage.sh", PROBE_TOKEN_USAGE_SH),
        ("box-codex-task.sh", PROBE_CODEX_TASK_SH),
        ("box-codex-telemetry.sh", PROBE_CODEX_TELEMETRY_SH),
        ("box-codex-hook.sh", PROBE_CODEX_HOOK_SH),
        ("box-handoff.sh", PROBE_HANDOFF_SH),
        ("box-session.sh", PROBE_SESSION_SH),
        ("sandbox-bootstrap.sh", BOOTSTRAP_SH),
        ("shared-home.sh", SHARED_HOME_SH),
        ("agent-guide.sh", AGENT_GUIDE_SH),
        ("install-codex-hooks.sh", INSTALL_CODEX_HOOKS_SH),
        ("mailbox.sh", MAILBOX_SH),
        ("statusline-command.sh", STATUSLINE_SH),
        ("sync-install.sh", SYNC_INSTALL_SH),
        ("sync-refresh.sh", SYNC_REFRESH_SH),
    ] {
        let p = bin.join(file);
        // temp + rename, not a bare write: these scripts are EXECUTED by live boxes through the
        // shared mount — a box invoking one mid-rewrite would run a truncated file.
        write_atomic(&p, &bin, body.as_bytes())?;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o755));
    }
    // The work-tracking documents the installer copies into place. Not executable, and not written
    // into memory/ or skills/ directly — those are the box's to curate, and the installer only
    // seeds them once a box is genuinely registered.
    let sync = store.join("skein").join("sync");
    fs::create_dir_all(&sync).map_err(|e| format!("mkdir {}: {e}", sync.display()))?;
    for (file, body) in [
        ("work-tracking.block.md", SYNC_BLOCK_MD),
        ("work-tracking.memory.md", SYNC_MEMORY_MD),
        ("work-tracking.skill.md", SYNC_SKILL_MD),
    ] {
        write_atomic(&sync.join(file), &sync, body.as_bytes())?;
    }
    let settings = store.join("settings.json");
    let current: serde_json::Value = fs::read_to_string(&settings)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let merged = settings_with_probe(&current);
    let bytes = serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?;
    write_atomic(&settings, store, &bytes)?;

    let skein_dir = store.join("skein");
    write_atomic(
        &skein_dir.join("SHARED-HOME.md"),
        &skein_dir,
        SHARED_HOME_GUIDE.as_bytes(),
    )?;
    write_atomic(
        &skein_dir.join("probe-revision"),
        &skein_dir,
        format!("{}\n", probe_revision()).as_bytes(),
    )?;
    let runtime_manifest = RUNTIME_ADAPTERS
        .iter()
        .map(|runtime| {
            format!(
                "{}\t{}\t{}\t{}\t{}",
                runtime.info.id,
                runtime.info.label,
                runtime.info.executable,
                runtime.instruction_file,
                runtime.instruction_override
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    write_atomic(
        &skein_dir.join("runtimes.tsv"),
        &skein_dir,
        runtime_manifest.as_bytes(),
    )?;

    // Codex loads user-level hooks installed by the kit. Keep the generated, provider-specific
    // hook source in the shared store so every box gets the same adapter without repo-side files.
    let codex_hooks = codex_hooks_with_probe();
    let codex_bytes = serde_json::to_vec_pretty(&codex_hooks).map_err(|e| e.to_string())?;
    write_atomic(
        &store.join("skein/codex-hooks.json"),
        &skein_dir,
        &codex_bytes,
    )
}

/// Add skein's probe hooks to a `settings.json` value, preserving every existing hook and never
/// duplicating skein's own on a re-run (idempotent). Pure — the testable core of `ensure_probe`.
fn settings_with_probe(existing: &serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Value};
    // (event, command, optional matcher) — status on the turn-boundary events, task on TodoWrite, and
    // the SessionStart bootstrap that bridges memory + surfaces the mailbox (so an empty store works).
    // Sub-agent tracking: PreToolUse(Task) and SubagentStop maintain an in-flight counter so a
    // Notification fired while the box is DELEGATING to sub-agents reads as "working", not "needs-input"
    // (it's waiting on its own agents, not on you). UserPromptSubmit/Stop reset the counter each turn.
    // The richer fleet-states ride extra lifecycle events (all best-effort: a Claude Code that predates
    // one simply never fires it): StopFailure→error (rate_limit/overloaded/…), PreCompact/PostCompact→
    // compacting (busy, not stuck), SessionEnd→ended (distinct from a liveness-derived "stale").
    // Every command is wrapped `bash "<script>" <args>` at wiring time (see `wire` below): invoking
    // the script path bare relies on the exec bit surviving the shared mount into the microVM — if
    // it's squashed, every hook fails "permission denied" on every event, silently. The status line
    // learned this lesson first (STATUSLINE_CMD was already bash-prefixed); now it's uniform.
    let entries: [(&str, String, Option<&str>); 23] = [
        (
            "UserPromptSubmit",
            format!("{PROBE_STATUS_CMD} working"),
            None,
        ),
        // Turn-boundary mailbox delivery (start of turn): surfaces unread mail as additional
        // context, so it's never gated behind a human asking "did you get that?".
        ("UserPromptSubmit", MAILBOX_INBOX_CMD.to_string(), None),
        // If Codex handed this box to Claude, inject the pending provider-neutral brief as context
        // on the next prompt (SessionStart below catches a newly-created Claude session sooner).
        (
            "UserPromptSubmit",
            format!("{PROBE_HANDOFF_CMD} claude"),
            None,
        ),
        // Notification: Claude Code's hook payload carries NO field naming which notification type
        // fired (confirmed against the hooks docs — there is no `notification_type` on the stdin
        // JSON); it disambiguates *before* invoking the hook, via each entry's own `matcher`. So
        // this MUST be three separate entries, each scoped to a matcher and calling a distinct
        // literal mode — a single entry trying to sniff the type from payload silently never
        // distinguishes anything and always falls through to one bucket.
        (
            "Notification",
            format!("{PROBE_STATUS_CMD} notify-blocked"),
            Some("permission_prompt|elicitation_dialog|agent_needs_input"),
        ),
        (
            "Notification",
            format!("{PROBE_STATUS_CMD} notify-waiting"),
            Some("idle_prompt"),
        ),
        (
            "Notification",
            format!("{PROBE_STATUS_CMD} notify-ignore"),
            Some("auth_success|elicitation_complete|elicitation_response|agent_completed"),
        ),
        // Belt-and-braces for Claude versions that include notification_type in the payload but do
        // not apply matcher routing consistently. The probe is a strict no-op when the field is
        // absent, so it cannot race the matcher-specific compatibility entries above.
        (
            "Notification",
            format!("{PROBE_STATUS_CMD} notify-auto"),
            None,
        ),
        ("Stop", format!("{PROBE_STATUS_CMD} waiting"), None),
        // box-session.sh stop: record the turn's last assistant message — the narrative signal the
        // inbox headline, fork-detector, and session digest read (session_signal in this file).
        ("Stop", format!("{PROBE_SESSION_CMD} stop"), None),
        // box-session.sh ask: record the prompt the agent is blocked on, same matcher set that
        // routes box-status.sh to notify-blocked.
        (
            "Notification",
            format!("{PROBE_SESSION_CMD} ask"),
            Some("permission_prompt|elicitation_dialog|agent_needs_input"),
        ),
        // box-diff.sh runs alongside box-status.sh on Stop: writes the branch-vs-base
        // patch + shortstat + commit list so the host can show them for clone-mode boxes.
        ("Stop", PROBE_DIFF_CMD.to_string(), None),
        // box-journal.sh runs alongside box-diff.sh on Stop: copies the journal tail to the store
        // so the Session tab can show it for clone-mode boxes (the host can't read the box's clone).
        ("Stop", PROBE_JOURNAL_CMD.to_string(), None),
        // box-token-usage.sh: appends this turn's token usage to a durable per-turn log.
        ("Stop", PROBE_TOKEN_USAGE_CMD.to_string(), None),
        // Turn-boundary mailbox delivery (end of turn): blocks the stop (exit 2) if mail arrived
        // mid-turn, so the agent can't end a turn without having seen it.
        ("Stop", MAILBOX_STOPCHECK_CMD.to_string(), None),
        (
            "PreToolUse",
            format!("{PROBE_STATUS_CMD} agent-start"),
            Some("Task"),
        ),
        (
            "SubagentStop",
            format!("{PROBE_STATUS_CMD} agent-stop"),
            None,
        ),
        ("StopFailure", format!("{PROBE_STATUS_CMD} error"), None),
        ("PreCompact", format!("{PROBE_STATUS_CMD} compacting"), None),
        ("PostCompact", format!("{PROBE_STATUS_CMD} compacted"), None),
        ("SessionEnd", format!("{PROBE_STATUS_CMD} ended"), None),
        ("PostToolUse", PROBE_TASK_CMD.to_string(), Some("TodoWrite")),
        ("SessionStart", BOOTSTRAP_CMD.to_string(), None),
        ("SessionStart", format!("{PROBE_HANDOFF_CMD} claude"), None),
    ];
    // Entries a *previous* skein version wired that this one has since replaced/renamed. Purely
    // additive merging (below) would otherwise leave these stale forever in an already-provisioned
    // project's settings.json — and here that's not just dead weight: the old single unconditional
    // `box-status.sh notify` entry (replaced by three matcher-scoped notify-blocked/waiting/ignore
    // entries, since Notification's payload carries no field saying which type fired) still exists
    // in any store provisioned before this change, but box-status.sh no longer has a `notify` case
    // at all — it would now hit the passthrough branch and write a literal `status:"notify"`. Retire
    // it explicitly so upgrading a long-lived project store self-heals instead of accumulating a
    // silently-wrong hook forever. Exact-match only, so a user's own hook of the same name is untouched.
    // `bash "<script>" <args>` — see the note above `entries`.
    let wire = |cmd: &str| -> String {
        match cmd.split_once(' ') {
            Some((script, args)) => format!("bash \"{script}\" {args}"),
            None => format!("bash \"{cmd}\""),
        }
    };
    // Retire the pre-`bash`-wrapping form of every current entry (stores provisioned before the
    // exec-bit hardening carry the bare-path commands; purely-additive merging would keep both and
    // fire each hook twice), plus the explicitly renamed one.
    let mut obsolete: Vec<(&str, String)> = entries
        .iter()
        .map(|(ev, cmd, _)| (*ev, cmd.clone()))
        .collect();
    obsolete.push(("Notification", format!("{PROBE_STATUS_CMD} notify")));

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
    for (event, stale_cmd) in &obsolete {
        if let Some(arr) = hooks.get_mut(*event).and_then(|a| a.as_array_mut()) {
            arr.retain(|e| {
                !e.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                    hs.iter().any(|h| {
                        h.get("command").and_then(|c| c.as_str()) == Some(stale_cmd.as_str())
                    })
                })
            });
        }
    }
    for (event, cmd, matcher) in entries {
        let cmd = wire(&cmd);
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
    // Default boxes to Claude Code's fullscreen (alternate-screen) renderer — it draws far better in
    // the browser PTY than the inline renderer, and equals `CLAUDE_CODE_NO_FLICKER=1` without needing
    // an env var (sbx has no --env). Additive: never clobber a `tui` already set in the store.
    root.entry("tui").or_insert_with(|| json!("fullscreen"));
    // A default status line so a box shows context/usage out of the box. `or_insert` — a store that
    // already sets its own `statusLine` keeps it.
    let status_line = root.entry("statusLine").or_insert_with(
        || json!({ "type": "command", "command": STATUSLINE_CMD, "refreshIntervalMs": 30_000 }),
    );
    // Upgrade only Skein's generated default. A user-owned status-line object remains untouched.
    if status_line.get("command").and_then(Value::as_str) == Some(STATUSLINE_CMD) {
        status_line
            .as_object_mut()
            .expect("generated statusLine is an object")
            .entry("refreshIntervalMs")
            .or_insert_with(|| json!(30_000));
    }
    out
}

/// Codex's native lifecycle adapter. Current Codex exposes the same turn-boundary vocabulary Skein
/// needs, but not Claude's Notification/TodoWrite events: PermissionRequest is the needs-input
/// signal, UserPromptSubmit carries the active objective, and PostToolUse supplies tool telemetry.
///
/// The kit installs this generated value as a user-level `~/.codex/hooks.json` inside the sandbox.
/// User-level placement avoids project-trust suppressing the adapter in a fresh clone; Codex is
/// launched with `--dangerously-bypass-hook-trust` because these exact commands are generated and
/// vetted by Skein itself.
fn codex_hooks_with_probe() -> serde_json::Value {
    use serde_json::{json, Map, Value};

    let command = |event: &str, file: &str, args: &str| {
        let suffix = if args.is_empty() {
            String::new()
        } else {
            format!(" {args}")
        };
        format!(
            "bash \"$(git rev-parse --show-toplevel)/.claude/skein/bin/box-codex-hook.sh\" {event} {file}{suffix}"
        )
    };
    let mut hooks = Map::<String, Value>::new();
    let mut add = |event: &str, matcher: Option<&str>, cmd: String| {
        let mut entry = json!({
            "hooks": [{ "type": "command", "command": cmd, "timeout": 30 }]
        });
        if let Some(m) = matcher {
            entry
                .as_object_mut()
                .expect("hook entry is an object")
                .insert("matcher".into(), Value::String(m.into()));
        }
        hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("hook event is an array")
            .push(entry);
    };

    add(
        "SessionStart",
        Some("startup|resume|clear|compact"),
        command("SessionStart", "sandbox-bootstrap.sh", ""),
    );
    add(
        "SessionStart",
        Some("startup|resume|clear|compact"),
        command("SessionStart", "box-handoff.sh", "codex"),
    );
    add(
        "UserPromptSubmit",
        None,
        command("UserPromptSubmit", "box-status.sh", "working"),
    );
    add(
        "UserPromptSubmit",
        None,
        command("UserPromptSubmit", "box-codex-task.sh", ""),
    );
    add(
        "UserPromptSubmit",
        None,
        command("UserPromptSubmit", "mailbox.sh", "inbox"),
    );
    add(
        "UserPromptSubmit",
        None,
        command("UserPromptSubmit", "box-handoff.sh", "codex"),
    );
    add(
        "PermissionRequest",
        None,
        command("PermissionRequest", "box-status.sh", "notify-blocked"),
    );
    // Clear a permission-blocked state once the approved tool actually completes, without resetting
    // the turn timer/subagent count as a fresh UserPromptSubmit would.
    add(
        "PostToolUse",
        Some("*"),
        command("PostToolUse", "box-status.sh", "working-tool"),
    );
    add(
        "PostToolUse",
        Some("*"),
        command("PostToolUse", "box-codex-telemetry.sh", "tool"),
    );
    add(
        "SubagentStart",
        None,
        command("SubagentStart", "box-status.sh", "agent-start"),
    );
    add(
        "SubagentStop",
        None,
        command("SubagentStop", "box-status.sh", "agent-stop"),
    );
    add(
        "PreCompact",
        Some("manual|auto"),
        command("PreCompact", "box-status.sh", "compacting"),
    );
    add(
        "PostCompact",
        Some("manual|auto"),
        command("PostCompact", "box-status.sh", "compacted"),
    );
    for (file, args) in [
        ("box-status.sh", "waiting"),
        ("box-session.sh", "stop"),
        ("box-diff.sh", ""),
        ("box-journal.sh", ""),
        ("box-codex-telemetry.sh", "stop"),
        ("mailbox.sh", "stop-check"),
    ] {
        add("Stop", None, command("Stop", file, args));
    }
    json!({ "hooks": hooks })
}

/// Recent commit subjects on the box's branch (newest first) — the agent's own changelog,
/// a free, accurate "what was done" with no model call. For direct-mode boxes, computed host-side.
/// For clone-mode boxes, reads the file box-diff.sh wrote. Empty when neither is available.
pub fn recent_commits(name: &str) -> Vec<String> {
    // Prefer box-diff.sh's commit file: the box is on the feature branch and knows its own
    // commits. Host-side git would run against whatever `dir` resolves to on the host — for
    // clone-mode boxes that's the HOST's checkout (main), which lists the wrong commits.
    if let Some(path) = store_for_box(name)
        .map(|s| s.join("diffs").join(format!("{name}.commits")))
        .filter(|p| p.exists())
    {
        if let Ok(s) = fs::read_to_string(&path) {
            let v: Vec<String> = s
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
    }
    // Fall back to host-side git — useful for direct-mode boxes where the host dir IS the
    // box's working tree (box-diff.sh may not have run yet on a fresh box).
    if let Some(dir) = lookup_dir(name) {
        if let Some(range) = git_range(&dir) {
            let r = format!("{range}..HEAD");
            let mut command = Command::new("git");
            command.args(["-C", &dir, "log", "--format=%s", "-n", "20", &r]);
            if let Ok(out) = bounded_output(&mut command, "git log", Duration::from_secs(15)) {
                if out.status.success() {
                    let v: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !v.is_empty() {
                        return v;
                    }
                }
            }
        }
    }
    vec![]
}

/// The agent's own turn-end journal (`.skein/journal.md`), if it keeps one — the best "what was
/// done" source because it's written with full context (see the CLAUDE.md ritual). Returns the tail
/// (last ~40 lines), capped, or None when the box keeps no journal.
///
/// Reads `<store>/journals/<vmid>.md` first — box-journal.sh's Stop-hook copy of the box's own
/// `.skein/journal.md`, the only way the host can see it for a clone-mode box (a box's private clone
/// isn't visible to the host at all; `dir` for a repo box is the *shared* host working clone, not the
/// box's own). Falls back to reading `<dir>/.skein/journal.md` directly for a direct-mode box, where
/// the host-mounted repo genuinely is the box's own working tree.
pub fn read_journal(name: &str) -> Option<String> {
    let from_store = store_for_box(name)
        .map(|s| s.join("journals").join(format!("{name}.md")))
        .and_then(|p| fs::read_to_string(p).ok());
    let dir = lookup_dir(name);
    let from_dir = dir
        .as_deref()
        .and_then(|d| fs::read_to_string(Path::new(d).join(".skein").join("journal.md")).ok());
    let txt = from_store.or(from_dir)?;
    let tail: Vec<&str> = txt.lines().rev().take(40).collect();
    let mut s: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    const CAP: usize = 4000;
    if s.len() > CAP {
        let cut = s.len() - CAP;
        s = format!("…\n{}", &s[cut..]);
    }
    Some(s).filter(|s| !s.trim().is_empty())
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
    let reg = registry_entry_for_box(name);
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
        diff: read_diffstat_file(name),
        commits: recent_commits(name),
        journal: read_journal(name),
        last_message,
        blocked_on,
        signal_ts: sig.map(|s| s.ts).filter(|t| !t.is_empty()),
        pause,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
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
    fn managed_registry_current_branch_beats_launch_branch() {
        let _g = ENV_LOCK.lock().unwrap();
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
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
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

    #[test]
    fn takeover_keeps_user_hooks_and_drops_generated_legacy_hooks() {
        let hooks = serde_json::json!({
            "Stop": [{"hooks": [
                {"type":"command", "command":"$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-status.sh waiting"},
                {"type":"command", "command":"$CLAUDE_PROJECT_DIR/.claude/hooks/user.sh"}
            ]}]
        });
        let clean = sanitized_user_hooks(&hooks);
        let text = clean.to_string();
        assert!(!text.contains("skein/bin"));
        assert!(text.contains("hooks/user.sh"));
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
        assert_eq!(
            by_name(
                &parse_boxes(r#"{"name":"solo","status":"running"}"#),
                "solo"
            )
            .live,
            Some(Liveness::Running)
        );

        // A name-keyed object map: {name: {..}}.
        let obj = r#"{"z":{"status":"running"}}"#;
        assert_eq!(
            by_name(&parse_boxes(obj), "z").live,
            Some(Liveness::Running)
        );

        // Garbage is unavailable; a valid empty document is authoritative.
        assert!(parse_boxes("not json").is_empty());
        assert!(parse_boxes("[]").is_empty());
        assert!(parse_boxes_checked("not json").is_none());
        assert_eq!(parse_boxes_checked("[]").unwrap().len(), 0);
        assert_eq!(parse_boxes_checked(r#"{"sandboxes":[]}"#).unwrap().len(), 0);
        assert!(parse_boxes_checked(r#"{"error":"API error"}"#).is_none());
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
    fn transient_fleet_failure_reuses_last_good_and_empty_success_replaces_it() {
        let box_ = SbxBox {
            name: "live-box".into(),
            agent: "codex".into(),
            live: Some(Liveness::Running),
            dir: "/work".into(),
        };
        let mut last_good = None;
        let (fresh, degraded) = resolve_fleet(Some(vec![box_]), &mut last_good);
        assert_eq!(fresh.unwrap().len(), 1);
        assert!(!degraded);

        let (retained, degraded) = resolve_fleet(None, &mut last_good);
        assert_eq!(retained.unwrap()[0].name, "live-box");
        assert!(degraded);

        let (empty, degraded) = resolve_fleet(Some(vec![]), &mut last_good);
        assert!(empty.unwrap().is_empty());
        assert!(!degraded);
        let (retained_empty, degraded) = resolve_fleet(None, &mut last_good);
        assert!(retained_empty.unwrap().is_empty());
        assert!(degraded);
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
                check: String::new(),
                plane_project: String::new(),
                sync_connection: String::new(),
                sync_gateway_url: String::new(),
            },
            Repo {
                id: "web-api".into(),
                source: "s".into(),
                work: "/w".into(),
                store: "/s".into(),
                agent: "claude".into(),
                check: String::new(),
                plane_project: String::new(),
                sync_connection: String::new(),
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
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("st").join(".claude");
        fs::create_dir_all(&store).unwrap();
        let repos = vec![Repo {
            id: "thing".into(),
            source: "s".into(),
            work: "/w".into(),
            store: store.to_string_lossy().to_string(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
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
    fn repo_launch_command_uses_skein_kit_and_persistent_session() {
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
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        };
        // box name is the slug `thing-feat-auth`; the REAL branch (with the slash) is feat/auth.
        let cmd = repo_launch_command_as("thing-feat-auth", &repo, "feat/auth", None);
        assert!(cmd.contains("sbx create --clone --kit"));
        assert!(cmd.contains("kit'") || cmd.contains("/kit"));
        assert!(cmd.contains("--name 'thing-feat-auth'"));
        assert!(cmd.contains("'claude'")); // registered sbx agent name as the positional
        assert!(cmd.contains("'/work/thing'"));
        assert!(cmd.contains("&& sbx 'exec' '-it' 'thing-feat-auth'"));
        assert!(cmd.contains("tmux new-session -d -s skein-agent"));
        assert!(cmd.contains("timeout 120 claude update"));
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
    fn codex_launch_records_runtime_and_bypasses_generated_hook_review() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let repo = Repo {
            id: "skein".into(),
            source: "s".into(),
            work: "/work/skein".into(),
            store: home.join("store/.claude").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        };
        let cmd = repo_launch_command_as("skein-codex", &repo, "codex", Some("codex"));
        assert!(cmd.contains("'codex'"));
        assert!(cmd.contains("tmux new-session -d -s skein-agent"));
        assert!(cmd.contains("timeout 120 codex update"));
        assert!(cmd.contains("codex --no-alt-screen --dangerously-bypass-hook-trust"));
        assert!(!cmd.contains("codex resume --last"));
        assert_eq!(
            launch_spec_agent(&repo, "skein-codex").as_deref(),
            Some("codex")
        );
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn codex_status_line_setup_defaults_without_overriding_user_choice() {
        let home = tempdir();
        let codex = home.join(".codex");
        let setup = runtime_adapter("codex").unwrap().interactive_setup;
        let run = || {
            Command::new("bash")
                .arg("-c")
                .arg(setup)
                .env("HOME", &home)
                .status()
                .unwrap()
        };

        assert!(run().success());
        let config = codex.join("config.toml");
        let generated = fs::read_to_string(&config).unwrap();
        assert!(generated.contains("[tui]"));
        assert!(generated.contains("status_line = [] # skein custom statusline"));

        fs::write(&config, "[tui]\nanimations = false\n").unwrap();
        assert!(run().success());
        let extended = fs::read_to_string(&config).unwrap();
        assert!(extended.contains("animations = false"));
        assert!(extended.contains("status_line = [] # skein custom statusline"));

        fs::write(
            &config,
            "[tui]\nstatus_line = [\"context-used\", \"five-hour-limit\", \"weekly-limit\", \"used-tokens\", \"git-branch\", \"model-with-reasoning\"]\n",
        )
        .unwrap();
        assert!(run().success());
        assert!(fs::read_to_string(&config)
            .unwrap()
            .contains("status_line = [] # skein custom statusline"));

        fs::write(
            &config,
            "[tui]\nstatus_line = null # skein custom statusline\n",
        )
        .unwrap();
        assert!(run().success());
        assert!(fs::read_to_string(&config)
            .unwrap()
            .contains("status_line = [] # skein custom statusline"));

        let chosen = "[tui]\nstatus_line = [\"model\"]\n";
        fs::write(&config, chosen).unwrap();
        assert!(run().success());
        assert_eq!(fs::read_to_string(config).unwrap(), chosen);
    }

    #[test]
    fn statusline_renderer_matches_bars_projection_colours_and_optional_segments() {
        use std::io::Write as _;

        let render = |input: &str| {
            // Exercise the same shell-quoted embedded form used by the Codex browser adapter.
            // Direct-workspace boxes do not necessarily mount the shared store, so a file-backed
            // test would miss the failure this path is designed to prevent.
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(format!("bash -c {}", sh_quote(STATUSLINE_SH)))
                .env("SKEIN_STATUSLINE_NOW", "1000000")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        let input = r#"{
          "context_window":{"used_percentage":25,"total_input_tokens":12345,"context_window_size":1000000},
          "rate_limits":{
            "five_hour":{"used_percentage":40,"resets_at":1009000},
            "seven_day":{"used_percentage":70,"resets_at":1302400}
          },
          "cost":{"total_cost_usd":1.234},
          "model":{"display_name":"Opus 4.8 (1M context)"}
        }"#;
        let line = render(input);

        assert!(line.contains("\x1b[0;32m███░░░░░░░░░"));
        assert!(line.contains("25%\x1b[0m \x1b[2m12.3k/1.0M"));
        assert!(line.contains("\x1b[0;31m████▒▒▒▒▒░░░"));
        assert!(line.contains("40%\x1b[0m→80%"));
        assert!(line.contains("2h30m left"));
        assert!(line.contains("\x1b[0;31m████████▒▒▒▒"));
        assert!(line.contains("70%\x1b[0m→140%"));
        assert!(line.contains("3d12h left"));
        assert!(line.contains("$1.23"));
        assert!(line.contains("Opus 4.8(1M)"));
        assert_eq!(line.matches(" │ ").count(), 4);

        let partial = render(
            r#"{"context_window":{"used_percentage":70,"total_input_tokens":70,"context_window_size":100}}"#,
        );
        assert!(partial.contains("\x1b[0;33m████████░░░░"));
        assert!(!partial.contains("5H"));
        assert!(!partial.contains("7D"));
        assert!(!partial.contains('$'));
        assert!(!partial.contains(" │ "));
    }

    #[test]
    fn codex_statusline_uses_default_quota_when_named_pool_arrives_last() {
        let home = tempdir();
        let sessions = home.join(".codex/sessions/2026/07/14");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            home.join(".codex/config.toml"),
            "[tui]\nstatus_line = [] # skein custom statusline\n",
        )
        .unwrap();
        fs::write(
            sessions.join("rollout.jsonl"),
            concat!(
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":100},"model_context_window":1000},"rate_limits":{"limit_id":"default-pool","limit_name":null,"primary":{"used_percent":18,"window_minutes":10080,"resets_at":2000000000},"secondary":null,"individual_limit":null}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":200},"model_context_window":1000},"rate_limits":{"limit_id":"named-pool","limit_name":"Future Model Pool","primary":{"used_percent":0,"window_minutes":10080,"resets_at":2100000000},"secondary":null,"individual_limit":null}}}"#,
                "\n",
                r#"{"type":"turn_context","payload":{"model":"future-model","effort":"medium"}}"#,
                "\n"
            ),
        )
        .unwrap();

        let command = runtime_adapter("codex").unwrap().statusline_input.unwrap();
        let output = Command::new("bash")
            .arg("-c")
            .arg(command)
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

        assert_eq!(payload["rate_limits"]["seven_day"]["used_percentage"], 18);
        assert_eq!(
            payload["rate_limits"]["seven_day"]["resets_at"],
            2000000000_i64
        );
        // Context remains tied to the newest token event, independent of quota-pool selection.
        assert_eq!(payload["context_window"]["total_input_tokens"], 200);
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
        // the full structure: skein runtime + the user-filled content homes.
        for d in [
            "mailbox",
            "status",
            "tasks",
            "journals",
            "handoffs",
            "skein/launch",
            "skein/bin",
            "memory",
            "skills",
            "hooks",
            "shared-rw",
            "shared-home",
        ] {
            assert!(store.join(d).is_dir(), "missing {d}");
        }
        // skein installs all the machinery so an empty store works end-to-end
        for f in [
            "skein/bin/box-status.sh",
            "skein/bin/box-journal.sh",
            "skein/bin/box-token-usage.sh",
            "skein/bin/box-codex-task.sh",
            "skein/bin/box-codex-telemetry.sh",
            "skein/bin/box-handoff.sh",
            "skein/bin/sandbox-bootstrap.sh",
            "skein/bin/shared-home.sh",
            "skein/bin/agent-guide.sh",
            "skein/bin/install-codex-hooks.sh",
            "skein/SHARED-HOME.md",
            "skein/bin/mailbox.sh",
            "skein/bin/statusline-command.sh",
            // Work tracking rides the store, not the kit — that is what lets a box created before
            // the feature existed still be wired up.
            "skein/bin/sync-install.sh",
            "skein/sync/work-tracking.block.md",
            "skein/sync/work-tracking.memory.md",
            "skein/sync/work-tracking.skill.md",
        ] {
            assert!(store.join(f).is_file(), "missing {f}");
        }
        assert!(store.join("settings.json").is_file());
        assert!(store.join("skein/codex-hooks.json").is_file());
        assert!(store.join("skein/probe-revision").is_file());
        assert!(store.join("skein/runtimes.tsv").is_file());
        // layout is documented for the user to (optionally) fill
        assert!(store.join("README.md").is_file());
        // settings wire the SessionStart bootstrap + a default statusLine
        let s: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(store.join("settings.json")).unwrap())
                .unwrap();
        assert!(s["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("statusline-command.sh"));
        assert_eq!(s["statusLine"]["refreshIntervalMs"], 30_000);
        assert!(s["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("sandbox-bootstrap.sh"));
        let kit = ensure_kit().unwrap();
        assert!(kit.join("spec.yaml").is_file());
        let kit_text = fs::read_to_string(kit.join("spec.yaml")).unwrap();
        assert!(
            !kit_text.contains("${"),
            "sbx treats dollar-brace shell expansions as kit placeholders"
        );
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn shared_home_links_two_private_homes_and_refuses_real_path() {
        let store = tempdir().join("store/.claude");
        ensure_store(&store).unwrap();
        let helper = store.join("skein/bin/shared-home.sh");
        let home_a = tempdir().join("home-a");
        let home_b = tempdir().join("home-b");
        fs::create_dir_all(&home_a).unwrap();
        fs::create_dir_all(&home_b).unwrap();

        let run = |home: &Path| {
            Command::new("bash")
                .arg(&helper)
                .arg(&store)
                .env("HOME", home)
                .output()
                .unwrap()
        };
        assert!(run(&home_a).status.success());
        assert!(run(&home_b).status.success());
        assert_eq!(
            fs::read_link(home_a.join("shared")).unwrap(),
            store.join("shared-home")
        );
        assert_eq!(
            fs::read_link(home_b.join("shared")).unwrap(),
            store.join("shared-home")
        );

        fs::write(home_a.join("shared/from-a.txt"), "visible in b").unwrap();
        assert_eq!(
            fs::read_to_string(home_b.join("shared/from-a.txt")).unwrap(),
            "visible in b"
        );
        fs::write(home_a.join("private-sentinel"), "private").unwrap();
        assert!(!home_b.join("private-sentinel").exists());

        fs::remove_file(home_b.join("shared")).unwrap();
        fs::create_dir(home_b.join("shared")).unwrap();
        fs::write(home_b.join("shared/do-not-clobber"), "mine").unwrap();
        let conflict = run(&home_b);
        assert!(!conflict.status.success());
        assert!(String::from_utf8_lossy(&conflict.stderr).contains("refusing to replace real path"));
        assert_eq!(
            fs::read_to_string(home_b.join("shared/do-not-clobber")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn agent_guide_uses_native_instruction_files_without_prompt_hook_bloat() {
        use std::os::unix::fs::symlink;

        let store = tempdir().join("store/.claude");
        let home = tempdir().join("home");
        let work = tempdir().join("work");
        ensure_store(&store).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(&work).unwrap();
        symlink(&store, work.join(".claude")).unwrap();
        fs::write(home.join(".codex/AGENTS.md"), "# My existing guidance\n").unwrap();
        let helper = store.join("skein/bin/agent-guide.sh");
        let run = |normal: &str, override_: &str| {
            Command::new("bash")
                .arg(&helper)
                .arg(&store)
                .arg(normal)
                .arg(override_)
                .env("HOME", &home)
                .output()
                .unwrap()
        };

        assert!(run(".codex/AGENTS.md", ".codex/AGENTS.override.md")
            .status
            .success());
        assert!(run(".codex/AGENTS.md", ".codex/AGENTS.override.md")
            .status
            .success());
        let agents = fs::read_to_string(home.join(".codex/AGENTS.md")).unwrap();
        assert!(agents.contains("My existing guidance"));
        assert_eq!(agents.matches("skein:shared-home:start").count(), 1);

        fs::write(
            home.join(".codex/AGENTS.override.md"),
            "# My temporary override\n",
        )
        .unwrap();
        assert!(run(".codex/AGENTS.md", ".codex/AGENTS.override.md")
            .status
            .success());
        let override_ = fs::read_to_string(home.join(".codex/AGENTS.override.md")).unwrap();
        assert!(override_.contains("My temporary override"));
        assert_eq!(override_.matches("skein:shared-home:start").count(), 1);

        assert!(run(".claude/CLAUDE.md", "").status.success());
        assert!(fs::read_to_string(home.join(".claude/CLAUDE.md"))
            .unwrap()
            .contains("$HOME/shared"));

        // Without a real takeover, the turn-scoped handoff hook must emit no context at all.
        let handoff = Command::new("bash")
            .arg(store.join("skein/bin/box-handoff.sh"))
            .arg("codex")
            .env("CLAUDE_PROJECT_DIR", &work)
            .env("SANDBOX_VM_ID", "box-a")
            .output()
            .unwrap();
        assert!(handoff.status.success());
        assert!(handoff.stdout.is_empty());
    }

    #[test]
    fn codex_hook_installer_preserves_user_hooks_and_is_idempotent() {
        let store = tempdir().join("store/.claude");
        let home = tempdir().join("home");
        ensure_store(&store).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(
            home.join(".codex/hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"hooks/user.sh"}]}]}}"#,
        )
        .unwrap();
        let installer = store.join("skein/bin/install-codex-hooks.sh");
        let run = || {
            Command::new("bash")
                .arg(&installer)
                .arg(&store)
                .env("HOME", &home)
                .status()
                .unwrap()
        };
        assert!(run().success());
        let once: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".codex/hooks.json")).unwrap())
                .unwrap();
        assert!(run().success());
        let twice: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".codex/hooks.json")).unwrap())
                .unwrap();
        assert_eq!(once, twice);
        let text = twice.to_string();
        assert!(text.contains("hooks/user.sh"));
        assert!(text.contains("box-status.sh"));
    }

    #[test]
    fn shared_home_import_is_dry_run_first_explicit_and_filtered() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let _g = ENV_LOCK.lock().unwrap();
        let skein_home = tempdir();
        let work = tempdir().join("work");
        let store = tempdir().join("store/.claude");
        let box_home = tempdir().join("box-home");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&box_home).unwrap();
        assert!(Command::new("git")
            .arg("init")
            .arg(&work)
            .status()
            .unwrap()
            .success());
        ensure_store(&store).unwrap();
        symlink(&store, work.join(".claude")).unwrap();
        fs::write(box_home.join("CASE_PREP.md"), "questions").unwrap();
        fs::create_dir_all(box_home.join("samples/target")).unwrap();
        fs::write(box_home.join("samples/reference.pdf"), "pdf").unwrap();
        fs::write(box_home.join("samples/locked.json"), "unreadable").unwrap();
        fs::set_permissions(
            box_home.join("samples/locked.json"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        fs::write(box_home.join("samples/.env"), "SECRET=never").unwrap();
        fs::write(box_home.join("samples/target/build.bin"), "large").unwrap();
        fs::create_dir_all(box_home.join(".ssh")).unwrap();
        fs::write(box_home.join(".ssh/id_ed25519"), "private").unwrap();
        fs::create_dir_all(box_home.join("workspace")).unwrap();
        symlink("CASE_PREP.md", box_home.join("shortcut")).unwrap();

        env::set_var("SKEIN_HOME", &skein_home);
        save_repos(&[Repo {
            id: "demo".into(),
            source: work.to_string_lossy().into_owned(),
            work: work.to_string_lossy().into_owned(),
            store: store.to_string_lossy().into_owned(),
            agent: "codex".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        let bin = tempdir().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let sbx = bin.join("sbx");
        fs::write(
            &sbx,
            r#"#!/usr/bin/env bash
set -e
[ "$1" = exec ]
box="$2"
shell="$5"
cd "$FAKE_BOX_WORK"
HOME="$FAKE_BOX_HOME" SANDBOX_VM_ID="$box" bash -c "$shell"
"#,
        )
        .unwrap();
        fs::set_permissions(&sbx, fs::Permissions::from_mode(0o755)).unwrap();
        let old_path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{old_path}", bin.display()));
        env::set_var("FAKE_BOX_HOME", &box_home);
        env::set_var("FAKE_BOX_WORK", &work);

        let inventory = shared_home_inventory("demo-old-claude").unwrap();
        let candidate = |name: &str| inventory.iter().find(|item| item.name == name).unwrap();
        assert!(candidate("CASE_PREP.md").eligible);
        assert!(candidate("samples").eligible);
        assert!(!candidate(".ssh").eligible);
        assert!(!candidate("workspace").eligible);
        assert!(!candidate("shortcut").eligible);
        assert!(!store.join("shared-home/CASE_PREP.md").exists());

        let result = import_shared_home(
            "demo-old-claude",
            &["CASE_PREP.md".into(), "samples".into()],
        )
        .unwrap();
        assert!(
            result.contains("locked.json"),
            "skipped path must be reported"
        );
        assert_eq!(
            fs::read_to_string(store.join("shared-home/CASE_PREP.md")).unwrap(),
            "questions"
        );
        assert!(store.join("shared-home/samples/reference.pdf").is_file());
        assert!(!store.join("shared-home/samples/locked.json").exists());
        assert!(!store.join("shared-home/samples/.env").exists());
        assert!(!store.join("shared-home/samples/target").exists());
        assert!(store
            .join("skein/imports")
            .read_dir()
            .unwrap()
            .next()
            .is_some());

        env::set_var("PATH", old_path);
        env::remove_var("FAKE_BOX_HOME");
        env::remove_var("FAKE_BOX_WORK");
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn ensure_store_scaffolds_without_clobbering_user_data() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("shared").join(".claude");

        // the user pre-populates the folder with their own data + a custom README.
        fs::create_dir_all(store.join("memory")).unwrap();
        fs::write(store.join("memory").join("mine.md"), "user memory").unwrap();
        fs::write(store.join("README.md"), "MY OWN README").unwrap();

        ensure_store(&store).unwrap();

        // scaffolding added the missing structure + machinery …
        assert!(store.join("skills").is_dir());
        assert!(store.join("skein/bin/box-status.sh").is_file());
        assert!(store.join("skein/bin/sandbox-bootstrap.sh").is_file());
        // … but never clobbered what the user already put there.
        assert_eq!(
            fs::read_to_string(store.join("memory").join("mine.md")).unwrap(),
            "user memory"
        );
        assert_eq!(
            fs::read_to_string(store.join("README.md")).unwrap(),
            "MY OWN README",
            "an existing README is left alone"
        );
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
        assert!(merged["statusLine"]["refreshIntervalMs"].is_null());
        let ups = merged["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert!(ups
            .iter()
            .any(|e| e["hooks"][0]["command"] == "slice-gate.sh"));
        // … and skein's are added — every one invoked via `bash "<script>" <args>` so a squashed
        // exec bit on the shared mount can't silently kill the whole probe set.
        assert!(ups.iter().any(|e| e["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(r#"box-status.sh" working"#)));
        assert!(ups
            .iter()
            .filter_map(|e| e["hooks"][0]["command"].as_str())
            .filter(|c| c.contains("skein/bin"))
            .all(|c| c.starts_with("bash \"")));
        // Stop: box-status.sh + box-session.sh + box-diff.sh + box-journal.sh + box-token-usage.sh
        // + mailbox.sh stop-check.
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 6);
        let post = merged["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post[0]["matcher"], "TodoWrite");

        // idempotent: re-running adds nothing.
        let again = settings_with_probe(&merged);
        assert_eq!(
            again["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            ups.len()
        );
        assert_eq!(again["hooks"]["Stop"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn settings_with_probe_from_empty() {
        let merged = settings_with_probe(&serde_json::json!({}));
        // Every event gets at least one hook entry.
        for ev in [
            "PreToolUse",
            "SubagentStop",
            "StopFailure",
            "PreCompact",
            "PostCompact",
            "SessionEnd",
            "PostToolUse",
        ] {
            assert_eq!(
                merged["hooks"][ev].as_array().unwrap().len(),
                1,
                "missing {ev}"
            );
        }
        assert_eq!(
            merged["hooks"]["SessionStart"].as_array().unwrap().len(),
            2,
            "SessionStart needs bootstrap + handoff context"
        );
        // UserPromptSubmit: box-status.sh (turn-state reset) + mailbox.sh inbox (turn-boundary
        // mail delivery — the fix for mail sitting unread past SessionStart).
        let ups_cmds: Vec<&str> = merged["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["hooks"][0]["command"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            ups_cmds.len(),
            3,
            "UserPromptSubmit needs status + mailbox + handoff hooks"
        );
        assert!(ups_cmds
            .iter()
            .any(|c| c.contains(r#"box-status.sh" working"#)));
        assert!(ups_cmds.iter().any(|c| c.contains(r#"mailbox.sh" inbox"#)));
        assert!(ups_cmds
            .iter()
            .any(|c| c.contains(r#"box-handoff.sh" claude"#)));
        // Stop: box-status.sh (turn-state) + box-session.sh (narrative signal) + box-diff.sh (diff
        // snapshot) + box-journal.sh (journal copy) + box-token-usage.sh (per-turn token log) +
        // mailbox.sh stop-check (blocks the stop if mail arrived mid-turn).
        assert_eq!(
            merged["hooks"]["Stop"].as_array().unwrap().len(),
            6,
            "Stop needs 6 hooks"
        );
        let stop_cmds: Vec<&str> = merged["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["hooks"][0]["command"].as_str().unwrap_or(""))
            .collect();
        assert!(
            stop_cmds
                .iter()
                .any(|c| c.contains(r#"box-status.sh" waiting"#)),
            "status hook missing"
        );
        assert!(
            stop_cmds
                .iter()
                .any(|c| c.contains(r#"box-session.sh" stop"#)),
            "session (narrative signal) hook missing"
        );
        assert!(
            stop_cmds.iter().any(|c| c.contains("box-diff.sh")),
            "diff hook missing"
        );
        assert!(
            stop_cmds.iter().any(|c| c.contains("box-journal.sh")),
            "journal hook missing"
        );
        assert!(
            stop_cmds.iter().any(|c| c.contains("box-token-usage.sh")),
            "token-usage hook missing"
        );
        assert!(
            stop_cmds
                .iter()
                .any(|c| c.contains(r#"mailbox.sh" stop-check"#)),
            "mailbox stop-check missing"
        );
        // Notification keeps matcher-scoped compatibility plus one unconditional payload fallback.
        let notif = merged["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(
            notif.len(),
            5,
            "Notification needs matcher hooks plus payload fallback"
        );
        let has = |m: &str, frag: &str| -> bool {
            notif.iter().any(|e| {
                e["matcher"] == m
                    && e["hooks"][0]["command"]
                        .as_str()
                        .is_some_and(|c| c.contains(frag))
            })
        };
        assert!(has(
            "permission_prompt|elicitation_dialog|agent_needs_input",
            r#"box-status.sh" notify-blocked"#
        ));
        assert!(
            has(
                "permission_prompt|elicitation_dialog|agent_needs_input",
                r#"box-session.sh" ask"#
            ),
            "session ask hook must ride the same blocked matcher"
        );
        assert!(has("idle_prompt", r#"box-status.sh" notify-waiting"#));
        assert!(has(
            "auth_success|elicitation_complete|elicitation_response|agent_completed",
            r#"box-status.sh" notify-ignore"#
        ));
        assert!(notif.iter().any(|e| e.get("matcher").is_none()
            && e["hooks"][0]["command"]
                .as_str()
                .is_some_and(|c| c.contains("notify-auto"))));
        // sub-agent tracking: PreToolUse is scoped to the Task tool
        assert_eq!(merged["hooks"]["PreToolUse"][0]["matcher"], "Task");
        // a default status line is wired when the store doesn't set one
        assert!(merged["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("statusline-command.sh"));
    }

    #[test]
    fn settings_with_probe_retires_the_old_unconditional_notify_entry() {
        // A project store provisioned by a pre-matcher-fix skein has this exact stale entry — no
        // matcher, calling a `notify` mode box-status.sh no longer implements at all (it would now
        // fall through to the passthrough branch and write a bogus status:"notify"). Upgrading must
        // remove it, not just add the three new matcher-scoped entries alongside it.
        let stale_cmd = format!("{PROBE_STATUS_CMD} notify");
        let existing = serde_json::json!({
            "hooks": {
                "Notification": [ { "hooks": [ { "type": "command", "command": stale_cmd } ] } ]
            }
        });
        let merged = settings_with_probe(&existing);
        let notif = merged["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(
            notif.len(),
            5,
            "the stale entry must be replaced by matcher hooks plus the safe payload fallback"
        );
        assert!(
            notif
                .iter()
                .all(|e| e["hooks"][0]["command"] != stale_cmd.as_str()),
            "stale entry should be gone"
        );
        assert_eq!(
            notif.iter().filter(|e| e.get("matcher").is_none()).count(),
            1
        );
        assert!(notif.iter().any(|e| e["hooks"][0]["command"]
            .as_str()
            .is_some_and(|command| command.contains("notify-auto"))));

        // Idempotent from here on: re-running doesn't reintroduce or duplicate anything.
        let again = settings_with_probe(&merged);
        assert_eq!(again["hooks"]["Notification"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn codex_hooks_cover_attention_session_telemetry_and_handoff() {
        let h = codex_hooks_with_probe();
        let hooks = h["hooks"].as_object().unwrap();
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PermissionRequest",
            "PostToolUse",
            "SubagentStart",
            "SubagentStop",
            "PreCompact",
            "PostCompact",
            "Stop",
        ] {
            assert!(
                hooks
                    .get(event)
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty()),
                "missing Codex {event}"
            );
        }
        let commands = |event: &str| -> Vec<&str> {
            hooks[event]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|e| e["hooks"][0]["command"].as_str())
                .collect()
        };
        assert!(commands("PermissionRequest")
            .iter()
            .any(|c| c.contains("notify-blocked")));
        assert!(commands("UserPromptSubmit")
            .iter()
            .any(|c| c.contains("box-codex-task.sh")));
        assert!(commands("Stop")
            .iter()
            .any(|c| c.contains("box-session.sh")));
        assert!(commands("Stop")
            .iter()
            .any(|c| c.contains("box-codex-telemetry.sh")));
        assert!(commands("SessionStart")
            .iter()
            .any(|c| c.contains("box-handoff.sh")));
        assert!(commands("SessionStart")
            .iter()
            .all(|c| c.contains("box-codex-hook.sh") && c.contains("SessionStart")));
    }

    #[test]
    fn codex_hook_adapter_wraps_context_and_silent_probes() {
        let dir = tempdir();
        let wrapper = dir.join("box-codex-hook.sh");
        fs::write(&wrapper, PROBE_CODEX_HOOK_SH).unwrap();
        fs::write(
            dir.join("emit.sh"),
            "#!/bin/sh\nprintf 'handoff context\\n'\n",
        )
        .unwrap();
        fs::write(dir.join("silent.sh"), "#!/bin/sh\nexit 0\n").unwrap();

        let contextual = Command::new("bash")
            .args([wrapper.to_str().unwrap(), "SessionStart", "emit.sh"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(contextual.status.success());
        let value: serde_json::Value = serde_json::from_slice(&contextual.stdout).unwrap();
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"],
            "handoff context"
        );

        let silent = Command::new("bash")
            .args([wrapper.to_str().unwrap(), "UserPromptSubmit", "silent.sh"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(silent.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&silent.stdout).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn settings_with_probe_upgrades_bare_path_commands_to_bash_wrapped() {
        // A store provisioned before the exec-bit hardening carries bare-path commands. The merge
        // must RETIRE those (they're skein's own, now re-wired via `bash "<script>" <args>`) —
        // additive-only merging would leave both forms and fire every hook twice per event.
        let existing = serde_json::json!({
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": format!("{PROBE_STATUS_CMD} waiting") } ] },
                    { "hooks": [ { "type": "command", "command": PROBE_DIFF_CMD } ] },
                    // a user's own Stop hook must survive the upgrade untouched
                    { "hooks": [ { "type": "command", "command": "my-own-stop-hook.sh" } ] }
                ]
            }
        });
        let merged = settings_with_probe(&existing);
        let stop_cmds: Vec<&str> = merged["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["hooks"][0]["command"].as_str().unwrap_or(""))
            .collect();
        assert!(
            stop_cmds.contains(&"my-own-stop-hook.sh"),
            "user hook must survive"
        );
        assert!(
            !stop_cmds
                .iter()
                .any(|c| *c == format!("{PROBE_STATUS_CMD} waiting") || *c == PROBE_DIFF_CMD),
            "bare-path skein commands must be retired, not duplicated"
        );
        // 6 skein Stop hooks (all bash-wrapped) + 1 user hook
        assert_eq!(stop_cmds.len(), 7);
    }

    #[test]
    fn native_launch_command_builds_create_then_persistent_attach() {
        let _g = ENV_LOCK.lock().unwrap();
        env::remove_var("SKEIN_LAUNCH_CMD");
        env::set_var("SKEIN_KIT", "/abs/kit");
        env::set_var("SKEIN_AGENT", "claude");
        env::set_var("SKEIN_STORE", "/abs/store");
        let cmd = launch_command("thing-feat-x", "feat-x");
        assert!(cmd.starts_with(
            "sbx create --clone --kit '/abs/kit' --name 'thing-feat-x' 'claude' . '/abs/store'"
        ));
        assert!(cmd.contains("&& sbx 'exec' '-it' 'thing-feat-x'"));
        assert!(cmd.contains("tmux new-session -d -s skein-agent"));
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
    fn the_transcript_reader_keeps_the_conversation_and_drops_the_bookkeeping() {
        // Line shapes taken from a real 14MB Claude Code record, whose 3800 lines are: user,
        // assistant, system, mode, permission-mode, last-prompt, file-history-snapshot,
        // file-history-delta, attachment, queue-operation, bridge-session. Only two are the
        // conversation; 913 of the "user" lines are tool_result payloads that would drown it.
        let body = concat!(
            r#"{"type":"mode","mode":"default"}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-07-31T08:10:00Z","message":{"role":"user","content":"restart the server"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-31T08:10:05Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"long private reasoning"},{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo build --bins","description":"build"}}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-07-31T08:10:09Z","message":{"role":"user","content":[{"type":"tool_result","content":"<12MB of build output>"}]}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-07-31T08:11:00Z","message":{"role":"assistant","content":[{"type":"text","text":"subagent says hi"}]}}"#,
            "\n",
            r#"{"type":"file-history-snapshot","messageId":"x"}"#,
            "\n",
        );
        let msgs = parse_transcript_jsonl(body);
        assert_eq!(
            msgs.len(),
            3,
            "two conversation turns + one subagent line: {msgs:#?}"
        );
        assert_eq!(msgs[0].role, "user");
        assert_eq!(
            msgs[0].text, "restart the server",
            "a string content body is the human's message"
        );
        assert_eq!(
            msgs[1].text, "On it.",
            "thinking is not the conversation and is dropped"
        );
        assert_eq!(
            msgs[1].tools,
            vec!["Bash(cargo build --bins)"],
            "tool calls are summarised, not inlined"
        );
        assert!(
            msgs[2].sidechain,
            "subagent chatter is marked so the UI can dim it"
        );
        // a tool_result-only turn carries no human-readable content and must not become a message
        assert!(
            !msgs.iter().any(|m| m.text.contains("12MB")),
            "tool results stay out of the view"
        );
    }

    #[test]
    fn a_half_line_from_the_tail_is_never_parsed_as_a_message() {
        // Reading the last N bytes of a JSONL file almost always lands mid-line. That fragment is
        // not a record; parsing it would render a message with no role and half a sentence.
        let fragment =
            r#"pe":"assistant","message":{"content":[{"type":"text","text":"...half a line"}]}}"#;
        assert!(parse_transcript_jsonl(fragment).is_empty());
        let after = format!(
            "{fragment}\n{}",
            r#"{"type":"user","message":{"content":"whole line"}}"#
        );
        // the reader drops everything up to the first newline when it tailed; the parser also
        // refuses the fragment on its own, so both halves of the defence hold
        assert_eq!(parse_transcript_jsonl(&after).len(), 1);
    }

    #[test]
    fn long_messages_and_tool_details_are_clipped_not_dumped() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("abcdefghij", 4), "abcd…");
        let huge = "x".repeat(TRANSCRIPT_MAX_TEXT + 500);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{huge}"}}]}}}}"#
        );
        let msgs = parse_transcript_jsonl(&line);
        assert_eq!(
            msgs[0].text.chars().count(),
            TRANSCRIPT_MAX_TEXT + 1,
            "clipped, with the ellipsis"
        );
        // a write tool's input is the whole file — the summary must take the path, not the content
        let write = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/src/lib.rs","content":"...entire file..."}}]}}"#;
        assert_eq!(
            parse_transcript_jsonl(write)[0].tools,
            vec!["Write(/src/lib.rs)"]
        );
    }

    #[test]
    fn a_checks_exit_code_comes_from_the_marker_not_the_shell() {
        // `sbx exec` reports the wrapper shell's status, so the check's own code rides a marker line.
        let raw = format!(
            "{VERIFY_FP}abc1234+992 1\nrunning 3 tests\ntest result: FAILED\n{VERIFY_EXIT}101\n"
        );
        let (fp, exit, body) = parse_verify_output(&raw);
        assert_eq!(fp, "abc1234+992 1");
        assert_eq!(exit, Some(101));
        assert!(body.contains("test result: FAILED"));
        assert!(
            !body.contains("SKEIN_VERIFY"),
            "markers are protocol, not output"
        );
        // A run that never reached the marker was killed or timed out — that is NOT a failing test,
        // and run_verify refuses to record it as one.
        let (_, none, _) = parse_verify_output("running 3 tests\n");
        assert_eq!(none, None);
    }

    #[test]
    fn the_stored_output_keeps_the_end_where_the_failure_is() {
        assert_eq!(tail_of("short", 100), "short");
        let long = format!("{}\nFAILED: the last line\n", "noise\n".repeat(4000));
        let cut = tail_of(&long, 200);
        assert!(cut.contains("FAILED: the last line"));
        assert!(cut.starts_with("… earlier output trimmed …"));
        assert!(cut.len() < 400);
        // never splits a line in half — the first kept line is a whole one
        assert!(cut.lines().nth(1).is_some_and(|l| l == "noise"));
    }

    #[test]
    fn a_repo_claims_work_through_the_connection_it_picks() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://shared.example",
            Some("pat_shared"),
        )
        .unwrap();
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: "shared".into(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        assert_eq!(sync_gateway_for_box("web-main"), "https://shared.example");
        // Two products tracked in different Plane instances can't share a claim namespace, and the
        // token a box carries is only valid at the gateway that minted it — so switching backlogs
        // switches the credential too, which is exactly what picking a whole connection buys.
        upsert_connection(Some("own"), "own", "https://own.example/", Some("pat_own")).unwrap();
        set_repo_settings("web", None, None, Some("own")).unwrap();
        assert_eq!(
            sync_gateway_for_box("web-main"),
            "https://own.example",
            "trailing slash trimmed so /mcp doesn't double up"
        );
        assert_eq!(
            connection_for_box("web-main").map(|c| connection_token(&c.id).unwrap()),
            Some("pat_own".to_string()),
            "the PAT that mints has to be the one that authenticates AT that gateway"
        );
        assert_eq!(
            sync_mcp_url(&sync_gateway_for_box("web-main")),
            "https://own.example/mcp"
        );
        // Clearing means not tracked — an explicit setting, not a gap to be filled by a default.
        set_repo_settings("web", None, None, Some("")).unwrap();
        assert!(connection_for_box("web-main").is_none());
        assert_eq!(sync_gateway_for_box("web-main"), "");
        // A selection naming nothing would read as "tracked" and behave as "not tracked".
        assert!(set_repo_settings("web", None, None, Some("nope")).is_err());
        // One call can carry every field, and the fields don't disturb each other.
        set_repo_settings("web", Some("cargo test"), None, Some("own")).unwrap();
        let saved = load_repos().into_iter().find(|r| r.id == "web").unwrap();
        assert_eq!(
            (saved.check.as_str(), saved.sync_connection.as_str()),
            ("cargo test", "own")
        );
        assert_eq!(saved.plane_project, "", "a field left None is left alone");
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // A box that belongs to no registered repo is skein's old single-repo layout. One connection is
    // unambiguous; two is a guess, and the wrong guess mints a real credential against the wrong
    // backlog — so it declines rather than picking.
    #[test]
    fn an_unregistered_box_only_inherits_a_connection_when_there_is_no_choice() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        assert!(
            connection_for_box("stray-main").is_none(),
            "none configured"
        );
        upsert_connection(Some("one"), "one", "https://one.example", None).unwrap();
        assert_eq!(
            connection_for_box("stray-main").map(|c| c.id),
            Some("one".into())
        );
        upsert_connection(Some("two"), "two", "https://two.example", None).unwrap();
        assert!(
            connection_for_box("stray-main").is_none(),
            "two backlogs and no repo to say which — refuse rather than guess"
        );
        // A *registered* repo with nothing picked is not a gap: it is "not tracked", and no number
        // of connections may override that.
        save_repos(&[Repo {
            id: "stray".into(),
            source: "/s".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        remove_connection("two").unwrap();
        assert!(
            connection_for_box("stray-main").is_none(),
            "an explicit 'not tracked' outranks a sole connection"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // The upgrade path off the old layout, where the gateway was per-repo and the PAT was one file
    // for the whole host. Silently dropping either half would leave a fleet that tracked work
    // yesterday and quietly stopped today.
    #[test]
    fn the_old_single_token_layout_becomes_named_connections() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        fs::create_dir_all(&*dir).unwrap();
        save_config(&Config {
            sync_gateway_url: "https://mcp.shared.example".into(),
            ..Default::default()
        })
        .unwrap();
        fs::write(dir.join("plane-token"), "plane_api_secret\n").unwrap();
        let repo = |id: &str, gw: &str| Repo {
            id: id.into(),
            source: format!("/src/{id}"),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: gw.into(),
        };
        save_repos(&[
            repo("web", ""),
            repo("bridge", "https://mcp.other.example/"),
            repo("also", "https://mcp.other.example"),
        ])
        .unwrap();

        let conns = load_connections();
        assert_eq!(
            conns.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["shared-example", "other-example"],
            "one connection per distinct gateway, named after its host"
        );
        let by_repo = |id: &str| {
            load_repos()
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
                .sync_connection
        };
        assert_eq!(by_repo("web"), "shared-example", "inherited the default");
        assert_eq!(by_repo("bridge"), "other-example");
        assert_eq!(
            by_repo("also"),
            "other-example",
            "the same URL twice is one connection, not two"
        );
        // Behaviour-preserving, including the part that was wrong: a repo on its own gateway was
        // being wired up with the host-wide PAT, so its connection starts with that same token.
        for c in &conns {
            assert_eq!(connection_token(&c.id).as_deref(), Some("plane_api_secret"));
        }
        // The legacy state is gone, so this runs exactly once.
        assert!(!dir.join("plane-token").exists());
        assert_eq!(load_config().sync_gateway_url, "");
        assert_eq!(load_repos()[0].sync_gateway_url, "");
        let again = load_connections();
        assert_eq!(
            again.len(),
            2,
            "second call reads the file, migrates nothing"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    #[test]
    fn a_fresh_host_is_left_alone_by_the_migration() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        assert!(load_connections().is_empty());
        assert!(
            !dir.join("connections.json").exists(),
            "nothing to migrate ⇒ no file invented"
        );
        env::remove_var("SKEIN_HOME");
    }

    // Removing a connection is a bigger edit than it looks: every repo pointing at it silently
    // stops tracking work. So it is refused, by name, rather than performed.
    #[test]
    fn a_connection_in_use_is_not_removed_out_from_under_its_repos() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://shared.example",
            Some("pat"),
        )
        .unwrap();
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: "shared".into(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        let e = remove_connection("shared").unwrap_err();
        assert!(e.contains("web"), "say which repo would lose tracking: {e}");
        assert!(remove_connection("ghost").is_err());
        set_repo_settings("web", None, None, Some("")).unwrap();
        remove_connection("shared").unwrap();
        assert!(load_connections().is_empty());
        assert!(
            connection_token("shared").is_none(),
            "the credential goes with the connection — a token nothing points at is one nobody rotates"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // An id becomes a filename under `tokens/`, so it is checked like one.
    #[test]
    fn a_connection_id_can_never_be_a_path() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        for bad in ["../evil", "a/b", ".ssh", "-lead", "UPPER"] {
            assert!(
                upsert_connection(Some(bad), "x", "https://x.example", Some("pat")).is_err(),
                "accepted {bad:?}"
            );
            assert!(set_connection_token(bad, "pat").is_err(), "wrote {bad:?}");
        }
        assert!(set_connection_token("", "pat").is_err());
        assert!(upsert_connection(None, "x", "not-a-url", None).is_err());
        // A derived id is always safe, however hostile the URL.
        let c = upsert_connection(None, "", "https://plane.example.com/mcp/", None).unwrap();
        assert_eq!(c.id, "plane-example-com", "mcp. stripped, dots to dashes");
        assert_eq!(c.label, "plane-example-com", "blank label ⇒ the host");
        assert_eq!(c.gateway_url, "https://plane.example.com/mcp");
        let d = upsert_connection(None, "", "https://plane.example.com", None).unwrap();
        assert_eq!(
            d.id, "plane-example-com-2",
            "a taken id is suffixed, never reused"
        );
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn a_repos_own_check_command_beats_the_global_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        save_config(&Config {
            check_command: "make test".into(),
            ..Default::default()
        })
        .unwrap();
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        assert_eq!(verify_command("web-main").as_deref(), Some("make test"));
        set_repo_settings("web", Some("npm test"), None, None).unwrap();
        assert_eq!(verify_command("web-main").as_deref(), Some("npm test"));
        // clearing it falls back, and clearing BOTH means verification is simply unavailable —
        // which the UI must show as "unconfigured", never as a failure.
        set_repo_settings("web", Some(""), None, None).unwrap();
        assert_eq!(verify_command("web-main").as_deref(), Some("make test"));
        save_config(&Config::default()).unwrap();
        assert_eq!(verify_command("web-main"), None);
        assert!(run_verify("web-main").is_err(), "no command ⇒ no run");
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // A project id is what an agent token binds to, and the only place a human ever sees one is
    // the Plane URL they are already looking at — so pasting that URL has to work.
    #[test]
    fn a_plane_project_is_read_out_of_whatever_was_pasted() {
        let id = "1e2a3b4c-5d6e-4f70-8912-abcdefabcdef";
        assert_eq!(plane_project_id(id).as_deref(), Some(id));
        assert_eq!(
            plane_project_id(&format!(
                "https://plane.example.net/acme/projects/{id}/issues"
            ))
            .as_deref(),
            Some(id)
        );
        assert_eq!(plane_project_id(&id.to_uppercase()).as_deref(), Some(id));
        assert_eq!(plane_project_id("  \n").as_deref(), None);
        assert_eq!(plane_project_id("my-project").as_deref(), None);
        // The dangerous near-miss: a longer hex run whose first 36 chars are uuid-shaped. Accepting
        // it would store a project that authenticates and then 403s inside a session hours later.
        assert_eq!(plane_project_id(&format!("{id}0")).as_deref(), None);
        assert_eq!(plane_project_id(&format!("0{id}")).as_deref(), None);
    }

    #[test]
    fn a_repo_refuses_a_project_no_uuid_can_be_read_from() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        assert!(set_repo_settings("web", None, Some("the backlog one"), None).is_err());
        assert_eq!(
            load_repos()[0].plane_project,
            "",
            "a refusal stores nothing"
        );
        // The URL is kept verbatim — the uuid is derived, so a board link stays possible.
        let url =
            "https://plane.example.net/acme/projects/1e2a3b4c-5d6e-4f70-8912-abcdefabcdef/issues";
        set_repo_settings("web", None, Some(url), None).unwrap();
        assert_eq!(load_repos()[0].plane_project, url);
        set_repo_settings("web", None, Some(""), None).unwrap();
        assert_eq!(load_repos()[0].plane_project, "", "empty clears it");
        env::remove_var("SKEIN_HOME");
    }

    // The Plane token is the one credential whose leak would let someone bypass every lease in the
    // fleet, so where it lives and who can read it is a claim worth a test rather than a comment.
    #[test]
    fn a_connections_token_is_private_to_this_host_and_never_in_a_config_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        assert!(!sync_status().ready, "nothing configured ⇒ not ready");

        upsert_connection(
            Some("shared"),
            "shared",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        assert!(!sync_status().ready, "a gateway alone cannot mint anything");
        set_connection_token("shared", "  plane_api_secret  ").unwrap();
        assert_eq!(
            connection_token("shared").as_deref(),
            Some("plane_api_secret"),
            "trimmed"
        );

        // Not in connections.json — the object the settings screen GETs.
        let listed = fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(
            !listed.contains("plane_api_secret"),
            "the token must never be written where the settings form can read it: {listed}"
        );
        // ...and not in what the cockpit is told either.
        let status = sync_status();
        assert!(status.ready && status.connections[0].token_set);
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            !json.contains("plane_api_secret"),
            "leaked to the browser: {json}"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("tokens").join("shared"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the token file must be owner-only");
        }

        // A blank token on a save means "unchanged" — opening Settings to fix a URL must not
        // silently delete the credential that makes the connection work.
        upsert_connection(
            Some("shared"),
            "renamed",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        assert!(
            connection_token("shared").is_some(),
            "a save is not a forget"
        );
        assert_eq!(sync_status().connections[0].label, "renamed");

        set_connection_token("shared", "").unwrap();
        assert!(connection_token("shared").is_none(), "empty forgets it");
        assert!(
            set_connection_token("shared", "").is_ok(),
            "forgetting twice is not an error"
        );
        assert!(!sync_status().ready);
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    #[test]
    fn the_gateway_endpoint_is_the_same_whichever_url_was_pasted() {
        let want = "https://plane.example.com/mcp";
        for pasted in [
            "https://plane.example.com",
            "https://plane.example.com/",
            "https://plane.example.com/mcp",
            "  https://plane.example.com/mcp  ",
        ] {
            assert_eq!(sync_mcp_url(pasted), want, "for {pasted:?}");
        }
    }

    // A refusal from the gateway is JSON written for a human. Showing "unexpected response" instead
    // sends the reader to the wrong place entirely — usually to the network, when the real problem
    // is that they pasted an agent token where a Plane one belongs.
    #[test]
    fn a_gateway_refusal_is_reported_in_the_gateways_own_words() {
        let body = r#"{"error":"UNAUTHENTICATED","message":"Plane rejected that personal token","recovery":"Create a new one under your profile"}"#;
        let said = gateway_said(body);
        assert!(
            said.contains("Plane rejected that personal token"),
            "{said}"
        );
        assert!(said.contains("Create a new one"), "{said}");
        // Not JSON at all — usually an HTML error page from something that is not the gateway.
        assert!(gateway_said("<html><body>404</body></html>").contains("html"));
        assert_eq!(gateway_said("   "), "the gateway returned nothing");
    }

    /// The same hash the scripts compute, so a fixture manifest says what a real install would have.
    /// Shelling out to `sha256sum` on purpose: a Rust implementation could agree with itself while
    /// disagreeing with the shell, which is the only thing that matters here.
    fn sha256_of(bytes: &[u8]) -> String {
        use std::io::Write;
        let mut child = Command::new("sha256sum")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    }

    /// Run sync-refresh.sh against a store + project laid out like a wired box, and return its
    /// report (stdout is machine-readable, stderr is prose).
    fn refresh_run(
        store: &Path,
        project: &Path,
        boxhome: &Path,
        args: &[&str],
    ) -> (String, String) {
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-refresh.sh"))
            .args(args)
            .env("HOME", boxhome)
            .env("WORKSPACE_DIR", project)
            .output()
            .unwrap();
        assert!(out.status.success(), "refresh must never fail a box");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The whole promise of the re-apply action in one test: it delivers a correction to what skein
    /// installed, and it does not touch what the box wrote.
    ///
    /// Worth doing end to end rather than unit-testing the classifier, because the failure that
    /// matters — silently overwriting a box's own rules — lives in the file handling, not the
    /// comparison. A box that finds its edits reverted has no reason to trust anything else here.
    #[test]
    fn a_refresh_replaces_what_skein_installed_and_keeps_what_the_box_wrote() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        let boxhome = home.join("boxhome");
        let state = boxhome.join(".local/state/skein");
        fs::create_dir_all(&state).unwrap();

        let slug = project.display().to_string().replace('/', "-");
        fs::write(state.join(format!("sync-{slug}.done")), "").unwrap();
        let src = store.join("skein").join("sync");

        // The skill: installed by skein, then upstream moved. The manifest carries what was written.
        fs::create_dir_all(store.join("skills/work-tracking")).unwrap();
        fs::write(store.join("skills/work-tracking/SKILL.md"), "OLD SKILL\n").unwrap();
        // The memory: the box rewrote it. No manifest entry can match, and it must survive.
        fs::create_dir_all(store.join("memory")).unwrap();
        fs::write(
            store.join("memory/work-tracking.md"),
            "the box's own words\n",
        )
        .unwrap();

        // The block: installed verbatim from the store, so it is skein's to correct. `block_of` in
        // the script reads the section without its trailing blank line, which is what is recorded.
        let block_now = fs::read_to_string(src.join("work-tracking.block.md")).unwrap();
        fs::write(
            project.join("CLAUDE.md"),
            format!("# proj\n\n---\n\n{block_now}\n## Later section\n\nkept\n"),
        )
        .unwrap();
        fs::write(
            state.join(format!("sync-{slug}.manifest")),
            format!(
                "skill\t{}\nmemory\t{}\nblock\t{}\n",
                sha256_of(b"OLD SKILL\n"),
                // A hash nothing can match: the box's memory is not what skein wrote.
                "0".repeat(64),
                sha256_of(block_now.trim_end().as_bytes()),
            ),
        )
        .unwrap();

        // Now move the reference on, exactly as a `git submodule update` + copy would.
        fs::write(src.join("work-tracking.skill.md"), "NEW SKILL\n").unwrap();
        fs::write(
            src.join("work-tracking.block.md"),
            "## Work tracking\n\nuse `decompose`, not capture per child\n",
        )
        .unwrap();

        let (report, _) = refresh_run(&store, &project, &boxhome, &[]);

        assert!(
            report.contains("skill\tstale"),
            "the skill skein installed, now superseded, must be offered: {report}"
        );
        assert!(
            report.contains("memory\tyours"),
            "a memory the box rewrote must be recognised as the box's: {report}"
        );
        assert_eq!(
            fs::read_to_string(store.join("skills/work-tracking/SKILL.md")).unwrap(),
            "NEW SKILL\n",
            "the correction was not delivered"
        );
        assert_eq!(
            fs::read_to_string(store.join("memory/work-tracking.md")).unwrap(),
            "the box's own words\n",
            "skein overwrote an edit it could see — the one thing a refresh must never do"
        );
        let claude = fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(
            claude.contains("use `decompose`, not capture per child"),
            "the block was not corrected: {claude}"
        );
        assert!(
            claude.contains("# proj")
                && claude.contains("## Later section")
                && claude.contains("kept"),
            "rewriting the section ate the rest of the file: {claude}"
        );
        env::remove_var("SKEIN_HOME");
    }

    /// A box wired up before the manifest existed. Neither state is knowable, so the refusal has to
    /// be explicit rather than silently sorted into "stale" (overwrites edits) or "yours" (delivers
    /// nothing, forever).
    #[test]
    fn without_a_record_of_what_was_installed_a_refresh_asks_rather_than_guesses() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        let boxhome = home.join("boxhome");
        let state = boxhome.join(".local/state/skein");
        fs::create_dir_all(&state).unwrap();
        let slug = project.display().to_string().replace('/', "-");
        fs::write(state.join(format!("sync-{slug}.done")), "").unwrap();
        fs::create_dir_all(store.join("skills/work-tracking")).unwrap();
        fs::write(
            store.join("skills/work-tracking/SKILL.md"),
            "PRE-MANIFEST\n",
        )
        .unwrap();

        let (report, _) = refresh_run(&store, &project, &boxhome, &[]);
        assert!(report.contains("skill\tunknown"), "{report}");
        assert_eq!(
            fs::read_to_string(store.join("skills/work-tracking/SKILL.md")).unwrap(),
            "PRE-MANIFEST\n",
            "an unknown document was rewritten without being asked"
        );
        let said = describe_refresh(&report, false);
        assert!(
            said.contains("Replace"),
            "the report has to name the way out, or an unknown document is a dead end: {said}"
        );

        // The human pressing Replace is the evidence that was missing.
        let (forced, _) = refresh_run(&store, &project, &boxhome, &["--force"]);
        assert!(forced.contains("skill\tunknown"), "{forced}");
        assert_eq!(
            fs::read_to_string(store.join("skills/work-tracking/SKILL.md")).unwrap(),
            fs::read_to_string(store.join("skein/sync/work-tracking.skill.md")).unwrap(),
            "Replace did not take it"
        );
        env::remove_var("SKEIN_HOME");
    }

    /// The signal is computed in Rust and read in the page by name, and nothing else connects them:
    /// rename one side and the button silently never appears, which looks exactly like "nothing to
    /// update" — the failure this whole feature exists to end. The browser smoke test cannot reach
    /// this path (its fixture box belongs to no repo, so the flag is always false), so the join is
    /// asserted here instead of left to a reader.
    #[test]
    fn the_page_reads_the_update_flag_by_the_name_the_fleet_sends() {
        let view = BoxView {
            docs_update: true,
            ..BoxView::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            json.contains("\"docs_update\":true"),
            "the fleet snapshot stopped carrying the flag: {json}"
        );
        assert!(
            include_str!("web/index.html").contains("b.docs_update"),
            "the cockpit no longer reads docs_update, so the update button can never appear"
        );
    }

    /// The button only appears when there is something to deliver, so the signal behind it has to be
    /// quiet by default — an indicator that is always lit is one nobody reads.
    #[test]
    fn the_cockpit_only_offers_an_update_when_the_store_has_a_newer_one() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        assert!(
            !sync_docs_available(&store),
            "nothing installed yet is Track work's job, not an update"
        );
        fs::create_dir_all(store.join("skills/work-tracking")).unwrap();
        fs::copy(
            store.join("skein/sync/work-tracking.skill.md"),
            store.join("skills/work-tracking/SKILL.md"),
        )
        .unwrap();
        assert!(
            !sync_docs_available(&store),
            "an up-to-date box must stay quiet"
        );
        fs::write(store.join("skills/work-tracking/SKILL.md"), "older\n").unwrap();
        assert!(sync_docs_available(&store));
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn wiring_a_box_up_refuses_before_it_spends_anything() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        // Nothing configured: the error has to name which half is missing, because "not configured"
        // sends someone to re-check the field they already filled in.
        let e = sync_provision_box("web-main").unwrap_err();
        assert!(e.contains("connection"), "{e}");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        let e = sync_provision_box("web-main").unwrap_err();
        assert!(e.contains("Plane token"), "{e}");
        // Configured, but the box is not running — refuse before minting a credential for a box
        // that cannot receive it.
        set_connection_token("shared", "plane_api_x").unwrap();
        let e = sync_provision_box("web-main").unwrap_err();
        assert!(e.contains("not running"), "{e}");
        assert!(
            sync_mint_token(
                "web-main",
                None,
                &SyncConnection {
                    id: "shared".into(),
                    label: "shared".into(),
                    gateway_url: "http://127.0.0.1:9".into(),
                }
            )
            .is_err(),
            "minting must not be attempted against an unreachable gateway in a test"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // A destroyed box takes its disk with it, not its credential — the token is a bearer token and
    // nothing about it is bound to the box. Teardown therefore revokes it, and must not depend on
    // that succeeding: `sbx rm` has already run by then.
    #[test]
    fn retiring_a_box_retires_its_token_but_never_blocks_on_it() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        // Not configured at all ⇒ a silent no-op, so a destroy stays quiet for anyone not tracking.
        assert!(sync_revoke_token("web-main").is_ok(), "nothing to revoke");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        assert!(
            sync_revoke_token("web-main").is_ok(),
            "a gateway with no stored PAT still has nothing to revoke"
        );
        // Configured, but pointed at nothing that answers: an error the caller LOGS rather than
        // one that aborts the teardown. The distinction is the whole point of the test.
        upsert_connection(
            Some("shared"),
            "shared",
            "http://127.0.0.1:9",
            Some("plane_api_x"),
        )
        .unwrap();
        assert!(
            sync_revoke_token("web-main").is_err(),
            "an unreachable gateway must be reported, not silently treated as revoked"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // The installer is shell that runs inside a box, so reading it proves nothing. Run it against a
    // real store, a real project and a fake `claude`, and check what it actually did.
    #[test]
    fn the_store_installer_registers_the_box_then_writes_the_rules() {
        use std::os::unix::fs::PermissionsExt;
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();

        // An ESTABLISHED box: a CLAUDE.md the team has evolved, memories they curated with their
        // own index, a skills dir, and a Codex config with hand-written entries. Wiring up work
        // tracking must add to all of it and replace none of it.
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n\nsome direction\n").unwrap();
        fs::write(
            store.join("memory").join("MEMORY.md"),
            "- [Our own note](ours.md) — hard-won\n",
        )
        .unwrap();
        fs::write(store.join("memory").join("ours.md"), "the note itself\n").unwrap();
        fs::create_dir_all(store.join("skills").join("ours")).unwrap();
        fs::write(store.join("skills").join("ours").join("SKILL.md"), "ours\n").unwrap();

        // A `claude` that records how it was called. The registration is an argv claim — the URL,
        // the bearer, the scope — and argv is the only place that claim is observable.
        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        let log = home.join("claude.log");
        fs::write(
            bin.join("claude"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();

        let boxhome = home.join("boxhome");
        fs::create_dir_all(boxhome.join(".codex")).unwrap();
        fs::write(
            boxhome.join(".codex").join("config.toml"),
            "[mcp_servers.something_else]\nurl = \"https://theirs.test\"\n",
        )
        .unwrap();
        let run = || {
            Command::new("bash")
                .arg(store.join("skein").join("bin").join("sync-install.sh"))
                .env("HOME", &boxhome)
                .env("WORKSPACE_DIR", &project)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("SYNC_GATEWAY_URL", "https://gw.test/")
                .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
                .output()
                .unwrap()
        };
        assert!(run().status.success());

        let calls = fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("mcp add --transport http sync https://gw.test/mcp"),
            "the endpoint is <base>/mcp, exactly once: {calls}"
        );
        assert!(calls.contains("Bearer sync_agent_abc"), "{calls}");
        assert!(calls.contains("--scope user"), "{calls}");

        // The rules land only after registration, and they land in the STORE — this repo's
        // `.claude` — so every box of the repo sees them, not just the one that was wired up.
        let claude_md = fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("## Work tracking"), "{claude_md}");
        assert!(
            claude_md.contains("some direction"),
            "it appends, never replaces"
        );
        assert!(store.join("memory/work-tracking.md").is_file());
        assert!(store.join("skills/work-tracking/SKILL.md").is_file());
        let index = fs::read_to_string(store.join("memory/MEMORY.md")).unwrap();
        assert!(index.contains("(work-tracking.md)"));

        // Nothing the box already had is touched. This is the whole contract for an existing box:
        // every write is an append or a create, never a replace.
        assert!(
            index.contains("[Our own note](ours.md)"),
            "the index was rewritten: {index}"
        );
        assert_eq!(
            fs::read_to_string(store.join("memory/ours.md")).unwrap(),
            "the note itself\n"
        );
        assert_eq!(
            fs::read_to_string(store.join("skills/ours/SKILL.md")).unwrap(),
            "ours\n"
        );
        let codex = fs::read_to_string(boxhome.join(".codex/config.toml")).unwrap();
        assert!(
            codex.contains("[mcp_servers.something_else]") && codex.contains("https://theirs.test"),
            "a hand-written Codex entry was lost: {codex}"
        );
        assert_eq!(
            codex.matches("[mcp_servers.sync]").count(),
            1,
            "the Codex block was written more than once: {codex}"
        );

        // Once, then hands off: the box may delete what it does not want, and a later start must
        // not restore it. Re-running is also how a box start behaves, so this is the common path.
        fs::remove_file(store.join("skills/work-tracking/SKILL.md")).unwrap();
        assert!(run().status.success());
        assert_eq!(
            fs::read_to_string(project.join("CLAUDE.md"))
                .unwrap()
                .matches("## Work tracking")
                .count(),
            1,
            "a second run appended the section again"
        );
        assert!(
            !store.join("skills/work-tracking/SKILL.md").exists(),
            "a deleted skill came back — the box cannot make its own edits stick"
        );
        env::remove_var("SKEIN_HOME");
    }

    // The ordering claim, which until this test was only a comment: rules are written only AFTER a
    // runtime actually registered. An instruction to "call capture" in a box whose registration
    // failed is a rule the agent cannot follow and will learn to read past — and it would sit in
    // CLAUDE.md looking exactly like a working one.
    #[test]
    fn a_failed_registration_installs_no_rules_for_tools_that_are_not_there() {
        use std::os::unix::fs::PermissionsExt;
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();

        // A `claude` that refuses — a bad URL, an unreachable gateway, a rejected token all land
        // here. No codex either, so nothing registers.
        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("claude"), "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();

        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success(), "still must not gate startup");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(
            said.contains("no runtime registered"),
            "it has to say so: {said}"
        );
        assert!(
            !fs::read_to_string(project.join("CLAUDE.md"))
                .unwrap()
                .contains("Work tracking"),
            "rules were written for tools the box does not have"
        );
        assert!(!store.join("memory/work-tracking.md").exists());
        assert!(!store.join("skills/work-tracking/SKILL.md").exists());
        // And nothing was stamped, so fixing the cause and starting again still works.
        assert!(!boxhome.join(".local/state/skein").exists());
        env::remove_var("SKEIN_HOME");
    }

    // A box with no credentials is not a broken box: startup runs this on every box, so it has to
    // be silent and change nothing until there is something to register.
    #[test]
    fn the_store_installer_does_nothing_at_all_without_credentials() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();

        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env_remove("SYNC_GATEWAY_URL")
            .env_remove("SYNC_AGENT_TOKEN")
            .output()
            .unwrap();
        assert!(out.status.success(), "it must never gate a box's startup");
        assert!(
            out.stderr.is_empty(),
            "a box without a tracker should start silently: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!fs::read_to_string(project.join("CLAUDE.md"))
            .unwrap()
            .contains("Work tracking"));
        assert!(!store.join("memory/work-tracking.md").exists());
        env::remove_var("SKEIN_HOME");
    }

    // The skill is upstream's, vendored here because `include_str!` runs at build time and sync is a
    // private repo — a build that needed it would fail for anyone without access, and skein does not
    // get to stop compiling over a work-tracking document.
    //
    // So the copy is what ships and the submodule is what it is checked against. Drift is the whole
    // risk of vendoring: a copy that silently falls behind teaches an agent a contract the gateway
    // no longer honours, and nothing announces it. `git submodule update --remote` then this test is
    // the upgrade.
    #[test]
    fn the_shipped_skill_is_upstreams_verbatim() {
        let upstream = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("upstream/sync/skills/work-tracking/SKILL.md");
        let Ok(theirs) = fs::read_to_string(&upstream) else {
            // A checkout without `--recursive`. Not a failure — the vendored copy is complete on its
            // own — but say so, because a guard that quietly checks nothing is worse than none.
            eprintln!(
                "skipping drift check: {} is absent — run `git submodule update --init`",
                upstream.display()
            );
            return;
        };
        assert_eq!(
            SYNC_SKILL_MD,
            theirs,
            "the vendored skill has drifted from upstream/sync. Do not edit the copy: change it in \
             the sync repo, then `cp upstream/sync/skills/work-tracking/SKILL.md \
             src/store/sync/work-tracking.skill.md` and update the commit in src/store/sync/UPSTREAM.md"
        );
    }

    // The block is derived by hand, not copied, so the verbatim check above cannot speak for it —
    // and it is the file that went stale first: upstream moved decomposition from `capture` per
    // child to `decompose`, and every box carried the superseded rule until someone read the diff.
    //
    // A rule naming a tool the block never mentions is the shape of that failure, so that is what is
    // asserted. The tool list comes from upstream's own toolspec rather than a copy here, which is
    // what keeps this from becoming another thing to remember to update.
    #[test]
    fn the_always_on_block_names_every_tool_upstreams_own_rules_do() {
        let up = Path::new(env!("CARGO_MANIFEST_DIR")).join("upstream/sync");
        let (Ok(agents), Ok(spec)) = (
            fs::read_to_string(up.join("AGENTS.md")),
            fs::read_to_string(up.join("server/src/toolspec.ts")),
        ) else {
            eprintln!(
                "skipping block check: upstream/sync absent — run `git submodule update --init`"
            );
            return;
        };

        let tools: Vec<&str> = spec
            .lines()
            .filter_map(|l| l.trim().strip_prefix("name: '")?.split('\'').next())
            .collect();
        assert!(
            tools.len() > 8,
            "read {} tool names from upstream's toolspec — the parse broke, and a guard that finds \
             no tools cannot fail",
            tools.len()
        );

        // Upstream's own always-on channel: the § Work tracking section of the file its agents read
        // on every request. The sections after it are about building sync itself and are not rules
        // any box of ours has to follow.
        let rules: String = agents
            .split("\n## ")
            .find(|s| s.starts_with("Work tracking"))
            .expect("upstream AGENTS.md has no § Work tracking")
            .to_string();

        for tool in tools {
            let named = |s: &str| s.contains(&format!("`{tool}`"));
            if named(&rules) && !named(SYNC_BLOCK_MD) {
                panic!(
                    "upstream's always-on rules name `{tool}` and src/store/sync/work-tracking.block.md \
                     does not. Every box gets the block, so a rule missing from it is a rule the fleet \
                     never follows — reconcile it against upstream/sync/AGENTS.md § Work tracking and \
                     server/src/mcphttp.ts INSTRUCTIONS."
                );
            }
        }
    }

    // The wiring, not the helper: a correct `sync_revoke_token` that teardown never calls leaves
    // exactly the live credential this exists to retire. Proven against a real socket, so the whole
    // path — destroy → curl → method, URL and bearer — is what is asserted.
    #[test]
    fn destroying_a_box_actually_sends_the_revocation() {
        use std::io::{Read, Write};
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Deadlined, not blocking: a regression here is "teardown stopped calling revoke", and a
        // blocking accept() turns that into a hung suite instead of a red test — which is how a
        // guard stops being read at all.
        listener.set_nonblocking(true).unwrap();
        let seen = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut buf = [0u8; 2048];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 22\r\n\r\n{\"revoked\":\"pro/gone\"}",
                        );
                        return String::from_utf8_lossy(&buf[..n]).into_owned();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return String::new(); // nothing ever asked to revoke
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => return format!("accept failed: {e}"),
                }
            }
        });

        upsert_connection(
            Some("shared"),
            "shared",
            &format!("http://127.0.0.1:{port}"),
            Some("plane_api_secret"),
        )
        .unwrap();
        env::set_var("SKEIN_DESTROY_CMD", "true"); // stand in for `sbx rm`
        env::set_var("SKEIN_REGISTRY", dir.join("sandboxes.json"));
        fs::write(dir.join("sandboxes.json"), "{}").unwrap();

        destroy_box("gone").unwrap();
        let request = seen.join().unwrap();
        assert!(
            !request.is_empty(),
            "teardown never asked the gateway to revoke anything — the box is gone, its token is not"
        );
        assert!(
            request.starts_with("DELETE /v1/agent-tokens/gone "),
            "{request}"
        );
        assert!(
            request.contains("Authorization: Bearer plane_api_secret"),
            "the PAT is what authorises a revocation — the box's own token cannot: {request}"
        );

        env::remove_var("SKEIN_DESTROY_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
    }

    // The transfer succeeding is not the claim; the gateway saying it revoked something is. Every
    // case below arrives as a perfectly successful curl, and reading any of them as "done" would
    // leave a live bearer token behind a box that no longer exists.
    #[test]
    fn only_the_gateway_saying_revoked_counts_as_revoked() {
        assert!(revocation_outcome(r#"{"revoked":"pro/web-main"}"#).is_ok());
        let e =
            revocation_outcome(r#"{"error":"NOT_FOUND","message":"no such agent"}"#).unwrap_err();
        assert!(e.contains("no such agent"), "{e}");
        // A proxy or the wrong host answering 200 with a page.
        assert!(revocation_outcome("<html>not the gateway</html>").is_err());
        // The shape that would slip through a bare "is it JSON?" check.
        assert!(revocation_outcome(r#"{"ok":true}"#).is_err());
        assert!(revocation_outcome("").is_err());
    }

    #[test]
    fn a_pass_goes_stale_the_moment_the_box_works_again() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"bx":{"branch":"b","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::set_var("SKEIN_LS_CMD", "false");
        fs::create_dir_all(dir.join("verify")).unwrap();
        fs::create_dir_all(dir.join("status")).unwrap();
        let checked_at = Utc::now() - chrono::Duration::seconds(300);
        fs::write(
            dir.join("verify/bx.json"),
            serde_json::to_string(&VerifyRecord {
                cmd: "cargo test".into(),
                exit: 0,
                ok: true,
                ts: checked_at.to_rfc3339(),
                secs: 42,
                fingerprint: "abc1234+0".into(),
                tail: "test result: ok".into(),
            })
            .unwrap(),
        )
        .unwrap();
        // nothing has happened in the box since: the pass stands
        let fresh = verify_summary("bx").expect("a record was written");
        assert!(fresh.ok && !fresh.stale);
        assert_eq!(fresh.age, "5m ago");
        assert_eq!(
            fresh.cmd, "cargo test",
            "a tick is meaningless without the command"
        );
        // the agent ended another turn after the check — the result now describes older code
        fs::write(
            dir.join("status/bx.json"),
            format!(
                r#"{{"status":"waiting","ts":"{}"}}"#,
                Utc::now().to_rfc3339()
            ),
        )
        .unwrap();
        assert!(verify_summary("bx").unwrap().stale);
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_LS_CMD");
    }

    #[test]
    fn only_one_check_runs_at_a_time_across_the_whole_fleet() {
        // The guard that keeps a "verify" from ever becoming N test suites fighting the dev's own
        // machine for cores. Held by an RAII flight, so a panicking run can't wedge the slot.
        let first = VerifyFlight::take("bx-one").unwrap();
        let same = VerifyFlight::take("bx-one").unwrap_err();
        assert!(same.contains("already being verified"));
        let other = VerifyFlight::take("bx-two").unwrap_err();
        assert!(
            other.contains("bx-one"),
            "say which box holds the slot: {other}"
        );
        drop(first);
        assert!(
            VerifyFlight::take("bx-two").is_ok(),
            "the slot frees on drop"
        );
    }

    #[test]
    fn a_listing_from_the_box_reads_types_the_way_the_box_sees_them() {
        // `find -printf '%y\t%Y\t%s\t%f\n'`: %y is the entry's own type, %Y the type after following
        // a symlink. A linked directory must read as a directory; a link pointing nowhere (%Y = N)
        // must still appear, because a file you can see is debuggable and one that vanished is not.
        let body = "d\td\t4096\tdocs\nf\tf\t120\tREADME.md\nl\td\t12\tlinked\nl\tN\t9\tbroken\nd\td\t4096\t.git\n";
        let mut entries = parse_guest_listing(body);
        sort_entries(&mut entries);
        let seen: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.dir)).collect();
        assert_eq!(
            seen,
            vec![
                ("docs", true),
                ("linked", true),
                ("broken", false),
                ("README.md", false)
            ],
            "dirs first (a symlinked dir among them), then files; .git omitted"
        );
        assert_eq!(entries[3].size, 120);
        // a line the box couldn't format is skipped rather than becoming a nameless row
        assert!(parse_guest_listing("garbage\n\n").is_empty());
    }

    #[test]
    fn the_box_answering_no_is_different_from_the_box_not_answering() {
        // OK carries its detail; a refusal is an ANSWER and must not fall through to the host clone
        // (that is how you end up reading a different branch's files and never being told).
        assert_eq!(
            split_guest_fs("SKEIN_FS OK 42\nbody").unwrap(),
            ("42".into(), "body".into())
        );
        assert_eq!(
            split_guest_fs("SKEIN_FS OK\nrows").unwrap(),
            ("".into(), "rows".into())
        );
        assert!(split_guest_fs("SKEIN_FS ESCAPE\n")
            .unwrap_err()
            .contains("escapes"));
        assert!(split_guest_fs("SKEIN_FS NOTDIR\n")
            .unwrap_err()
            .contains("not a directory"));
        assert!(split_guest_fs("bash: sbx: command not found").is_err());
        // the preamble refuses traversal before it resolves anything
        let pre = guest_fs_preamble("../../etc");
        assert!(pre.contains("realpath -m") && pre.contains("SKEIN_FS ESCAPE"));
    }

    #[test]
    fn an_empty_root_says_whether_the_checkout_is_the_problem() {
        // The bug this whole path exists for: a host clone holding nothing but `.git` listed as
        // "empty", so the Files tab looked broken while the box had a full tree.
        let bare = annotate_listing(FileListing {
            path: String::new(),
            entries: vec![],
            source: "host".into(),
            note: String::new(),
        });
        assert_eq!(bare.note, "this workspace has no files in it");
        // an empty SUBdirectory is just an empty directory — no alarming note
        let sub = annotate_listing(FileListing {
            path: "docs".into(),
            entries: vec![],
            source: "box".into(),
            note: String::new(),
        });
        assert!(sub.note.is_empty());
        // and a fallback keeps its own explanation, with the emptiness appended
        let fell_back = annotate_listing(FileListing {
            path: String::new(),
            entries: vec![],
            source: "host".into(),
            note: "read from the host clone — this box isn't running".into(),
        });
        assert!(fell_back.note.contains("isn't running") && fell_back.note.contains("no files"));
    }

    #[test]
    fn file_api_lists_reads_and_guards_the_workspace() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().join("ws");
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap(); // must be hidden from listings
        fs::write(dir.join("README.md"), "# hi").unwrap();
        fs::write(dir.join("docs").join("a.txt"), "aaa").unwrap();
        // a symlink pointing OUTSIDE the workspace must not be traversable
        let _ = std::os::unix::fs::symlink("/etc", dir.join("esc"));
        // one pointing INSIDE it is an ordinary directory, and must list as one
        let _ = std::os::unix::fs::symlink(dir.join("docs"), dir.join("linked"));
        let reg = dir.parent().unwrap().join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"bx":{{"branch":"b","dir":"{}","lastSeen":"2026-01-01T00:00:00Z","status":""}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::set_var("SKEIN_LS_CMD", "false"); // no sbx here — registry is the lookup path

        let l = list_box_files("bx", "").unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(!names.contains(&".git"), ".git must be omitted");
        assert_eq!(names[0], "docs", "dirs sort first");
        assert!(names.contains(&"README.md"));
        let (bytes, truncated) = read_box_file("bx", "README.md").unwrap();
        assert!(!truncated);
        assert_eq!(bytes, b"# hi");
        assert_eq!(list_box_files("bx", "docs").unwrap().entries.len(), 1);
        // a symlinked directory reads as a directory (type follows the link), and opens
        assert!(l.entries.iter().any(|e| e.name == "linked" && e.dir));
        assert_eq!(list_box_files("bx", "linked").unwrap().entries.len(), 1);
        // traversal / absolute / symlink-escape / bad-name are all rejected
        assert!(read_box_file("bx", "../sandboxes.json").is_err());
        assert!(read_box_file("bx", "/etc/passwd").is_err());
        assert!(list_box_files("bx", "esc").is_err());
        assert!(read_box_file("../bx", "README.md").is_err());

        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_LS_CMD");
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
                    "thing-other":{{"branch":"o","dir":"/d","lastSeen":"{}","status":"error"}}}}"#,
                secs_ago(7200),
                secs_ago(7200)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_SELF", "thing-self");
        // Force `fleet_boxes()` to None so the board is built from the registry (the "sbx can't be
        // consulted" path). Without this the test would behave differently on a host that has sbx.
        env::set_var("SKEIN_LS_CMD", "false");

        let v = load_views().unwrap();
        let self_v = v.iter().find(|b| b.name == "thing-self").unwrap();
        let other_v = v.iter().find(|b| b.name == "thing-other").unwrap();
        assert_eq!(self_v.state, "live"); // promoted despite a 2h-old lastSeen
        assert_eq!(other_v.state, "stale"); // stale sticky error cannot resurrect a dead peer

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_SELF");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn load_views_drops_registry_only_boxes_when_sbx_is_authoritative() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        // Registry remembers two boxes, but sbx only lists one — the other was destroyed and its
        // registry entry lingered (e.g. a delist that failed on a corrupt registry).
        fs::write(
            &reg,
            format!(
                r#"{{"thing-live":{{"branch":"l","dir":"/d","lastSeen":"{}","status":""}},
                    "thing-ghost":{{"branch":"g","dir":"/d","lastSeen":"{}","status":""}}}}"#,
                secs_ago(60),
                secs_ago(60)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::remove_var("SKEIN_SELF");
        // sbx is consulted and lists only thing-live, so thing-ghost must not show up.
        env::set_var(
            "SKEIN_LS_CMD",
            r#"printf '[{"name":"thing-live","status":"running"}]'"#,
        );

        let v = load_views().unwrap();
        assert!(v.iter().any(|b| b.name == "thing-live"));
        assert!(
            !v.iter().any(|b| b.name == "thing-ghost"),
            "a destroyed box that sbx no longer lists must not linger on the board"
        );

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn load_views_treats_successful_empty_sbx_fleet_as_authoritative() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"dead-box":{{"branch":"old","dir":"/d","lastSeen":"{}","status":"error"}}}}"#,
                secs_ago(86_400)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::remove_var("SKEIN_SELF");
        env::set_var("SKEIN_LS_CMD", "printf '[]'");

        assert!(
            !load_views()
                .unwrap()
                .iter()
                .any(|view| view.name == "dead-box"),
            "a valid empty sbx response must not resurrect a registry-only box"
        );

        env::remove_var("SKEIN_LS_CMD");
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
        // per-box runtime files that delisting must also clean up
        fs::create_dir_all(dir.join("status")).unwrap();
        fs::create_dir_all(dir.join("skein/launch")).unwrap();
        fs::write(dir.join("status/thing-x.json"), "{}").unwrap();
        fs::write(dir.join("skein/launch/thing-x.json"), "{}").unwrap();

        delist_box("thing-x").unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        assert!(after.get("thing-x").is_none());
        assert!(after.get("thing-y").is_some()); // didn't clobber the rest
                                                  // the box's status + launch files are gone, not left stale
        assert!(!dir.join("status/thing-x.json").exists());
        assert!(!dir.join("skein/launch/thing-x.json").exists());
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
    fn destroy_succeeds_even_when_registry_is_unparseable() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        // a registry too broken to even self-heal: delist will fail, but the sandbox is already gone.
        fs::write(&reg, "{ not json at all").unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_DESTROY_CMD", "true"); // teardown "succeeds"
                                                   // destroy must still report success so the cockpit closes the tab over the removed box.
        assert!(destroy_box("thing-x").is_ok());
        env::remove_var("SKEIN_DESTROY_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    // A diff is only meaningful against a base you can name, and the base has to be the REMOTE
    // branch — measuring against a local ref is how the old host-side path produced a confident
    // wrong answer for every clone-mode box.
    #[test]
    fn the_diff_base_ladder_prefers_the_configured_remote_branch() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        save_config(&Config::default()).unwrap();
        assert_eq!(
            diff_base_refs(),
            ["origin/main", "origin/master", "main", "master"],
            "remote refs first; the local ones are a last resort for a repo with no remote"
        );
        save_config(&Config {
            base_branch: "develop".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            diff_base_refs().first().map(String::as_str),
            Some("origin/develop"),
            "a repo whose base branch is `develop` must not be diffed against main"
        );
        save_config(&Config {
            base_branch: "main".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            diff_base_refs(),
            ["origin/main", "origin/master", "main", "master"],
            "configuring the default must not duplicate it in the ladder"
        );
        // The script names every ref it will try, so a base that never resolves is visible in the
        // command rather than being silently swallowed into `HEAD`.
        let script = diff_script(&diff_base_refs());
        assert!(script.contains("origin/main"), "{script}");
        assert!(script.contains("merge-base"), "{script}");
        assert!(
            script.contains("show-toplevel"),
            "the diff runs at the box's repo root, not wherever the shell landed: {script}"
        );
        env::remove_var("SKEIN_HOME");
    }

    // The box answers with the base on the first line and the patch after it. A patch can contain
    // anything — including that marker — so only the first line may ever be read as one.
    #[test]
    fn the_base_is_read_from_the_first_line_and_only_the_first_line() {
        let (base, patch) = split_diff("SKEIN_DIFF_BASE origin/main\ndiff --git a/x b/x\n+ok\n");
        assert_eq!(base, "origin/main");
        assert_eq!(patch, "diff --git a/x b/x\n+ok\n");
        // A patch that quotes the marker must not move the base.
        let (base, patch) = split_diff("SKEIN_DIFF_BASE HEAD\n+SKEIN_DIFF_BASE origin/evil\n");
        assert_eq!(base, "HEAD");
        assert!(patch.contains("origin/evil"), "kept in the patch, not read");
        // No marker at all (an old box, or a shell that died early) is a patch with an unknown
        // base — reported as HEAD rather than guessed at.
        let (base, patch) = split_diff("diff --git a/x b/x\n");
        assert_eq!(base, "HEAD");
        assert_eq!(patch, "diff --git a/x b/x\n");
        assert_eq!(split_diff("").0, "HEAD");
    }

    #[test]
    fn git_range_handles_repo_and_nonrepo() {
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
        // A repo with no remote still yields a usable range — the local branch tail of the ladder.
        let range = git_range(d).expect("a git repo yields a range");
        assert!(!range.is_empty());

        let empty = tempdir(); // not a git repo → None, never explodes
        assert!(git_range(empty.to_str().unwrap()).is_none());
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
        // attach opens the agent inside a persistent `skein-agent` tmux session so the live process
        // survives a disconnect; `claude --continue` is the (re)create command.
        let a = attach_argv("thing-x", "/d");
        assert_eq!(&a[..3], ["exec", "-it", "thing-x"]);
        assert!(a
            .last()
            .unwrap()
            .contains("tmux new-session -d -s skein-agent"));
        assert!(a.last().unwrap().contains("claude --continue"));
        assert!(a.last().unwrap().contains("timeout 120 claude update"));
        assert!(
            a.last().unwrap().find("tmux has-session").unwrap()
                < a.last().unwrap().find("timeout 120 claude update").unwrap(),
            "the updater must run only inside the missing-session branch"
        );
        assert!(a.last().unwrap().contains("tmux is required"));
        assert!(a.last().unwrap().contains("tmux -u attach-session"));
        assert!(!a.last().unwrap().contains("else exec bash"));
        let first = initial_attach_argv_as("thing-x", "claude");
        assert!(first
            .last()
            .unwrap()
            .contains("tmux new-session -d -s skein-agent"));
        assert!(first.last().unwrap().contains(r#""claude ||"#));
        assert!(!first.last().unwrap().contains("--continue"));
        // a start that fails outright holds the session open as a shell instead of vanishing and
        // leaving the next attach to die on tmux's "can't find session".
        for cmd in [a.last().unwrap(), first.last().unwrap()] {
            assert!(cmd.contains("keeping this session as a shell"));
        }
        // the guard is embedded double-quoted in the outer shell, so it must hold no variable for
        // that shell to expand before tmux ever sees it.
        let guard = guarded_agent_command("claude", "claude --continue");
        assert!(guard.starts_with("claude --continue || {"));
        assert!(!guard.contains('$'), "{guard}");
        assert!(first.last().unwrap().contains("waiting for box setup"));
        assert!(!a.last().unwrap().contains("waiting for box setup"));
        // claude resumes its transcript; a non-claude agent starts bare (its binary name).
        // Both real runtimes fall back to a fresh conversation: a replacement/cleared box has no
        // transcript, and without the fallback "No conversation found" killed the session on attach.
        assert_eq!(agent_resume_cmd("claude"), "claude --continue || claude");
        assert!(agent_resume_cmd("codex").contains("resume --last"));
        assert!(agent_resume_cmd("codex").contains("||"));
        assert_eq!(agent_resume_cmd("shell"), "shell");
        let codex = attach_argv_as("thing-x", "/d", "codex");
        assert!(codex.last().unwrap().contains("skein-agent-codex"));
        assert!(codex.last().unwrap().contains("resume --last"));
        assert!(codex.last().unwrap().contains("--no-alt-screen"));
        assert!(codex.last().unwrap().contains("timeout 120 codex update"));
        assert!(codex.last().unwrap().contains("install-codex-hooks.sh"));
        assert!(
            codex
                .last()
                .unwrap()
                .find("install-codex-hooks.sh")
                .unwrap()
                < codex.last().unwrap().find("tmux new-session").unwrap(),
            "Codex hooks must be installed before the resumed process starts"
        );
        assert!(codex.last().unwrap().contains("agent-guide.sh"));
        assert!(
            codex.last().unwrap().find("agent-guide.sh").unwrap()
                < codex.last().unwrap().find("tmux new-session").unwrap(),
            "durable instructions must be installed before Codex starts"
        );
        // shell requires the same durable-session substrate; it never opens a reload-fragile shell.
        let sh = shell_argv("thing-x");
        assert_eq!(&sh[..3], ["exec", "-it", "thing-x"]);
        assert!(sh
            .last()
            .unwrap()
            .contains("tmux new-session -d -s skein-shell"));
        assert!(sh.last().unwrap().contains("tmux is required"));
        assert!(sh.last().unwrap().contains("tmux -u attach-session"));
        assert!(!sh.last().unwrap().contains("exec bash -li"));
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn drop_dest_keeps_names_and_stays_inside_the_batch_dir() {
        let (dir, path) = drop_dest("b1", "notes.pdf").unwrap();
        assert_eq!(dir, "/tmp/skein-drop-b1");
        assert_eq!(path, "/tmp/skein-drop-b1/notes.pdf"); // real filename survives
                                                          // a dropped folder keeps its structure under the batch dir
        let (dir, path) = drop_dest("b1", "corpus/2026/deed.docx").unwrap();
        assert_eq!(dir, "/tmp/skein-drop-b1/corpus/2026");
        assert_eq!(path, "/tmp/skein-drop-b1/corpus/2026/deed.docx");
        // traversal, absolute paths, separators and shell metacharacters can't escape or inject
        for rel in [
            "../../etc/passwd",
            "/etc/passwd",
            "..\\..\\win.ini",
            "a b; rm -rf ~/'x'.mp4",
        ] {
            let (_, p) = drop_dest("b1", rel).unwrap();
            assert!(
                p.starts_with("/tmp/skein-drop-b1/") && !p.contains("..") && !p.contains('\''),
                "{rel} → {p}"
            );
        }
        // an unusable batch id still yields a usable (generated) one
        for batch in ["", "../..", "..", "-"] {
            let (dir, _) = drop_dest(batch, "x.txt").unwrap();
            assert!(
                dir.starts_with("/tmp/skein-drop-") && !dir.contains("..") && dir.len() > 16,
                "batch {batch:?} → {dir}"
            );
        }
        assert!(drop_dest("b1", &"d/".repeat(30)).is_err()); // depth-capped
    }

    #[test]
    fn safe_component_caps_length_but_keeps_extension() {
        let long = format!("{}.mp4", "n".repeat(200));
        let s = safe_component(&long);
        assert_eq!(s.chars().count(), 80);
        assert!(
            s.ends_with(".mp4"),
            "extension tells the agent the type: {s}"
        );
        // a multibyte name must truncate on a char boundary, never panic
        let s = safe_component(&format!("{}.pdf", "é".repeat(120)));
        assert_eq!(s.chars().count(), 80);
        assert!(s.ends_with(".pdf"));
    }

    #[test]
    fn safe_component_keeps_readable_names() {
        // letters of any script survive; only the shell/path-hostile characters collapse to '-'
        assert_eq!(safe_component("née deed.pdf"), "née-deed.pdf");
        assert_eq!(safe_component("契約書.docx"), "契約書.docx");
        assert_eq!(safe_component("a'b\"c;d|e$f*g.txt"), "a-b-c-d-e-f-g.txt");
        assert_eq!(safe_component(".hidden"), "hidden");
    }

    #[test]
    fn box_write_argv_creates_the_dir_and_avoids_a_pty() {
        let argv = box_write_argv(
            "thing-x",
            "/tmp/skein-drop-b1",
            "/tmp/skein-drop-b1/a b.pdf",
        )
        .unwrap();
        assert_eq!(&argv[..5], ["exec", "-i", "thing-x", "sh", "-c"]);
        assert_eq!(
            argv[5],
            "mkdir -p '/tmp/skein-drop-b1' && cat > '/tmp/skein-drop-b1/a b.pdf'"
        );
        assert!(box_write_argv("../escape", "/tmp/x", "/tmp/x/y").is_err());
    }

    #[test]
    fn pct_decode_recovers_unicode_filenames() {
        assert_eq!(pct_decode("n%C3%A9e%20deed.pdf"), "née deed.pdf");
        assert_eq!(pct_decode("plain.txt"), "plain.txt");
        assert_eq!(pct_decode("100%"), "100%"); // dangling escape left verbatim
        assert_eq!(pct_decode("a%zz"), "a%zz");
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
    fn read_journal_prefers_store_over_host_dir() {
        // Simulates the clone-mode bug directly: `dir` (the registered box dir) is the HOST's
        // shared working clone, which never has the box's own `.skein/journal.md` — only
        // box-journal.sh's copy in the store does. read_journal must find it there.
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let work = dir.join("work");
        fs::create_dir_all(&work).unwrap(); // no .skein/journal.md here — the clone-mode case
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

        // Nothing in the store yet either → None, not a panic.
        assert_eq!(read_journal("thing-x"), None);

        // box-journal.sh's copy lands in <store>/journals/<name>.md.
        fs::create_dir_all(dir.join("journals")).unwrap();
        fs::write(
            dir.join("journals").join("thing-x.md"),
            "did: x / next: y / blocked-on: reviewer\n",
        )
        .unwrap();
        assert!(read_journal("thing-x")
            .unwrap()
            .contains("blocked-on: reviewer"));

        // Direct-mode compatibility: with no store copy, falls back to the host dir directly.
        fs::remove_file(dir.join("journals").join("thing-x.md")).unwrap();
        fs::create_dir_all(work.join(".skein")).unwrap();
        fs::write(
            work.join(".skein").join("journal.md"),
            "did: a / next: b / blocked-on: nothing\n",
        )
        .unwrap();
        assert!(read_journal("thing-x")
            .unwrap()
            .contains("blocked-on: nothing"));

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn box_token_usage_sums_new_assistant_entries_and_is_idempotent() {
        // Shells out to the installed script directly (like the mailbox round-trip test) so this
        // proves the real jq pipeline, not just a Rust-side assumption about its behavior.
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let script = store.join("skein").join("bin").join("box-token-usage.sh");

        // The turn-start marker box-status.sh's `working` mode writes — read here to compute
        // duration_secs. Backdated so the test doesn't depend on real wall-clock timing.
        let start_dir = store.join("telemetry").join(".turn-start");
        fs::create_dir_all(&start_dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        fs::write(start_dir.join("boxA"), (now - 5).to_string()).unwrap();

        let transcript = home.join("transcript.jsonl");
        fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"hi"}}"#, "\n",
                r#"{"type":"assistant","message":{"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":200},"content":[{"type":"tool_use","name":"Bash"}]}}"#, "\n",
            ),
        )
        .unwrap();

        // box-token-usage.sh resolves its store via `git -C $CLAUDE_PROJECT_DIR rev-parse
        // --show-toplevel` (falling back to $CLAUDE_PROJECT_DIR itself when it's not a git repo,
        // as here) + `.claude` — so this must point at the store's *parent*, not `home`.
        let project_dir = store.parent().unwrap().to_path_buf();
        let run = || -> std::process::Output {
            use std::io::Write as _;
            let mut child = Command::new("bash")
                .arg(&script)
                .env("SANDBOX_VM_ID", "boxA")
                .env("CLAUDE_PROJECT_DIR", &project_dir)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn box-token-usage.sh");
            write!(
                child.stdin.take().unwrap(),
                r#"{{"transcript_path":"{}"}}"#,
                transcript.display()
            )
            .unwrap();
            child.wait_with_output().expect("run box-token-usage.sh")
        };

        assert!(run().status.success());
        let log = store.join("telemetry").join("boxA.jsonl");
        let entries: Vec<serde_json::Value> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries.len(), 1, "one turn logged");
        assert_eq!(entries[0]["input"], 100);
        assert_eq!(entries[0]["output"], 50);
        assert_eq!(entries[0]["cache_creation"], 200);
        assert_eq!(entries[0]["total"], 350);
        assert_eq!(entries[0]["tools"]["Bash"], 1);
        let duration = entries[0]["duration_secs"].as_i64().unwrap();
        assert!((4..=6).contains(&duration), "duration was {duration}");

        // No new transcript lines: rerunning must not duplicate the entry.
        assert!(run().status.success());
        let lines_after: usize = fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(lines_after, 1, "must not re-log unchanged transcript");

        // A second turn with two assistant entries (a tool-call round trip) sums both.
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        use std::io::Write as _;
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":"more"}}}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"usage":{{"input_tokens":2,"output_tokens":782,"cache_read_input_tokens":447904,"cache_creation_input_tokens":1247}},"content":[{{"type":"tool_use","name":"Bash"}},{{"type":"tool_use","name":"Read"}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"usage":{{"input_tokens":5,"output_tokens":100,"cache_read_input_tokens":448000,"cache_creation_input_tokens":0}},"content":[{{"type":"tool_use","name":"Read"}}]}}}}"#).unwrap();
        drop(f);
        assert!(run().status.success());
        let entries2: Vec<serde_json::Value> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries2.len(), 2);
        assert_eq!(entries2[1]["input"], 7);
        assert_eq!(entries2[1]["output"], 882);
        assert_eq!(entries2[1]["cache_read"], 895904);
        assert_eq!(entries2[1]["tools"]["Bash"], 1);
        assert_eq!(
            entries2[1]["tools"]["Read"], 2,
            "counts across both assistant entries"
        );
    }

    #[test]
    fn resume_box_guards_name_and_launches() {
        let _g = ENV_LOCK.lock().unwrap();
        assert!(resume_box("../escape", "go").is_err()); // name guard
        let dir = tempdir();
        let registry = dir.join("sandboxes.json");
        fs::write(
            &registry,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"","status":"waiting"}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &registry);
        env::set_var(
            "SKEIN_LS_CMD",
            r#"printf '%s\n' '[{"name":"thing-x","agent":"claude","status":"running"}]'"#,
        );
        // A stub stands in for the runtime. Resume now verifies liveness and records a durable log
        // before reporting success, rather than merely proving that a detached shell forked.
        env::set_var("SKEIN_RESUME_CMD", "true {name} {prompt}");
        env::remove_var("SKEIN_REPO");
        let result = resume_box("thing-x", "");
        assert!(result.is_ok(), "{result:?}");
        assert!(dir.join("status/thing-x.resume.log").is_file());
        env::remove_var("SKEIN_RESUME_CMD");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
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
        env::set_var(
            "SKEIN_LS_CMD",
            r#"printf '%s\n' '{"name":"box-route","agent":"claude","status":"running"}' '{"name":"box-decide","agent":"claude","status":"running"}'"#,
        );
        env::remove_var("SKEIN_REPO");

        env::set_var("SKEIN_AI", "on");
        let (resumed, held) = resume_batch(&["box-route".to_string(), "box-decide".to_string()]);
        assert_eq!(resumed, vec!["box-route".to_string()]); // ROUTINE → continued
        assert_eq!(held, vec!["box-decide".to_string()]); // DECISION → held for the human

        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_CLAUDE_BIN");
        env::remove_var("SKEIN_RESUME_CMD");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn a_boxs_changed_files_are_read_from_the_patch_it_reported() {
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
        assert_eq!(
            changed_files("box-b"),
            vec!["src/only_b.rs", "src/shared.rs"]
        );
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn mailbox_turn_boundary_delivery_round_trip() {
        // Proves the P0 fix at the shell level: mail delivered at UserPromptSubmit (inbox) and
        // blocked-and-surfaced at Stop (stop-check), not just once at SessionStart. Two vmids
        // sharing one temp store stand in for two boxes sharing one shared mount.
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let mailbox_sh = store.join("skein").join("bin").join("mailbox.sh");

        let run = |vmid: &str, args: &[&str]| -> std::process::Output {
            Command::new("bash")
                .arg(&mailbox_sh)
                .args(args)
                .env("SANDBOX_VM_ID", vmid)
                .output()
                .expect("run mailbox.sh")
        };

        let sent = run(
            "boxA",
            &[
                "send",
                "--to",
                "broadcast",
                "--kind",
                "note",
                "--body",
                "hello from A",
            ],
        );
        assert!(
            sent.status.success(),
            "send failed: {}",
            String::from_utf8_lossy(&sent.stderr)
        );

        // Box B's UserPromptSubmit-equivalent surfaces it once …
        let inbox1 = run("boxB", &["inbox"]);
        assert!(inbox1.status.success());
        let out1 = String::from_utf8_lossy(&inbox1.stdout);
        assert!(
            out1.contains("hello from A"),
            "expected message in inbox, got: {out1}"
        );
        // … and never again (seenBy dedup).
        let inbox2 = run("boxB", &["inbox"]);
        assert!(inbox2.status.success());
        assert!(String::from_utf8_lossy(&inbox2.stdout).trim().is_empty());
        // The sender never sees its own broadcast.
        let inbox_a = run("boxA", &["inbox"]);
        assert!(String::from_utf8_lossy(&inbox_a.stdout).trim().is_empty());

        // A fresh message + the Stop-boundary check: blocks (exit 2), body on stderr.
        let sent2 = run(
            "boxA",
            &[
                "send",
                "--to",
                "broadcast",
                "--kind",
                "note",
                "--body",
                "stop-check test",
            ],
        );
        assert!(sent2.status.success());
        let stop1 = run("boxC", &["stop-check"]);
        assert_eq!(
            stop1.status.code(),
            Some(2),
            "stop-check must block on unread mail"
        );
        assert!(String::from_utf8_lossy(&stop1.stderr).contains("stop-check test"));
        // Repeat: already seen, silent success — the same message can't block twice.
        let stop2 = run("boxC", &["stop-check"]);
        assert_eq!(stop2.status.code(), Some(0));
        assert!(stop2.stderr.is_empty());
    }

    #[test]
    fn relay_cross_project_mail_delivers_across_stores() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        // Keep store_dir()'s legacy git-toplevel fallback from picking up this checkout's own
        // store and adding a spurious third store to the sweep.
        env::set_var(
            "SKEIN_REGISTRY",
            home.join("no-such-dir").join("sandboxes.json"),
        );

        let store_a = home.join("repos").join("a").join("store").join(".claude");
        let store_b = home.join("repos").join("b").join("store").join(".claude");
        fs::create_dir_all(store_a.join("mailbox")).unwrap();
        fs::create_dir_all(store_b.join("mailbox")).unwrap();
        save_repos(&[
            Repo {
                id: "a".into(),
                source: "a".into(),
                work: "a".into(),
                store: store_a.to_string_lossy().into_owned(),
                agent: "claude".into(),
                check: String::new(),
                plane_project: String::new(),
                sync_connection: String::new(),
                sync_gateway_url: String::new(),
            },
            Repo {
                id: "b".into(),
                source: "b".into(),
                work: "b".into(),
                store: store_b.to_string_lossy().into_owned(),
                agent: "claude".into(),
                check: String::new(),
                plane_project: String::new(),
                sync_connection: String::new(),
                sync_gateway_url: String::new(),
            },
        ])
        .unwrap();

        // A box in project A writes an "all-projects" broadcast (as mailbox.sh would, once a
        // box uses that keyword).
        let msg = Message {
            from: "boxA".into(),
            to: "all-projects".into(),
            kind: "note".into(),
            branch: "master".into(),
            body: "cross-project hello".into(),
            ts: "2026-01-01T00:00:00Z".into(),
            seen_by: vec![],
            relayed_to: vec![],
            origin_project: String::new(),
        };
        fs::write(
            store_a.join("mailbox").join("1.json"),
            serde_json::to_string(&msg).unwrap(),
        )
        .unwrap();

        relay_cross_project_mail().unwrap();

        // A copy landed in B's mailbox, rewritten to broadcast (B's own local match), tagged with
        // provenance, and with a fresh (unrelayed) seenBy so B's boxes still see it as unread.
        let b_files: Vec<_> = fs::read_dir(store_b.join("mailbox"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(b_files.len(), 1, "expected exactly one relayed copy in B");
        let copy: Message =
            serde_json::from_str(&fs::read_to_string(b_files[0].path()).unwrap()).unwrap();
        assert_eq!(copy.to, "broadcast");
        assert_eq!(copy.body, "cross-project hello");
        assert_eq!(copy.origin_project, "a");
        assert!(copy.seen_by.is_empty());

        // Idempotent: a second sweep doesn't duplicate the delivery.
        relay_cross_project_mail().unwrap();
        let b_files2: Vec<_> = fs::read_dir(store_b.join("mailbox"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(
            b_files2.len(),
            1,
            "relay must not duplicate on a repeat sweep"
        );

        // The origin message is marked relayedTo, which is what makes the sweep idempotent.
        let origin: Message = serde_json::from_str(
            &fs::read_to_string(store_a.join("mailbox").join("1.json")).unwrap(),
        )
        .unwrap();
        assert!(!origin.relayed_to.is_empty());

        // project:<id> direct addressing: only the named project gets a copy, also rewritten to
        // that project's own broadcast (never delivered as a literal "project:b" no box matches).
        let msg2 = Message {
            from: "boxA".into(),
            to: "project:b".into(),
            kind: "note".into(),
            branch: "master".into(),
            body: "hi just b".into(),
            ts: "2026-01-01T00:01:00Z".into(),
            seen_by: vec![],
            relayed_to: vec![],
            origin_project: String::new(),
        };
        fs::write(
            store_a.join("mailbox").join("2.json"),
            serde_json::to_string(&msg2).unwrap(),
        )
        .unwrap();
        relay_cross_project_mail().unwrap();
        let b_files3: Vec<_> = fs::read_dir(store_b.join("mailbox"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(
            b_files3.len(),
            2,
            "the project:b message should also land in B"
        );
        let copy2 = b_files3
            .iter()
            .map(|e| {
                serde_json::from_str::<Message>(&fs::read_to_string(e.path()).unwrap()).unwrap()
            })
            .find(|m| m.body == "hi just b")
            .expect("project:b copy present");
        assert_eq!(copy2.to, "broadcast");

        env::remove_var("SKEIN_HOME");
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
        assert!(!html.contains("/vendor/addon-webgl.js"));
        assert!(html.contains("customGlyphs:true"));
        assert!(html.contains(".agent-statusline"));
        assert!(html.contains("white-space:pre;"));
        assert!(html.contains("replace(/ /g,\"&nbsp;\")"));
        assert!(html.contains(".agent-statusline { display:block; }"));
        assert!(!html.contains("cdn.jsdelivr"));
        assert!(html.contains("id=\"drestart\""));
        assert!(!html.contains(">Create PR</button>"));
    }

    // ---------- the level signal (screen classification + fusion) ----------
    // Fixtures are REAL captures from a live Claude Code pane (docs/turn-state.md §4.1), not
    // invented strings — the grammar is only worth what its evidence is.

    fn obs(tail: &[&str]) -> PaneObs {
        PaneObs {
            ts: 1,
            tail: tail.iter().map(|l| l.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The exact bottom-of-pane of a Claude permission prompt (a WebFetch approval).
    const PERMISSION_TAIL: &[&str] = &[
        "● Fetch(https://example.com/robots.txt)",
        "────────────────────────────────────────────────────────────",
        " Fetch",
        "   url: \"https://example.com/robots.txt\", prompt: \"Return the raw content\"",
        "   Claude wants to fetch content from example.com",
        " Do you want to allow Claude to fetch this content?",
        " ❯ 1. Yes",
        "   2. Yes, and don't ask again for example.com",
        "   3. No, and tell Claude what to do differently (esc)",
    ];

    /// The same pane one keystroke later: dialog gone, composer and hint line back.
    const ANSWERED_TAIL: &[&str] = &[
        "  ⎿  Interrupted · What should Claude do instead?",
        "✻ Baked for 1m 26s",
        "                                              ● high · /effort",
        "────────────────────────────────────────────────────────────",
        "❯ ",
        "────────────────────────────────────────────────────────────",
        "  ⏸ manual mode on · ? for shortcuts · ← for agents",
    ];

    #[test]
    fn classify_pane_reads_a_real_permission_prompt() {
        assert_eq!(
            classify_pane("claude", &obs(PERMISSION_TAIL)),
            Screen::Blocked(Blocked::Permission)
        );
        // …and the moment it is answered the same predicate says "not blocked" — the whole point of
        // a level signal: nothing has to fire an event for the chip to clear.
        assert_eq!(
            classify_pane("claude", &obs(ANSWERED_TAIL)),
            Screen::Waiting
        );
    }

    #[test]
    fn classify_pane_separates_the_four_blocking_kinds() {
        let question = &[
            " Would you like to proceed with this plan?",
            " ❯ 1. Yes, and auto-accept edits",
            "   2. No, keep planning",
        ];
        assert_eq!(
            classify_pane("claude", &obs(question)),
            Screen::Blocked(Blocked::Question)
        );
        let trust = &[
            " Do you trust the files in this folder?",
            " ❯ 1. Yes, proceed",
            "   2. No, exit",
        ];
        assert_eq!(
            classify_pane("claude", &obs(trust)),
            Screen::Blocked(Blocked::Trust)
        );
        // Quota reads like an error but the move is yours, so it ranks as "needs you", not "error".
        let quota = &[
            "✗ Claude usage limit reached · resets at 3pm",
            "❯ ",
            "  ? for shortcuts",
        ];
        assert_eq!(
            classify_pane("claude", &obs(quota)),
            Screen::Blocked(Blocked::Auth)
        );
        let api = &[
            "  ⎿  API Error: 500 Internal Server Error",
            "❯ ",
            "  ? for shortcuts",
        ];
        assert!(matches!(
            classify_pane("claude", &obs(api)),
            Screen::Error(_)
        ));
    }

    #[test]
    fn classify_pane_busy_survives_the_composer_being_visible() {
        // A real working pane: the spinner line carries the token counter, and the hint line is
        // still on screen — so "composer present" must not be read as "idle".
        let busy = &[
            "  Searched for 6 patterns, read 2 files",
            "✢ Whirlpooling… (5m 13s · ↓ 16.1k tokens)",
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        ];
        assert_eq!(classify_pane("claude", &obs(busy)), Screen::Busy);
        // The other half of the same claim, from the title: braille spins, ✳ does not.
        let mut spinning = obs(&["  ⏸ manual mode on · ? for shortcuts"]);
        spinning.title = "⠂ Claude Code".into();
        assert_eq!(classify_pane("claude", &spinning), Screen::Busy);
        let mut idle = obs(&["  ⏸ manual mode on · ? for shortcuts"]);
        idle.title = "✳ Claude Code".into();
        assert_eq!(classify_pane("claude", &idle), Screen::Waiting);
    }

    #[test]
    fn classify_pane_will_not_be_fooled_by_the_agent_talking_about_dialogs() {
        // This very repo's docs contain the sentence below. Prose is not a dialog: without an option
        // list, and with the composer on screen, it is just a box that is waiting for you.
        let prose = &[
            "● I asked: \"Do you want to allow Claude to fetch this content?\" and it said yes.",
            "✻ Baked for 12s",
            "❯ ",
            "  ⏸ manual mode on · ? for shortcuts",
        ];
        assert_eq!(classify_pane("claude", &obs(prose)), Screen::Waiting);
    }

    #[test]
    fn classify_pane_sees_a_dead_agent_and_defers_on_other_runtimes() {
        let mut dead = obs(&[]);
        dead.dead = 1;
        assert_eq!(classify_pane("claude", &dead), Screen::Dead);
        // The launch guard dropped to a shell: the TUI is gone, so the box is not "working".
        let shell = &[
            "skein: claude could not start — see the error above; keeping this session as a shell",
            "agent@skein-box:~/work/skein$",
        ];
        assert_eq!(classify_pane("claude", &obs(shell)), Screen::Dead);
        // A runtime nobody has read the screen of must defer to the hook edges rather than guess.
        assert_eq!(
            classify_pane("gemini", &obs(PERMISSION_TAIL)),
            Screen::Unknown
        );
        // …but a dead window needs no grammar, so that still reports across runtimes.
        assert_eq!(classify_pane("gemini", &dead), Screen::Dead);
        assert_eq!(classify_pane("codex", &dead), Screen::Dead);
    }

    #[test]
    fn screen_health_says_which_half_of_turn_state_is_actually_running() {
        let fresh = || PaneObs {
            ts: Utc::now().timestamp(),
            tail: ANSWERED_TAIL.iter().map(|l| l.to_string()).collect(),
            ..Default::default()
        };
        // Reading the screen: no caveat to show.
        assert_eq!(screen_health("claude", Some(&fresh()), true), "");
        // No observer has ever written: the common case until a box is reattached.
        assert_eq!(screen_health("claude", None, true), "none");
        // An observer that stopped — the agent session went away, or it was killed.
        let stopped = PaneObs {
            ts: Utc::now().timestamp() - (PANE_FRESH_SECS + 5),
            ..fresh()
        };
        assert_eq!(screen_health("claude", Some(&stopped), true), "stale");
        // A screen the grammar does not recognise. Distinct from "stale" because the fix is
        // different: this one is a skein bug to report, not a box to reattach.
        let unreadable = PaneObs {
            tail: vec!["something no grammar has ever seen".into()],
            ..fresh()
        };
        assert_eq!(
            screen_health("claude", Some(&unreadable), true),
            "unreadable"
        );
        // A runtime with no grammar at all is hooks-only by design, not by fault.
        assert_eq!(screen_health("gemini", None, true), "unsupported");
        assert!(has_screen_grammar("claude") && has_screen_grammar("codex"));
        assert!(!has_screen_grammar("gemini"));
        // A box that isn't running has no screen to read, so there is nothing to caveat.
        for h in [None, Some(&fresh()), Some(&stopped)] {
            assert_eq!(screen_health("claude", h, false), "");
        }
        // A crashed agent is a real reading, not a failure to read one.
        let dead = PaneObs { dead: 1, ..fresh() };
        assert_eq!(screen_health("claude", Some(&dead), true), "");
    }

    #[test]
    fn each_runtimes_grammar_owns_its_glyphs_because_skein_knows_the_runtime() {
        // The runtime is never inferred from the screen — `load_views` resolves it from sbx metadata,
        // the box's launch spec, or the repo default, and hands it to `classify_pane`. So each table
        // reads only its own selection glyph, and one agent showing the *other's* dialog — pasted into
        // a message, or quoted in a doc, both routine in this repo — is not a live dialog.
        let codex_dialog = &[
            "  Would you like to run the following command?",
            "› 1. Yes, proceed (y)",
            "  2. No, and tell Codex what to do differently (esc)",
            "  Press enter to confirm or esc to cancel",
        ];
        assert_eq!(
            classify_pane("codex", &obs(codex_dialog)),
            Screen::Blocked(Blocked::Permission)
        );
        assert_eq!(classify_pane("claude", &obs(codex_dialog)), Screen::Unknown);
        assert!(is_option_line("› 1. Yes, proceed (y)", &['›', '>']));
        assert!(!is_option_line("› 1. Yes, proceed (y)", &['❯', '>']));
        // And the marker itself is required: an unmarked numbered row is just a numbered list, which
        // agents write all the time — including in the message this test was written from.
        assert!(!is_option_line("  2. No, keep planning", &['❯', '>']));
        assert!(!is_option_line(
            "  2. The observer was capturing scrollback, so a status line stayed current.",
            &['❯', '>']
        ));
    }

    #[test]
    fn claude_busy_holds_still_across_the_status_lines_shifting_tail() {
        // Sampled from a real skein box every 2s through four minutes of continuous work. The old
        // predicate matched the *end* of this line (`tokens)`), so consecutive samples classified
        // Busy / Waiting / Waiting / Busy — the board flapping the user reported. All four are the
        // same state and must classify identically.
        for line in [
            "✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)",
            "✽ Beboppin'… (3m 43s · ↓ 12.9k tokens · thinking)",
            "✻ Beboppin'… (4m 6s · ↓ 13.7k tokens · thought for 10s)",
            "· Beboppin'… (1m 16s · ↓ 500 tokens · thought for 70s)",
            "✢ Whirlpooling… (5m 13s · ↓ 16.1k tokens)",
            "✻ Thinking… (2s)",
            // The animation's whole observed frame set — `· ✢ * ✶ ✻ ✽`, including the plain ASCII
            // `*`, which an earlier draft of this predicate threw out as "markdown, not a spinner".
            "* Beboppin'… (13m 7s · ↓ 40.4k tokens)",
            "✶ Beboppin'… (13m 19s · ↓ 40.6k tokens)",
        ] {
            assert!(
                is_working_status_line(line),
                "should read as working: {line}"
            );
        }
        // …and the whole pane, as the observer really recorded it mid-turn: a configured statusline,
        // no `esc to interrupt` anywhere on screen, and a title whose glyph is `_`, not braille. Every
        // other busy signal is absent, so the status line has to carry this alone.
        let mut real = obs(&[
            "✻ Sautéed for 24m 3s",
            "● Running 4 shell commands…",
            "  ⎿  $ python3 -c \"import json\"",
            "✽ Beboppin'… (3m 43s · ↓ 12.9k tokens · thinking)",
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  CTX █░░░░ 16% 163.4k/1.0M │ 5H ░░░░ 0%→0% 4h50m left │ $8.80 │ Opus 5",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        ]);
        real.title = "_ Claude Code".into();
        real.title_age = 2000;
        assert_eq!(classify_pane("claude", &real), Screen::Busy);
    }

    #[test]
    fn claude_is_waiting_once_the_status_line_becomes_a_completion_marker() {
        // The same pane after the turn ends: the status line is replaced in place by `… for <time>`,
        // which must NOT read as working — it stays on screen for the whole of the next turn.
        let mut idle = obs(&[
            "● Right. First, evidence from a real skein box — my own.",
            "✻ Sautéed for 24m 3s",
            "● Running 4 shell commands…",
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  CTX █░░░░ 16% 163.4k/1.0M │ $8.80 │ Opus 5",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        ]);
        idle.title = "_ Claude Code".into();
        assert_eq!(classify_pane("claude", &idle), Screen::Waiting);
        // A tool announcement carries the ellipsis but no elapsed time, so it is not the status line.
        assert!(!is_working_status_line("● Running 4 shell commands…"));
        assert!(!is_working_status_line(
            "● Searching for 2 patterns, running 5 shell commands…"
        ));
        assert!(!is_working_status_line("✻ Sautéed for 24m 3s"));
        // The agent's own prose about elapsed times is not a status line either.
        assert!(!is_working_status_line(
            "  and the sampler ran… (30s of wall clock) before I stopped it"
        ));
        // Nor is a captured status line the agent is *displaying* — a quoted fixture, or a diff line
        // (both were on this box's screen while this very test was being written).
        assert!(!is_working_status_line(
            "            \"✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)\","
        ));
        assert!(!is_working_status_line(
            "      8171 +            \"✽ Beboppin'… (3m 39s)\""
        ));
    }

    #[test]
    fn a_status_line_scrolled_up_the_screen_is_not_the_current_one() {
        // Same line, two positions. Directly above the composer it is the pane's own status line;
        // twelve lines up it is the agent showing one — a log, a capture, an earlier turn left on a
        // screen that has since gone quiet.
        let composer = [
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        ];
        let mut live: Vec<&str> =
            vec!["● reading a file", "✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)"];
        live.extend(composer);
        assert_eq!(classify_pane("claude", &obs(&live)), Screen::Busy);
        let mut displayed: Vec<&str> = vec!["✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)"];
        displayed.extend([
            "● one", "  two", "  three", "  four", "  five", "  six", "  seven",
        ]);
        displayed.extend(composer);
        assert_eq!(classify_pane("claude", &obs(&displayed)), Screen::Waiting);
    }

    #[test]
    fn claude_composer_survives_a_configured_statusline() {
        // A box whose statusline replaces the hint line, and whose mode footer is off screen: the
        // bare prompt row between the rules is still proof the TUI is alive and taking input, so the
        // box reads `waiting` instead of falling back to a stale edge that says `working`.
        let bare = &[
            "✻ Sautéed for 14m 21s",
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  CTX █░░░ 0% 0/1.0M │ 5H ███ 53%→54% 4m left │ 7D ███ 28%→31%",
        ];
        assert_eq!(classify_pane("claude", &obs(bare)), Screen::Waiting);
    }

    #[test]
    fn claude_busy_outranks_an_error_line_left_over_in_the_tail() {
        // An "API Error" from the previous turn is still in the visible tail while the next turn is
        // running. The turn is demonstrably live, so there is nothing for a human to do — reading
        // history as current state is the same fault as latching an edge.
        let retrying = &[
            "  ⎿  API Error: 500 Internal Server Error",
            "✻ Whirlpooling… (12s · ↓ 1.2k tokens)",
            "  ⏸ manual mode on · ? for shortcuts",
        ];
        assert_eq!(classify_pane("claude", &obs(retrying)), Screen::Busy);
    }

    // ---------- Codex 0.145.0, captured live from a box (docs/turn-state.md §6b) ----------

    /// A shell-command approval, exactly as Codex draws it. Note there is no composer footer: a
    /// dialog *replaces* it.
    const CODEX_APPROVAL: &[&str] = &[
        "› run the shell command: date",
        "• I’ll run date and report its output.",
        "• Running date",
        "  Would you like to run the following command?",
        "  Environment: local",
        "  $ date",
        "› 1. Yes, proceed (y)",
        "  2. Yes, and don't ask again for commands that start with `date` (p)",
        "  3. No, and tell Codex what to do differently (esc)",
        "  Press enter to confirm or esc to cancel",
    ];

    /// The same pane after pressing esc — and a minefield: a `■ ` notice line, a monthly-limit
    /// warning, and the user's own prompt echoed with the same `› ` glyph the options use.
    const CODEX_ANSWERED: &[&str] = &[
        "› run the shell command: date",
        "• I’ll run date and report its output.",
        "✗ You canceled the request to run date",
        "• Ran date",
        "  └ (no output)",
        "⚠ Heads up, you have less than 25% of your monthly limit left. Run /status for a breakdown.",
        "■ Conversation interrupted - tell the model what to do differently. Something went wrong? Hit `/feedback` to report the",
        "issue.",
        "› Improve documentation in @filename",
        "  ? for shortcuts                                          100% context left",
    ];

    #[test]
    fn classify_pane_reads_codexs_approval_and_watches_it_clear() {
        let mut dialog = obs(CODEX_APPROVAL);
        dialog.title = "[ ! ] Action Required | skein".into();
        assert_eq!(
            classify_pane("codex", &dialog),
            Screen::Blocked(Blocked::Permission)
        );
        // One keystroke later: no confirm footer, composer back, title back to the plain directory.
        let mut answered = obs(CODEX_ANSWERED);
        answered.title = "skein".into();
        assert_eq!(classify_pane("codex", &answered), Screen::Waiting);
    }

    #[test]
    fn classify_pane_reads_codexs_edit_approval_too() {
        // A different body under the same footer — which is why the footer, not the wording of any
        // one dialog, is the predicate.
        let mut edits = obs(&[
            "• Added scratch-hello.txt (+1 -0)",
            "    1 +hello",
            "  Would you like to make the following edits?",
            "› 1. Yes, proceed (y)",
            "  2. Yes, and don't ask again for these files (a)",
            "  3. No, and tell Codex what to do differently (esc)",
            "  Press enter to confirm or esc to cancel",
        ]);
        edits.title = "[ . ] Action Required | skein".into();
        assert_eq!(
            classify_pane("codex", &edits),
            Screen::Blocked(Blocked::Permission)
        );
    }

    #[test]
    fn classify_pane_names_the_wall_codex_puts_in_front_of_skeins_own_hooks() {
        // skein installs its probes into every store, so "N hooks are new or changed" is the trust
        // gate a skein box really hits — and it blocks *before* any hook could fire to report it.
        let trust = &[
            "  Hooks need review",
            "  19 hooks are new or changed.",
            "  Hooks can run outside the sandbox after you trust them.",
            "› 1. Review hooks",
            "  2. Trust all and continue",
            "  3. Continue without trusting (hooks won't run)",
            "  Press enter to confirm or esc to go back",
        ];
        assert_eq!(
            classify_pane("codex", &obs(trust)),
            Screen::Blocked(Blocked::Trust)
        );
        // Onboarding with no credentials: the options switch to a plain `> 1.` and the footer to
        // "continue", and nothing will ever run until a human signs in.
        let signin = &[
            "  Welcome to Codex, OpenAI's command-line coding agent",
            "  Sign in with ChatGPT to use Codex as part of your paid plan",
            "  or connect an API key for usage-based billing",
            "> 1. Sign in with ChatGPT",
            "     Usage included with Plus, Pro, Business, and Enterprise plans",
            "  2. Sign in with Device Code",
            "  3. Provide your own API key",
            "  Press enter to continue",
        ];
        assert_eq!(
            classify_pane("codex", &obs(signin)),
            Screen::Blocked(Blocked::Auth)
        );
        // A picker is a question, not a permission: nothing is waiting on a yes.
        let picker = &[
            "  Select a model",
            "› 1. gpt-5.6-sol (current)",
            "  2. gpt-5.6-terra",
            "  Press enter to confirm or esc to go back",
        ];
        assert_eq!(
            classify_pane("codex", &obs(picker)),
            Screen::Blocked(Blocked::Question)
        );
    }

    #[test]
    fn codexs_action_required_title_covers_a_dialog_we_have_no_wording_for() {
        // The next Codex release can reword any dialog body; the title marker is the backstop, and it
        // is honest about not knowing which kind. Verified to appear only while a decision is
        // pending — it clears on approve *and* on esc, and stays clear through a finished turn.
        let mut unknown_dialog = obs(&["  Some future prompt nobody has captured", "  ▸ pick one"]);
        unknown_dialog.title = "[ ! ] Action Required | skein".into();
        assert_eq!(
            classify_pane("codex", &unknown_dialog),
            Screen::Blocked(Blocked::Question)
        );
        // The bracket animates, so only the words can be matched.
        assert!(title_has_attention("[ ! ] Action Required | skein"));
        assert!(title_has_attention("[ . ] Action Required | skein"));
        assert!(!title_has_attention("⠧ skein"));
    }

    #[test]
    fn classify_pane_reads_codexs_working_line_and_its_notices() {
        // Codex keeps the composer on screen while it works, so "composer present" cannot mean idle.
        let mut busy = obs(&[
            "› run the shell command: date",
            "• Working (1s • esc to interrupt)",
            "› Improve documentation in @filename",
            "  ? for shortcuts                                          100% context left",
        ]);
        busy.title = "⠴ skein".into();
        assert_eq!(classify_pane("codex", &busy), Screen::Busy);
        // A failure is a `■ ` line carrying a JSON payload…
        let failed = &[
            "› run the shell command: date",
            "■ {\"detail\":\"The 'gpt-5.6-sol' model is not supported when using Codex with a ChatGPT account.\"}",
            "  ? for shortcuts                                          100% context left",
        ];
        assert!(matches!(
            classify_pane("codex", &obs(failed)),
            Screen::Error(_)
        ));
        // …while the prose `■ ` notice and the monthly-limit warning in CODEX_ANSWERED are neither an
        // error nor an auth block. That pane is simply waiting for you.
        assert_eq!(
            classify_pane("codex", &obs(CODEX_ANSWERED)),
            Screen::Waiting
        );
    }

    #[test]
    fn fuse_status_clears_an_edge_that_nothing_ever_cleared() {
        // The bug, as recorded in this box's own hook-log: `blocked` written at 13:27, nothing until
        // Stop at 13:47. A screen observation taken at 13:30 showing a composer ends it.
        let edge = Some(("blocked".to_string(), 1000));
        let level = Some((Screen::Waiting, 1180));
        assert_eq!(fuse_status(edge, level), (Some("waiting".into()), ""));
    }

    #[test]
    fn fuse_status_lets_a_newer_edge_lead_then_the_next_sample_confirms() {
        // A Notification fires 5s after the last sample: show it at once (latency), don't wait.
        let edge = Some(("blocked".to_string(), 1205));
        let level = Some((Screen::Waiting, 1200));
        assert_eq!(
            fuse_status(edge.clone(), level),
            (Some("blocked".into()), "")
        );
        // The next sample sees the dialog and names which kind it is.
        assert_eq!(
            fuse_status(edge, Some((Screen::Blocked(Blocked::Permission), 1210))),
            (Some("blocked".into()), "permission")
        );
    }

    #[test]
    fn fuse_status_without_an_observation_is_exactly_todays_behaviour() {
        for status in ["blocked", "working", "waiting", "error", "ended"] {
            assert_eq!(
                fuse_status(Some((status.to_string(), 10)), None),
                (Some(status.to_string()), "")
            );
        }
        // An unreadable screen defers too, rather than inventing a state.
        assert_eq!(
            fuse_status(Some(("blocked".into(), 10)), Some((Screen::Unknown, 99))),
            (Some("blocked".into()), "")
        );
        // No edge and no observation: nothing claimed, so liveness decides downstream.
        assert_eq!(fuse_status(None, None), (None, ""));
    }

    #[test]
    fn fuse_status_reports_a_crashed_agent_but_keeps_a_human_set_outcome() {
        assert_eq!(
            fuse_status(Some(("working".into(), 10)), Some((Screen::Dead, 20))),
            (Some("ended".into()), "")
        );
        // `done` is a human's verdict on the work, not a claim about the process.
        assert_eq!(
            fuse_status(Some(("done".into(), 10)), Some((Screen::Dead, 20))),
            (Some("done".into()), "")
        );
    }

    #[test]
    fn title_activity_names_the_tool_but_not_the_idle_title() {
        assert_eq!(
            title_activity("✳ Run bash command true").as_deref(),
            Some("Run bash command true")
        );
        assert_eq!(title_activity("⠂ Claude Code"), None);
        assert_eq!(title_activity("✳ Claude Code"), None);
        assert_eq!(title_activity(""), None);
    }

    #[test]
    fn title_text_is_only_a_task_while_it_is_demonstrably_fresh() {
        // Observed live: Claude Code kept a finished tool's description in the title through a later,
        // unrelated turn. So "busy" alone is not enough to believe the title's text — the observer
        // must have watched it change, and recently.
        let stale = PaneObs {
            ts: 1,
            title: "⠂ Fetch and quote robots.txt file".into(),
            title_age: 600,
            ..Default::default()
        };
        let fresh = PaneObs {
            title_age: 3,
            ..stale.clone()
        };
        let unwitnessed = PaneObs {
            title_age: -1,
            ..stale.clone()
        };
        let task_from_title = |runtime: &str, o: &PaneObs| {
            runtime == "claude"
                && (0..=TITLE_FRESH_SECS).contains(&o.title_age)
                && title_activity(&o.title).is_some()
        };
        let usable = |o: &PaneObs| task_from_title("claude", o);
        assert!(
            usable(&fresh),
            "a title seen changing 3s ago names current work"
        );
        assert!(
            !usable(&stale),
            "ten minutes old is the residue of an earlier tool call"
        );
        assert!(
            !usable(&unwitnessed),
            "never seen changing ⇒ no claim at all"
        );
        // Codex's title is the working directory, not the running tool, so it names no task however
        // fresh it is — otherwise the task column would just repeat the box's folder name.
        let codex = PaneObs {
            title: "⠧ skein".into(),
            title_age: 2,
            ..stale.clone()
        };
        assert!(!task_from_title("codex", &codex));
    }
    #[test]
    fn read_pane_ignores_an_observation_that_has_gone_stale() {
        let _g = ENV_LOCK.lock().unwrap();
        let store = tempdir().join(".claude");
        fs::create_dir_all(store.join("status")).unwrap();
        let reg = store.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        let now = Utc::now().timestamp();
        let write = |ts: i64| {
            fs::write(
                store.join("status/thing-x.pane.json"),
                format!(r#"{{"contract":1,"ts":{ts},"age":0,"moving":1,"dead":0,"title":"⠂ Claude Code","cmd":"bash","tail":["❯ ","  ? for shortcuts"]}}"#),
            )
            .unwrap();
        };
        write(now);
        assert!(read_pane("thing-x").is_some(), "a fresh sample counts");
        write(now - (PANE_FRESH_SECS + 5));
        assert!(
            read_pane("thing-x").is_none(),
            "a sample older than three heartbeats must stop counting, so a dead observer degrades \
             to hook-only turn-state instead of freezing the board"
        );
        env::remove_var("SKEIN_REGISTRY");
    }
}
