//! Cross-runtime takeover: hand one box's work to a new box running a different agent.
//!
//! Not a conversion. The source box is snapshotted and left alone, and a *new* single-runtime box
//! is created from that snapshot — so a takeover that goes wrong costs a box, never the work. The
//! snapshot is immutable by construction: it is streamed out of the source as a file, and nothing
//! here writes back into the box it came from.
//!
//! What crosses the boundary is deliberately narrow. The repository bundle and the worktree carry
//! the code; a handoff brief carries what the agent was doing; the shared agent context is merged
//! rather than copied over, so the destination's own state is never clobbered. Generated hooks are
//! dropped and only the user's own are kept — a hook written by a previous skein, replayed into a
//! new box, is how a takeover would resurrect a configuration nobody chose.

use crate::handoff::prepare_handoff_for;
use crate::kit::{ensure_kit, ensure_store};
use crate::place::{fleet_sandbox, place_of, placed_boxes};
use crate::probes::ensure_probe_in;
use crate::repos::{agent_for_box, box_name, load_repos, repo_for_box, Repo};
use crate::runtime::{
    guarded_agent_command, runtime_adapter, valid_runtime, TMUX_AGENT_CONTRACT, TMUX_CONFIGURE,
};
use crate::sandbox::sbx_guest_output;
use crate::sbx::fleet_boxes;
use crate::util::valid_name;
use crate::util::{bounded_output, slug, write_atomic};
use chrono::Utc;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

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
    // There was a second resolution here, by WORKSPACE: a box whose directory was the host
    // checkout belonged to the repo adopted from that path. Only a local-path repo could ever
    // match, and there are none — a repo is a remote now (`repos::add_repo`).
    None
}

