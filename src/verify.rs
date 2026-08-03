//! Does a box's work actually build and pass?
//!
//! Runs the repo's check command **inside the box** and keeps the result. Two deliberate
//! non-features: it never runs itself (a check is a real test suite burning cores on the user's
//! machine, and six boxes doing it at once is six suites competing with their own work), and a
//! pass never outlives its code — the fingerprint it recorded says when the result went stale.

use crate::config::*;
use crate::util::*;
use crate::{
    agent_for_box, box_liveness, classify_pane, fuse_status, read_pane, repo_for_box,
    sbx_guest_output, status_edge, store_for_box, valid_name, Liveness,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

/// Cap on the stored output. Enough to see which test failed and why; not enough to bloat a store
/// that syncs into every box.
pub(crate) const VERIFY_TAIL_BYTES: usize = 6000;

/// A check that hasn't finished in 15 minutes is a hang, not a slow suite.
pub(crate) const VERIFY_TIMEOUT: Duration = Duration::from_secs(900);

pub(crate) const VERIFY_FP: &str = "SKEIN_VERIFY_FP ";

pub(crate) const VERIFY_EXIT: &str = "SKEIN_VERIFY_EXIT ";

/// One recorded check, in `<store>/verify/<name>.json` — same shape and place as every other
/// per-box signal, so it survives a cockpit restart and is readable by anything else that wants it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRecord {
    /// what was run, verbatim — a green tick means nothing without it
    pub cmd: String,
    pub exit: i32,
    pub ok: bool,
    /// RFC3339, when the run finished
    pub ts: String,
    pub secs: u64,
    /// the box's HEAD + worktree checksum at the moment of the run: *what* was checked
    #[serde(default)]
    pub fingerprint: String,
    /// tail of the combined output (stdout+stderr interleaved, as a human would have seen it)
    #[serde(default)]
    pub tail: String,
}

/// What a fleet row shows: the outcome, and whether the box has moved on since.
#[derive(Debug, Clone, Serialize)]
pub struct VerifySummary {
    pub ok: bool,
    /// the box has ended a turn since this check ran — the result describes older work
    pub stale: bool,
    pub age: String,
    pub cmd: String,
}

/// The check command for a box: its repo's override, else the global default. None ⇒ unconfigured,
/// which is not an error — most repos won't have one until someone sets it.
pub fn verify_command(name: &str) -> Option<String> {
    let per_repo = repo_for_box(name)
        .map(|r| r.check.trim().to_string())
        .filter(|c| !c.is_empty());
    per_repo.or_else(|| {
        let global = load_config().check_command.trim().to_string();
        (!global.is_empty()).then_some(global)
    })
}

pub(crate) fn verify_path(name: &str) -> Option<PathBuf> {
    valid_name(name)
        .then(|| store_for_box(name))
        .flatten()
        .map(|store| store.join("verify").join(format!("{name}.json")))
}

