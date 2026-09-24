//! The in-box probe scripts, and the hook wiring that makes a box report at all.
//!
//! skein ships these hook scripts so a box reports working / waiting / needs-input and its current
//! task without the *repo* providing anything. Neither the hooks nor the scripts they run are in the
//! store, which every box of the repo can write: both load from skein's read-only plugin — the
//! wiring since SKEIN-1062 ([`turn_state_hooks`]), the scripts since SKEIN-1144
//! ([`plugin_install`]). The store still gets copies, for the kit and for an agent that runs
//! `mailbox.sh` by hand; no hook runs them. See `docs/self-sufficient.md`.
//!
//! This is still the highest-blast-radius write in the system — it edits a settings file the user
//! also edits, in every store, on every upgrade, to take skein's past hooks back out and to keep the
//! `tui` and `statusLine` defaults. Which is why it retires skein's own past entries by shape rather
//! than by wholesale replacement, and why every one of those rules has a test that would fail loudly
//! rather than quietly overwrite someone.

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
// skein ships these hook scripts into the store and into its plugin, so a box reports
// working/waiting/needs-input + its current task without the *repo* providing anything. The hooks
// load from skein's plugin (`turn_state_hooks`) and run the plugin's copies (`plugin_install`).
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
// default status line. Copies live in `<store>/skein/bin/` (skein-owned namespace), refreshed each
// run; what a box RUNS is the plugin's copy under `.skein` ([`plugin_install`], SKEIN-1144/1149).
const BOOTSTRAP_SH: &str = include_str!("store/sandbox-bootstrap.sh");
const SHARED_HOME_SH: &str = include_str!("store/shared-home.sh");
const SHARED_HOME_GUIDE: &str = include_str!("store/SHARED-HOME.md");
const AGENT_GUIDE_SH: &str = include_str!("store/agent-guide.sh");
const INSTALL_CODEX_HOOKS_SH: &str = include_str!("store/install-codex-hooks.sh");
const MAILBOX_SH: &str = include_str!("store/mailbox.sh");
const STATUSLINE_SH: &str = include_str!("store/statusline-command.sh");
// Box-side path of the store's copies of the scripts (the store is linked at `<clone>/.claude`).
// **No hook runs these any more** (SKEIN-1144): they are the commands a past skein wired, kept so
// that [`store_settings`] can retire them from a store in exactly the spelling it wrote. The hooks
// run the plugin's copies instead — see [`PLUGIN_PROBE`].
const PROBE_STATUS_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-status.sh";
const PROBE_TASK_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-task.sh";
const PROBE_DIFF_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-diff.sh";
const PROBE_JOURNAL_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-journal.sh";
const PROBE_TOKEN_USAGE_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-token-usage.sh";
const PROBE_HANDOFF_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-handoff.sh";
const PROBE_SESSION_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/box-session.sh";
const BOOTSTRAP_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/sandbox-bootstrap.sh";
/// The status line a past skein set as the store's default, running the store's copy of the
/// renderer. Kept only so [`store_settings`] can move exactly this string to [`statusline_cmd`]
/// (SKEIN-1149); a status line a person set is any other string, and stays as it is.
const STORE_ERA_STATUSLINE_CMD: &str =
    "bash $CLAUDE_PROJECT_DIR/.claude/skein/bin/statusline-command.sh";
// mailbox.sh hook entries — turn-boundary delivery so mail is re-checked every turn, not just at
// SessionStart. `inbox` (UserPromptSubmit) surfaces unread mail as additional context at the start
// of a turn; `stop-check` (Stop) blocks the stop with exit 2 + stderr if mail arrived mid-turn, so a
// message can never sit unread just because nobody happened to ask. Both mark seenBy on delivery,
// so the same message can't fire twice.
const MAILBOX_INBOX_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/mailbox.sh inbox";
const MAILBOX_STOPCHECK_CMD: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/mailbox.sh stop-check";

/// Where every command above pointed: the store's `skein/bin/`, which every box of the repo can
/// write (docs/threat-model.md, "its own repo's store").
const STORE_PROBE: &str = "$CLAUDE_PROJECT_DIR/.claude/skein/bin/";
/// Where the turn-state hooks' scripts are now (SKEIN-1144): a `probe/` directory inside the plugin
/// that carries the hooks, under the fleet root's `.skein`, which the launcher binds read-only into
/// every box. `${CLAUDE_PLUGIN_ROOT}` is Claude Code's name for whichever variant loaded, so each
/// variant runs its own copy. Not the plugin's `bin/`, which is `skein-resources` and `skein-mcp`.
const PLUGIN_PROBE: &str = "${CLAUDE_PLUGIN_ROOT}/probe/";
/// The directory [`PLUGIN_PROBE`] names, relative to a plugin variant's root.
const PLUGIN_PROBE_DIR: &str = "probe";

/// The turn-state variant's [`PLUGIN_PROBE_DIR`] as a box names it from anything that is not a
/// Claude plugin hook, and so has no `${CLAUDE_PLUGIN_ROOT}`: Codex's hooks, the attach shell, the
/// status line, the tracker's installer. By the fleet root the launcher passes every box, under the
/// `.skein` it binds read-only there. That variant, and not the full one, because it is installed
/// whatever the fleet's switch says.
pub(crate) fn box_probe_dir() -> String {
    format!(
        "${{SKEIN_FLEET_ROOT:-/boxes}}/.skein/{}/{PLUGIN_PROBE_DIR}",
        crate::runtime::TURN_STATE_PLUGIN
    )
}

/// skein's default status line: the plugin's copy of the renderer (SKEIN-1149), not the store's,
/// which every box of the repo can write.
fn statusline_cmd() -> String {
    format!("bash \"{}/statusline-command.sh\"", box_probe_dir())
}

/// What a box runs from skein when it starts rather than from a hook (SKEIN-1149): the helpers the
/// kit runs (`shared-home.sh` and `agent-guide.sh`, which [`plugin_probe_scripts`] already carries,
/// and these), the ones the attach shell and the tracker run, and the status line. Installed into
/// the turn-state variant's [`PLUGIN_PROBE_DIR`] beside [`CODEX_HOOKS_JSON`], the wiring
/// `install-codex-hooks.sh` reads from its own directory. A helper left in the store is one a
/// sibling box can rewrite and this box then runs as it starts.
/// `every_start_helper_runs_from_the_read_only_plugin` holds every caller to this list.
fn start_helpers() -> [(&'static str, &'static str); 5] {
    [
        ("install-codex-hooks.sh", INSTALL_CODEX_HOOKS_SH),
        ("sync-install.sh", SYNC_INSTALL_SH),
        ("sync-refresh.sh", SYNC_REFRESH_SH),
        ("statusline-command.sh", STATUSLINE_SH),
        ("box-pane.sh", PROBE_PANE_SH),
    ]
}

/// Every shell skein runs in a box, as it starts or as it is attached to, that runs one of
/// [`start_helpers`] by path — each with what it is — except the kit's provisioning script, which
/// is a file of its own (`kit/skein-startup.sh`). The production strings themselves, so that
/// `every_start_helper_runs_from_the_read_only_plugin` and the isolation test that runs them in a
/// box are reading what a box is given rather than a copy.
pub fn start_invocations() -> Vec<(&'static str, String)> {
    let codex = crate::runtime::resolve_runtime("codex");
    vec![
        ("Codex's setup", codex.interactive_setup.to_string()),
        (
            "the attach shell's guide refresh",
            crate::runtime::agent_instruction_setup(codex),
        ),
        (
            "the attach shell's screen observer",
            crate::runtime::pane_observer_start("skein-agent", ""),
        ),
        (
            "the tracker's install",
            crate::tracking::SYNC_INSTALL_IN_BOX.to_string(),
        ),
        (
            "the tracker's refresh",
            crate::tracking::sync_refresh_in_box(false),
        ),
        ("the status line skein sets", statusline_cmd()),
    ]
}

/// Codex's generated hooks, which `install-codex-hooks.sh` merges into a box's `~/.codex/hooks.json`.
/// Beside the installer in the read-only plugin (SKEIN-1149); it used to be the store's
/// `skein/codex-hooks.json`, which a sibling box could rewrite.
const CODEX_HOOKS_JSON: &str = "codex-hooks.json";

/// The scripts installed into each plugin variant's [`PLUGIN_PROBE_DIR`]: every script a turn-state
/// hook runs, Claude's or Codex's, and every script one of those runs in turn. A script left out of
/// this list and run by one that is in it is a script a sibling box can still rewrite, which is why
/// the siblings are here: `sandbox-bootstrap.sh` runs `shared-home.sh`, `agent-guide.sh` and
/// `mailbox.sh` from its own directory, and `box-codex-hook.sh` runs the probe it is named from its
/// own directory. `plugin_probe_scripts_are_every_script_a_hook_runs` holds the list to that.
fn plugin_probe_scripts() -> [(&'static str, &'static str); 14] {
    [
        ("box-status.sh", PROBE_STATUS_SH),
        ("box-task.sh", PROBE_TASK_SH),
        ("box-diff.sh", PROBE_DIFF_SH),
        ("box-journal.sh", PROBE_JOURNAL_SH),
        ("box-token-usage.sh", PROBE_TOKEN_USAGE_SH),
        ("box-handoff.sh", PROBE_HANDOFF_SH),
        ("box-session.sh", PROBE_SESSION_SH),
        ("sandbox-bootstrap.sh", BOOTSTRAP_SH),
        ("shared-home.sh", SHARED_HOME_SH),
        ("agent-guide.sh", AGENT_GUIDE_SH),
        ("mailbox.sh", MAILBOX_SH),
        ("box-codex-hook.sh", PROBE_CODEX_HOOK_SH),
        ("box-codex-task.sh", PROBE_CODEX_TASK_SH),
        ("box-codex-telemetry.sh", PROBE_CODEX_TELEMETRY_SH),
    ]
}

