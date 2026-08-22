//! The in-box probe scripts, and the hook wiring that makes a box report at all.
//!
//! skein ships these hook scripts and wires them into the store's `settings.json`, so a box reports
//! working / waiting / needs-input and its current task without the *repo* providing anything. The
//! store is linked into every box by the kit, so every box's agent loads these hooks. See
//! `docs/self-sufficient.md`.
//!
//! This is the highest-blast-radius write in the system — it edits a settings file the user also
//! edits, in every store, on every upgrade. Which is why the merge is additive and idempotent, why
//! it retires skein's own past entries by shape rather than by wholesale replacement, and why every
//! one of those rules has a test that would fail loudly rather than quietly overwrite someone.

use crate::kit::ensure_store;
use crate::registry::all_stores;
use crate::repos::{agent_for_box, load_repos};
use crate::runtime::runtime_adapter;
use crate::runtime::RUNTIME_ADAPTERS;
use crate::sandbox::sbx_guest_output;
use crate::util::sh_quote;
use crate::util::valid_name;
use crate::util::write_atomic;
use std::fs;
use std::path::Path;
use std::time::Duration;

// ---------- the turn-state probe (skein-owned, installed into the shared store) ----------
// skein ships these hook scripts and wires them into the store's settings.json, so a box reports
// working/waiting/needs-input + its current task without the *repo* providing anything. The store is
// linked into every box by the kit, so every box's Claude loads these hooks. See docs/self-sufficient.md.
/// Wires a box to the `sync` work tracker and installs the discipline for it. Lives in the store
/// rather than the kit on purpose: a kit only reaches boxes created after it changed, and an
/// existing box has to be wireable too. See `sync_provision_box`.
const SYNC_INSTALL_SH: &str = include_str!("store/sync-install.sh");
const SYNC_REFRESH_SH: &str = include_str!("store/sync-refresh.sh");
/// The documents that installer places, once a box is actually registered: the always-on rules, the
/// memory, and the on-demand skill for Plane's full surface.
///
/// The skill is three files because upstream's is, and `SKILL.md` links to the other two by name —
/// shipping it alone would hand a box a playbook with two dead ends in it. They are only used where
/// the sync *plugin* cannot go, which today means Codex: the plugin carries this same skill and
/// keeps it current, so a box with the plugin gets it from there instead of from a vendored copy
/// pinned to whatever commit skein last pulled.
const SYNC_BLOCK_MD: &str = include_str!("store/sync/work-tracking.block.md");
const SYNC_MEMORY_MD: &str = include_str!("store/sync/work-tracking.memory.md");
const SYNC_SKILL_MD: &str = include_str!("store/sync/work-tracking.skill.md");
const SYNC_ORGANISING_MD: &str = include_str!("store/sync/work-tracking.organising.md");
const SYNC_TROUBLESHOOTING_MD: &str = include_str!("store/sync/work-tracking.troubleshooting.md");

const PROBE_STATUS_SH: &str = include_str!("probe/box-status.sh");
// box-pane.sh: NOT hook-driven. Started detached by the attach command (see agent_attach_argv)
// and it outlives the attach, because the states it exists to catch — a crashed agent, a trust
// prompt before any session exists, a dialog dismissed with esc — are exactly the ones where no
// hook will ever fire. Its output is the level half of turn-state (see read_pane/classify_pane).
pub(crate) const PROBE_PANE_SH: &str = include_str!("probe/box-pane.sh");
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
        ("work-tracking.organising.md", SYNC_ORGANISING_MD),
        ("work-tracking.troubleshooting.md", SYNC_TROUBLESHOOTING_MD),
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
    publish_sync_gateway(store)?;

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