pub(crate) fn replacement_name(
    repo: &Repo,
    source: &str,
    branch: &str,
    target_runtime: &str,
) -> String {
    let base = if source == repo.id {
        format!("{}-{target_runtime}", box_name(&repo.id, branch))
    } else if source.starts_with(&format!("{}-", repo.id)) {
        format!("{source}-{target_runtime}")
    } else {
        format!("{}-{source}-{target_runtime}", repo.id)
    };
    let base = slug(&base);
    // Every name already in use, sandboxes AND boxes in the fleet. Counting only sandboxes would
    // hand the replacement the name of a live fleet box, and `start_box` refuses a tree that
    // already exists — so the takeover would fail on a collision it had just created for itself.
    let mut existing = fleet_boxes()
        .unwrap_or_default()
        .into_iter()
        .map(|box_| box_.name)
        .collect::<BTreeSet<_>>();
    // Unconditionally, because a fleet always has a name (SKEIN-772): `config::load_config` repairs
    // a blank `fleet_sandbox` (`src/config.rs:459`) before anybody reads it, so the `!is_empty()`
    // that used to wrap this had no false arm — it was the *else* that was dead, and skipping the
    // placed boxes is exactly the collision the comment above says must not happen.
    existing.extend(
        placed_boxes(&fleet_sandbox())
            .into_iter()
            .map(|(name, _)| name),
    );
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

/// How long one artifact may take to stream out of a box before the copy is a wedge and says so.
///
/// Five minutes, because a repository bundle over a slow crossing legitimately takes minutes and a
/// deadline that fires on a healthy copy costs the takeover the thing it exists to move.
///
/// **A function and not a `const` so `$SKEIN_ARTIFACT_COPY_MS` can shorten it**, which is what lets
/// a test drive a real wedge against the execution seam in seconds instead of waiting five minutes
/// for the deadline it is checking. Same shape as `skein-server`'s `upload_stall` and `knock::grace`
/// — and the same rule: a value that does not parse, or is zero, is the default rather than an
/// error, because a mistyped knob must not disable a deadline.
///
/// **It can only ever SHORTEN the wait**, and that is the `min` below rather than a sentence: this
/// is read from the environment, a box writes into a shared `~/.local/bin` on a shared uid, and a
/// knob that could lengthen a deadline is a knob that can hold a host-side `sbx exec` open for as
/// long as it likes. Clamped, the worst a wrong value does is fail a healthy copy early.
fn artifact_copy_budget() -> Duration {
    const DEFAULT: Duration = Duration::from_secs(300);
    match std::env::var("SKEIN_ARTIFACT_COPY_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(ms) if ms > 0 => Duration::from_millis(ms).min(DEFAULT),
        _ => DEFAULT,
    }
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
    // Through `spawning`, like every other fleet-scope command this crate runs: the argv is built
    // here and spawned here, so a test that reaches this without a stand-in reads a real box's
    // file (SKEIN-764). `Place::command` is not usable instead — stdout goes to a file rather than
    // a pipe, which is the whole point of this function.
    let place = place_of(name).ok_or("invalid box name")?;
    let argv = place.spawning(place.raw_argv(&["cat", guest]));
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        // Its own process group, so the deadline below ends the WORK and not whichever process
        // this side happens to hold a handle on. `sbx` is not a leaf program — it starts work of
        // its own, and the `cat` that actually streams the artifact is somewhere under it — so
        // `child.kill()` here used to leave a wedged copy running with `ppid` 1 and nothing that
        // would reap it (SKEIN-912, SKEIN-916). It has to be a NEW group: the child would
        // otherwise inherit skein's, and a negative kill against that is skein killing itself.
        //
        // **The cost, paid rather than taken**, exactly as `util::run_bounded` states it: a child
        // in its own group no longer shares the terminal's foreground group, so the terminal's
        // Ctrl-C reaches skein and not this command — where before it reached both. The guard
        // below puts this group where `util::forward_interrupts`'s handler will find it, so a
        // Ctrl-C ends it as surely as the deadline does. What this loop does NOT have is
        // `run_bounded`'s grace arm, so a command that ignores `SIGINT` is waited on until the
        // 300s deadline; a second Ctrl-C `SIGKILL`s the group and leaves at once.
        .process_group(0)
        .spawn()
        .map_err(|e| format!("sbx exec not runnable: {e}"))?;
    // Registered before this side can block on anything, so a Ctrl-C arriving between the spawn and
    // the first `try_wait` finds the group rather than an empty table.
    let forwarding = crate::util::forwarding(child.id() as libc::pid_t);
    let started = std::time::Instant::now();
    let budget = artifact_copy_budget();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= budget => {
                // The GROUP, not the pid: see the spawn above. `end_group` reaps as well, or every
                // timeout on the path a sick fleet takes over and over leaves a zombie behind.
                crate::util::end_group(&mut child);
                let _ = fs::remove_file(&tmp);
                let _ = fs::remove_file(&err);
                // `{budget:?}` and not a number: `Duration`'s own `Debug` is the human form, so
                // the default renders word for word as the hand-written sentence this replaces
                // did, and a shortened budget says `4s` or `500ms` rather than a wrong number.
                return Err(format!("copying {guest} exceeded {budget:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                crate::util::end_group(&mut child);
                return Err(format!("waiting for artifact copy: {e}"));
            }
        }
    };
    // Dropped here rather than at the end of the scope: between the reap inside `try_wait` above
    // and this line the handler's table still names a pid the kernel may now reuse.
    drop(forwarding);
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
///
/// **Refuses, and changes nothing, if either `settings.json` is there and will not parse** — the
/// target's because the merge is written back over it, the snapshot's because a takeover that
/// silently carries no settings across is the same loss one step earlier.
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
    // Both settings reads refuse on a file that is there and will not parse, rather than reading it
    // as nothing. The target's is the destructive half — the merge below is written straight back
    // over it, so an unparseable `settings.json` read as `{}` loses every setting the person put in
    // it (the SKEIN-359 class, and the same one `probes::ensure_probe_in` was carrying). The
    // source's is the quiet half: read as absent, a snapshot whose settings will not parse carries
    // *none* of the user's shared context across and reports the takeover as a success. A takeover
    // is exactly the moment somebody is watching and can fix the file, so both say so instead.
    let source_settings: Option<serde_json::Value> =
        crate::util::read_json_or_why(&staging.join("settings.json")).map_err(|why| {
            format!(
                "not merging this snapshot's shared context — skein cannot read its settings \
                 ({why}). Going on would carry none of them into the new box while reporting the \
                 takeover as done. Nothing has been changed; fix or move that file and try again."
            )
        })?;
    if let Some(mut source) = source_settings {
        let settings_path = target_store.join("settings.json");
        // `update_json`: read, merge and write under one lock, and refuse over a target it could
        // not read. `Value::default()` is `Null` for a target nobody has written yet — normalised
        // to `{}` here, because the merge below is a no-op on a non-object and would then write
        // `null` where the old code wrote an object.
        crate::util::update_json::<serde_json::Value, ()>(&settings_path, |target| {
            if !target.is_object() {
                *target = serde_json::json!({});
            }
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
            Ok(())
        })?;
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
    ensure_kit()?;
    // A takeover builds a whole new box, so it builds one the same way everything else does. It used
    // to have a second path — `sbx create`, when no fleet sandbox was named — which handed the
    // replacement its own microVM and left it with no placement record, so skein then addressed it as
    // a sandbox named after itself.
    crate::fleet::start_box(
        &replacement.target,
        &repo,
        &replacement.branch,
        "exec bash -l",
        // A takeover replaces one person's box with another runtime of the same box. The purpose
        // does not change, and naming it here rather than defaulting keeps that a statement.
        crate::place::Purpose::Manual,
    )?;
    // Every `tmux` here is the BOX's server, socket-qualified because the sandbox is shared. Bare,
    // two boxes would both find a live `skein-agent` on the sandbox's one server and the takeover
    // would attach its new runtime to another box's session. Same rule as `agent_attach_argv`.
    // The takeover has just created this box in the fleet, so it has a placement record. Erroring
    // rather than falling back to a sandbox named after it: if that record is missing something went
    // wrong a step earlier, and addressing a guess would report it as a tmux failure three lines down.
    let place = place_of(&replacement.target).ok_or_else(|| {
        format!(
            "{} was created but has no placement record, so skein cannot reach it",
            replacement.target
        )
    })?;
    let tmux = place.tmux();
    let setup_wait = crate::fleet::initial_setup_wait();
    let shell = format!(
        "{setup_wait}command -v {} >/dev/null 2>&1 || {{ echo 'target runtime is missing' >&2; exit 1; }}; command -v tmux >/dev/null 2>&1 || exit 1; {}; {tmux} new-session -d -s skein-agent {:?}; {configure}{tmux} set-option -t skein-agent @skein-agent-contract {TMUX_AGENT_CONTRACT}",
        runtime.info.executable,
        runtime.interactive_setup,
        guarded_agent_command(
            runtime.info.id,
            &crate::runtime::for_box(runtime.interactive_start, &replacement.target),
        ),
        configure = TMUX_CONFIGURE.replace("tmux ", &format!("{tmux} ")),
    );
    sbx_guest_output(&replacement.target, &shell, Duration::from_secs(660)).map(|_| ())
}

pub fn replace_box(source: &str, target_runtime: &str) -> Result<Replacement, String> {
    let replacement = prepare_replacement(source, target_runtime)?;
    launch_replacement(&replacement)?;
    // Into the log skein does not own (§9.5 R6). A takeover hands one agent the work, the branch and
    // the credentials of another, which is the kind of thing somebody reconstructing a week later
    // needs to be able to find — and this is skein's own act rather than one the warden ran.
    // Reported only when it succeeded: a refusal spends nothing and changes nothing, and the
    // failures here are refusals (`prepare_replacement` returns before anything moves).
    crate::warden_client::reported(
        &format!("takeover-{source}"),
        "handed a box to another agent",
        &format!("{source} → {target_runtime}"),
    );
    Ok(replacement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::{save_repos, REPOS_CACHE};
    use crate::testutil::*;
    use std::env;

    /// Copying an artifact out of a box is a crossing like any other, and goes through the seam
    /// (SKEIN-764).
    ///
    /// [`copy_guest_file`] builds its argv with [`crate::place::Place::raw_argv`] and then spawns it
    /// itself, because stdout goes to a FILE — which is why it cannot use `Place::command`, and why
    /// it sat outside both halves of the seam: no substitution, and no refusal. In a test process
    /// that meant a real `nsenter … cat` at the address in the placement record, on the owner's
    /// live fleet, with the failure swallowed by the `?` in the caller.
    ///
    /// **Both directions in one test.** With a stand-in the copy is answered by it, which is the
    /// half that proves the substitution now reaches this path at all; with none it is refused.
    /// Either alone would pass with the fix half-made.
    ///
    /// **What makes it fail:** replacing `place.spawning(place.raw_argv(..))` in
    /// [`copy_guest_file`] with the bare `raw_argv(..)` it used to be. The first half then copies
    /// nothing (the real crossing fails and the host file is not written) and the second comes back
    /// `Ok` instead of refusing.
    #[test]
    fn copying_an_artifact_out_of_a_box_goes_through_the_seam() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        placed("web-main");

        // With a stand-in: answered by it, and the bytes land in the host file. Nothing crossed.
        let landed = home.join("artifact");
        {
            let _stood_in = crate::place::seam::install(Box::new(|argv: &[String]| {
                assert!(
                    argv.iter().any(|a| a == "cat"),
                    "the copy did not offer its own argv to the seam: {argv:?}"
                );
                Some(vec![
                    "sh".into(),
                    "-c".into(),
                    "printf 'from the box'".into(),
                ])
            }));
            copy_guest_file("web-main", "/tmp/whatever", &landed).expect("the stand-in answered");
        }
        assert_eq!(
            fs::read_to_string(&landed).unwrap(),
            "from the box",
            "the substitution did not reach this path, so it is still spawning the real crossing"
        );

        // With none: refused before the spawn, rather than reaching the address in the record.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let answered = std::panic::catch_unwind(|| {
            copy_guest_file("web-main", "/tmp/whatever", &home.join("second"))
        });
        std::panic::set_hook(hook);
        crate::place::forget_place("web-main");
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");

        let said = match answered {
            Ok(outcome) => panic!(
                "a copy with no stand-in installed reached the real box instead of being refused: \
                 {outcome:?}"
            ),
            Err(e) => e
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "<not a string>".into()),
        };
        assert!(
            said.contains("no stand-in is installed"),
            "the refusal has to say what is missing, or it tells a contributor nothing: {said}"
        );
    }

    /// **A wedged artifact copy loses the copy, not only the process skein recorded.**
    ///
    /// [`copy_guest_file`] is the strongest of the four sites SKEIN-916 names: `sbx` starts work of
    /// its own, so a `child.kill()` on the recorded pid left a wedged copy running on the host with
    /// `ppid` 1 and nothing that would reap it — once per takeover of a box that had stopped
    /// answering.
    ///
    /// **Both halves, in that order.** The grandchild is asserted RUNNING while the copy is still
    /// inside its deadline, and gone after it. Without the first half a stand-in that started
    /// nothing at all would pass the second and report the kill working (SKEIN-833).
    ///
    /// **Four seconds rather than five minutes**, through `$SKEIN_ARTIFACT_COPY_MS` — see
    /// [`artifact_copy_budget`] for why the knob exists and why it can only shorten the wait.
    ///
    /// **What makes it fail:** putting back the two lines this replaced — `.process_group(0)` off
    /// the spawn and `let _ = child.kill(); let _ = child.wait();` in place of `end_group`. The
    /// kill then reaches the shell, the `sleep` it started is reparented to init and goes on
    /// running, and `gone` fires naming the pids it can still see.
    #[test]
    fn a_wedged_artifact_copy_takes_its_grandchildren_with_it() {
        let _g = env_lock();
        let home = tempdir();
        // Pinned rather than set: `EnvPins` puts them back from `Drop`, so a failing assertion
        // below does not leave them pointing at a `TempDir` the next test will not find.
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home);
        pins.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        pins.set("SKEIN_ARTIFACT_COPY_MS", "4000");
        placed("web-main");

        let escapee = crate::place::grouptest::escapee(home.as_ref(), "takeover-copy");
        let argv = escapee.argv();
        let _stood_in =
            crate::place::seam::install(Box::new(move |_argv: &[String]| Some(argv.clone())));

        // On a thread, because the assertion that matters first is about the world WHILE the copy
        // is still inside its deadline.
        let landed = home.join("artifact");
        let copying =
            std::thread::spawn(move || copy_guest_file("web-main", "/tmp/whatever", &landed));

        let pid = escapee.there();
        let outcome = copying.join().expect("the copying thread panicked");
        let said = outcome.expect_err("a script that sleeps for 600s came back inside 4s");
        assert!(
            said.contains("exceeded"),
            "the deadline is not what ended this, so what follows is not about the deadline: {said}"
        );
        escapee.gone(pid);
        crate::place::forget_place("web-main");
    }

    /// The branch and the commit come from the BOX, through its placement.
    ///
    /// **Observed through the execution seam.** The oracle used to be a fake `sbx` on `$PATH`
    /// recording what it was handed; with no hop to intercept it would be bypassed and these `git`
    /// commands would run against whatever checkout the suite is standing in (SKEIN-592). The seam
    /// records the same argv and answers it the same way.
    #[test]
    fn preparing_a_takeover_asks_the_source_box_itself() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        // **A fleet root of this test's own, and it was not needed until the artifact copy stopped
        // failing** (SKEIN-764). `copy_guest_file` used to spawn its `nsenter … cat` straight past
        // the seam: the real command ran, could not reach `/boxes/web-main`, and the `?` on it
        // returned `prepare_replacement` before it got as far as `prepare_handoff_for`. So the
        // reach was real, the error was swallowed, and the half of this function below the copy
        // was never executed by this test at all. With the copy going through `Place::spawning`
        // the stand-in answers it, execution continues, and `prepare_handoff_for` →
        // `digest::session_digest` → `sbx::box_liveness` → `fleet::fleet_liveness` resolves the
        // fleet root — which unpinned is `/boxes`, the owner's live fleet, and `util::fleet_root`
        // refuses it. Note `placed` writes a LITERAL `/boxes/web-main/tree` into the placement
        // record, so pinning this does not move the address the assertions below read.
        env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        // The source box has to be reachable to be asked, which now means it has to be placed.
        placed("web-main");
        env::set_var("SKEIN_LS_CMD", "false");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let log = home.join("argv.log");
        let log_at = log.clone();
        let _stood_in = crate::place::seam::install(Box::new(move |argv: &[String]| {
            use std::io::Write;
            let all = argv.join(" ");
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_at)
                .unwrap();
            writeln!(f, "{all}").unwrap();
            let answer = if all.contains("abbrev-ref") {
                "echo feat/auth"
            } else if all.contains("rev-parse HEAD") {
                "echo 0123456789abcdef"
            } else {
                ":"
            };
            Some(vec!["sh".to_string(), "-c".into(), answer.to_string()])
        }));

        // Refusals come before anything is spent, and each names what is actually wrong.
        let e = prepare_replacement("web-main", "claude").unwrap_err();
        assert!(e.contains("already uses"), "{e}");
        let e = prepare_replacement("web-main", "codex").unwrap_err();
        assert!(e.contains("managed repo"), "unregistered box: {e}");
        assert!(!log.exists(), "a refusal must not have touched the box");

        save_repos(&[Repo {
            read_prs: false,
            id: "web".into(),
            source: "/src/web".into(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();
        fs::create_dir_all(home.join("work")).unwrap();
        let _ = prepare_replacement("web-main", "codex");

        let asked = fs::read_to_string(&log).unwrap_or_default();
        // Addressed through the placement, because a box is not a sandbox: the crossing is
        // `nsenter` into the namespace the record names, and it lands in that box's own tree.
        // `exec web-main` was right only while every box had a VM of its own — and the hop that
        // used to precede it, `exec skein-fleet`, is gone with the host (SKEIN-576).
        assert!(
            asked.contains("nsenter") && asked.contains("/boxes/web-main/tree"),
            "the box is reached through its placement: {asked}"
        );
        assert!(
            !asked.split_whitespace().any(|w| w == "sbx"),
            "a hop into the sandbox came back: {asked}"
        );
        assert!(
            asked.contains("git rev-parse --abbrev-ref HEAD"),
            "the branch must come from the BOX, not the host clone — it is a different checkout: {asked}"
        );
        assert!(
            asked.contains("git rev-parse HEAD"),
            "and so must the commit it snapshots: {asked}"
        );

        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");
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

    /// **A takeover never writes its merge over a `settings.json` it could not read** — neither the
    /// new box's nor the old box's.
    ///
    /// Both reads were `read_to_string(..).ok().and_then(from_str(..).ok())`, which answers "not
    /// there" to a file that is very much there. On the target that turned the merge into a
    /// deletion of every setting in it; on the snapshot it made a takeover that carried none of
    /// the user's settings across report itself as done.
    ///
    /// **What would make this fail:** put either of those two `.ok().and_then(..)` reads back. The
    /// target's breaks the first `expect_err` and the byte comparison under it; the snapshot's
    /// breaks the third block, where an unreadable snapshot is silently skipped and the call
    /// returns `Ok`. Both done, both watched fail, both restored.
    #[test]
    fn a_takeover_refuses_over_settings_it_cannot_read() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let root = tempdir();

        // The snapshot the takeover reads: `shared-context.tgz`, exactly as `prepare_replacement`
        // leaves it, carrying the old box's settings.
        let source = root.join("source-store");
        fs::create_dir_all(&source).unwrap();
        let tarball = root.join("snapshot").join("shared-context.tgz");
        fs::create_dir_all(tarball.parent().unwrap()).unwrap();
        let snapshot = root.join("snapshot");
        let pack = |body: &str| {
            fs::write(source.join("settings.json"), body).unwrap();
            let ok = Command::new("tar")
                .arg("-czf")
                .arg(&tarball)
                .arg("-C")
                .arg(&source)
                .arg("settings.json")
                .status()
                .expect("tar to run")
                .success();
            assert!(ok, "could not build the snapshot archive");
        };
        pack(r#"{"model": "opus", "hooks": {}}"#);

        let target = root.join("target-store");
        fs::create_dir_all(&target).unwrap();
        let settings = target.join("settings.json");

        // 1. The destructive half: the new box's own settings will not parse.
        const THEIRS: &str = "{\n  \"permissions\": { \"allow\": [\"Bash(git status)\"] },,\n}\n";
        fs::write(&settings, THEIRS).unwrap();
        let why = merge_shared_context(&snapshot, &target)
            .expect_err("the merge went over settings it could not read");
        assert!(
            why.contains("settings.json"),
            "the refusal has to name the file: {why}"
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            THEIRS,
            "the target's unreadable settings were replaced by the merge"
        );

        // 2. The half the refusal is closest to breaking: a target with no settings yet still
        // takes the merge, and comes away with the source's keys *and* skein's defaults (its hooks
        // load from skein's plugin, not from here — SKEIN-1062).
        fs::remove_file(&settings).unwrap();
        merge_shared_context(&snapshot, &target).expect("a fresh target refused the merge");
        let merged: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            merged["model"], "opus",
            "the old box's settings did not reach the new one: {merged}"
        );
        assert!(
            merged["statusLine"].is_object(),
            "the new box came away without skein's settings: {merged}"
        );

        // 3. The quiet half: the *snapshot's* settings will not parse. Silently carrying none of
        // them across and reporting success is the same loss, one step earlier.
        pack("{\"model\": \"opus\",,}");
        let before = fs::read_to_string(&settings).unwrap();
        let why = merge_shared_context(&snapshot, &target)
            .expect_err("an unreadable snapshot was taken as a snapshot with nothing in it");
        assert!(
            why.contains("cannot read"),
            "the refusal has to say what could not be read: {why}"
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            before,
            "a refused merge still changed the target"
        );

        env::remove_var("SKEIN_HOME");
    }
}
