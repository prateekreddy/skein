//! skein core — read the shared sbx registry and derive fleet views.
//!
//! This is the single source of truth shared by the CLI (`skein`) and the server
//! (`skein-server`). It owns no state the sandboxes don't already write; it only reads
//! `sandboxes.json` and derives status. See ARCHITECTURE.md.

// Modules are public and there are no re-exports at the root. There used to be sixteen
// `pub use <mod>::*` lines here, and they cost more than they looked: every cross-module reference
// resolved through a flat namespace, so `crate::signals::` matched nothing from anywhere and the
// declared `mod` graph carried no information at all — none of `docs/architecture.md` §14's
// dependency rules could be checked against it. A reference now says where it comes from, and the
// price is paid at the import rather than hidden in the façade.
//
// What is below the modules is `use`, not `pub use`, and the distinction is the whole point: those
// are what THIS file's own body needs, not a surface anyone else reaches through.
pub mod ai;
pub mod answer;
pub mod apiauth;
pub mod codeowners;
pub mod config;
pub mod contracts;
pub mod diff;
pub mod digest;
pub mod files;
pub mod fleet;
pub mod gitgate;
pub mod github;
pub mod handoff;
pub mod health;
pub mod kit;
pub mod mailbox;
pub mod moduledocs;
pub mod place;
pub mod probes;
pub mod prq;
pub mod repos;
pub mod review;
pub mod runtime;
pub mod sandbox;
pub mod sharedhome;
pub mod signals;
pub mod substrate;
pub mod takeover;
#[cfg(test)]
mod testutil;
pub mod tracking;
pub mod transcript;
pub mod util;

use crate::diff::{read_diffstat_file, DiffStat};
use crate::fleet::{box_disk_limit, fleet_disk_usage, fleet_liveness};
use crate::mailbox::sandboxes_in;
use crate::place::{fleet_sandbox, placed_boxes, shared_record};
use crate::repos::{
    branch_from_box, launch_spec_agent, launch_spec_branch, load_repos, repo_for_box,
};
use crate::runtime::{default_agent, valid_runtime};
use crate::signals::{
    classify_message, classify_pane, current_status_detail, current_task, fuse_status,
    is_generic_wait, pane_is_fresh, probe_is_stale, read_pane_raw, screen_health, session_signal,
    status_edge, title_activity, Pause, Screen, TITLE_FRESH_SECS,
};
use crate::tracking::sync_docs_available;
use crate::util::{ago, bounded_output, clip, first_line, output_with_timeout_why, shorten, Gate};

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
    /// this repo's store holds work-tracking documents newer than the ones installed from it, so a
    /// re-apply has something to deliver. Repo-scoped and host-side, because that half of the answer
    /// is free; whether *this box's* CLAUDE.md is stale can only be read inside the box, and is not
    /// worth waking one on every snapshot to find out. See [`sync_docs_available`].
    #[serde(default)]
    pub docs_update: bool,
    /// This sandbox is not a box skein placed — so skein can see it, and can do nothing with it.
    ///
    /// Two kinds land here and they need no telling apart: a sandbox someone created with `sbx`
    /// directly, and a box from a skein old enough to give every box its own microVM. Neither has a
    /// placement record, a store skein provisioned, or the tmux contract the cockpit attaches through.
    ///
    /// The board *hides* these by default and reveals them on the `foreign:` filter, because they are
    /// on the list only as an artefact of how the list is built: `sbx ls` is authoritative for which
    /// sandboxes exist, and it does not know which of them are skein's. Showing them made a first run
    /// on a machine with other sandboxes look like a fleet full of broken boxes.
    #[serde(default)]
    pub foreign: bool,
    /// Whether this box's GitHub credential is scoped to its own repository — `None` when the fleet
    /// cannot scope at all, so there is no distinction to draw.
    ///
    /// On the row because it is otherwise invisible: a scoped box and an unscoped one look
    /// identical everywhere in the cockpit, so "did the switch take" had no answer short of trying
    /// a push inside the box and reading the 403. Three-valued for the same reason `legacy` is
    /// suppressed on an unadopted host — badging every row before the distinction exists would
    /// label the normal case as the odd one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped: Option<bool>,
    /// MiB this box occupies on the fleet's shared disk, and what it is allowed. Absent for a box
    /// with a sandbox of its own, whose disk is nobody else's problem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_limit_mb: Option<u64>,
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
    // Boxes in the shared sandbox are not sandboxes, so `sbx ls` has never heard of them — without
    // this the entire fleet is invisible on the board while running perfectly. Their placement
    // records are the register. The sandbox itself is *not* a box: it would otherwise appear as one,
    // permanently stale, with no repo and no branch.
    let fleet = fleet_sandbox();
    if !fleet.is_empty() {
        names.remove(&fleet);
        names.extend(placed_boxes(&fleet).into_iter().map(|(name, _)| name));
    }
    // Hoisted: this reads the config and the stored-credential list, and `load_views` runs on every
    // 2s tick for every box. Asked once per pass rather than once per box per pass.
    let scopable = crate::gitgate::can_issue_write_tokens();

    // One measurement for the whole board, not one per row: it is a single `du` in the sandbox, and
    // asking per box would be one round trip each for a number they all read from the same walk.
    let usage = fleet_disk_usage();

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
            // Liveness through `box_liveness`, never straight off the `sbx ls` row — for a box in
            // the fleet the two disagree, and the row wins in the worst possible way.
            //
            // A migrated box's old sandbox is STOPPED, not destroyed (that is deliberate: it is the
            // undo). It keeps the box's name, so `sbx ls` still lists it, `s` is `Some`, and its
            // `live` is `Stopped` — which `state_with` turns into `stale`, discarding a perfectly
            // fresh `waiting` the box's own hooks just wrote. Every migrated box read `stale` on the
            // board while working, and the per-box session view — which already went through
            // `box_liveness` — disagreed with the board about the same box at the same moment.
            //
            // `box_liveness` knows a placed box's liveness is its tmux server in the shared sandbox,
            // and falls back to exactly this row for a box that is still its own VM.
            let live = box_liveness(&name);
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
                hook_health,
                screen_health: screen.to_string(),
                // Two file reads against the repo's store — no box is woken to answer this, which is
                // what makes it affordable on a signal computed for every row on every snapshot.
                docs_update: repo
                    .as_ref()
                    .is_some_and(|rp| sync_docs_available(Path::new(&rp.store))),
                // The placement record is the whole test: writing one is what makes a sandbox a box
                // skein owns. This used to be conditional on a fleet being configured, because an
                // unconfigured host gave every box its own VM and labelling all of them would have
                // marked the normal case as the odd one. There is no such host now.
                foreign: shared_record(&name).is_none(),
                scoped: scopable.then(|| crate::gitgate::box_is_scoped(&name)),
                disk_mb: usage.get(&name).copied(),
                disk_limit_mb: usage.get(&name).and(box_disk_limit(&name)),
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

/// Micro-cache over `sbx ls`: `load_views` used to re-run it once at the top and then again via
/// `lookup_dir` for every box the registry didn't know (`read_journal` → `lookup_dir` →
/// `fleet_boxes`) — an N-box fleet paid 1+N subprocess spawns per 2s tick, times open browser tabs.
///
/// A [`Gate`] rather than a plain cache, because the tabs were still multiplying: the answer is
/// only remembered once the first `sbx ls` *returns*, so every tab that missed together spawned its
/// own. See the note on `Gate` — it also carries the last good answer and the backoff that lets a
/// struggling daemon recover instead of being re-asked every 1.5s forever.
static FLEET_GATE: Gate<Vec<SbxBox>> = Gate::new();

/// How often skein is willing to spawn `sbx ls` while it is answering.
const FLEET_FRESH: Duration = Duration::from_millis(1500);