/// Publish the gateway this repo's boxes should use, as a file in the store.
///
/// A file rather than `env.SYNC_MCP_URL` in the store's `settings.json`, and that is a correction:
/// project-scope `env` does **not** reach the plugin's `.mcp.json` expansion. Measured in a box —
/// with the variable set only in project settings and stripped from the environment, the plugin
/// still resolved to the default gateway compiled into it. User scope does work, so the arrangement
/// is: the host publishes the URL here, and `sync-install.sh` reads it on box start and writes it
/// into that box's own `~/.claude/settings.json`.
///
/// The effect is the same one write per repo — every box of it picks the URL up by starting, and
/// boxes that do not exist yet get it too — but it rests on a mechanism that was tested rather than
/// one that reads plausibly.
///
/// The URL is not a credential; it is the string the settings screen already shows. That is why it
/// can live in a store shared by every box of the repo, while the OAuth grant that authenticates
/// against it stays box-private.
///
/// No connection ⇒ the file is *removed*: a repo unwired from its tracker must stop pointing boxes
/// at it, and a stale URL here would outlive the decision to disconnect.
fn publish_sync_gateway(store: &Path) -> Result<(), String> {
    // The repo is found from the store rather than passed in, because every caller of this already
    // has the store and only some of them have the repo.
    let gateway = load_repos()
        .into_iter()
        .find(|r| Path::new(&r.store) == store)
        .and_then(|r| crate::tracking::connection_for_repo(&r))
        .map(|c| c.gateway_url)
        .filter(|u| !u.trim().is_empty())
        .map(|u| crate::tracking::sync_mcp_url(&u));

    let dir = store.join("skein").join("sync");
    let path = dir.join("gateway");
    match gateway {
        Some(url) => {
            fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
            write_atomic(&path, &dir, format!("{url}\n").as_bytes())
        }
        None => match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("removing {}: {e}", path.display())),
        },
    }
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
    let entries: [(&str, String, Option<&str>); 24] = [
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
        ("SessionStart", format!("{PROBE_STATUS_CMD} started"), None),
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
    // The pre-`skein/` store layout: probes lived directly under `<store>/bin/`, so a store
    // provisioned back then still wires `$CLAUDE_PROJECT_DIR/.claude/bin/…` — a path that stopped
    // existing when the store grew a `skein/` subdirectory. Additive merging keeps it *beside* the
    // correct entry, so the box works perfectly and announces a failure at every session start:
    //   SessionStart:resume hook error … /…/.claude/bin/sandbox-bootstrap.sh: not found
    // Seen on a box whose store predates the rename. Retire both spellings, since a store old enough
    // to have the old path may carry either the bare or the bash-wrapped form.
    for (event, legacy) in entries
        .iter()
        .filter_map(|(ev, cmd, _)| {
            let old = cmd.replace("/.claude/skein/bin/", "/.claude/bin/");
            (old != *cmd).then_some((*ev, old))
        })
        .collect::<Vec<_>>()
    {
        obsolete.push((event, wire(&legacy)));
        obsolete.push((event, legacy));
    }

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
    // NOT `crossSessionInbound` — deliberately, and this is where it was tried first.
    //
    // This file is the project store, which is REPO scope, and a repository's settings can only ever
    // *tighten* that setting: "your own 'accept' cannot override a repo tightening", in Claude
    // Code's own words. So `accept` here grants nothing, while looking exactly like it does — a real
    // message to a fleet box expired unapproved with this in place. It is set on each box's own
    // settings by `box-session.sh` instead, which is user scope, next to the registry share that
    // makes the box findable in the first place.
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
    // Same reason as the Claude table: SessionEnd records `ended`, and without this nothing ever
    // unrecords it — a restarted box reads "ended" while its new session waits at the prompt. The
    // script itself only clears that one state, so the `compact` source stays truthful.
    add(
        "SessionStart",
        Some("startup|resume|clear|compact"),
        command("SessionStart", "box-status.sh", "started"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use crate::util::sh_quote;
    use std::process::{Command, Stdio};

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
        // bootstrap + handoff context, and the turn-state clear: SessionEnd writes `ended`, and a
        // session that has just started is not one that ended.
        let starts = merged["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            starts.len(),
            3,
            "SessionStart needs bootstrap + handoff + started"
        );
        assert!(
            starts.iter().any(|h| h["hooks"][0]["command"]
                .as_str()
                .unwrap_or_default()
                .contains("box-status.sh\" started")),
            "without it a restarted box reads `ended` while waiting at its prompt: {starts:?}"
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
    fn settings_with_probe_retires_the_pre_skein_store_layout() {
        // Probes used to live directly under `<store>/bin/`. A store provisioned then still wires
        // `$CLAUDE_PROJECT_DIR/.claude/bin/…`, which has not existed since the store grew a
        // `skein/` subdirectory — and because merging is additive it sat *beside* the correct entry,
        // so the box worked perfectly and announced a hook failure at every session start:
        //   SessionStart:resume hook error … /…/.claude/bin/sandbox-bootstrap.sh: not found
        let legacy = BOOTSTRAP_CMD.replace("/.claude/skein/bin/", "/.claude/bin/");
        assert!(legacy.contains("/.claude/bin/"), "the rename this retires");
        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    // bare and bash-wrapped: a store old enough to have this may carry either
                    { "hooks": [ { "type": "command", "command": legacy } ] },
                    { "hooks": [ { "type": "command", "command": format!("bash \"{legacy}\"") } ] },
                    { "hooks": [ { "type": "command", "command": "my-own-start-hook.sh" } ] }
                ]
            }
        });
        let merged = settings_with_probe(&existing);
        let cmds: Vec<&str> = merged["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["hooks"][0]["command"].as_str().unwrap_or(""))
            .collect();
        assert!(
            !cmds.iter().any(|c| c.contains("/.claude/bin/")),
            "a path that no longer exists must not stay wired: {cmds:?}"
        );
        assert!(
            cmds.iter().any(|c| c.contains(BOOTSTRAP_CMD)),
            "and the current one must be there: {cmds:?}"
        );
        assert!(
            cmds.contains(&"my-own-start-hook.sh"),
            "a hook skein did not write is not skein's to retire: {cmds:?}"
        );
    }

    /// A box that failed to install its plugins tries again; a box that succeeded does not.
    ///
    /// The marker used to be written unconditionally, and every step in that block is `|| true`. So
    /// a box that could not reach the marketplace — no network yet, an unauthenticated agent, a slow
    /// first boot — finished having installed nothing and was marked done permanently. Verified in
    /// this fleet: the marker present since Aug 4, and `claude plugin list` empty.
    ///
    /// It also recorded only *that* it ran, never *what* was wanted, so enabling a plugin later
    /// reached no box that already existed. That half has not bitten yet only because nothing has
    /// been enabled — it would have, on the first one.
    #[test]
    fn a_box_retries_the_plugins_it_failed_to_install() {
        use std::os::unix::fs::PermissionsExt;
        let block: String = BOOTSTRAP_SH
            .lines()
            .skip_while(|l| !l.starts_with("marker="))
            .take_while(|l| !l.starts_with("# --- surface unread"))
            .collect::<Vec<_>>()
            .join("\n");
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let home = root.join("home");
        let store = root.join("store");
        let bin = root.join("bin");
        for d in [&home.join(".claude"), &store, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            store.join("settings.json"),
            r#"{"enabledPlugins":{"beta@market":true,"alpha@market":true,"off@market":false}}"#,
        )
        .unwrap();

        // A `claude` whose installs either work or silently do not — which is the whole question
        // here, since the real one fails exactly that quietly.
        let fake_claude = |installs: bool| {
            let script = format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n\
                 'plugin list') cat {listed} 2>/dev/null || true ;;\n\
                 'plugin install') {act} ;;\n\
                 *) : ;;\nesac\nexit 0\n",
                listed = root.join("listed").display(),
                act = if installs {
                    format!("echo \"$3\" >> {}", root.join("listed").display())
                } else {
                    ":".to_string()
                },
            );
            std::fs::write(bin.join("claude"), script).unwrap();
            std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        };
        let marker = home.join(".claude/.skein-plugins-materialized");
        let run = || {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "set -uo pipefail\nexport HOME={home}\nexport PATH={bin}:$PATH\nstore={store}\n\
                     {block}\nwait\n",
                    home = home.display(),
                    bin = bin.display(),
                    store = store.display(),
                ))
                .output()
                .expect("bash to run the bootstrap's plugin block");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            std::fs::read_to_string(&marker).unwrap_or_default()
        };

        // Installs that quietly do nothing must leave no marker, or this box never tries again.
        fake_claude(false);
        assert_eq!(
            run(),
            "",
            "a box that installed nothing was marked done for good"
        );
        assert!(
            !marker.exists(),
            "the marker was written despite installing nothing"
        );

        // Now they work. The marker records the set, sorted, and only the enabled ones.
        fake_claude(true);
        let done = run();
        assert_eq!(done, "alpha@market beta@market ", "recorded: {done:?}");
        assert!(
            !done.contains("off@market"),
            "a disabled plugin was installed anyway"
        );

        // Same set again is a no-op — no reinstalling on every single box start.
        std::fs::remove_file(root.join("listed")).unwrap();
        fake_claude(false);
        assert_eq!(
            run(),
            "alpha@market beta@market ",
            "a settled box did the work again"
        );

        // Enabling one more must reach a box that already exists. This is the half that would have
        // bitten on the first plugin ever enabled: the old marker said "done" and meant it forever.
        std::fs::write(
            store.join("settings.json"),
            r#"{"enabledPlugins":{"beta@market":true,"alpha@market":true,"gamma@market":true}}"#,
        )
        .unwrap();
        fake_claude(true);
        assert_eq!(
            run(),
            "alpha@market beta@market gamma@market ",
            "a newly enabled plugin never reached an existing box"
        );
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
        // The plugin's copy, which is where this skill lives now that upstream ships one. All three
        // files, because `SKILL.md` links to the other two by name — a drift check on the entry
        // point alone would pass while the box followed a link to a stale page.
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("upstream/sync/plugin/skills/work-tracking");
        for (theirs, ours, name) in [
            (
                dir.join("SKILL.md"),
                SYNC_SKILL_MD,
                "work-tracking.skill.md",
            ),
            (
                dir.join("organising.md"),
                SYNC_ORGANISING_MD,
                "work-tracking.organising.md",
            ),
            (
                dir.join("troubleshooting.md"),
                SYNC_TROUBLESHOOTING_MD,
                "work-tracking.troubleshooting.md",
            ),
        ] {
            let Ok(upstream) = fs::read_to_string(&theirs) else {
                // A checkout without `--recursive`. Not a failure — the vendored copies are complete
                // on their own — but say so, because a guard that quietly checks nothing is worse
                // than none.
                eprintln!(
                    "skipping drift check: {} is absent — run `git submodule update --init`",
                    theirs.display()
                );
                return;
            };
            assert_eq!(
                ours,
                upstream,
                "the vendored {name} has drifted from upstream/sync. Do not edit the copy: change \
                 it in the sync repo, then `cp {} src/store/sync/{name}` and update the commit in \
                 src/store/sync/UPSTREAM.md",
                theirs.display()
            );
        }
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

    /// The launcher exports `SKEIN_TMUX_SOCK`; the observer has to read it.
    ///
    /// It did not, and nothing said so: `pane_observer_start` set the variable, `box-pane.sh` used
    /// bare `tmux`, and in a fleet box that reaches the sandbox's default socket — which does not
    /// exist. The first tick got `error connecting to /tmp/tmux-1000/default`, read the empty answer
    /// as "the agent's window is gone", wrote one dead observation and exited. Every fleet box then
    /// had a stale sample and the board said "screen lost" for all of them, permanently.
    ///
    /// A contract between a shell string and a shell script has no compiler behind it, so this is
    /// the only thing that can hold the two halves together.
    #[test]
    fn the_screen_observer_reads_the_socket_its_launcher_exports() {
        let launcher =
            crate::runtime::pane_observer_start("skein-agent", "/boxes/web-main/session.sock");
        assert!(
            launcher.contains("SKEIN_TMUX_SOCK='/boxes/web-main/session.sock'"),
            "the launcher must name the box's own tmux server: {launcher}"
        );
        assert!(
            PROBE_PANE_SH.contains("SKEIN_TMUX_SOCK"),
            "box-pane.sh must READ what the launcher exports, or it talks to the wrong tmux server \
             and reports every box's agent as gone"
        );
        assert!(
            PROBE_PANE_SH.contains("tmux -S \"$SKEIN_TMUX_SOCK\""),
            "and it must use it as tmux's socket, not merely mention it"
        );
        // A box that IS its own sandbox has no socket, and must keep the bare call.
        let alone = crate::runtime::pane_observer_start("skein-agent", "");
        assert!(
            !alone.contains("SKEIN_TMUX_SOCK"),
            "a legacy box's tmux is the only one there: {alone}"
        );
    }

    /// Linux only: runs Codex's `interactive_setup`, which uses `sed -i` with no argument. That is
    /// GNU's spelling and it is the right one — the script runs inside the box, never on the host
    /// — but BSD `sed` reads the FILENAME as the script and fails with `invalid command code`.
    #[cfg(target_os = "linux")]
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

    /// Linux only, for the reason above: it reads the config the same `sed -i` writes.
    #[cfg(target_os = "linux")]
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

    /// Linux only: drives `box-token-usage.sh` as a script, in the userland it is installed into.
    #[cfg(target_os = "linux")]
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
}
