//! skein core — read the shared sbx registry and derive fleet views.
//!
//! This is the single source of truth shared by the CLI (`skein`) and the server
//! (`skein-server`). It owns no state the sandboxes don't already write; it only reads
//! `sandboxes.json` and derives status. See ARCHITECTURE.md.

mod ai;
mod answer;
mod config;
mod diff;
mod files;
mod fleet;
mod health;
mod mailbox;
mod place;
mod repos;
mod runtime;
mod sandbox;
mod ship;
mod signals;
#[cfg(test)]
mod testutil;
mod tracking;
mod transcript;
mod util;
mod verify;

pub use ai::*;
pub use answer::*;
pub use config::*;
pub use diff::*;
pub use files::*;
pub use fleet::*;
pub use health::*;
pub use mailbox::*;
pub use place::*;
pub use repos::*;
pub use runtime::*;
pub use sandbox::*;
pub use ship::*;
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
    // A shared box is not in `sbx ls` — no sandbox carries its name — so asking there reports every
    // one of them as gone. Its anchor IS its liveness: the tmux server lives exactly as long as the
    // box, so a live pid is a running box and a dead one is a stopped box with its tree intact.
    if let Some(rec) = shared_record(name) {
        return Some(
            if std::path::Path::new(&format!("/proc/{}", rec.ns_pid)).exists() {
                Liveness::Running
            } else {
                Liveness::Stopped
            },
        );
    }
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

const KIT_SPEC_YAML: &str = include_str!("kit/spec.yaml");

/// The provisioning script, kept as a real file rather than inline in the kit spec.
///
/// It has two callers that must not drift: sbx runs it as this kit's startup hook in a `--clone`
/// sandbox, and [`crate::fleet::provision_script`] runs the same bytes inside a box's namespace in
/// the shared sandbox. Provisioning is a dozen steps — the store link, the settings merge, the
/// branch checkout, the handoff restore, shared-home, the agent guide, the Codex hooks, the skills,
/// the boot report, the sync install — and a second implementation of them for the fleet path would
/// be a second set of ways for a box to come up looking healthy with no hooks wired.
pub(crate) const KIT_STARTUP_SH: &str = include_str!("kit/skein-startup.sh");

/// The marker line in the spec that [`kit_spec`] replaces with the script body.
const KIT_STARTUP_MARKER: &str = "        # @SKEIN_STARTUP_SCRIPT@";

/// The kit spec with the startup script spliced back into its `content:` block.
///
/// A YAML block scalar carries its indentation, so the script is re-indented to the eight spaces the
/// `content: |` level expects — and blank lines stay genuinely blank, because trailing whitespace on
/// an otherwise empty line would change the block's detected indentation.
fn kit_spec() -> String {
    let body: String = KIT_STARTUP_SH
        .lines()
        .map(|l| {
            if l.is_empty() {
                "\n".to_string()
            } else {
                format!("        {l}\n")
            }
        })
        .collect();
    KIT_SPEC_YAML
        .lines()
        .map(|l| {
            if l.starts_with(KIT_STARTUP_MARKER) {
                body.clone()
            } else {
                format!("{l}\n")
            }
        })
        .collect::<String>()
}