/// Enumerate the fleet from sbx. `None` when sbx can't be consulted (not installed, errored,
/// unparseable, or hung past the timeout) *and* nothing was ever learned — callers then fall back to
/// the registry. Override with `$SKEIN_LS_CMD` (run via `sh -c`; must emit the `sbx ls --json`
/// shape). Micro-cached — see [`FLEET_GATE`].
pub fn fleet_boxes() -> Option<Vec<SbxBox>> {
    // Zero: tests swap $SKEIN_LS_CMD per case and run in parallel — a process-wide gate would serve
    // one test's fleet to another. Prod (server/CLI) keeps it.
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        FLEET_FRESH
    };
    FLEET_GATE.get(fresh, || {
        let asked = env::var("SKEIN_LS_CMD").ok().filter(|s| !s.is_empty());
        let label = format!(
            "`{}`",
            asked.clone().unwrap_or_else(|| "sbx ls --json".into())
        );
        let mut cmd = match asked {
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
        let got = output_with_timeout_why(&mut cmd, Duration::from_secs(5))
            .and_then(|o| match o.status.success() {
                true => Ok(o),
                false => Err(format!(
                    "{label} exited {}{}",
                    o.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "on a signal".into()),
                    match String::from_utf8_lossy(&o.stderr).trim() {
                        "" => String::new(),
                        said => format!(" — {}", clip(said, 200)),
                    }
                )),
            })
            .and_then(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                parse_boxes_checked(&out).ok_or_else(|| {
                    format!(
                        "{label} answered, but not with a fleet listing skein can read{}",
                        match out.trim() {
                            "" => " — it printed nothing at all".to_string(),
                            said => format!(": {}", clip(said.lines().next().unwrap_or(""), 200)),
                        }
                    )
                })
            });
        match got {
            Ok(boxes) => {
                remember_fleet_failure(None);
                Some(boxes)
            }
            Err(why) => {
                remember_fleet_failure(Some(why));
                None
            }
        }
    })
}

/// Why the last `sbx ls` produced no fleet, or `None` when it produced one.
///
/// [`fleet_boxes`] answers with an `Option`, and four unrelated failures arrive as the same `None`:
/// sbx is not on this process's PATH, the call outlived its budget, it exited non-zero, or it
/// printed something that is not a listing. Every caller then says a version of "sbx did not
/// answer" — true of one of those four and misleading about the other three. The one that sent a
/// user looking in the wrong place was PATH: `sbx ls` worked in their terminal, so the message read
/// as skein being wrong about a working sbx.
///
/// Written by whichever call actually ran, rather than recomputed by asking again, because a second
/// ask is a different question: a daemon that has recovered would report success while the board is
/// still showing the failure that is being explained.
static FLEET_WHY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn remember_fleet_failure(why: Option<String>) {
    if let Ok(mut slot) = FLEET_WHY.lock() {
        *slot = why;
    }
}

/// The last [`fleet_boxes`] failure in words, for the places that report one: the server's log, the
/// health banner and `skein doctor`.
pub fn fleet_failure() -> Option<String> {
    FLEET_WHY.lock().ok().and_then(|why| why.clone())
}

/// Whether the fleet snapshot on offer is a remembered one because the last `sbx ls` failed. The
/// cockpit says so rather than presenting stale data as live.
pub fn fleet_degraded() -> bool {
    FLEET_GATE.degraded()
}