/// The last recorded check for a box, if any.
pub fn read_verify(name: &str) -> Option<VerifyRecord> {
    let raw = fs::read_to_string(verify_path(name)?).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The row's version: outcome + whether the box has worked since. Staleness comes free from the
/// turn-state edge we already read — no second exec to re-fingerprint the tree, which is the whole
/// reason the check is worth doing at all.
pub(crate) fn verify_summary(name: &str) -> Option<VerifySummary> {
    let rec = read_verify(name)?;
    let at = DateTime::parse_from_rfc3339(&rec.ts).ok()?.timestamp();
    let moved = status_edge(name).map(|(_, ts)| ts).unwrap_or(0);
    let secs = (Utc::now().timestamp() - at).max(0);
    Some(VerifySummary {
        ok: rec.ok,
        stale: moved > at,
        age: ago(secs),
        cmd: rec.cmd,
    })
}

/// Split the guest's combined output into (fingerprint, exit code, what a human should read). The
/// check's own exit code can't come from the process status — `sbx exec` reports the wrapper
/// shell's — so the wrapper prints it on a marker line. A missing marker means the run never
/// reached the end (killed, timed out, box died mid-check), which is NOT a failing test and must
/// never be recorded as one.
pub(crate) fn parse_verify_output(raw: &str) -> (String, Option<i32>, String) {
    let mut fingerprint = String::new();
    let mut exit = None;
    let mut body = String::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix(VERIFY_FP) {
            fingerprint = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix(VERIFY_EXIT) {
            exit = rest.trim().parse().ok();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    (fingerprint, exit, body)
}

/// Why a verify must not start right now, if it must not. A check while the agent is mid-turn would
/// have the two of them writing the same tree — and a red result would be the collision, not the code.
pub(crate) fn verify_is_unsafe_now(name: &str) -> Option<String> {
    let agent = agent_for_box(name);
    let level = read_pane(name).map(|obs| (classify_pane(&agent, &obs), obs.ts));
    let (fused, _) = fuse_status(status_edge(name), level);
    let state = fused?;
    matches!(state.as_str(), "working" | "running" | "compacting").then(|| {
        format!("{name} is mid-turn ({state}) — verify when it stops, or the check and the agent fight over the same files")
    })
}

/// One verify at a time, fleet-wide. Not a queue: a second request is refused immediately and says
/// which box holds the slot, because silently queueing a 15-minute suite behind another is worse
/// than saying no. This is the guard that keeps "verify" from ever becoming a fork bomb of test runs.
pub(crate) static VERIFY_INFLIGHT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[derive(Debug)]
pub(crate) struct VerifyFlight;

impl VerifyFlight {
    pub(crate) fn take(name: &str) -> Result<Self, String> {
        let mut slot = VERIFY_INFLIGHT
            .lock()
            .map_err(|_| "verify lock poisoned".to_string())?;
        if let Some(other) = slot.as_deref() {
            return Err(if other == name {
                format!("{name} is already being verified")
            } else {
                format!("a verify is already running in {other} — one at a time, so checks don't fight your own work for cores")
            });
        }
        *slot = Some(name.to_string());
        Ok(VerifyFlight)
    }
}

impl Drop for VerifyFlight {
    fn drop(&mut self) {
        if let Ok(mut slot) = VERIFY_INFLIGHT.lock() {
            *slot = None;
        }
    }
}

/// Run the box's check command inside the box and record what happened. Blocking and slow by
/// nature (it is a test suite) — callers run it off the request thread.
pub fn run_verify(name: &str) -> Result<VerifyRecord, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let cmd = verify_command(name).ok_or_else(|| {
        "no check command for this box — set one in Settings → Workflow, or per repo".to_string()
    })?;
    if box_liveness(name) != Some(Liveness::Running) {
        return Err(format!("{name} is not running — start it before verifying"));
    }
    if let Some(reason) = verify_is_unsafe_now(name) {
        return Err(reason);
    }
    let _flight = VerifyFlight::take(name)?;
    // The wrapper: fingerprint what we're about to check, run the command with stderr folded in
    // (a failing suite says the useful part there), then report its exit code on a marker line.
    // The check runs in a SUBSHELL, not a brace group: a command containing `exit 1` — or any
    // `set -e` script — would otherwise exit the wrapper itself, taking the marker with it and
    // turning an honest failure into "the check never reported an exit code".
    let script = format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null)\"; [ -n \"$root\" ] && cd \"$root\"; \
         printf '{VERIFY_FP}%s\\n' \"$(git rev-parse --short HEAD 2>/dev/null)+$(git status --porcelain 2>/dev/null | cksum | tr -d ' ')\"; \
         ( {cmd} ) 2>&1; printf '{VERIFY_EXIT}%s\\n' \"$?\""
    );
    let started = std::time::Instant::now();
    let raw = sbx_guest_output(name, &script, VERIFY_TIMEOUT)?;
    let (fingerprint, exit, body) = parse_verify_output(&raw);
    let exit = exit.ok_or_else(|| {
        format!(
            "the check never reported an exit code — it was killed, or ran past the {}s limit",
            VERIFY_TIMEOUT.as_secs()
        )
    })?;
    let record = VerifyRecord {
        cmd,
        exit,
        ok: exit == 0,
        ts: Utc::now().to_rfc3339(),
        secs: started.elapsed().as_secs(),
        fingerprint,
        tail: tail_of(body.trim_end(), VERIFY_TAIL_BYTES),
    };
    let path = verify_path(name).ok_or("no store for this box")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    fs::write(
        &path,
        serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(record)
}
