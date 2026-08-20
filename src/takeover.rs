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
use crate::repos::{agent_for_box, box_name, ensure_gh_secret, load_repos, repo_for_box, Repo};
use crate::runtime::{
    guarded_agent_command, runtime_adapter, valid_runtime, INITIAL_SETUP_WAIT, TMUX_AGENT_CONTRACT,
    TMUX_CONFIGURE,
};
use crate::sandbox::sbx_guest_output;
use crate::util::{bounded_output, expand_tilde, slug, write_atomic};
use crate::{fleet_boxes, lookup_dir, valid_name};
use chrono::Utc;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
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
    // Through `lookup_dir`, which knows about placed boxes too: asking `sbx ls` alone meant a
    // takeover of any fleet box whose name does not match `<repo>-<branch>` refused outright.
    let dir = lookup_dir(name)?;
    let wanted = PathBuf::from(expand_tilde(&dir));
    let wanted = wanted.canonicalize().unwrap_or(wanted);
    load_repos().into_iter().find(|repo| {
        let work = PathBuf::from(&repo.work);
        work.canonicalize().unwrap_or(work) == wanted
    })
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
    let fleet = fleet_sandbox();
    if !fleet.is_empty() {
        existing.extend(placed_boxes(&fleet).into_iter().map(|(name, _)| name));
    }
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
    ensure_kit()?;
    let _ = ensure_gh_secret();
    // A takeover builds a whole new box, so it builds one the same way everything else does. It used
    // to have a second path — `sbx create`, when no fleet sandbox was named — which handed the
    // replacement its own microVM and left it with no placement record, so skein then addressed it as
    // a sandbox named after itself.
    crate::fleet::start_box(
        &replacement.target,
        &repo,
        &replacement.branch,
        "exec bash -l",
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
    let shell = format!(
        "{INITIAL_SETUP_WAIT}command -v {} >/dev/null 2>&1 || {{ echo 'target runtime is missing' >&2; exit 1; }}; command -v tmux >/dev/null 2>&1 || exit 1; {}; {}; {tmux} new-session -d -s skein-agent {:?}; {configure}{tmux} set-option -t skein-agent @skein-agent-contract {TMUX_AGENT_CONTRACT}",
        runtime.info.executable,
        runtime.interactive_setup,
        runtime.update_before_start,
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
    Ok(replacement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::{save_repos, REPOS_CACHE};
    use crate::testutil::*;
    use std::env;

    #[test]
    fn preparing_a_takeover_asks_the_source_box_itself() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        // The source box has to be reachable to be asked, which now means it has to be placed.
        placed("web-main");
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
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        fs::create_dir_all(home.join("work")).unwrap();
        let _ = prepare_replacement("web-main", "codex");

        let asked = fs::read_to_string(&log).unwrap_or_default();
        // Addressed through the placement — `exec skein-fleet nsenter … ` — because a box is not a
        // sandbox. `exec web-main` was right only while every box had a VM of its own.
        assert!(
            asked.contains("exec skein-fleet") && asked.contains("nsenter"),
            "the box is reached through its placement: {asked}"
        );
        assert!(
            asked.contains("git rev-parse --abbrev-ref HEAD"),
            "the branch must come from the BOX, not the host clone — it is a different checkout: {asked}"
        );
        assert!(
            asked.contains("git rev-parse HEAD"),
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
}