/// One box's run-state from sbx — a single-box view of [`fleet_boxes`].
fn box_liveness(name: &str) -> Option<Liveness> {
    // A shared box is not in `sbx ls` — no sandbox carries its name — so asking there reports every
    // one of them as gone. Its anchor IS its liveness: the tmux server lives exactly as long as the
    // box, so a live pid is a running box and a dead one is a stopped box with its tree intact.
    if shared_record(name).is_some() {
        // Asked of the sandbox, not of the host's /proc — see `fleet_liveness`. A box missing from
        // the sweep is one the sandbox could not answer for (it is stopped, or sbx did not reply),
        // and "cannot tell" is `None`, not "stopped".
        return fleet_liveness().get(name).map(|live| {
            if *live {
                Liveness::Running
            } else {
                Liveness::Stopped
            }
        });
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
    // A box in the fleet is not a sandbox, so `sbx ls` has never heard of it — but its placement
    // record names its checkout, which is the very thing being asked for.
    if let Some(tree) = shared_record(name)
        .map(|r| r.tree)
        .filter(|t| !t.is_empty())
    {
        return Some(tree);
    }
    fleet_boxes()?
        .into_iter()
        .find(|b| b.name == name)
        .map(|b| b.dir)
        .filter(|d| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{load_config, save_config, Config};
    use crate::place::{forget_place, record_place, PlaceRecord};

    use crate::kit::ensure_store;
    use crate::repos::box_name;
    use crate::repos::branch_of;
    use crate::repos::Repo;
    use crate::repos::{
        is_git_url, is_ssh_url, repin_branch, repo_id_from_source, save_repos, set_repo_settings,
        ssh_to_https, write_launch_spec_for_agent, REPOS_CACHE,
    };
    use crate::runtime::{guarded_agent_command, runtime_adapter};
    use crate::sandbox::{
        agent_resume_cmd, attach_argv, attach_argv_as, box_write_argv, delist_box, destroy_box,
        drop_dest, initial_attach_argv_as, launch_command, repo_launch_command_as, resume_batch,
        resume_box, shell_argv, stop_box,
    };
    use crate::takeover::replacement_name;
    use crate::testutil::*;
    use crate::tracking::{
        connection_for_box, connection_token, describe_refresh, gateway_said, load_connections,
        plane_project_id, remove_connection, revocation_outcome, set_box_tracking,
        set_connection_token, sync_gateway_for_box, sync_mcp_url, sync_mint_token,
        sync_provision_box, sync_revoke_token, sync_status, upsert_connection, SyncConnection,
    };
    use crate::util::sh_quote;
    use crate::util::slug;
    use crate::util::{host_of, pct_decode, safe_component, GATE_MAX_INTERVAL};

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

    // The takeover path reaches into the source box for its branch and HEAD, and until now nothing
    // exercised that. The guard is deliberately the *argv*: `Place` is about to change how skein
    // addresses a box, and this is the contract it must not silently alter.
    /// Four different faults, four different sentences.
    ///
    /// `fleet_boxes` answers `Option`, so a caller can only say "no fleet" — and every one of them
    /// said a version of "sbx did not answer". That is true of one of the four, and it sent a user
    /// looking at the wrong thing: their `sbx ls` worked in a terminal, so the message read as skein
    /// being wrong rather than as skein being started without the PATH that finds it.
    #[test]
    fn a_fleet_that_could_not_be_listed_says_why_not() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);

        env::set_var("SKEIN_LS_CMD", "echo 'sbx: daemon not running' >&2; exit 3");
        assert!(fleet_boxes().is_none());
        let why = fleet_failure().expect("a failure with no reason is the bug being fixed");
        assert!(why.contains("exited 3"), "{why}");
        // What it said on the way out. A non-zero exit with the tool's own complaint discarded is a
        // diagnosis thrown away at the only moment it was available.
        assert!(why.contains("daemon not running"), "{why}");

        // Answered, but not with a listing — an older sbx, a `--json` it does not know, a wrapper
        // printing a banner. Reported as "did not answer" this looks like a dead daemon.
        env::set_var("SKEIN_LS_CMD", "echo not-json-at-all");
        assert!(fleet_boxes().is_none());
        let why = fleet_failure().expect("a reason");
        assert!(why.contains("not with a fleet listing"), "{why}");
        assert!(
            why.contains("not-json-at-all"),
            "the output it did give: {why}"
        );

        // And a success clears it, so a stale reason is never reported over a working fleet.
        env::set_var("SKEIN_LS_CMD", "echo '[]'");
        assert_eq!(fleet_boxes().map(|b| b.len()), Some(0));
        assert_eq!(fleet_failure(), None);

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_HOME");
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
        // A minute, so nothing here ages out mid-test: what is under test is which answer the gate
        // hands back, not when it decides to ask again.
        let long = Duration::from_secs(60);
        // Static because the gate now refreshes behind its caller, which needs it to outlive the
        // call. Every `get` here is preceded by an `invalidate`, so nothing carries between cases.
        static GATE: Gate<Vec<SbxBox>> = Gate::new();
        let gate = &GATE;
        gate.invalidate();

        assert_eq!(gate.get(long, || Some(vec![box_])).unwrap().len(), 1);
        assert!(!gate.degraded());

        // A failure must not report the fleet gone — the board would blank itself on one slow tick.
        gate.invalidate();
        assert_eq!(gate.get(long, || None).unwrap()[0].name, "live-box");
        assert!(gate.degraded());

        // An empty fleet is an *answer*, not a failure: it replaces last-known-good, and the boxes
        // that were there do not come back the next time sbx cannot be reached.
        gate.invalidate();
        assert!(gate.get(long, || Some(vec![])).unwrap().is_empty());
        assert!(!gate.degraded());
        gate.invalidate();
        assert!(gate.get(long, || None).unwrap().is_empty());
        assert!(gate.degraded());
    }

    /// The property the board's tick depends on: however many callers arrive together, the sandbox
    /// is asked once. Check-then-act gave every browser tab its own subprocess, because the answer
    /// was only remembered once the first one returned.
    #[test]
    fn concurrent_callers_ask_the_sandbox_once_between_them() {
        static GATE: Gate<u32> = Gate::new();
        static ASKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        GATE.invalidate();
        ASKS.store(0, std::sync::atomic::Ordering::Relaxed);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    GATE.get(Duration::from_secs(60), || {
                        ASKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // Long enough that the others are certainly waiting on the lane rather
                        // than having missed each other by luck.
                        std::thread::sleep(Duration::from_millis(50));
                        Some(1)
                    })
                });
            }
        });
        assert_eq!(
            ASKS.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "eight simultaneous callers must cost one subprocess, not eight"
        );
    }

    /// What a caller pays when the sandbox is wedged. Measured against a hung sbx daemon, a board
    /// refresh walked four gates in series and took 31 seconds to hand back the answers it already
    /// had — so the property is that ageing out costs the caller *nothing* once the gate holds one.
    #[test]
    fn an_aged_out_answer_is_served_at_once_and_refreshed_behind_the_caller() {
        static GATE: Gate<u32> = Gate::new();
        static ASKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        use std::sync::atomic::Ordering::Relaxed;
        const HUNG: Duration = Duration::from_millis(300);
        let fresh = Duration::from_millis(20);
        GATE.invalidate();
        ASKS.store(0, Relaxed);

        // Cold: nothing to serve, so this caller does have to wait.
        assert_eq!(GATE.get(fresh, || Some(1)), Some(1));
        assert_eq!(
            ASKS.load(Relaxed),
            0,
            "the cold ask is the one under test next"
        );

        // Now aged out, with the sandbox hung. The caller must not wait on it.
        std::thread::sleep(fresh * 2);
        let began = std::time::Instant::now();
        let answer = GATE.get(fresh, || {
            ASKS.fetch_add(1, Relaxed);
            std::thread::sleep(HUNG);
            Some(2)
        });
        assert_eq!(
            answer,
            Some(1),
            "the remembered answer, not a wait for a new one"
        );
        assert!(
            began.elapsed() < HUNG / 2,
            "a stale gate must not make its caller sit out the timeout: waited {:?}",
            began.elapsed()
        );

        // …and the refresh really did run, so the *next* caller finds a fresh answer.
        std::thread::sleep(HUNG * 2);
        assert_eq!(
            ASKS.load(Relaxed),
            1,
            "exactly one refresh, behind the caller"
        );
        assert_eq!(
            GATE.get(fresh, || Some(3)),
            Some(2),
            "refreshed to the new value"
        );
    }

    /// The exception, and why it is one: `invalidate` means skein has just *changed* the thing being
    /// asked about, so the remembered answer is wrong rather than merely old. Serving it while a
    /// refresh ran behind would show a box as stopped immediately after starting it.
    #[test]
    fn a_gate_skein_has_invalidated_makes_its_caller_wait_for_the_truth() {
        static GATE: Gate<u32> = Gate::new();
        let fresh = Duration::from_millis(20);
        GATE.invalidate();
        assert_eq!(GATE.get(fresh, || Some(1)), Some(1));

        GATE.invalidate();
        assert_eq!(
            GATE.get(fresh, || Some(2)),
            Some(2),
            "an invalidated gate must return what it just asked for, not what it remembered"
        );
    }

    /// The property that lets a struggling daemon recover: consecutive failures space the attempts
    /// out instead of re-arming at the same interval, and one success puts it straight back.
    #[test]
    fn repeated_failure_asks_less_often_and_success_restores_the_cadence() {
        static GATE: Gate<u32> = Gate::new();
        let gate = &GATE;
        gate.invalidate();
        let fresh = Duration::from_millis(100);
        assert_eq!(gate.interval(fresh), fresh, "healthy: ask at the full rate");

        for expected in [200u64, 400, 800] {
            gate.invalidate();
            gate.get(fresh, || None);
            assert_eq!(gate.interval(fresh), Duration::from_millis(expected));
        }
        // Capped, so a daemon that comes back is still noticed within half a minute.
        for _ in 0..20 {
            gate.invalidate();
            gate.get(fresh, || None);
        }
        assert_eq!(gate.interval(fresh), GATE_MAX_INTERVAL);

        gate.invalidate();
        gate.get(fresh, || Some(7));
        assert_eq!(gate.interval(fresh), fresh);
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
                plane_project: String::new(),
                sync_connection: String::new(),
                review_queue: true,
                sync_gateway_url: String::new(),
            },
            Repo {
                id: "web-api".into(),
                source: "s".into(),
                work: "/w".into(),
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
            work: "/w".into(),
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
    fn repo_launch_command_uses_skein_kit_and_persistent_session() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::remove_var("SKEIN_AGENT");
        // This is the sandbox-per-box path, which is now the opt-in one.
        save_config(&Config {
            fleet_sandbox: String::new(),
            ..Config::default()
        })
        .unwrap();
        let store = home.join("st").join(".claude");
        let repo = Repo {
            id: "thing".into(),
            source: "s".into(),
            work: "/work/thing".into(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        // box name is the slug `thing-feat-auth`; the REAL branch (with the slash) is feat/auth.
        let cmd = repo_launch_command_as("thing-feat-auth", &repo, "feat/auth", None);
        // `skein start`, not `sbx create`: a box is assembled inside the shared sandbox by a sequence
        // of round-trips, which no single shell line can express. The attach happens after the box
        // exists, because its argv names a placement that does not exist yet when this string is built.
        assert!(
            cmd.contains(
                " start 'thing-feat-auth' --branch 'feat/auth' --agent 'claude' --attach"
            ),
            "the launcher carries the real branch and the runtime: {cmd}"
        );
        assert!(
            !cmd.contains("sbx create"),
            "there is no sandbox to create for a box: {cmd}"
        );
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
        // The sandbox-per-box path — the fleet builds its session a different way.
        save_config(&Config {
            fleet_sandbox: String::new(),
            ..Config::default()
        })
        .unwrap();
        let repo = Repo {
            id: "skein".into(),
            source: "s".into(),
            work: "/work/skein".into(),
            store: home.join("store/.claude").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        // The launcher carries the runtime choice; what that runtime then *does* on attach — the
        // update, the hook-trust bypass, no `resume --last` on a first start — is
        // `initial_attach_argv_as`'s business and is asserted there. This string used to contain both,
        // because `sbx create … && sbx exec …` was one line; it is now `skein start --attach`.
        let cmd = repo_launch_command_as("skein-codex", &repo, "codex", Some("codex"));
        assert!(
            cmd.contains("--agent 'codex'"),
            "the runtime override has to reach the launcher: {cmd}"
        );
        placed("skein-codex");
        let attach = initial_attach_argv_as("skein-codex", "codex");
        let shell = attach.last().unwrap();
        assert!(shell.contains("new-session -d -s skein-agent"), "{shell}");
        assert!(shell.contains("timeout 120 codex update"), "{shell}");
        assert!(
            shell.contains("codex --no-alt-screen --dangerously-bypass-hook-trust"),
            "{shell}"
        );
        assert!(!shell.contains("codex resume --last"), "{shell}");
        // What the box will be recorded as, which is what a later attach reads back.
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
    fn shared_home_links_two_private_homes_and_refuses_real_path() {
        let store_tmp = tempdir();
        let store = store_tmp.join("store/.claude");
        ensure_store(&store).unwrap();
        let helper = store.join("skein/bin/shared-home.sh");
        let home_a_tmp = tempdir();
        let home_a = home_a_tmp.join("home-a");
        let home_b_tmp = tempdir();
        let home_b = home_b_tmp.join("home-b");
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

        let store_tmp = tempdir();
        let store = store_tmp.join("store/.claude");
        let home_tmp = tempdir();
        let home = home_tmp.join("home");
        let work_tmp = tempdir();
        let work = work_tmp.join("work");
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
        let store_tmp = tempdir();
        let store = store_tmp.join("store/.claude");
        let home_tmp = tempdir();
        let home = home_tmp.join("home");
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

    // `shared-paths.txt` surfaces a repo's gitignored essentials into a box — `.env`, and for some
    // repos the `CLAUDE.md` the box takes its direction from. It read them from
    // `/run/sandbox/source`, a mount only `--clone` mode has, so in the fleet the whole mechanism
    // was inert: not broken links, *nothing*, and nothing said so.
    //
    // In the fleet the mirror is the repo's real work tree, mounted read-write — so every entry
    // takes the `rw` shape there, seeded into the store and linked from there. A box must not be
    // able to edit the host's own checkout, which the read-only bind used to guarantee for free.
    // Three resolutions that all asked `sbx ls` who a box is. A box in the fleet is not a sandbox
    // and never appears there, so each one quietly failed for the whole fleet: its workspace was
    // unknown, a takeover of it refused, and a replacement could be handed the name of a live box.
    #[test]
    fn a_box_in_the_fleet_can_still_be_found_by_name() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        // `sbx ls` knows nothing — the fleet's normal state.
        env::set_var("SKEIN_LS_CMD", "echo '[]'");
        save_config(&Config {
            fleet_sandbox: "skein-fleet".into(),
            ..load_config()
        })
        .unwrap();
        record_place(
            "web-main",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
            },
        )
        .unwrap();

        assert_eq!(
            lookup_dir("web-main").as_deref(),
            Some("/boxes/web-main/tree"),
            "a placed box knows its own checkout even when sbx has never heard of it"
        );
        // And a name already taken by a fleet box must not be handed out again.
        let repo = Repo {
            id: "web".into(),
            source: String::new(),
            work: String::new(),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        record_place(
            "web-main-codex",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 2,
                home: "/boxes/web-main-codex/home".into(),
                tree: "/boxes/web-main-codex/tree".into(),
                sock: "/boxes/web-main-codex/session.sock".into(),
            },
        )
        .unwrap();
        let picked = replacement_name(&repo, "web-main", "main", "codex");
        assert_ne!(
            picked, "web-main-codex",
            "that name is a live box in the fleet; `start_box` would refuse its existing tree"
        );
        forget_place("web-main-codex");

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        forget_place("web-main");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[test]
    fn a_box_with_no_clone_mirror_still_gets_the_repos_shared_paths() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let dir = tempdir();
        let store = dir.join("store").join(".claude");
        let work = dir.join("work"); // the host checkout: what /run/sandbox/source used to be
        let tree = dir.join("tree"); // the box's own clone
        ensure_store(&store).unwrap();
        for d in [&work, &tree] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(work.join(".env"), "SECRET=from-host\n").unwrap();
        fs::write(work.join("CLAUDE.md"), "# direction\n").unwrap();
        fs::write(store.join("shared-paths.txt"), ".env\nCLAUDE.md\n").unwrap();
        fs::write(
            store.join("skein").join("mirror"),
            format!("{}\n", work.display()),
        )
        .unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .status()
            .unwrap()
            .success());
        symlink(&store, tree.join(".claude")).unwrap();
        // The wreckage an earlier migration left: a link into a mount this box does not have.
        symlink("/run/sandbox/source/.env", tree.join(".env")).unwrap();

        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein/bin/sandbox-bootstrap.sh"))
            .env("CLAUDE_PROJECT_DIR", &tree)
            .env("HOME", &home)
            .env("SKEIN_BOX", "demo-main")
            // This box may itself be clone-mode, so name the mirror rather than letting the
            // script find the harness's own /run/sandbox/source.
            .env("SKEIN_MIRROR", &work)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");

        for name in [".env", "CLAUDE.md"] {
            let dst = tree.join(name);
            let target = fs::read_link(&dst).unwrap_or_else(|e| panic!("{name}: {e}"));
            // Canonical, because the box reaches its store through a symlink: the same directory
            // has two spellings and only one of them is the one written here.
            let target = fs::canonicalize(&target).unwrap();
            assert!(
                target.starts_with(fs::canonicalize(store.join("shared-rw")).unwrap()),
                "{name} must resolve through the store, never straight at the host checkout: {}",
                target.display()
            );
            assert!(
                fs::read_to_string(&dst).unwrap().contains("from-host") || name == "CLAUDE.md",
                "{name} must carry the host's content"
            );
        }
        // The point of routing through the store: writing here must not touch the host checkout.
        fs::write(tree.join(".env"), "SECRET=changed\n").unwrap();
        assert_eq!(
            fs::read_to_string(work.join(".env")).unwrap(),
            "SECRET=from-host\n",
            "a box must never be able to edit the host's own working copy"
        );
    }

    // A box belongs to a registered repo, and there is no other way to make one. The env-var mode
    // ($SKEIN_KIT/$SKEIN_STORE/$SKEIN_AGENT with a `sandboxes.json`) built a box as its own microVM
    // via `sbx create`; both went at once, because a box in the shared sandbox is assembled from a
    // repo to clone and a store to mount, and that mode supplied neither per box.
    //
    // What matters is that the refusal is *legible*: this string is run by `sh -c` in a terminal, so
    // an unregistered name has to explain itself there rather than fail somewhere in sbx.
    #[test]
    fn a_box_outside_a_registered_repo_is_refused_with_the_fix_in_the_message() {
        let _g = env_lock();
        env::set_var("SKEIN_HOME", tempdir());
        env::remove_var("SKEIN_LAUNCH_CMD");
        let cmd = launch_command("nobody-x", "x");
        assert!(
            cmd.contains("belongs to no registered repo") && cmd.contains("skein add"),
            "the terminal must be told what to do about it: {cmd}"
        );
        assert!(
            cmd.contains("exit 1") && !cmd.contains("sbx create"),
            "and nothing may be created: {cmd}"
        );

        // $SKEIN_LAUNCH_CMD still wins outright, with {branch}/{name} substituted + shell-quoted.
        // It is the seam for anyone driving box creation themselves, and it never consulted repos.
        env::set_var("SKEIN_LAUNCH_CMD", "setup.sh {branch} {name}");
        assert_eq!(launch_command("thing-x", "x"), "setup.sh 'x' 'thing-x'");
        env::remove_var("SKEIN_LAUNCH_CMD");
        env::remove_var("SKEIN_HOME");
    }

    // Every box is brought up by `skein start` inside the shared sandbox. This used to be one of two
    // shapes, chosen by whether `fleet_sandbox` was named; clearing it gave each box its own microVM.
    // That model is gone — reservations sum, and eight of them do not fit on one machine — so the
    // remaining job of this test is that the launcher skein hands to `sh -c` is actually runnable.
    #[test]
    fn a_box_is_brought_up_by_skein_rather_than_by_sbx_create() {
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
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        let fleet = launch_command("web-feat-x", "feat/x");
        assert!(
            fleet.contains(" start 'web-feat-x' --branch 'feat/x' --agent 'claude'"),
            "a box is brought up by skein, not by `sbx create`: {fleet}"
        );
        assert!(
            !fleet.contains("sbx create"),
            "there is no sandbox to create for a box: {fleet}"
        );
        // Runnable, not merely correct. The cockpit is normally run straight out of a build, where
        // nothing called `skein` is on $PATH — creating a box died on `sh: skein: command not found`
        // and then reconnected forever onto a sandbox that was never made.
        let launcher = fleet.split_whitespace().next().unwrap();
        assert!(
            launcher.ends_with("skein") || launcher.ends_with("skein'"),
            "the launcher must name the skein binary: {fleet}"
        );
        if launcher != "skein" {
            assert!(
                std::path::Path::new(launcher.trim_matches('\'')).is_file(),
                "an absolute launcher must exist, or `sh -c` cannot run it: {launcher}"
            );
        }
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
        placed("web-feat-x");
        let repo = Repo {
            id: "web".into(),
            source: "git@github.com:o/web.git".into(),
            work: home.join("repos/web/work").to_string_lossy().into(),
            store: home
                .join("repos/web/store/.claude")
                .to_string_lossy()
                .into(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
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
        // `--name` sits between the two, so this asserts the pair rather than the spelling.
        assert!(
            restored.contains("claude --name") && restored.contains("--continue"),
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

    /// A box chooses at creation; the repo's setting is the default it starts from.
    ///
    /// Without this, "use sync for this box?" could only be answered for every box of a repo at
    /// once — so one box doing untracked exploratory work meant either untracking its repo or
    /// minting it a token against a backlog it will never claim from.
    #[test]
    fn a_box_can_claim_somewhere_other_than_its_repo_or_nowhere_at_all() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        upsert_connection(
            Some("team"),
            "team",
            "https://team.example",
            Some("pat_team"),
        )
        .unwrap();
        upsert_connection(
            Some("solo"),
            "solo",
            "https://solo.example",
            Some("pat_solo"),
        )
        .unwrap();
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: "team".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        assert_eq!(
            connection_for_box("web-main").map(|c| c.id),
            Some("team".to_string()),
            "no choice of its own means the repo's"
        );

        set_box_tracking("web-main", Some("solo")).unwrap();
        assert_eq!(
            connection_for_box("web-main").map(|c| c.id),
            Some("solo".to_string()),
            "the box's own choice wins over its repo's"
        );
        assert_eq!(
            connection_for_box("web-other").map(|c| c.id),
            Some("team".to_string()),
            "and it is one box's choice, not the repo's — its siblings are untouched"
        );

        // Empty is a decision, not an absence: this box claims nowhere.
        set_box_tracking("web-main", Some("")).unwrap();
        assert!(connection_for_box("web-main").is_none());
        assert_eq!(sync_gateway_for_box("web-main"), "");

        // Clearing hands the box back to its repo, rather than leaving it permanently untracked.
        set_box_tracking("web-main", None).unwrap();
        assert_eq!(
            connection_for_box("web-main").map(|c| c.id),
            Some("team".to_string())
        );
        // Idempotent: clearing a box that never chose is not an error.
        set_box_tracking("web-main", None).unwrap();
        // Restored, or the next test to take `env_lock` inherits a SKEIN_HOME naming a
        // directory this test's guard has already removed — and writes through it, which
        // recreates the tree as a leak nobody owns.
        std::env::remove_var("SKEIN_HOME");
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
            plane_project: String::new(),
            sync_connection: "shared".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        assert_eq!(sync_gateway_for_box("web-main"), "https://shared.example");
        // Two products tracked in different Plane instances can't share a claim namespace, and the
        // token a box carries is only valid at the gateway that minted it — so switching backlogs
        // switches the credential too, which is exactly what picking a whole connection buys.
        upsert_connection(Some("own"), "own", "https://own.example/", Some("pat_own")).unwrap();
        set_repo_settings("web", None, Some("own"), None).unwrap();
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
        set_repo_settings("web", None, Some(""), None).unwrap();
        assert!(connection_for_box("web-main").is_none());
        assert_eq!(sync_gateway_for_box("web-main"), "");
        // A selection naming nothing would read as "tracked" and behave as "not tracked".
        assert!(set_repo_settings("web", None, Some("nope"), None).is_err());
        // One call can carry every field, and the fields don't disturb each other.
        set_repo_settings("web", None, Some("own"), None).unwrap();
        let saved = load_repos().into_iter().find(|r| r.id == "web").unwrap();
        assert_eq!(saved.sync_connection, "own");
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
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
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
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
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
            plane_project: String::new(),
            sync_connection: "shared".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        let e = remove_connection("shared").unwrap_err();
        assert!(e.contains("web"), "say which repo would lose tracking: {e}");
        assert!(remove_connection("ghost").is_err());
        set_repo_settings("web", None, Some(""), None).unwrap();
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

    /// The cockpit speaks the box's *own ask*, and it speaks on its own switch.
    ///
    /// Two things about the mouth fail silently, which is the worst way for a voice to fail — you
    /// cannot tell "nothing needs me" from "it stopped talking". Both are one careless edit away:
    ///
    /// 1. **The words.** Speaking `headline` is the whole point — "example-box-1 wants permission. Run
    ///    rm -rf build?" is actionable where "example-box-1 needs a decision" is only a reason to go and
    ///    look, which is the trip this feature exists to save. Folding it back onto the notification
    ///    text would sound identical to someone who never heard the good version.
    /// 2. **The switch.** Notifications need a browser permission that may have been refused;
    ///    speaking needs none. Gating voice on `alertsOn` would silence the half that still works,
    ///    for people who had already said no to the half that does not.
    #[test]
    fn the_cockpit_speaks_the_boxs_own_ask_on_a_switch_of_its_own() {
        let page = include_str!("web/index.html");
        let view = BoxView {
            headline: Some("Run rm -rf build?".into()),
            ..BoxView::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            json.contains("\"headline\":\"Run rm -rf build?\""),
            "the fleet snapshot stopped carrying the ask, so there is nothing to say: {json}"
        );
        assert!(
            page.contains("b.headline") && page.contains("forSpeech"),
            "the cockpit no longer speaks the box's own words"
        );
        // The property, not its spelling: both channels are now driven by one announcer, so what
        // matters is that each still consults its OWN switch there and the two are never conjoined.
        assert!(
            page.contains("if (voiceOn) say(") && page.contains("if (alertsOn) {"),
            "voice lost its own switch — gated on alerts, it dies wherever notifications were refused"
        );
        assert!(
            !page.contains("voiceOn && alertsOn") && !page.contains("alertsOn && voiceOn"),
            "the two channels were tied together; refusing notifications must not take speech with it"
        );
    }

    /// Nothing a misheard word can reach is hard to undo.
    ///
    /// Speech recognition is wrong sometimes — that is not a defect to engineer away, it is the
    /// medium. So the design constraint is not accuracy, it is *blast radius*: every verb the ear
    /// accepts is either read-only or reversible, and the destructive ones are absent rather than
    /// confirmed. A confirmation is the wrong answer here because the whole point of the ear is that
    /// you are not looking at the screen; a dialog you cannot see is a dialog you will dismiss by
    /// saying the next thing.
    ///
    /// The second half is subtler and just as easy to lose: the ear has to reach you *inside a
    /// focused terminal*. The fleet keymap deliberately yields every key to one, so push-to-talk
    /// cannot live there — answering a box while heads-down in another one is the entire use, and an
    /// ear that only works on the board is an ear you would never reach for.
    #[test]
    fn a_misheard_word_cannot_cost_a_branch() {
        let page = include_str!("web/index.html");
        let verbs: String = page
            .lines()
            .skip_while(|l| !l.contains("const VOICE_VERBS"))
            .take_while(|l| !l.starts_with("];"))
            .collect();
        assert!(
            verbs.contains("resumeBox"),
            "the verb table was not found at all"
        );
        for reckless in ["destroyBox", "mergePr", "stopBox", "shipBox", "takeover"] {
            assert!(
                !verbs.contains(reckless),
                "`{reckless}` is reachable by voice; a word heard wrong must cost a glance, not work"
            );
        }
        // Push-to-talk on its own handler, keyed by code so it survives a focused terminal.
        assert!(
            page.contains("AltRight"),
            "the ear has no push-to-talk key, so it can only be reached from the board"
        );
        // The fleet keymap must still hand every key to a terminal — the ear works *because* that
        // guard is there, and removing it would be a far worse regression than losing the ear.
        assert!(
            page.contains("if (inTerm || inField) return;"),
            "the fleet keymap stopped yielding to a focused terminal"
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
        // The plugin IS the registration for Claude now. Upstream ships the same `sync` server
        // inside it, so skein registering its own would shadow the plugin's — a hand-added entry
        // wins — and the box would end up with the tools and none of the monitor or hooks.
        assert!(
            calls.contains("plugin marketplace add prateekreddy/sync"),
            "{calls}"
        );
        assert!(calls.contains("plugin install sync@sync"), "{calls}");
        assert!(
            !calls.contains("mcp add"),
            "skein must not register `sync` for Claude any more — the plugin declares it: {calls}"
        );
        // And the old one is taken away, because every box wired before today still carries it.
        assert!(calls.contains("mcp remove sync"), "{calls}");

        // The gateway still comes from the same place; only where it lands has changed. The plugin
        // declares its url as `${SYNC_MCP_URL:-…}`, which is upstream's supported seam and the only
        // one that survives a plugin update.
        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(boxhome.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["env"]["SYNC_MCP_URL"], "https://gw.test/mcp");

        // Codex keeps the token route: plugins are a Claude Code feature, so for Codex this is not
        // a fallback, it is the only way it ever gets these tools.
        let codex = fs::read_to_string(boxhome.join(".codex/config.toml")).unwrap();
        assert!(codex.contains("[mcp_servers.sync]"), "{codex}");
        assert!(codex.contains("Bearer sync_agent_abc"), "{codex}");
        assert!(
            codex.contains("https://theirs.test"),
            "it appends, never rewrites: {codex}"
        );

        // The rules land only after registration, and they land in the STORE — this repo's
        // `.claude` — so every box of the repo sees them, not just the one that was wired up.
        let claude_md = fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("## Work tracking"), "{claude_md}");
        assert!(
            claude_md.contains("some direction"),
            "it appends, never replaces"
        );
        assert!(store.join("memory/work-tracking.md").is_file());
        // Still shipped here because this box has Codex, which cannot install a Claude Code plugin
        // and would otherwise be left with the tools and no playbook for them. The sibling test
        // covers the other half: with the plugin and no Codex, this copy is skipped.
        assert!(store.join("skills/work-tracking/SKILL.md").is_file());
        assert!(store.join("skills/work-tracking/organising.md").is_file());
        assert!(store
            .join("skills/work-tracking/troubleshooting.md")
            .is_file());
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

    /// Wiring a repo to a tracker points every box of it at that gateway, in one write.
    ///
    /// The store is project scope for every box of the repo, and `env` is honoured there — so this
    /// reaches boxes that do not exist yet, which is what makes it one action per repo instead of
    /// one per box. Without it the plugin falls back to the default gateway compiled into it, and a
    /// box would quietly claim work on somebody else's tracker.
    #[test]
    fn a_repos_store_points_its_boxes_at_the_gateway_its_connection_names() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        fs::create_dir_all(&store).unwrap();

        crate::tracking::save_connections(&[crate::tracking::SyncConnection {
            id: "c1".into(),
            label: "ours".into(),
            gateway_url: "https://gw.test".into(),
        }])
        .unwrap();
        let repo = Repo {
            id: "r1".into(),
            source: "s".into(),
            work: "/w".into(),
            store: store.display().to_string(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: "c1".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        save_repos(std::slice::from_ref(&repo)).unwrap();

        ensure_store(&store).unwrap();
        let gateway = store.join("skein/sync/gateway");
        // `/mcp` under the base, the same normalisation every other caller uses — a pasted endpoint
        // must not become `/mcp/mcp`.
        assert_eq!(
            fs::read_to_string(&gateway).unwrap().trim(),
            "https://gw.test/mcp"
        );

        // Unwired again: the file goes, rather than outliving the decision to disconnect. A stale
        // URL here is worse than none — it keeps pointing boxes at a tracker the repo has left.
        save_repos(&[Repo {
            sync_connection: String::new(),
            ..repo
        }])
        .unwrap();
        ensure_store(&store).unwrap();
        assert!(
            !gateway.exists(),
            "an unwired repo must stop pointing its boxes anywhere"
        );

        env::remove_var("SKEIN_HOME");
    }

    /// A box with no minted token is still wired up: the URL alone is enough now.
    ///
    /// The token stopped being what Claude authenticates with the moment the plugin took over the
    /// server — it signs in over OAuth. Gating the whole install on a token would mean a repo could
    /// not be pointed at a tracker without one being minted per box, which is the per-box work this
    /// change exists to remove.
    #[test]
    fn the_url_alone_wires_a_box_up_and_the_token_is_only_codexs() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();

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

        // The URL arrives the way the host publishes it for the whole repo — a file in the store,
        // no token and no environment override anywhere. This is the chain end to end: the host
        // writes one file, and a box start turns it into that box's own user-scope setting, which
        // is the scope the plugin actually reads.
        fs::write(store.join("skein/sync/gateway"), "https://gw.test/mcp\n").unwrap();
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env_remove("SYNC_MCP_URL")
            .env_remove("SYNC_GATEWAY_URL")
            .env_remove("SYNC_AGENT_TOKEN")
            .output()
            .unwrap();
        assert!(out.status.success());

        let calls = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains("plugin install sync@sync"),
            "no token is not a reason to skip the plugin: {} / {calls}",
            String::from_utf8_lossy(&out.stderr)
        );
        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(boxhome.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["env"]["SYNC_MCP_URL"], "https://gw.test/mcp");
        // And Codex gets nothing rather than a `Bearer ` that 401s on first use — a registration
        // that looks complete is a worse place to find out than here.
        assert!(!boxhome.join(".codex/config.toml").exists());

        env::remove_var("SKEIN_HOME");
    }

    /// A box wired up before the plugin existed still gets it.
    ///
    /// This is the whole reason the plugin sits ahead of the stamp gate. The stamp means "this box
    /// owns its CLAUDE.md, memory and skill now", and re-asserting over it is what that gate exists
    /// to prevent — but the plugin is not a re-assertion, it is something upstream started shipping
    /// after these boxes were wired. Behind the gate it would have reached only boxes created from
    /// here on, which for an established fleet is none of them.
    #[test]
    fn an_already_wired_box_still_picks_up_the_plugin() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        // The box's own CLAUDE.md, as it has evolved since — untouched by anything below.
        fs::write(project.join("CLAUDE.md"), "# proj\n\nours\n").unwrap();

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

        // Wired up on some earlier day: the stamp is there, and so is the skill it installed.
        let boxhome = home.join("boxhome");
        let state = boxhome.join(".local/state/skein");
        fs::create_dir_all(&state).unwrap();
        let slug = project.display().to_string().replace('/', "-");
        fs::write(state.join(format!("sync-{slug}.done")), "").unwrap();

        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test/")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success());

        let calls = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains("plugin install sync@sync"),
            "a stamped box must still get the plugin: {calls}"
        );
        assert!(
            state.join("sync-plugin.done").is_file(),
            "and record it, so the next start is a file test rather than a subprocess"
        );
        // Everything the stamp protects is still untouched: the gate did its job for the things it
        // was guarding, and only the plugin came through ahead of it.
        assert_eq!(
            fs::read_to_string(project.join("CLAUDE.md")).unwrap(),
            "# proj\n\nours\n"
        );

        // Second start: the marker is believed, and `claude` is not asked again.
        fs::write(&log, "").unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test/")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            !fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("plugin install"),
            "installed once, then the box owns it — removing it must stay removed"
        );

        env::remove_var("SKEIN_HOME");
    }

    /// With the plugin installed and no Codex on the box, skein must NOT also write its own copy of
    /// the skill.
    ///
    /// Two copies of one skill is a fork, not a redundancy. The vendored copy is pinned to whatever
    /// upstream commit skein last pulled, so the first time an argument name changes the box holds
    /// two contradictory descriptions of the same tool with nothing to say which is older — and the
    /// skill's own advice is to trust the tool list over anything written in it, which is advice a
    /// stale copy gives just as confidently.
    #[test]
    fn the_plugins_skill_is_not_shadowed_by_a_vendored_copy() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();

        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("claude"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();

        // No `.codex/config.toml` and no `codex` on PATH — a Claude-only box, where the plugin is
        // the whole story.
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test/")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success());

        assert!(
            !store.join("skills/work-tracking/SKILL.md").exists(),
            "the plugin ships this skill and keeps it current; skein's pinned copy must not sit \
             beside it: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The two skein still owns, because the plugin cannot write either: the block is per-repo
        // and always in context, and the memory is in skein's own format.
        let claude_md = fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("## Work tracking"), "{claude_md}");
        assert!(store.join("memory/work-tracking.md").is_file());

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
            // Cleared explicitly, because this one is inherited from the *developer's* environment
            // rather than set by the fixture: Claude Code injects `env` from settings into every
            // subprocess it spawns, so a machine that has this variable set at all would otherwise
            // make the installer wire a box up here and the test would fail describing a bug that
            // does not exist.
            .env_remove("SYNC_MCP_URL")
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

    // A fleet box is not a sandbox, so `sbx ls` never mentions it. Until the placement records were
    // consulted here, a perfectly healthy fleet was simply absent from the board — and the sandbox
    // hosting it sat there instead, looking like a stale box nobody could attach to.
    #[test]
    fn the_board_shows_boxes_in_the_shared_sandbox_and_not_the_sandbox_itself() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        env::set_var("SKEIN_LS_CMD", "echo '[{\"name\":\"skein-fleet\"}]'");
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        record_place(
            "demo-task",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/home/agent".into(),
                tree: "/boxes/demo-task/tree".into(),
                sock: "/boxes/demo-task/session.sock".into(),
            },
        )
        .unwrap();

        let names: Vec<String> = load_views().unwrap().into_iter().map(|v| v.name).collect();
        assert!(
            names.iter().any(|n| n == "demo-task"),
            "a placed box must appear even though sbx has never heard of it: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "skein-fleet"),
            "the sandbox that hosts the boxes is not itself a box: {names:?}"
        );

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    // The board has to say which boxes still cost a whole VM, because nothing else on the row does:
    // a legacy box and a fleet box look and behave identically right up until the host runs out of
    // memory to reserve. It marks the exception, not the norm — the shared sandbox is the default.
    #[test]
    fn the_board_marks_a_box_that_still_owns_a_whole_vm() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        env::set_var(
            "SKEIN_LS_CMD",
            r#"echo '[{"name":"skein-fleet"},{"name":"old-box"}]'"#,
        );
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        record_place(
            "demo-task",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/boxes/demo-task/home".into(),
                tree: "/boxes/demo-task/tree".into(),
                sock: "/boxes/demo-task/session.sock".into(),
            },
        )
        .unwrap();

        let tagged = || -> Vec<(String, bool)> {
            load_views()
                .unwrap()
                .into_iter()
                .map(|v| (v.name, v.foreign))
                .collect()
        };
        let rows = tagged();
        assert!(
            rows.contains(&("old-box".into(), true)),
            "a sandbox skein did not place is not one of its boxes: {rows:?}"
        );
        assert!(
            rows.contains(&("demo-task".into(), false)),
            "a box skein placed is the ordinary case and carries no tag: {rows:?}"
        );

        // The flag is only half the feature: a row that carries it and a cockpit that ignores it look
        // identical from here, and that is how a tag silently stops appearing. The cockpit both hides
        // these rows by default and reveals them on `foreign:`, so it has to read the flag twice.
        let page = include_str!("web/index.html");
        assert!(
            page.contains("b.foreign"),
            "the cockpit no longer reads the flag, so foreign boxes would show as ordinary ones"
        );
        assert!(
            page.contains("foreign:"),
            "without the filter keyword there is no way to see them at all"
        );

        forget_place("demo-task");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    // Migration STOPS the old sandbox rather than destroying it — that is the undo. So `sbx ls`
    // goes on listing a stopped sandbox with the box's name long after the box itself moved into
    // the fleet and is running there. Read liveness off that row and every migrated box shows
    // `stale` on the board while working, which is exactly what happened: the board and the per-box
    // session view disagreed about the same box at the same moment, because only one of them asked
    // `box_liveness`.
    #[test]
    fn a_migrated_boxs_stopped_old_sandbox_does_not_make_it_look_dead() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(
            home.join("sandboxes.json"),
            format!(
                r#"{{"demo-task":{{"branch":"b","dir":"/boxes/demo-task/tree","lastSeen":"{}","status":"waiting"}}}}"#,
                secs_ago(20)
            ),
        )
        .unwrap();
        // The husk the migration left behind, still carrying the box's name.
        env::set_var(
            "SKEIN_LS_CMD",
            r#"echo '[{"name":"skein-fleet"},{"name":"demo-task","status":"stopped"}]'"#,
        );
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        record_place(
            "demo-task",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/boxes/demo-task/home".into(),
                tree: "/boxes/demo-task/tree".into(),
                sock: "/boxes/demo-task/session.sock".into(),
            },
        )
        .unwrap();

        // The fleet's own liveness sweep — the only thing that knows whether the box is running.
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{path}", bin.display()));
        let sweep = |answer: &str| {
            let p = bin.join("sbx");
            fs::write(&p, format!("#!/bin/sh\necho '{answer}'\n")).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        };
        let state_of = || {
            load_views()
                .unwrap()
                .into_iter()
                .find(|v| v.name == "demo-task")
                .expect("the migrated box must still be on the board")
                .state
        };

        sweep("demo-task 1");
        assert_eq!(
            state_of(),
            "waiting",
            "the box's own turn state, not the state of the sandbox it moved out of"
        );

        // And the reverse, so this is a fix rather than a suppression: when the box's session really
        // is gone, the board still says so.
        sweep("demo-task 0");
        assert_eq!(state_of(), "stale");

        env::set_var("PATH", path);
        forget_place("demo-task");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
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

        record_place(
            "thing-x",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 4242,
                home: "/home/agent".into(),
                tree: "/boxes/thing-x/tree".into(),
                sock: "/boxes/thing-x/session.sock".into(),
            },
        )
        .unwrap();
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();

        // Liveness comes from the SANDBOX, which is the only place the box's tmux server exists.
        // The pid in the record is deliberately not consulted: it belongs to the sandbox's pid
        // namespace, so checking it against the host's /proc asks about an unrelated process — and
        // on macOS, where there is no /proc, reported every running box as stopped.
        let fake = dir.join("bin");
        fs::create_dir_all(&fake).unwrap();
        let path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{path}", fake.display()));
        let sweep = |answer: &str| {
            use std::os::unix::fs::PermissionsExt;
            let p = fake.join("sbx");
            fs::write(&p, format!("#!/bin/sh\necho '{answer}'\n")).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        };

        sweep("thing-x 1");
        assert_eq!(
            box_liveness("thing-x"),
            Some(Liveness::Running),
            "the box's own tmux server IS its liveness — sbx ls knows nothing about a shared box"
        );

        // Dead session: stopped, not missing. The tree is still there to restart from.
        sweep("thing-x 0");
        assert_eq!(box_liveness("thing-x"), Some(Liveness::Stopped));

        // Sandbox stopped, or sbx silent: unknown, which must not be reported as stopped.
        sweep("");
        assert_eq!(box_liveness("thing-x"), None);

        env::set_var("PATH", path);
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
        // Placed, because an unplaced box has no argv: it is addressed through the record, and there
        // is no longer a fallback that treats the name as a sandbox.
        placed("thing-x");
        // attach opens the agent inside a persistent `skein-agent` tmux session so the live process
        // survives a disconnect; `claude --continue` is the (re)create command.
        let a = attach_argv("thing-x", "/d");
        assert_eq!(&a[..3], ["exec", "-it", "skein-fleet"]);
        assert!(a.last().unwrap().contains("new-session -d -s skein-agent"));
        assert!(a.last().unwrap().contains("claude --name"));
        assert!(a.last().unwrap().contains("--continue"));
        assert!(a.last().unwrap().contains("timeout 120 claude update"));
        assert!(
            a.last().unwrap().find("has-session").unwrap()
                < a.last().unwrap().find("timeout 120 claude update").unwrap(),
            "the updater must run only inside the missing-session branch"
        );
        assert!(a.last().unwrap().contains("tmux is required"));
        assert!(a.last().unwrap().contains("-u attach-session"));
        assert!(!a.last().unwrap().contains("else exec bash"));
        let first = initial_attach_argv_as("thing-x", "claude");
        assert!(first
            .last()
            .unwrap()
            .contains("new-session -d -s skein-agent"));
        // A fresh start, guarded — and carrying THIS box's name, resolved on the host. A leftover
        // `{box}` or a `$SKEIN_BOX` for the attach shell to expand would both name every box in the
        // sandbox the same thing, which is the failure `for_box` exists to prevent.
        assert!(first
            .last()
            .unwrap()
            .contains(r#""claude --name 'thing-x' ||"#));
        assert!(!first.last().unwrap().contains("{box}"));
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
        // The raw template, `{box}` unresolved — `for_box` fills it in at the call sites above,
        // where the name is known. Asserted as a template on purpose: the placeholder being here is
        // what gives `for_box` something to do, and its absence would silently un-name every box.
        assert_eq!(
            agent_resume_cmd("claude"),
            "claude --name '{box}' --continue || claude --name '{box}'"
        );
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
                < codex.last().unwrap().find("new-session").unwrap(),
            "Codex hooks must be installed before the resumed process starts"
        );
        assert!(codex.last().unwrap().contains("agent-guide.sh"));
        assert!(
            codex.last().unwrap().find("agent-guide.sh").unwrap()
                < codex.last().unwrap().find("new-session").unwrap(),
            "durable instructions must be installed before Codex starts"
        );
        // shell requires the same durable-session substrate; it never opens a reload-fragile shell.
        let sh = shell_argv("thing-x");
        assert_eq!(&sh[..3], ["exec", "-it", "skein-fleet"]);
        assert!(sh.last().unwrap().contains("new-session -d -s skein-shell"));
        assert!(sh.last().unwrap().contains("tmux is required"));
        assert!(sh.last().unwrap().contains("-u attach-session"));
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
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);

        assert!(box_write_argv("../escape", "/tmp/x", "/tmp/x/y").is_err());
        // An unplaced name is refused rather than addressed. It used to build `sbx exec -i thing-x`,
        // which is a sandbox name — true only under the per-VM model, and a guess for anything else.
        assert!(
            box_write_argv("thing-x", "/tmp/x", "/tmp/x/y").is_err(),
            "a box skein has not placed has nowhere for a write to land"
        );

        // A box is NOT a sandbox: it lives inside the shared one, so the write has to enter its
        // namespace. Addressing `sbx exec -i <box>` made every paste, drop and file pick fail with
        // "no sandbox named …" — surfaced in the browser as "attach failed", which points at the
        // terminal rather than at the upload.
        record_place(
            "thing-x",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 4242,
                home: "/boxes/thing-x/home".into(),
                tree: "/boxes/thing-x/tree".into(),
                sock: "/boxes/thing-x/session.sock".into(),
            },
        )
        .unwrap();
        let argv = box_write_argv(
            "thing-x",
            "/tmp/skein-drop-b1",
            "/tmp/skein-drop-b1/a b.pdf",
        )
        .unwrap();
        assert_eq!(&argv[..4], ["sbx", "exec", "-i", "skein-fleet"]);
        assert!(
            argv.iter().any(|a| a.contains("nsenter")),
            "the write has to land in the box's namespace, not the sandbox's: {argv:?}"
        );
        assert!(
            argv.last()
                .unwrap()
                .contains("mkdir -p '/tmp/skein-drop-b1' && cat > '/tmp/skein-drop-b1/a b.pdf'"),
            "a space in the name must survive quoting: {argv:?}"
        );
        forget_place("thing-x");
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn pct_decode_recovers_unicode_filenames() {
        assert_eq!(pct_decode("n%C3%A9e%20deed.pdf"), "née deed.pdf");
        assert_eq!(pct_decode("plain.txt"), "plain.txt");
        assert_eq!(pct_decode("100%"), "100%"); // dangling escape left verbatim
        assert_eq!(pct_decode("a%zz"), "a%zz");
    }

    #[test]
    fn boxes_sharing_one_sandbox_each_report_under_their_own_name() {
        // The regression that made the fleet's first migrated box show `stale` on the board while
        // it was visibly working: every probe keyed its signals on SANDBOX_VM_ID, which names the
        // VM. One box per VM made that an identity by accident; several boxes in one sandbox all
        // answer with the SAME string, so they overwrite one another's status and the board — which
        // looks up each box by name — finds nothing for any of them.
        //
        // Runs the installed script, not a Rust-side model of it, because the bug was in the shell.
        let _g = env_lock();
        let store_tmp = tempdir();
        let store = store_tmp.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let script = store.join("skein").join("bin").join("box-status.sh");
        let project_dir = store.parent().unwrap().to_path_buf();

        let report = |box_name: &str, mode: &str| {
            let out = Command::new("bash")
                .arg(&script)
                .arg(mode)
                .env("CLAUDE_PROJECT_DIR", &project_dir)
                // What both boxes agree on: they are in one sandbox, so this is the same for each.
                .env("SANDBOX_VM_ID", "skein-fleet")
                .env("SKEIN_BOX", box_name)
                .stdin(std::process::Stdio::null())
                .output()
                .expect("run box-status.sh");
            assert!(out.status.success(), "{box_name} {mode}");
            // A UserPromptSubmit hook's stdout is injected into the prompt, so it must stay empty.
            assert!(out.stdout.is_empty(), "{box_name} {mode} wrote to stdout");
        };
        report("alpha", "working");
        report("beta", "waiting");

        let status = |name: &str| -> serde_json::Value {
            let p = store.join("status").join(format!("{name}.json"));
            serde_json::from_str(
                &fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
            )
            .unwrap()
        };
        assert_eq!(status("alpha")["status"], "working");
        assert_eq!(status("beta")["status"], "waiting");
        assert!(
            !store.join("status").join("skein-fleet.json").exists(),
            "a box must never report under the name of the sandbox holding it"
        );
        // The heartbeat is per box too: hook health that pools every box into one log cannot say
        // WHICH box's probes have gone quiet, which is the only question it is asked.
        for name in ["alpha", "beta"] {
            assert!(
                store
                    .join("hook-log")
                    .join(format!("{name}.jsonl"))
                    .exists(),
                "{name} left no heartbeat"
            );
        }

        // A legacy box sets no SKEIN_BOX and is alone in its VM: there the VM name IS the box name,
        // and it has to keep working exactly as before.
        let out = Command::new("bash")
            .arg(&script)
            .arg("waiting")
            .env("CLAUDE_PROJECT_DIR", &project_dir)
            .env("SANDBOX_VM_ID", "old-style-box")
            .env_remove("SKEIN_BOX")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run box-status.sh");
        assert!(out.status.success());
        assert_eq!(status("old-style-box")["status"], "waiting");
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
                // See the mailbox round-trip test: the box wins over the VM, and leaving SKEIN_BOX
                // to be inherited files this turn's tokens under whichever box ran the test.
                .env("SKEIN_BOX", "boxA")
                .env("SANDBOX_VM_ID", "the-shared-sandbox")
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