/// Content-derived revision for lifecycle *wiring* loaded when an agent starts. Probe scripts live
/// on the shared mount and update in place, so hashing their bodies made harmless docs/implementation
/// edits demand agent restarts. Only generated Claude/Codex hook configuration belongs here — the
/// store's settings, the plugin's turn-state hooks and Codex's, since a box loads each of them only
/// when its agent starts.
fn probe_revision() -> String {
    let mut hash = 0xcbf29ce484222325u64;
    let claude = serde_json::to_vec(&store_settings(&serde_json::json!({}))).unwrap_or_default();
    let plugin = serde_json::to_vec(&turn_state_hooks()).unwrap_or_default();
    let codex = serde_json::to_vec(&codex_hooks_with_probe()).unwrap_or_default();
    for body in [&claude, &plugin, &codex] {
        for byte in body {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

/// Install skein's turn-state probe into the shared store: write the hook scripts to
/// `<store>/skein/bin/`, and take skein's own hooks back **out** of `<store>/settings.json`
/// ([`store_settings`]: the repo's own hooks are preserved, re-runs change nothing). The hooks come
/// from skein's plugin ([`turn_state_hooks`]) and run the plugin's copies of these scripts, not
/// these (SKEIN-1144); the store's copies are for the kit and the agent's own use.
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
///
/// **Refuses, and changes nothing, over a `settings.json` that is there and will not parse.** An
/// unparseable settings file is still every setting the person put in it; writing skein's hooks
/// over it would be the only irreversible thing in this function.
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
    // The store's `settings.json` is the one file skein writes that a person also edits by hand,
    // and this line runs over every store at every server start. So the retire goes through
    // [`crate::util::update_json`]: the read, the merge and the write happen under one lock, and a
    // file that is **there and will not parse** is refused rather than read as `{}` and written
    // over. `fs::read_to_string(..).ok().and_then(from_str(..).ok()).unwrap_or_else(json!({}))` is
    // exactly the idiom SKEIN-347/359 turned the rest of the tree around on — see `update_json`'s
    // own doc — and this site kept it: one trailing comma in somebody's settings, or the
    // zero-length file a crash between `write_atomic`'s write and its rename leaves, and the next
    // server start replaced the lot with skein's hooks and nothing else.
    //
    // A store nobody has written settings for still gets skein's `tui` and `statusLine` defaults:
    // `read_json_or_why` answers `Ok(None)` for an absent file, `Value::default()` is `Null`, and
    // `store_settings` already normalises a non-object to `{}` (see its `!out.is_object()`
    // branch). Only "there and unreadable" refuses.
    let settings = store.join("settings.json");
    crate::util::update_json::<serde_json::Value, ()>(&settings, |current| {
        let merged = store_settings(current);
        *current = merged;
        Ok(())
    })?;
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

    // Codex's generated hooks are NOT written here any more (SKEIN-1149): a store's copy is one a
    // sibling box can rewrite, and the installer merged whatever it found into this box's
    // `~/.codex/hooks.json`. They ship in skein's read-only plugin instead ([`plugin_install`]),
    // beside the installer that reads them. A store written before still holds a copy; nothing
    // reads it.
    Ok(())
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

/// skein's 24 turn-state hooks, as (event, command, optional matcher), before [`wire`] wraps each
/// command. **They load from skein's plugin, not from the store's `settings.json`** (SKEIN-1062,
/// box-plugin §2.1): [`turn_state_hooks`] is the plugin's `hooks/hooks.json`, and every box's argv
/// names a plugin that carries them whatever the fleet's switch says ([`crate::runtime::for_box`]).
/// The store's `settings.json` is writable by every box of the repo, so a box could delete a
/// sibling's hooks there; it cannot unload a `--plugin-dir` the read-only launcher passes.
///
/// Each command runs the plugin's own copy of its script ([`PLUGIN_PROBE`], SKEIN-1144): this is
/// [`store_era_entries`] with the store's `skein/bin/` swapped for it, so the two tables cannot
/// disagree about anything but where the script is.
fn turn_state_entries() -> [(&'static str, String, Option<&'static str>); 24] {
    store_era_entries().map(|(event, cmd, matcher)| {
        let script = cmd
            .strip_prefix(STORE_PROBE)
            .expect("every store-era command runs a script from the store's skein/bin/");
        (event, format!("{PLUGIN_PROBE}{script}"), matcher)
    })
}

/// The same 24 hooks as a past skein wired them, each running the **store's** copy of its script.
/// Nothing runs these now: the table is what [`store_settings`] retires from a store provisioned
/// before the move, in every spelling a past skein wrote, and what [`turn_state_entries`] is
/// derived from.
fn store_era_entries() -> [(&'static str, String, Option<&'static str>); 24] {
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
    // learned this lesson first (its command was already bash-prefixed); now it's uniform.
    //
    // These are the store-era strings; [`turn_state_entries`] points each at the plugin's copy.
    [
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
    ]
}

/// `bash "<script>" <args>` — see the note in [`turn_state_entries`].
fn wire(cmd: &str) -> String {
    match cmd.split_once(' ') {
        Some((script, args)) => format!("bash \"{script}\" {args}"),
        None => format!("bash \"{cmd}\""),
    }
}

/// The turn-state plugin's `hooks/hooks.json`: every entry of [`turn_state_entries`], wired, in
/// table order within each event. Installed by `fleet::install_launcher` through
/// [`plugin_install`], into both of the plugin's variants.
pub(crate) fn turn_state_hooks() -> serde_json::Value {
    hooks_json(turn_state_entries())
}

/// A `hooks.json` value from a table of (event, command, optional matcher), wired.
fn hooks_json(entries: [(&'static str, String, Option<&'static str>); 24]) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let mut hooks = Map::<String, Value>::new();
    for (event, cmd, matcher) in entries {
        let mut entry = json!({ "hooks": [ { "type": "command", "command": wire(&cmd) } ] });
        if let Some(m) = matcher {
            entry
                .as_object_mut()
                .expect("hook entry is an object")
                .insert("matcher".into(), Value::String(m.to_string()));
        }
        hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("hook event is an array")
            .push(entry);
    }
    json!({ "hooks": hooks })
}

/// Every file of both plugin variants as `fleet::install_launcher` writes them: (absolute path,
/// bytes). The full plugin is [`crate::runtime::PLUGIN_FILES`] with the turn-state hooks added to its
/// `hooks/hooks.json`; the narrow one is the same manifest with the turn-state hooks alone. Both
/// sets of hooks come from [`turn_state_hooks`], so the two cannot drift apart. The directories
/// are [`crate::runtime`]'s, because the argv names them; the bytes are this module's, because the
/// hooks are.
///
/// Each variant also carries the scripts its hooks run, under [`PLUGIN_PROBE_DIR`] (SKEIN-1144),
/// so that what a turn-state hook executes is as read-only as the hook itself.
pub(crate) fn plugin_install() -> Vec<(String, String)> {
    plugin_install_under(&crate::util::fleet_root())
}

/// [`plugin_install`] under a fleet root the caller names — the isolation tests install the plugin
/// into a fixture fleet with this, and run its hooks from there.
pub fn plugin_install_under(fleet_root: &str) -> Vec<(String, String)> {
    let turn_state = turn_state_hooks();
    let pretty = |v: &serde_json::Value| {
        serde_json::to_string_pretty(v).expect("a hooks value serialises") + "\n"
    };
    let full = crate::runtime::plugin_dir_under(fleet_root);
    let mut out: Vec<(String, String)> = crate::runtime::PLUGIN_FILES
        .iter()
        .map(|(rel, body)| {
            let body = match *rel {
                "hooks/hooks.json" => pretty(&with_turn_state(body, &turn_state)),
                _ => body.to_string(),
            };
            (format!("{full}/{rel}"), body)
        })
        .collect();
    let narrow = crate::runtime::turn_state_plugin_dir_under(fleet_root);
    for (rel, body) in crate::runtime::PLUGIN_FILES {
        if *rel == ".claude-plugin/plugin.json" {
            out.push((format!("{narrow}/{rel}"), body.to_string()));
        }
    }
    out.push((format!("{narrow}/hooks/hooks.json"), pretty(&turn_state)));
    for dir in [&full, &narrow] {
        for (file, body) in plugin_probe_scripts() {
            out.push((format!("{dir}/{PLUGIN_PROBE_DIR}/{file}"), body.to_string()));
        }
    }
    // What a box runs as it starts, and the Codex wiring (SKEIN-1149): the turn-state variant only,
    // because that is the one [`box_probe_dir`] names.
    for (file, body) in start_helpers() {
        out.push((
            format!("{narrow}/{PLUGIN_PROBE_DIR}/{file}"),
            body.to_string(),
        ));
    }
    out.push((
        format!("{narrow}/{PLUGIN_PROBE_DIR}/{CODEX_HOOKS_JSON}"),
        pretty(&codex_hooks_with_probe()),
    ));
    out
}

/// A plugin `hooks.json` with every turn-state entry appended after its own, event by event.
fn with_turn_state(own: &str, turn_state: &serde_json::Value) -> serde_json::Value {
    let mut merged: serde_json::Value =
        serde_json::from_str(own).expect("the plugin's own hooks.json parses");
    let into = merged["hooks"]
        .as_object_mut()
        .expect("the plugin's hooks.json has a hooks object");
    for (event, groups) in turn_state["hooks"].as_object().into_iter().flatten() {
        into.entry(event.clone())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .expect("a hook event is an array")
            .extend(groups.as_array().into_iter().flatten().cloned());
    }
    merged
}

/// The store's `settings.json`, with **no skein hooks in it**: every hook command a past skein
/// wired there is retired, the person's own hooks are kept exactly, and skein's `tui` and
/// `statusLine` defaults are added where the store sets none. Idempotent. Pure — the testable core
/// of `ensure_probe_in`.
///
/// The hooks it retires now load from skein's plugin ([`turn_state_hooks`]). Before SKEIN-1062
/// this function *added* them; it keeps the retire half it always had and loses the add half.
fn store_settings(existing: &serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Value};
    // The store-era table, not [`turn_state_entries`]: the plugin's commands were never written
    // into a store, and the store's were (SKEIN-1144).
    let entries = store_era_entries();
    // Every spelling of skein's own hooks that a store can carry. Exact-match only, so a user's own
    // hook of the same name is untouched:
    // - the current, `bash`-wrapped form, which every store provisioned before SKEIN-1062 carries;
    // - the pre-`bash`-wrapping form (stores provisioned before the exec-bit hardening carry the
    //   bare-path commands);
    // - the old single unconditional `box-status.sh notify` entry, replaced by the matcher-scoped
    //   notify-blocked/waiting/ignore entries because Notification's payload carries no field
    //   saying which type fired. box-status.sh no longer has a `notify` case at all, so it would
    //   write a literal `status:"notify"`;
    // - the pre-`skein/` store layout, both spellings (below).
    let mut obsolete: Vec<(&str, String)> = entries
        .iter()
        .flat_map(|(ev, cmd, _)| [(*ev, cmd.clone()), (*ev, wire(cmd))])
        .collect();
    obsolete.push(("Notification", format!("{PROBE_STATUS_CMD} notify")));
    // The pre-`skein/` store layout: probes lived directly under `<store>/bin/`, so a store
    // provisioned back then still wires `$CLAUDE_PROJECT_DIR/.claude/bin/…` — a path that stopped
    // existing when the store grew a `skein/` subdirectory. Additive merging kept it *beside* the
    // correct entry, so the box worked perfectly and announced a failure at every session start:
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
    // Retire, and never add: a store with no `hooks` key is not given one.
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        let had_any = !hooks.is_empty();
        let mut emptied = Vec::new();
        for (event, stale_cmd) in &obsolete {
            if let Some(arr) = hooks.get_mut(*event).and_then(|a| a.as_array_mut()) {
                let before = arr.len();
                arr.retain(|e| {
                    !e.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                        hs.iter().any(|h| {
                            h.get("command").and_then(|c| c.as_str()) == Some(stale_cmd.as_str())
                        })
                    })
                });
                if arr.is_empty() && before > 0 {
                    emptied.push(*event);
                }
            }
        }
        // An event this retired down to nothing goes with its last entry, so an upgraded store
        // reads like a fresh one. An event the person left empty themselves is theirs, and stays.
        for event in emptied {
            hooks.remove(event);
        }
        if had_any && hooks.is_empty() {
            root.remove("hooks");
        }
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
        || json!({ "type": "command", "command": statusline_cmd(), "refreshIntervalMs": 30_000 }),
    );
    // skein's past default ran the store's copy of the renderer, which a sibling box can rewrite.
    // Exactly that string moves to the plugin's copy (SKEIN-1149); the rest of the object, and any
    // status line a person set, stays exactly as it is.
    if status_line.get("command").and_then(Value::as_str) == Some(STORE_ERA_STATUSLINE_CMD) {
        status_line["command"] = json!(statusline_cmd());
    }
    // Upgrade only Skein's generated default. A user-owned status-line object remains untouched.
    if status_line.get("command").and_then(Value::as_str) == Some(statusline_cmd().as_str()) {
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
        // The plugin's read-only copy (SKEIN-1144), not the store's, which every box of the repo
        // can write. By its path inside the box, because Codex has no `${CLAUDE_PLUGIN_ROOT}`: the
        // fleet root the launcher passes every box, and the directory it binds read-only there.
        // `box-codex-hook.sh` runs `{file}` from its own directory, so the probe is the plugin's too.
        format!(
            "bash \"{dir}/box-codex-hook.sh\" {event} {file}{suffix}",
            dir = box_probe_dir(),
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

    /// A command string of every hook in a settings value, event by event.
    fn commands(v: &serde_json::Value) -> Vec<(String, String)> {
        v["hooks"]
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(event, groups)| {
                groups
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
                    .filter_map(|h| h["command"].as_str())
                    .map(|c| (event.clone(), c.to_string()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// **A store provisioned before SKEIN-1062 loses every skein hook and keeps every one of its
    /// own**, and a second pass changes nothing.
    ///
    /// The store carries all 24 entries exactly as the old merge wrote them — built from
    /// [`store_era_entries`], so this is the real set and not a sample — beside the person's own
    /// hooks and status line. What would make it fail: the retire list losing the current
    /// `bash`-wrapped spelling (every upgraded store would keep all 24 and each box would fire each
    /// hook twice, once from the plugin and once from here); the add half coming back; or the
    /// retire matching by anything looser than the exact command (the person's hooks would go).
    #[test]
    fn an_upgraded_store_loses_skeins_hooks_and_keeps_its_own() {
        let mut existing = hooks_json(store_era_entries());
        existing["statusLine"] =
            serde_json::json!({ "type": "command", "command": "statusline.sh" });
        let theirs = [
            ("UserPromptSubmit", "slice-gate.sh"),
            ("PreToolUse", "commit-guard.sh"),
            ("Stop", "my-own-stop-hook.sh"),
        ];
        for (event, command) in theirs {
            existing["hooks"][event].as_array_mut().unwrap().push(
                serde_json::json!({ "hooks": [ { "type": "command", "command": command } ] }),
            );
        }
        let skeins = commands(&hooks_json(store_era_entries()));
        assert_eq!(skeins.len(), 24, "the fixture is not the real set");
        assert_eq!(commands(&existing).len(), 27, "the fixture did not build");

        let merged = store_settings(&existing);
        let left = commands(&merged);
        assert_eq!(
            left,
            vec![
                ("PreToolUse".to_string(), "commit-guard.sh".to_string()),
                ("Stop".to_string(), "my-own-stop-hook.sh".to_string()),
                ("UserPromptSubmit".to_string(), "slice-gate.sh".to_string()),
            ],
            "the store should keep exactly the person's own hooks: {merged}"
        );
        // Their status line is theirs.
        assert_eq!(merged["statusLine"]["command"], "statusline.sh");
        assert!(merged["statusLine"]["refreshIntervalMs"].is_null());
        // An event skein alone used goes with its last entry.
        assert!(merged["hooks"].get("SessionEnd").is_none(), "{merged}");

        assert_eq!(
            store_settings(&merged),
            merged,
            "a second pass changed the store"
        );
    }

    /// **The plugin's turn-state hooks are the set the store used to carry**, event by event and
    /// matcher by matcher. A fresh store's settings carry none of them, and still get skein's
    /// `tui` and status line.
    ///
    /// What would make it fail: an entry dropped from [`turn_state_entries`] or its matcher moved,
    /// or [`store_settings`] adding hooks again.
    #[test]
    fn the_turn_state_plugin_carries_every_hook_and_a_fresh_store_none() {
        let fresh = store_settings(&serde_json::json!({}));
        assert!(
            fresh.get("hooks").is_none(),
            "a fresh store was given hooks: {fresh}"
        );
        assert_eq!(fresh["tui"], "fullscreen");
        let merged = turn_state_hooks();
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
        assert!(fresh["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("statusline-command.sh"));
        // every command runs through `bash`, so a squashed exec bit cannot silence them
        let all = commands(&merged);
        assert_eq!(all.len(), 24);
        assert!(all.iter().all(|(_, c)| c.starts_with("bash \"")), "{all:?}");
    }

    /// **A `settings.json` skein cannot parse is left exactly as the person left it.**
    ///
    /// This is the module header's own promise — "every one of those rules has a test that would
    /// fail loudly rather than quietly overwrite someone" — for the one rule that had none. The
    /// installer read the file with
    /// `read_to_string(..).ok().and_then(from_str(..).ok()).unwrap_or_else(json!({}))`, which
    /// answers `{}` to *both* "no file" and "a file I could not parse", and then wrote the merge
    /// back. One trailing comma, and the next `skein-server` start — this runs over every store at
    /// startup — replaced the file with skein's hooks and nothing else.
    ///
    /// **What would make this fail:** put that idiom back in `ensure_probe_in` in place of the
    /// `update_json` call. The `expect_err` goes first, and the byte comparison right behind it.
    /// Done, watched fail, restored.
    ///
    /// Asserted on the bytes on disk, not on the error: the nice wording is the smaller half, and
    /// what matters is that the file the person edited is still the file the person edited.
    #[test]
    fn a_settings_file_skein_cannot_parse_is_never_written_over() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let store_tmp = tempdir();
        let store = store_tmp.join("store").join(".claude");
        fs::create_dir_all(&store).unwrap();
        let settings = store.join("settings.json");

        // A settings file somebody edited by hand and left one comma too many in. The value is
        // recognisable so the assertion is about *this* file surviving, not about "a file exists".
        const THEIRS: &str =
            "{\n  \"env\": { \"MY_OWN_SETTING\": \"do-not-lose-me\" },\n  \"tui\": \"inline\",\n}\n";
        fs::write(&settings, THEIRS).unwrap();

        let why = ensure_probe_in(&store)
            .expect_err("skein wired its hooks into a settings file it could not read");
        assert!(
            why.contains("settings.json"),
            "the refusal has to name the file the person must go and fix: {why}"
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            THEIRS,
            "the settings file skein could not parse was overwritten by the installer"
        );

        // The zero-length file a crash between `write_atomic`'s write and its rename leaves — the
        // way this file becomes unparseable without anybody typing anything.
        fs::write(&settings, b"").unwrap();
        assert!(
            ensure_probe_in(&store).is_err(),
            "a zero-length settings file was accepted as an empty one"
        );
        assert_eq!(
            fs::read(&settings).unwrap(),
            b"",
            "the truncated settings file was written over instead of reported"
        );

        // The other half, and the one line the refusal is closest to breaking: a store nobody has
        // written settings for is *not* unreadable, and must still come up with skein's defaults.
        fs::remove_file(&settings).unwrap();
        ensure_probe_in(&store).expect("a store with no settings.json yet could not be set up");
        let wired: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(
            wired["statusLine"].is_object(),
            "a fresh store came away without skein's status line: {wired}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn store_settings_retires_the_old_unconditional_notify_entry() {
        // A project store provisioned by a pre-matcher-fix skein has this exact stale entry — no
        // matcher, calling a `notify` mode box-status.sh no longer implements at all (it would now
        // fall through to the passthrough branch and write a bogus status:"notify"). Upgrading must
        // remove it; it is in no table any more, so only the explicit retire line can.
        let stale_cmd = format!("{PROBE_STATUS_CMD} notify");
        let existing = serde_json::json!({
            "hooks": {
                "Notification": [
                    { "hooks": [ { "type": "command", "command": stale_cmd } ] },
                    { "hooks": [ { "type": "command", "command": "my-own-notify.sh" } ] }
                ]
            }
        });
        let merged = store_settings(&existing);
        assert_eq!(
            commands(&merged),
            vec![("Notification".to_string(), "my-own-notify.sh".to_string())],
            "the stale entry should be gone and the person's kept: {merged}"
        );
        assert_eq!(store_settings(&merged), merged);
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
    fn store_settings_retires_bare_path_commands() {
        // A store provisioned before the exec-bit hardening carries bare-path commands. They are
        // skein's own, and the plugin now runs the `bash "<script>" <args>` form of each, so a bare
        // one left here would fire every hook twice per event.
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
        let merged = store_settings(&existing);
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
        // no skein Stop hooks at all: only the user's
        assert_eq!(stop_cmds, vec!["my-own-stop-hook.sh"]);
    }

    #[test]
    fn store_settings_retires_the_pre_skein_store_layout() {
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
        let merged = store_settings(&existing);
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
            !cmds.iter().any(|c| c.contains(BOOTSTRAP_CMD)),
            "and the current one loads from the plugin, not from here: {cmds:?}"
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
        // What the block's own `claude` calls, and bash about them, printed (SKEIN-888). The shipped
        // block sends all of it to /dev/null and swallows every failure with `|| true`, so when a
        // leg below recorded the wrong set under load there was nothing to say why — a `claude`
        // that ran and listed nothing and one that never ran at all (bash's "Text file busy", say)
        // read the same. Redirected here, in the test's copy only, and printed with any failure.
        let trace = root.join("trace");
        let traced = block
            .replace(" >/dev/null 2>&1", &format!(" >>{} 2>&1", trace.display()))
            .replace(" 2>/dev/null", &format!(" 2>>{}", trace.display()));
        assert!(
            traced.matches(&*trace.display().to_string()).count() >= 5,
            "the bootstrap's plugin block no longer silences its `claude` calls the way this test \
             rewrites, so a failure below would say nothing about them:\n{block}"
        );
        let block = traced;
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
                "#!/bin/sh\necho \"claude $* (installs: {installs})\" >> {trace}\n\
                 case \"$1 $2\" in\n\
                 'plugin list') cat {listed} 2>/dev/null || true ;;\n\
                 'plugin install') {act} ;;\n\
                 *) : ;;\nesac\nexit 0\n",
                trace = trace.display(),
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
        // Each run's trace on its own, so a failure shows the leg that failed and not the ones
        // before it.
        let run = || {
            let _ = std::fs::remove_file(&trace);
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
        let printed = || {
            format!(
                "\n--- what the block's `claude` calls printed, and bash about them:\n{}",
                std::fs::read_to_string(&trace).unwrap_or_else(|e| format!("(no trace: {e})"))
            )
        };

        // Installs that quietly do nothing must leave no marker, or this box never tries again.
        fake_claude(false);
        assert_eq!(
            run(),
            "",
            "a box that installed nothing was marked done for good{}",
            printed()
        );
        assert!(
            !marker.exists(),
            "the marker was written despite installing nothing"
        );

        // Now they work. The marker records the set, sorted, and only the enabled ones.
        fake_claude(true);
        let done = run();
        assert_eq!(
            done,
            "alpha@market beta@market ",
            "recorded: {done:?}{}",
            printed()
        );
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
            "a settled box did the work again{}",
            printed()
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
            "a newly enabled plugin never reached an existing box{}",
            printed()
        );
    }

    // The skill is upstream's, vendored here because `include_str!` runs at build time against this
    // committed copy, not the submodule — a clone without `--recursive` leaves `upstream/sync` empty
    // (CONTRIBUTING.md's "Setting up"), so reading from it directly would fail for anyone who cloned
    // plainly, and skein does not get to stop compiling over a work-tracking document. sync itself is
    // public (SKEIN-880); that was never the reason to vendor.
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
                crate::testutil::skip(&format!(
                    "no drift check: {} is absent — run `git submodule update --init`",
                    theirs.display()
                ));
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
            crate::testutil::skip(
                "no block check: upstream/sync absent — run `git submodule update --init`",
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
                // The setup ends by running Codex's hook installer from the fleet root's `.skein`;
                // pinned to an empty one, so this never runs the live fleet's copy.
                .env("SKEIN_FLEET_ROOT", &home)
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
        // `ensure_store` publishes the sync gateway, which reads `repos.json` — so it resolves
        // `config::skein_home`, refused rather than answered in a test since SKEIN-626. Unpinned it
        // read the owner's live `~/.skein/repos.json`; it only passed because a neighbour in this
        // process had left `$SKEIN_HOME` set (SKEIN-646). The sibling above pins it the same way.
        let _g = env_lock();
        let skein_home = tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let store_tmp = tempdir();
        let store = store_tmp.join("store/.claude");
        let home_tmp = tempdir();
        let home = home_tmp.join("home");
        ensure_store(&store).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        // The person's own hook, and one a store-era skein wrote, which ran the store's copy of
        // the adapter (SKEIN-1144): the first stays and the second goes.
        let store_era = r#"bash \"$(git rev-parse --show-toplevel)/.claude/skein/bin/box-codex-hook.sh\" Stop box-status.sh waiting"#;
        fs::write(
            home.join(".codex/hooks.json"),
            format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"hooks/user.sh"}}]}},{{"hooks":[{{"type":"command","command":"{store_era}"}}]}}]}}}}"#
            ),
        )
        .unwrap();
        // The copy skein runs, from its read-only plugin, which reads the hooks from beside itself
        // (SKEIN-1149) — installed into a fixture fleet as `fleet::install_launcher` installs it.
        let fleet = tempdir();
        let fleet_root = fleet.to_string_lossy().into_owned();
        for (path, body) in plugin_install_under(&fleet_root) {
            let path = Path::new(&path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let installer = format!(
            "{}/{PLUGIN_PROBE_DIR}/install-codex-hooks.sh",
            crate::runtime::turn_state_plugin_dir_under(&fleet_root)
        );
        let run = || {
            Command::new("bash")
                .arg(&installer)
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
        assert!(
            !text.contains(".claude/skein/bin/"),
            "a store-era hook survived: {text}"
        );
        std::env::remove_var("SKEIN_HOME");
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
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        std::env::set_var("SKEIN_HOME", &store_tmp);
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
        //
        // `SKEIN_FLEET_ROOT` at an empty directory is what MAKES this a legacy box rather than a
        // wish that it is one. The probe decides from whether skein's launcher is installed in this
        // sandbox, and this suite may itself be running inside a fleet sandbox where it IS — in
        // which case, without this, the run below is a shared box with no identity and the script
        // is right to write nothing. The variable is the host's own
        // (`fleet::fleet_root`), read the same way and meaning the same thing on both sides.
        let elsewhere = tempdir();
        let out = Command::new("bash")
            .arg(&script)
            .arg("waiting")
            .env("CLAUDE_PROJECT_DIR", &project_dir)
            .env("SANDBOX_VM_ID", "old-style-box")
            .env("SKEIN_FLEET_ROOT", &*elsewhere)
            .env_remove("SKEIN_BOX")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run box-status.sh");
        assert!(out.status.success());
        assert_eq!(status("old-style-box")["status"], "waiting");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **A screen observation must be the box's own, and the reader must be able to check.**
    ///
    /// The same identity trap as the test above, in the one signal where getting it wrong is
    /// invisible. `box-status.sh` misfiled makes a box report *nothing* — a hole you can see. A
    /// screen observation misfiled is well-formed, fresh, and classifies perfectly, so it renders
    /// as that box's turn state with nothing to mark it: a box that needs you reads as busy, or
    /// the reverse, and the board looks entirely normal.
    ///
    /// The residue is on disk and it is unambiguous. Five separate repo stores under
    /// `~/.skein/repos/*/store/.claude/status/` each hold a `skein-fleet.pane.json` — `skein-fleet`
    /// is `config::default_fleet_sandbox`, the SANDBOX's name, and no box has ever been called
    /// that. All five were written within five minutes of one another on 2026-08-04, one per box,
    /// each `dead:1` with an empty title. That is every box in the shared sandbox falling through
    /// `${SKEIN_BOX:-${SANDBOX_VM_ID:-...}}` to the sandbox's name at once (`wrap` in
    /// src/place/argv.rs describes the same event from the launcher's side).
    ///
    /// Two halves, and the test drives both, because each covers what the other cannot:
    ///   · the probe refuses to write when it cannot establish which box it is in — which fixes
    ///     new observations but says nothing about a file already on disk;
    ///   · the observation names its box, so the reader can refuse one that names someone else —
    ///     which is the only thing that helps when the writer was some other, older probe.
    #[test]
    fn a_screen_observation_is_filed_under_its_own_box_and_says_which() {
        use std::os::unix::fs::PermissionsExt;

        let _g = env_lock();
        // **The default, said rather than inherited.** The assertion below is that the scripts'
        // hard-coded `/boxes` is what `fleet::box_session_path` derives — which is only a claim
        // about the default if nothing has moved it. The lock serialises the tests that set it and
        // does not put it back, so this reads whatever the previous holder left unless it says.
        std::env::remove_var("SKEIN_FLEET_ROOT");
        let home = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        std::env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let script = store.join("skein").join("bin").join("box-pane.sh");
        let project_dir = store.parent().unwrap().to_path_buf();
        let status = store.join("status");

        // A stub tmux that fails, which is what the script sees when the agent's window is gone.
        // That path writes exactly one observation and exits, so the script terminates on its own
        // and the test needs no tmux server, no timeout and no kill — and it is still the real
        // `write_obs`, which is where the box's name has to appear.
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("tmux");
        fs::write(&stub, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        // `sock` is the tell. `pane_observer_start` (src/runtime.rs) exports SKEIN_TMUX_SOCK for a
        // box in a shared sandbox and exports nothing when the sandbox IS the box, so it is the one
        // thing already in the environment that says whether SANDBOX_VM_ID names this box or the
        // thing holding it.
        let run = |skein_box: Option<&str>, sock: Option<&str>| {
            let mut c = Command::new("bash");
            c.arg(&script)
                .arg("skein-agent")
                .env("PATH", &path)
                .env("CLAUDE_PROJECT_DIR", &project_dir)
                // What every box in one sandbox agrees on, and why it is not an identity.
                .env("SANDBOX_VM_ID", "skein-fleet")
                .stdin(Stdio::null());
            match skein_box {
                Some(b) => c.env("SKEIN_BOX", b),
                None => c.env_remove("SKEIN_BOX"),
            };
            match sock {
                Some(s) => c.env("SKEIN_TMUX_SOCK", s),
                None => c.env_remove("SKEIN_TMUX_SOCK"),
            };
            let out = c.output().expect("run box-pane.sh");
            assert!(out.status.success(), "box-pane.sh exited {:?}", out.status);
            out
        };
        let obs = |name: &str| -> Option<crate::signals::PaneObs> {
            let p = status.join(format!("{name}.pane.json"));
            serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
        };

        // 1. The box says who it is. That name is the filename AND it is inside the file.
        run(Some("alpha"), Some("/no/such/session.sock"));
        let alpha = obs("alpha").expect("alpha wrote no observation");
        assert_eq!(
            alpha.box_name, "alpha",
            "the observation must name the box it is about, or the filename is a claim nothing \
             can check"
        );

        // 2. A shared sandbox with no SKEIN_BOX: the old chain would have written
        //    `skein-fleet.pane.json` here, over whatever was already there. Nothing is written.
        run(None, Some("/no/such/session.sock"));
        assert!(
            !status.join("skein-fleet.pane.json").exists(),
            "a box with no identity filed its screen under the sandbox's name — the 2026-08-04 \
             residue, reproduced"
        );
        let host = String::from_utf8_lossy(
            &Command::new("hostname")
                .output()
                .map(|o| o.stdout)
                .unwrap_or_default(),
        )
        .trim()
        .to_string();
        if !host.is_empty() {
            assert!(
                !status.join(format!("{host}.pane.json")).exists(),
                "and not under the hostname either, which in a shared sandbox is the same trap"
            );
        }

        // 3. A legacy box — no SKEIN_BOX and no socket, alone in its VM, where the VM name IS the
        //    box name. Unchanged: refusing here would take the screen away from every box that has
        //    not been migrated, to prevent a collision that cannot happen with one box per VM.
        run(None, None);
        let legacy = obs("skein-fleet").expect("a legacy box must still report under its VM name");
        assert_eq!(legacy.box_name, "skein-fleet");

        // 4. The reader's half. `alpha`'s own observation is hers; read under any other name it is
        //    refused rather than classified, and `screen_health` says which fault it is — "none"
        //    would send somebody to restart an observer that is running fine.
        assert!(crate::signals::pane_usable(&alpha, "alpha"));
        assert!(!crate::signals::pane_usable(&alpha, "beta"));
        assert_eq!(
            crate::signals::screen_health("claude", "beta", Some(&alpha), true),
            "misfiled"
        );
        assert_eq!(
            crate::signals::screen_health("claude", "alpha", Some(&alpha), true),
            ""
        );

        // 5. And an observation from a probe that predates the field is still read. Every box in
        //    the fleet is running one until it is reattached, and refusing them would turn a
        //    hypothetical misattribution into a certain, fleet-wide loss of the screen signal.
        let old = crate::signals::PaneObs {
            box_name: String::new(),
            ..alpha.clone()
        };
        assert!(
            crate::signals::pane_usable(&old, "anybody"),
            "an observation that names nobody could not be checked — that is not the same as \
             being wrong"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// **No probe files a signal under the sandbox's name — every one of them, every branch.**
    ///
    /// `box-pane.sh` was fixed alone (the test above). The identical chain,
    /// `${SKEIN_BOX:-${SANDBOX_VM_ID:-$(hostname)}}`, was still in ten other scripts, and the
    /// residue proves each of them ran it: under `~/.skein/repos/*/store/.claude/` there are
    /// `status/skein-fleet.json` and `.agents` (box-status.sh), `sessions/skein-fleet.json`
    /// (box-session.sh), `diffs/skein-fleet.{json,patch,commits}` (box-diff.sh),
    /// `telemetry/skein-fleet.jsonl` (box-token-usage.sh), `hook-log/skein-fleet.jsonl`,
    /// `skein/boot/skein-fleet.json` (sandbox-bootstrap.sh) — and a `skein-fleet` row in one
    /// registry. `skein-fleet` is `config::default_fleet_sandbox`; no box has ever been called that.
    ///
    /// Three worlds, because the fix is a three-way decision and two of the three are ways to be
    /// wrong:
    ///   1. **the box says who it is** — SKEIN_BOX wins over everything else in the environment;
    ///   2. **a shared sandbox that cannot say** — no SKEIN_BOX, and skein's launcher installed in
    ///      this sandbox. Nothing is written. A signal that is absent reads as a box that has not
    ///      reported, which is true; a signal under the wrong name is well-formed, fresh, and
    ///      renders as another box's state with nothing to mark it;
    ///   3. **a legacy box alone in its VM** — no SKEIN_BOX and no launcher, where the sandbox's
    ///      name IS the box's. Unchanged, because refusing here would take every unmigrated box's
    ///      signals away to prevent a collision that cannot happen with one box per VM.
    ///
    /// The invariant is checked over the whole store rather than per script: after every run, no
    /// file anywhere under it may be named after the sandbox or the host. That is what makes this a
    /// test of the *rule* and not of eleven separate spellings of it — a new probe added tomorrow
    /// with the old chain in it fails here the first time it writes anything.
    #[cfg(target_os = "linux")]
    #[test]
    fn no_probe_files_a_signal_under_the_sandboxs_name() {
        use std::io::Write as _;

        let _g = env_lock();
        let home = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        std::env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let bin = store.join("skein").join("bin");
        let project_dir = store.parent().unwrap().to_path_buf();

        // A sandbox that holds boxes is one skein installed its launcher into
        // (`fleet::box_session_path`), and that is the only thing a HOOK can read to tell the two
        // worlds apart — it is not started by the attach, so `SKEIN_TMUX_SOCK`, which box-pane.sh
        // uses for exactly this question, never reaches it. Two roots here, one of each kind.
        let fleet_root = home.join("boxes");
        fs::create_dir_all(fleet_root.join(".skein")).unwrap();
        fs::write(
            fleet_root.join(".skein").join("box-session.sh"),
            "#!/bin/sh\n",
        )
        .unwrap();
        // **What the two sides have to agree on is the path, and both halves of it are read here
        // rather than restated.** Eleven shipped scripts spell the launcher
        // `"${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh"`, so the root's default is the
        // shell's own literal and the suffix is `box_session_path`'s.
        //
        // It used to compare `box_session_path()` against the literal with nothing pinned, which
        // made it an assertion about `fleet_root()`'s DEFAULT — the reading that a test process
        // must no longer make, because unpinned it is the owner's live fleet (SKEIN-690). Pinning
        // at the shell's default gives up that one claim and keeps the part that drifts: the
        // suffix, and that a probe's spelling still matches the host's.
        const ROOT_IN_THE_PROBES: &str = "/boxes";
        assert!(
            PROBE_SESSION_SH.contains(&format!(
                "\"${{SKEIN_FLEET_ROOT:-{ROOT_IN_THE_PROBES}}}/.skein/box-session.sh\""
            )),
            "the probes no longer look for the launcher where this test says they do, so what it \
             compares the host against is a path nothing uses"
        );
        std::env::set_var("SKEIN_FLEET_ROOT", ROOT_IN_THE_PROBES);
        assert_eq!(
            crate::fleet::box_session_path(),
            format!("{ROOT_IN_THE_PROBES}/.skein/box-session.sh"),
            "the probes decide which world they are in by looking for the launcher at this path; \
             if the host puts it somewhere else, every hook in a fleet box takes the legacy arm"
        );
        // And for the rest of this test, the fixture — nothing below reads the root in this
        // process, and a `/boxes` left behind is a live fleet the next holder of the lock inherits.
        std::env::set_var("SKEIN_FLEET_ROOT", &fleet_root);
        let vm_root = home.join("no-fleet-here");
        fs::create_dir_all(&vm_root).unwrap();

        // What each script needs in order to have something to say. A probe that exits early
        // because its input was missing would pass every assertion below without ever reaching the
        // identity block, so each of these is chosen to reach a write.
        let transcript = home.join("transcript.jsonl");
        fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"usage":{"input_tokens":9,"output_tokens":3},"content":[{"type":"text","text":"done"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::create_dir_all(project_dir.join(".skein")).unwrap();
        fs::write(
            project_dir.join(".skein").join("journal.md"),
            "did x / next y\n",
        )
        .unwrap();
        let transcript_arg = format!(r#"{{"transcript_path":"{}"}}"#, transcript.display());

        // script, argv, stdin.
        let probes: &[(&str, &[&str], &str)] = &[
            ("box-status.sh", &["working"], ""),
            (
                "box-session.sh",
                &["stop"],
                r#"{"last_assistant_message":"the turn ended"}"#,
            ),
            ("box-diff.sh", &[], ""),
            ("box-journal.sh", &[], ""),
            (
                "box-task.sh",
                &[],
                r#"{"tool_input":{"todos":[{"status":"in_progress","activeForm":"Reading the store"}]}}"#,
            ),
            (
                "box-codex-task.sh",
                &[],
                r#"{"prompt":"look at the store"}"#,
            ),
            ("box-token-usage.sh", &[], &transcript_arg),
            (
                "box-codex-telemetry.sh",
                &["tool"],
                r#"{"tool_name":"Bash"}"#,
            ),
            ("box-handoff.sh", &["codex"], "{}"),
            (
                "mailbox.sh",
                &["send", "--to", "somebody", "--body", "a note"],
                "",
            ),
            ("sandbox-bootstrap.sh", &[], "{}"),
        ];

        // Every file under the store, with its CONTENT. Compared before and after a run, because
        // "wrote nothing" is the assertion for the middle world and there is no other way to state
        // it — and by content rather than by path, because two probes write the same file
        // (`tasks/<box>.json`, from TodoWrite and from Codex's prompt) and a path-set comparison
        // would call the second one's write "nothing happened".
        fn files(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
            let mut out = std::collections::BTreeMap::new();
            let Ok(entries) = fs::read_dir(dir) else {
                return out;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(files(&p));
                } else {
                    let bytes = fs::read(&p).unwrap_or_default();
                    out.insert(p, bytes);
                }
            }
            out
        }

        let run = |script: &str,
                   args: &[&str],
                   stdin: &str,
                   skein_box: Option<&str>,
                   vm: &str,
                   root: &Path| {
            let mut c = Command::new("bash");
            c.arg(bin.join(script))
                .args(args)
                .env("CLAUDE_PROJECT_DIR", &project_dir)
                .env("HOME", &*home)
                // What every box in one sandbox agrees on, and why it is not an identity.
                .env("SANDBOX_VM_ID", vm)
                .env("SKEIN_FLEET_ROOT", root)
                .env_remove("SKEIN_TMUX_SOCK")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            match skein_box {
                Some(b) => c.env("SKEIN_BOX", b),
                None => c.env_remove("SKEIN_BOX"),
            };
            let mut child = c.spawn().unwrap_or_else(|e| panic!("spawn {script}: {e}"));
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin.as_bytes())
                .unwrap_or_else(|e| panic!("{script} stdin: {e}"));
            child.wait_with_output().expect("run a probe")
        };

        // The one thing that must be true after every single run, in every world: no signal
        // anywhere under the store carries the sandbox's name or the host's.
        let host = String::from_utf8_lossy(
            &Command::new("hostname")
                .output()
                .map(|o| o.stdout)
                .unwrap_or_default(),
        )
        .trim()
        .to_string();
        // Seeded, not written by a probe: a pending brief under every name box-handoff.sh might
        // pick, including the sandbox's. Exempt from the invariant below because this test put them
        // there — and the sandbox's is asserted at the end to be still sitting there untouched,
        // which is the only way to show the probe did not reach for it.
        let handoffs = store.join("handoffs");
        fs::create_dir_all(&handoffs).unwrap();
        let seeded: Vec<std::path::PathBuf> = ["alpha", "skein-fleet", "old-style-box", &host]
            .iter()
            .filter(|who| !who.is_empty())
            .map(|who| handoffs.join(format!("{who}.codex.pending.md")))
            .collect();

        let no_sandbox_named = |after: &std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
                                script: &str,
                                vm: &str| {
            for (p, bytes) in after {
                if seeded.contains(p) {
                    continue;
                }
                let rel = p.strip_prefix(&store).unwrap_or(p).to_string_lossy();
                assert!(
                    !rel.contains(vm),
                    "{script} filed a signal under the sandbox's name: {rel}"
                );
                assert!(
                    host.is_empty() || !rel.contains(&host),
                    "{script} filed a signal under the hostname, which in a shared sandbox is \
                         the same trap: {rel}"
                );
                // The registry is the one signal whose KEY is the box name and whose filename
                // is not, so a path-shaped invariant walks straight past it — and it is the
                // misfiling with the longest reach: the host reads `sandboxes.json` every
                // couple of seconds, a row there is a box the board can be asked to show, and
                // nothing ever removes it, because `delist_box` and `destroy_box` drop a box's
                // rows BY NAME and no box is named after the sandbox. One such row is on disk
                // in sync's registry, left by the version of these scripts this replaces.
                if rel.ends_with("sandboxes.json") {
                    let reg: serde_json::Value =
                        serde_json::from_slice(bytes).unwrap_or(serde_json::Value::Null);
                    if let Some(o) = reg.as_object() {
                        assert!(
                            !o.contains_key(vm),
                            "{script} registered the SANDBOX as if it were a box: {rel} has a \
                                 `{vm}` row"
                        );
                    }
                }
            }
        };

        for (script, args, stdin) in probes {
            // A fresh pending brief per pass, since box-handoff.sh consumes the one it finds.
            for p in &seeded {
                fs::write(p, "brief\n").unwrap();
            }
            // 1. The box says who it is. It writes, and under its own name.
            let before = files(&store);
            let out = run(
                script,
                args,
                stdin,
                Some("alpha"),
                "skein-fleet",
                &fleet_root,
            );
            assert!(out.status.success(), "{script}: {out:?}");
            let after = files(&store);
            assert!(
                after != before,
                "{script} wrote nothing at all, so this test is not exercising it"
            );
            no_sandbox_named(&after, script, "skein-fleet");

            // 2. A shared sandbox with no SKEIN_BOX: the old chain wrote `skein-fleet.*` here, on
            //    top of whatever already owned that name. Nothing at all is written now.
            let before = files(&store);
            let out = run(script, args, stdin, None, "skein-fleet", &fleet_root);
            assert!(out.status.success(), "{script}: {out:?}");
            let after = files(&store);
            no_sandbox_named(&after, script, "skein-fleet");
            if *script != "sandbox-bootstrap.sh" {
                // sandbox-bootstrap.sh is the exception and says so in its own comment: most of
                // what it does — the shared-home contract, the memory bridge — is not keyed on
                // identity and is the same work whoever this turns out to be, so it skips only the
                // parts that write under a name. Every other probe writes under a name or not at
                // all, and so has nothing left to do.
                assert_eq!(
                    after, before,
                    "{script} wrote something while it could not say which box it was"
                );
            }

            // 3. A legacy box: no SKEIN_BOX, no launcher in this sandbox, so the sandbox's name IS
            //    the box's. Unchanged — this is the path that has always worked.
            let before = files(&store);
            let out = run(script, args, stdin, None, "old-style-box", &vm_root);
            assert!(out.status.success(), "{script}: {out:?}");
            let after = files(&store);
            assert!(
                after != before,
                "{script} stopped reporting for a legacy box, which is a certain loss traded \
                 against a collision that cannot happen with one box per VM"
            );
            no_sandbox_named(&after, script, "skein-fleet");
        }

        // The sandbox's brief was there the whole time and nothing ever took it. A consumed one
        // would mean a box read handoff context addressed to a name that is not its own — the same
        // fault as a misfiled write, in the one probe whose signal travels the other way.
        assert!(
            handoffs.join("skein-fleet.codex.pending.md").exists(),
            "box-handoff.sh consumed the brief filed under the SANDBOX's name"
        );

        // `sandbox-bootstrap.sh` is the one script that resolves an identity and then hands it to
        // ANOTHER, and the delivery it drives has to keep working now that mailbox.sh refuses a
        // turn it cannot attribute. What this pins is the delivery, not the spelling of the
        // handover: `SKEIN_BOX=` and `SANDBOX_VM_ID=` on that line behave identically in every
        // reachable world, since a child inherits whichever variable the parent resolved FROM —
        // measured, by making the swap and watching the whole suite stay green. The line reads
        // `SKEIN_BOX` because that is what the value is, and because a box name in the variable
        // that means "the sandbox" is how this class of bug is spelled.
        let mail = store.join("mailbox");
        fs::create_dir_all(&mail).unwrap();
        fs::write(
            mail.join("handover-check.json"),
            r#"{"from":"beta","to":"alpha","kind":"note","branch":"main","body":"addressed to alpha","ts":"1","seenBy":[],"relayedTo":[],"originProject":""}"#,
        )
        .unwrap();
        let out = run(
            "sandbox-bootstrap.sh",
            &[],
            "{}",
            Some("alpha"),
            "skein-fleet",
            &fleet_root,
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("addressed to alpha"),
            "sandbox-bootstrap.sh did not deliver mail addressed to the box it had just \
             identified. A box whose inbox stops being surfaced comes up looking perfectly \
             healthy and simply never sees its mail. stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );

        // The signals whose reader can CHECK the filename rather than believe it say so inside the
        // file. Not every probe can: a `.patch`, a `.commits` list and a journal `.md` are opaque
        // bytes with nowhere to put a name, and for those the refusal above is the whole fix.
        let claims = |kind: &str, name: &str| -> String {
            let p = store.join(kind).join(format!("{name}.json"));
            let v: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
            )
            .unwrap();
            v.get("box")
                .and_then(|b| b.as_str())
                .unwrap_or_default()
                .to_string()
        };
        for kind in ["status", "sessions", "tasks"] {
            assert_eq!(
                claims(kind, "alpha"),
                "alpha",
                "{kind}/alpha.json does not say whose it is, so the filename is a claim nothing \
                 can check"
            );
        }

        // The reader's half, on the file the probe actually wrote. Read under any other name it is
        // refused rather than believed.
        let status: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(store.join("status").join("alpha.json")).unwrap(),
        )
        .unwrap();
        assert!(crate::signals::signal_is_ours(&status, "alpha"));
        assert!(!crate::signals::signal_is_ours(&status, "beta"));
        // And one from a probe that predates the field is still read, because every box in the
        // fleet is running one until it is reattached.
        assert!(crate::signals::signal_is_ours(
            &serde_json::json!({"status": "working"}),
            "anybody"
        ));
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// Linux only: drives `box-token-usage.sh` as a script, in the userland it is installed into.
    #[cfg(target_os = "linux")]
    #[test]
    fn box_token_usage_sums_new_assistant_entries_and_is_idempotent() {
        // Shells out to the installed script directly (like the mailbox round-trip test) so this
        // proves the real jq pipeline, not just a Rust-side assumption about its behavior.
        let _g = env_lock();
        let home = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        std::env::set_var("SKEIN_HOME", &home);
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
        std::env::remove_var("SKEIN_HOME");
    }

    /// **A freshly scaffolded store's `settings.json` carries no skein hook, and a box still
    /// reports its turn state — through the hooks of either variant of skein's plugin** (SKEIN-1062).
    ///
    /// The whole path, with nothing hand-written in between: [`ensure_probe_in`] scaffolds the
    /// store, [`plugin_install_under`] gives the bytes `fleet::install_launcher` puts under
    /// `.skein`, written here into a fixture fleet root, and each variant's own `UserPromptSubmit`
    /// and `Stop` commands are run the way Claude Code runs a hook — through a shell, with
    /// `$CLAUDE_PROJECT_DIR` and `${CLAUDE_PLUGIN_ROOT}` set — against the plugin's own copy of
    /// `box-status.sh` (SKEIN-1144). The board reads what they write, in the store.
    ///
    /// What would make it fail: the store scaffold writing skein's hooks again (the first
    /// assertion); a variant losing its turn-state hooks (no command is found); a command the
    /// plugin carries that no longer reaches a script the plugin installs, or a plugin copy that
    /// no longer finds the store from the project (no state is written); the same for Codex's
    /// commands, which name the turn-state variant by the fleet root.
    #[test]
    fn a_fresh_store_has_no_skein_hooks_and_the_plugin_still_reports_turn_state() {
        let _g = env_lock();
        let home = tempdir();
        let fleet = tempdir();
        let fleet_root = fleet.to_string_lossy().into_owned();
        let mut env = env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &Path);
        env.set("SKEIN_FLEET_ROOT", &fleet_root);
        // Not under a git checkout: the probe resolves its store from `git rev-parse` and would
        // otherwise climb out into a real one.
        let root = tempdir();
        let store = root.join(".claude");
        ensure_probe_in(&store).expect("scaffold the store");
        let installed = plugin_install_under(&fleet_root);
        for (path, body) in &installed {
            let path = Path::new(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }

        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(store.join("settings.json")).unwrap())
                .unwrap();
        assert!(
            settings.get("hooks").is_none(),
            "a fresh store's settings.json carries hooks: {settings}"
        );
        assert!(settings["statusLine"].is_object(), "{settings}");

        for dir in [
            crate::runtime::plugin_dir_under(&fleet_root),
            crate::runtime::turn_state_plugin_dir_under(&fleet_root),
        ] {
            let path = format!("{dir}/hooks/hooks.json");
            let hooks: serde_json::Value = serde_json::from_str(
                &installed
                    .iter()
                    .find(|(p, _)| *p == path)
                    .unwrap_or_else(|| panic!("nothing is installed at {path}"))
                    .1,
            )
            .unwrap();
            let run = |event: &str, fragment: &str| {
                let (_, command) = commands(&hooks)
                    .into_iter()
                    .find(|(e, c)| e == event && c.contains(fragment))
                    .unwrap_or_else(|| panic!("{path} has no {event} hook running {fragment}"));
                let out = Command::new("bash")
                    .arg("-c")
                    .arg(&command)
                    .env("CLAUDE_PROJECT_DIR", root.as_ref() as &Path)
                    .env("CLAUDE_PLUGIN_ROOT", &dir)
                    .env("SKEIN_BOX", "example")
                    .stdin(Stdio::null())
                    .output()
                    .expect("bash");
                assert!(out.status.success(), "{command}: {out:?}");
            };
            let status = || {
                let text = fs::read_to_string(store.join("status/example.json"))
                    .unwrap_or_else(|e| panic!("{path}'s hook wrote no state: {e}"));
                let v: serde_json::Value = serde_json::from_str(&text).expect(&text);
                v["status"].as_str().unwrap_or_default().to_string()
            };
            run("UserPromptSubmit", r#"box-status.sh" working"#);
            assert_eq!(status(), "working", "through {path}");
            run("Stop", r#"box-status.sh" waiting"#);
            assert_eq!(status(), "waiting", "through {path}");
            fs::remove_file(store.join("status/example.json")).unwrap();
        }

        // And Codex's, which reach the same plugin copy by the fleet root rather than by
        // `${CLAUDE_PLUGIN_ROOT}`, from the box's working directory rather than a project variable.
        let codex = codex_hooks_with_probe();
        for (event, mode) in [("UserPromptSubmit", "working"), ("Stop", "waiting")] {
            let (_, command) = commands(&codex)
                .into_iter()
                .find(|(e, c)| e == event && c.contains(&format!("box-status.sh {mode}")))
                .unwrap_or_else(|| panic!("Codex has no {event} hook for box-status.sh"));
            let out = Command::new("bash")
                .arg("-c")
                .arg(&command)
                .current_dir(root.as_ref() as &Path)
                .env_remove("CLAUDE_PROJECT_DIR")
                .env("SKEIN_FLEET_ROOT", &fleet_root)
                .env("SKEIN_BOX", "example")
                .stdin(Stdio::null())
                .output()
                .expect("bash");
            assert!(out.status.success(), "{command}: {out:?}");
            let text = fs::read_to_string(store.join("status/example.json"))
                .unwrap_or_else(|e| panic!("Codex's {event} hook wrote no state: {e}"));
            let v: serde_json::Value = serde_json::from_str(&text).expect(&text);
            assert_eq!(v["status"], mode, "through Codex's {event}");
        }
    }

    /// **A repo that ships its own `.claude/` loses the copies of skein's hooks a past merge put
    /// in its `settings.json`, and keeps its own.** The kit's case 2 (`kit/skein-startup.sh`)
    /// merged the store's hooks into the repo's settings file additively, so every such box
    /// carries all 24; with the plugin running them too, each would fire twice.
    ///
    /// Runs the kit's own jq program, cut out of the shipped script, over the settings a box of
    /// that kind has today. What would make it fail: the `unskein` pass dropped from that program
    /// (all 24 stay); or it matching anything outside skein's namespace (the repo's hook goes).
    #[test]
    fn a_repo_shipped_settings_file_loses_skeins_copied_hooks() {
        if Command::new("jq").arg("--version").output().is_err() {
            crate::testutil::skip("no jq here, and the kit's merge is jq");
            return;
        }
        let script = crate::kit::KIT_STARTUP_SH;
        let open = "merged=\"$(jq -s '";
        let close = "' \"$rc/settings.json\" \"$store/settings.json\"";
        assert_eq!(script.matches(open).count(), 1, "the kit's merge moved");
        let from = script.find(open).unwrap() + open.len();
        let program = &script[from..from + script[from..].find(close).expect("merge end")];

        // What the kit copied was the store's hooks, so the store-era commands (SKEIN-1144).
        let mut repo = hooks_json(store_era_entries());
        repo["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "hooks": [ { "type": "command", "command": "repo-own-stop.sh" } ] }));
        repo["model"] = serde_json::json!("example-model");
        assert_eq!(commands(&repo).len(), 25, "the fixture did not build");
        let store = store_settings(&serde_json::json!({}));

        let dir = tempdir();
        let (a, b) = (dir.join("repo.json"), dir.join("store.json"));
        fs::write(&a, repo.to_string()).unwrap();
        fs::write(&b, store.to_string()).unwrap();
        let out = Command::new("jq")
            .arg("-s")
            .arg(program)
            .arg(&a)
            .arg(&b)
            .output()
            .expect("jq");
        assert!(out.status.success(), "{out:?}");
        let merged: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            commands(&merged),
            vec![("Stop".to_string(), "repo-own-stop.sh".to_string())],
            "{merged}"
        );
        assert_eq!(merged["model"], "example-model");
        assert_eq!(merged["statusLine"], store["statusLine"], "{merged}");
    }

    /// **skein's turn-state hooks load whichever way the fleet's switch is set**, and off drops
    /// only the resource holds, the monitor and the `skein_*` tools (the owner's decision on
    /// SKEIN-1057, carried out by SKEIN-1062).
    ///
    /// Follows the argv to the bytes: for each switch value, the directory `for_box` names, then
    /// the files [`plugin_install`] puts there. What would make it fail: off dropping the flag
    /// (no directory is named, so no turn-state hook loads); the turn-state variant not installed,
    /// or installed without its hooks; either variant missing any of the 24 entries; the full one
    /// losing its resource hooks; or the narrow one carrying the holds, the monitor or the tools.
    #[test]
    fn turn_state_hooks_load_whichever_way_the_switch_is_set() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_FLEET_ROOT", "/fleet-root-example");
        env.set("SKEIN_HOME", &home);
        let claude = crate::runtime::runtime_adapter("claude").unwrap();
        let installed = plugin_install();
        let file = |dir: &str, rel: &str| {
            let path = format!("{dir}/{rel}");
            installed
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, body)| body.clone())
        };
        let turn_state = turn_state_hooks();
        let groups = |hooks: &serde_json::Value, event: &str| -> Vec<serde_json::Value> {
            hooks["hooks"][event]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        for switch in ["on", "off"] {
            env.set("SKEIN_BOX_PLUGIN", switch);
            let resolved = crate::runtime::for_box(claude.interactive_start, "web-main");
            let dir = resolved
                .split_once("--plugin-dir '")
                .and_then(|(_, rest)| rest.split_once('\''))
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_else(|| panic!("switch {switch}: the argv names no plugin: {resolved}"));

            let manifest = file(&dir, ".claude-plugin/plugin.json")
                .unwrap_or_else(|| panic!("switch {switch}: no plugin is installed at {dir}"));
            let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
            assert_eq!(manifest["name"], "skein", "switch {switch}");
            let hooks: serde_json::Value = serde_json::from_str(
                &file(&dir, "hooks/hooks.json")
                    .unwrap_or_else(|| panic!("switch {switch}: {dir} has no hooks")),
            )
            .unwrap();
            let mut count = 0;
            for (event, wanted) in turn_state["hooks"].as_object().unwrap() {
                let have = groups(&hooks, event);
                for group in wanted.as_array().unwrap() {
                    assert!(
                        have.contains(group),
                        "switch {switch}: {dir} does not load {event} {group}"
                    );
                    count += 1;
                }
            }
            assert_eq!(count, 24, "switch {switch}");

            let resources = hooks.to_string().contains("bin/skein-resources");
            let tools = file(&dir, ".mcp.json").is_some();
            let monitor = file(&dir, "monitors/monitors.json").is_some();
            match switch {
                "on" => assert!(resources && tools && monitor, "on lost the holds or tools"),
                _ => assert!(
                    !resources && !tools && !monitor,
                    "off still loads a hold ({resources}), the tools ({tools}) or the monitor \
                     ({monitor})"
                ),
            }
        }
    }

    /// The script a hook command runs: `bash "<script>" …` or `python3 "<script>" …`, the two
    /// shapes skein's hooks take.
    fn script_of(command: &str) -> String {
        let rest = command
            .split_once(" \"")
            .map(|(_, r)| r)
            .unwrap_or_else(|| panic!("not an interpreter and a quoted script: {command}"));
        let (script, _) = rest
            .split_once('"')
            .unwrap_or_else(|| panic!("unterminated script path: {command}"));
        script.to_string()
    }

    /// **Every turn-state hook, Claude's and Codex's, runs a script skein's plugin installs under
    /// the fleet root's `.skein`** — the directory the launcher binds read-only into every box —
    /// **and none runs anything out of the store**, which every box of the repo can write
    /// (SKEIN-1144).
    ///
    /// Resolved the way the box resolves it: `${CLAUDE_PLUGIN_ROOT}` is the variant that loaded,
    /// `${SKEIN_FLEET_ROOT:-/boxes}` the fleet root, and the resolved path has to be one
    /// [`plugin_install_under`] actually writes. Codex's hooks run `box-codex-hook.sh`, which runs
    /// the probe it is named from its own directory, so that probe has to be installed beside it.
    ///
    /// What would make it fail: a command put back on `$CLAUDE_PROJECT_DIR/.claude/skein/bin/`
    /// (it does not start with `${CLAUDE_PLUGIN_ROOT}/`); Codex's command put back on
    /// `$(git rev-parse --show-toplevel)/.claude/…`; or a script a hook names dropped from
    /// [`plugin_probe_scripts`] (the resolved path is not installed).
    #[test]
    fn every_turn_state_hook_runs_a_script_the_read_only_plugin_installs() {
        let root = "/fleet-root-example";
        let installed: std::collections::BTreeSet<String> = plugin_install_under(root)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        let read_only = format!("{root}/.skein/");
        let resolves = |path: &str, from: &str| {
            assert!(
                path.starts_with(&read_only) && installed.contains(path),
                "{from} runs {path}, which the plugin does not install under {read_only}"
            );
        };

        let mut seen = 0;
        for dir in [
            crate::runtime::plugin_dir_under(root),
            crate::runtime::turn_state_plugin_dir_under(root),
        ] {
            let path = format!("{dir}/hooks/hooks.json");
            let body = &plugin_install_under(root)
                .into_iter()
                .find(|(p, _)| *p == path)
                .unwrap_or_else(|| panic!("nothing is installed at {path}"))
                .1;
            let hooks: serde_json::Value = serde_json::from_str(body).unwrap();
            for (event, command) in commands(&hooks) {
                assert!(
                    !command.contains(".claude/"),
                    "{path}: {event} still names the store: {command}"
                );
                let script = script_of(&command);
                let rel = script
                    .strip_prefix("${CLAUDE_PLUGIN_ROOT}/")
                    .unwrap_or_else(|| panic!("{path}: {event} runs {script}, not the plugin's"));
                resolves(&format!("{dir}/{rel}"), &format!("{path} {event}"));
                seen += 1;
            }
        }
        // 24 in the turn-state variant, 24 and the plugin's own 3 in the full one.
        assert_eq!(seen, 24 + 24 + 3, "the hooks read are not the whole set");

        let codex = codex_hooks_with_probe();
        let mut codex_seen = 0;
        for (event, command) in commands(&codex) {
            let script = script_of(&command);
            let rel = script
                .strip_prefix("${SKEIN_FLEET_ROOT:-/boxes}/")
                .unwrap_or_else(|| panic!("Codex {event} runs {script}, not the fleet's"));
            let adapter = format!("{root}/{rel}");
            resolves(&adapter, &format!("Codex {event}"));
            // `bash "<adapter>" <Event> <probe> …`: the probe runs from the adapter's directory.
            let probe = command
                .split_whitespace()
                .nth(3)
                .unwrap_or_else(|| panic!("Codex {event} names no probe: {command}"));
            let dir = adapter.rsplit_once('/').unwrap().0;
            resolves(&format!("{dir}/{probe}"), &format!("Codex {event}"));
            codex_seen += 1;
        }
        assert!(codex_seen > 0, "Codex has no hooks to check");
    }

    /// **A script the plugin installs runs nothing out of the store**, and every sibling it runs
    /// by its own directory is installed beside it (SKEIN-1144).
    ///
    /// Moving the hook's own script is not enough: `sandbox-bootstrap.sh` runs `shared-home.sh`,
    /// `agent-guide.sh` and `mailbox.sh`, and a helper run from the store is a helper a sibling box
    /// can rewrite and this box's SessionStart then executes. What would make it fail: any of
    /// those put back on `$store/skein/bin/…` (a code line names `skein/bin`), or a helper run as
    /// `$here/<name>` that [`plugin_probe_scripts`] does not install.
    #[test]
    fn plugin_probe_scripts_are_every_script_a_hook_runs() {
        let names: Vec<&str> = plugin_probe_scripts().iter().map(|(n, _)| *n).collect();
        let mut siblings = 0;
        for (name, body) in plugin_probe_scripts() {
            for (n, line) in body.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with('#') {
                    continue;
                }
                let code = line.split(" #").next().unwrap_or(line);
                assert!(
                    !code.contains("skein/bin"),
                    "{name}:{} runs something from the store: {line}",
                    n + 1
                );
                let mut rest = code;
                while let Some(at) = rest.find("$here/") {
                    let tail = &rest[at + "$here/".len()..];
                    let helper: String = tail
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                        .collect();
                    if helper.ends_with(".sh") {
                        assert!(
                            names.contains(&helper.as_str()),
                            "{name}:{} runs {helper} beside itself, and the plugin does not \
                             install it",
                            n + 1
                        );
                        siblings += 1;
                    }
                    rest = tail;
                }
            }
        }
        assert!(
            siblings >= 3,
            "sandbox-bootstrap.sh's three helpers were not found, so this checked nothing"
        );
    }

    /// **Everything a box runs from skein as it starts, rather than from a hook, is a copy skein's
    /// read-only plugin installs under `.skein`**, and nothing runs one out of the store, which
    /// every box of the repo can write (SKEIN-1149).
    ///
    /// Every caller, read as the shell it is: the kit's provisioning script, Codex's setup, the
    /// attach shell's guide refresh and screen observer, the tracker's install and refresh, and the
    /// status line skein sets — fresh, and moved from the store-era default. Each path a caller
    /// names for one of these helpers is resolved the way the box resolves it
    /// (`${SKEIN_FLEET_ROOT:-/boxes}` is the fleet root; the kit's `$skein_probe` is its own
    /// directory's `plugin-turn-state/probe`, and the fleet runs it from `.skein`) and has to be a
    /// file [`plugin_install_under`] writes there.
    ///
    /// What would make it fail: any caller put back on `$store/skein/bin/…` or on the checkout's
    /// `.claude/skein/bin/…` (the path does not resolve under `.skein`); a helper dropped from
    /// [`start_helpers`] (the resolved path is not installed); the status line's upgrade removed
    /// (the store-era command still runs the store's copy); or a helper nothing calls any more
    /// (the last assertion, so the list cannot outlive its callers).
    #[test]
    fn every_start_helper_runs_from_the_read_only_plugin() {
        let _g = env_lock();
        let home = tempdir();
        let root = "/fleet-root-example";
        let mut env = env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &Path);
        env.set("SKEIN_FLEET_ROOT", root);
        let installed: std::collections::BTreeSet<String> = plugin_install_under(root)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        let probe_dir = format!(
            "{}/{PLUGIN_PROBE_DIR}",
            crate::runtime::turn_state_plugin_dir_under(root)
        );

        // The kit finds its helpers from where it runs, and the fleet runs it from `.skein`.
        let kit_probe =
            r#"skein_probe="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/plugin-turn-state/probe""#;
        let kit = crate::kit::KIT_STARTUP_SH;
        assert_eq!(
            kit.lines()
                .filter(|l| l.starts_with("skein_probe="))
                .count(),
            1,
            "the kit no longer names its helpers' directory in one place"
        );
        assert!(
            kit.lines().any(|l| l == kit_probe),
            "the kit's helpers' directory is not its own directory's plugin-turn-state/probe"
        );
        let kit_at = crate::fleet::box_provision_path();
        let kit_dir = kit_at.rsplit_once('/').expect("a directory").0;
        assert_eq!(
            kit_dir,
            format!("{root}/.skein"),
            "the fleet no longer runs the kit from .skein, so its own directory is not read-only"
        );
        let kit_probe_dir = format!(
            "{kit_dir}/{}/{PLUGIN_PROBE_DIR}",
            crate::runtime::TURN_STATE_PLUGIN
        );
        // Its code, without comments and without the settings merge, whose `/.claude/skein/bin/`
        // is the pattern that RETIRES skein's store-era hooks from a repo's settings, not a path it runs.
        let open = "merged=\"$(jq -s '";
        let close = "' \"$rc/settings.json\" \"$store/settings.json\"";
        let from = kit.find(open).expect("the kit's merge moved");
        let to = from + kit[from..].find(close).expect("merge end");
        let kit_code: String = format!("{}{}", &kit[..from], &kit[to..])
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        let status_line = |existing: serde_json::Value| {
            store_settings(&existing)["statusLine"]["command"]
                .as_str()
                .expect("a status line command")
                .to_string()
        };
        let mut callers: Vec<(&str, String)> = start_invocations();
        callers.extend([
            ("the kit", kit_code),
            (
                "the tracker's forced refresh",
                crate::tracking::sync_refresh_in_box(true),
            ),
            (
                "a fresh store's status line",
                status_line(serde_json::json!({})),
            ),
            (
                "an upgraded store's status line",
                status_line(serde_json::json!({
                    "statusLine": { "type": "command", "command": STORE_ERA_STATUSLINE_CMD }
                })),
            ),
        ]);

        let mut helpers: Vec<&str> = start_helpers().iter().map(|(n, _)| *n).collect();
        helpers.extend(["shared-home.sh", "agent-guide.sh"]);
        let mut called = std::collections::BTreeSet::new();
        for (caller, text) in &callers {
            assert!(
                !text.contains("skein/bin/"),
                "{caller} still runs something out of the store: {text}"
            );
            let mut found = 0;
            for helper in &helpers {
                let mut from = 0;
                while let Some(at) = text[from..].find(helper).map(|i| from + i) {
                    from = at + helper.len();
                    let start = text[..at]
                        .rfind(|c: char| matches!(c, '"' | '\'' | ' ' | '=' | ';' | '\n'))
                        .map_or(0, |i| i + 1);
                    let named = &text[start..from];
                    let resolved = named
                        .replace("${SKEIN_FLEET_ROOT:-/boxes}", root)
                        .replace("$skein_probe", &kit_probe_dir);
                    assert!(
                        resolved.starts_with(&format!("{root}/.skein/"))
                            && installed.contains(&resolved),
                        "{caller} runs {named}, which is not a file skein's plugin installs under \
                         .skein (resolved to {resolved})"
                    );
                    called.insert(*helper);
                    found += 1;
                }
            }
            assert!(
                found > 0,
                "{caller} names no helper, so this checked nothing"
            );
        }
        for helper in &helpers {
            assert!(
                called.contains(helper),
                "nothing runs {helper} any more, so it has no business in start_helpers"
            );
        }

        // Codex's wiring, which its installer reads from its own directory rather than the store.
        assert!(
            installed.contains(&format!("{probe_dir}/{CODEX_HOOKS_JSON}")),
            "the plugin does not carry Codex's hooks beside their installer"
        );
        assert!(
            INSTALL_CODEX_HOOKS_SH.contains(r#"source_hooks="$here/codex-hooks.json""#),
            "the Codex installer reads its hooks from somewhere other than beside itself"
        );
        // And no helper runs anything out of the store in turn.
        for (name, body) in start_helpers() {
            for line in body.lines().filter(|l| !l.trim_start().starts_with('#')) {
                assert!(
                    !line.contains("$store/skein/bin") && !line.contains("bash \"$store"),
                    "{name} runs something from the store: {line}"
                );
            }
        }
    }

    /// **skein's past default status line moves to the plugin's copy of the renderer, and a
    /// status line a person set is kept exactly** (SKEIN-1149).
    ///
    /// What would make it fail: the upgrade removed (the store-era command survives, so every box
    /// keeps running the store's copy, which a sibling can rewrite); the upgrade matching anything
    /// looser than skein's exact old string (the person's own command below also names
    /// `statusline-command.sh`, and it would be taken); or the upgrade replacing the object rather
    /// than its command (the person-chosen interval on the old default would go).
    #[test]
    fn a_store_era_status_line_moves_to_the_plugin_and_a_persons_own_stays() {
        let upgraded = store_settings(&serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": STORE_ERA_STATUSLINE_CMD,
                "refreshIntervalMs": 5_000,
                "padding": 1
            }
        }));
        assert_eq!(
            upgraded["statusLine"],
            serde_json::json!({
                "type": "command",
                "command": statusline_cmd(),
                "refreshIntervalMs": 5_000,
                "padding": 1
            }),
            "skein's old default was not moved to the plugin's copy, or lost what was beside it"
        );
        assert_eq!(
            store_settings(&upgraded),
            upgraded,
            "a second pass changed it"
        );

        let theirs = serde_json::json!({
            "type": "command",
            "command": "bash $CLAUDE_PROJECT_DIR/tools/statusline-command.sh",
            "padding": 2
        });
        let kept = store_settings(&serde_json::json!({ "statusLine": theirs.clone() }));
        assert_eq!(
            kept["statusLine"], theirs,
            "a person's own status line was changed"
        );
    }
}