/// Install skein's own sbx kit into `~/.skein/kit/spec.yaml` so native launch can `--kit` it without
/// the repo shipping a kit. Embedded via `include_str!`; rewritten each call (idempotent).
pub fn ensure_kit() -> Result<PathBuf, String> {
    let kit = skein_home().join("kit");
    fs::create_dir_all(&kit).map_err(|e| format!("mkdir {}: {e}", kit.display()))?;
    let spec = kit.join("spec.yaml");
    fs::write(&spec, kit_spec()).map_err(|e| format!("write {}: {e}", spec.display()))?;
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
    let argv = place_of(name)
        .ok_or("invalid box name")?
        .raw_argv(&["cat", guest]);
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
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
    use crate::testutil::*;
    use std::process::Stdio;

    // The provisioning script is one file with two callers — the kit hook and the fleet path — and
    // the kit's copy is spliced into a YAML block scalar, where indentation IS the syntax. A line
    // that lands at the wrong depth ends the block early, and the failure is not a parse error: sbx
    // writes a truncated script, the box comes up with no hooks, and it looks perfectly healthy.
    #[test]
    fn the_kit_carries_the_same_provisioning_script_the_fleet_runs() {
        let spec = kit_spec();
        assert!(
            !spec.contains("@SKEIN_STARTUP_SCRIPT@"),
            "the marker survived, so the kit would install a script that is only a comment"
        );
        // Every line of the script, at the block's indentation — including the ones the shell needs
        // at column 0 and the ones already indented inside it.
        for line in KIT_STARTUP_SH.lines().filter(|l| !l.is_empty()) {
            assert!(
                spec.contains(&format!("\n        {line}\n")),
                "not spliced at the block's depth: {line:?}"
            );
        }
        // A blank line carrying eight spaces would deepen the block's detected indentation and take
        // the rest of the script with it.
        assert!(
            !spec.contains("\n        \n"),
            "a blank line was padded, which re-indents everything after it"
        );
        assert!(
            spec.contains("\n  startup:\n"),
            "the splice must not disturb what follows the block"
        );
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

    // The takeover path reaches into the source box for its branch and HEAD, and until now nothing
    // exercised that. The guard is deliberately the *argv*: `Place` is about to change how skein
    // addresses a box, and this is the contract it must not silently alter.
    #[test]
    fn preparing_a_takeover_asks_the_source_box_itself() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_LS_CMD", "false");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let sbx = bin.join("sbx");
        fs::write(
            &sbx,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n\
                 case \"$*\" in\n\
                   *abbrev-ref*) echo feat/auth ;;\n\
                   *rev-parse\\ HEAD*) echo 0123456789abcdef ;;\n\
                 esac\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&sbx, fs::Permissions::from_mode(0o755)).unwrap();
        let old_path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{old_path}", bin.display()));

        // Refusals come before anything is spent, and each names what is actually wrong.
        let e = prepare_replacement("web-main", "claude").unwrap_err();
        assert!(e.contains("already uses"), "{e}");
        let e = prepare_replacement("web-main", "codex").unwrap_err();
        assert!(e.contains("managed repo"), "unregistered box: {e}");
        assert!(!log.exists(), "a refusal must not have touched the box");

        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: home.join("work").to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        fs::create_dir_all(home.join("work")).unwrap();
        let _ = prepare_replacement("web-main", "codex");

        let asked = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            asked.contains("exec web-main bash -lc git rev-parse --abbrev-ref HEAD"),
            "the branch must come from the BOX, not the host clone — it is a different checkout: {asked}"
        );
        assert!(
            asked.contains("exec web-main bash -lc git rev-parse HEAD"),
            "and so must the commit it snapshots: {asked}"
        );

        env::set_var("PATH", old_path);
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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

        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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

    // The switch. One config value decides whether a NEW box gets its own microVM or a namespace in
    // the shared sandbox — and it must decide only that: the attach half is identical either way, so
    // that everything downstream of launch (the tmux session, the runtime setup, the reconnect) has
    // exactly one shape to know about. Boxes already running keep their own model regardless; this
    // is the creation path, not a reinterpretation of an existing box.
    #[test]
    fn the_fleet_flag_changes_how_a_box_is_created_and_nothing_else() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::remove_var("SKEIN_LAUNCH_CMD");
        save_repos(&[Repo {
            id: "web".into(),
            source: "git@github.com:o/web.git".into(),
            work: home.join("repos/web/work").to_string_lossy().into(),
            store: home
                .join("repos/web/store/.claude")
                .to_string_lossy()
                .into(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        let own = launch_command("web-feat-x", "feat/x");
        assert!(
            own.starts_with("sbx create --clone --kit "),
            "the default is still a sandbox per box: {own}"
        );

        save_config(&Config {
            fleet_sandbox: "skein-fleet".into(),
            ..load_config()
        })
        .unwrap();
        let fleet = launch_command("web-feat-x", "feat/x");
        assert!(
            fleet.starts_with("skein start 'web-feat-x' --branch 'feat/x' --agent 'claude'"),
            "a box in the fleet is brought up by skein, not by `sbx create`: {fleet}"
        );
        assert!(
            !fleet.contains("sbx create"),
            "there is no sandbox to create for this box: {fleet}"
        );
        // The half that must NOT change: the same attach, into the same named session.
        let attach_of = |cmd: &str| cmd.split_once("&& ").map(|(_, a)| a.to_string()).unwrap();
        assert_eq!(
            attach_of(&fleet),
            attach_of(&own),
            "the fleet must not grow a second way to start an agent"
        );

        env::remove_var("SKEIN_HOME");
    }

    // A box rebuilt from a snapshot is new to sbx but not new to its user. The resize restores the
    // previous conversation into the fresh checkout — and starting the runtime clean would leave
    // that transcript on disk unread, with the agent opening an empty session against a tree full of
    // context it appears not to remember. The launch spec's handoff dir is the signal.
    #[test]
    fn a_box_rebuilt_from_a_snapshot_resumes_instead_of_starting_over() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::remove_var("SKEIN_LAUNCH_CMD");
        let repo = Repo {
            id: "web".into(),
            source: "git@github.com:o/web.git".into(),
            work: home.join("repos/web/work").to_string_lossy().into(),
            store: home
                .join("repos/web/store/.claude")
                .to_string_lossy()
                .into(),
            agent: "claude".into(),
            check: String::new(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        };
        save_repos(std::slice::from_ref(&repo)).unwrap();

        // A box with no snapshot behind it starts fresh, as it always has.
        write_launch_spec_for_agent("web-feat-x", "feat/x", &repo, "claude").unwrap();
        let fresh = initial_attach_argv_as("web-feat-x", "claude").join(" ");
        assert!(
            !fresh.contains("--continue"),
            "an ordinary new box has nothing to continue: {fresh}"
        );

        // The same box, rebuilt: the spec now carries the snapshot it was restored from.
        let dir = Path::new(&repo.store).join("skein/launch");
        fs::write(
            dir.join("web-feat-x.json"),
            r#"{"branch":"feat/x","agent":"claude",
                "handoff":{"source":"web-feat-x","dir":"skein/handoff-snapshots/web-feat-x/r1"}}"#,
        )
        .unwrap();
        let restored = initial_attach_argv_as("web-feat-x", "claude").join(" ");
        assert!(
            restored.contains("claude --continue"),
            "the restored conversation would have gone unread: {restored}"
        );
        // The fallback is what makes this safe for a cross-runtime takeover, whose target has no
        // native transcript of its own and must not be left with a failed command.
        assert!(
            restored.contains("|| claude"),
            "a box with nothing to continue must still start: {restored}"
        );
        env::remove_var("SKEIN_HOME");
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
        let _g = env_lock();
        env::set_var("HOME", "/home/me");
        assert_eq!(shorten("/home/me/work/x"), "~/work/x");
        assert_eq!(shorten("/other/x"), "/other/x");
    }

    #[test]
    fn a_repo_claims_work_through_the_connection_it_picks() {
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
    fn load_views_promotes_only_the_self_box_when_quiet() {
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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

    // A box in a shared sandbox is attached to through its namespace, and every tmux call names its
    // own server. Both matter for the same reason: session names are identical across boxes, so a
    // bare `tmux has-session -t skein-agent` on the sandbox's default socket would find a NEIGHBOUR's
    // agent and attach the user straight into someone else's turn.
    // The three lifecycle calls that used to name a SANDBOX after the box. For a shared box no such
    // sandbox exists, so `sbx ls` reported it dead, `sbx stop` missed, and `sbx rm -f` would have
    // aimed a destructive command at whatever sandbox happened to share the name.
    #[test]
    fn a_shared_boxs_lifecycle_never_names_a_sandbox_after_the_box() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);

        // Alive: this very process stands in for the box's tmux server.
        record_place(
            "thing-x",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: std::process::id(),
                home: "/home/agent".into(),
                tree: "/boxes/thing-x/tree".into(),
                sock: "/boxes/thing-x/session.sock".into(),
            },
        )
        .unwrap();
        assert_eq!(
            box_liveness("thing-x"),
            Some(Liveness::Running),
            "the anchor IS the liveness — sbx ls knows nothing about a shared box"
        );

        // Dead anchor: stopped, not missing. The tree is still there to restart from.
        record_place(
            "thing-x",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 0,
                home: "/home/agent".into(),
                tree: "/boxes/thing-x/tree".into(),
                sock: "/boxes/thing-x/session.sock".into(),
            },
        )
        .unwrap();
        assert_eq!(box_liveness("thing-x"), Some(Liveness::Stopped));

        forget_place("thing-x");
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn a_shared_box_is_attached_through_its_namespace_and_its_own_tmux_server() {
        let _g = env_lock();
        env::set_var("SKEIN_HOME", tempdir());
        record_place(
            "thing-x",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: std::process::id(), // alive, so the record is followed
                home: "/boxes/thing-x/home".into(),
                tree: "/boxes/thing-x/tree".into(),
                sock: "/boxes/thing-x/session.sock".into(),
            },
        )
        .unwrap();

        for argv in [attach_argv("thing-x", "/d"), shell_argv("thing-x")] {
            assert_eq!(
                &argv[..3],
                ["exec", "-it", "skein-fleet"],
                "the sandbox is the fleet's, not the box's"
            );
            assert!(
                argv.contains(&"--preserve-credentials".to_string()),
                "the attach itself runs in the namespace — its setup writes the box's HOME and tree"
            );
            let shell = argv.last().unwrap();
            assert!(
                shell.contains("export HOME='/boxes/thing-x/home'"),
                "nsenter carries the CALLER's environment in, so HOME must be set explicitly"
            );
            // Every tmux call, not just the attach: has-session, new-session, set-option and the
            // server-global configuration all have to land on this box's server.
            for call in shell.match_indices("tmux ").map(|(i, _)| &shell[i..]) {
                assert!(
                    call.starts_with("tmux -S '/boxes/thing-x/session.sock'")
                        || call.starts_with("tmux is required")
                        || call.starts_with("tmux >/dev/null"),
                    "a tmux call went to the default socket: {call:.60}"
                );
            }
        }

        // The observer runs its own tmux commands in a detached process, so it needs the socket too
        // — otherwise turn state for this box would be read off a neighbour's pane.
        assert!(attach_argv("thing-x", "/d")
            .last()
            .unwrap()
            .contains("SKEIN_TMUX_SOCK='/boxes/thing-x/session.sock'"));

        forget_place("thing-x");
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn shell_and_attach_argv_differ() {
        let _g = env_lock();
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
    fn read_journal_prefers_store_over_host_dir() {
        // Simulates the clone-mode bug directly: `dir` (the registered box dir) is the HOST's
        // shared working clone, which never has the box's own `.skein/journal.md` — only
        // box-journal.sh's copy in the store does. read_journal must find it there.
        let _g = env_lock();
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
        let _g = env_lock();
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
        let _g = env_lock();
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
    #[test]
    #[cfg(unix)]
    fn resume_batch_holds_real_decisions_when_ai_on() {
        if Command::new("sh").arg("-c").arg("true").output().is_err() {
            return;
        }
        let _g = env_lock();
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
}
