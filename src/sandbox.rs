//! The `sbx` seam: everything skein does *to* a sandbox.
//!
//! Every call into a box funnels through a handful of helpers here — [`sbx_guest_output`],
//! [`guest_write`], the launch/attach argv builders and the lifecycle commands. That is deliberate:
//! it is the one place that knows a box is a sandbox, so changing what backs a box is a change to
//! this module rather than to every feature that touches one.

use crate::config::*;
use crate::fleet::box_root;
use crate::place::{forget_place, own_sandbox, place_of, shared_record};
use crate::runtime::*;
use crate::util::*;
use crate::{
    agent_for_box, ai_says_hold, box_liveness, branch_from_box, ensure_kit, ensure_store,
    fleet_boxes, locate_registry, parse_registry, repo_for_box, store_dir, store_for_box,
    sync_revoke_token, valid_name, write_launch_spec_for_agent, Liveness, Repo,
};
use chrono::Utc;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// The host shell command that launches a new box for `branch`. Override with
/// $SKEIN_LAUNCH_CMD (a template; `{branch}` is substituted); default assumes
/// `setup-sandbox.sh` is on PATH.
pub fn launch_command(name: &str, branch: &str) -> String {
    launch_command_with_agent(name, branch, None)
}

/// Build a launch command with an optional per-box runtime override. The repo's configured agent
/// remains the default; the cockpit uses this seam when the New box dialog explicitly selects
/// Claude or Codex.
pub fn launch_command_with_agent(name: &str, branch: &str, agent: Option<&str>) -> String {
    if let Ok(t) = env::var("SKEIN_LAUNCH_CMD") {
        if !t.is_empty() {
            return t
                .replace("{branch}", &sh_quote(branch))
                .replace("{name}", &sh_quote(name));
        }
    }
    native_launch_command(name, branch, agent)
}

/// skein's own launch command, used when `$SKEIN_LAUNCH_CMD` is unset — so a box can be created
/// without the repo shipping a `setup-sandbox.sh`. It creates without attaching, then opens the
/// runtime in Skein's persistent tmux session:
///   `sbx create --clone [--kit <kit>] --name <name> <agent> . <store> && sbx exec … tmux …`
/// The box's bootstrap derives the branch from the name (`thing-<branch>` → `<branch>`) and checks
/// it out, so no branch arg is needed. `agent` (`$SKEIN_AGENT`, default `claude`) is the per-runtime
/// seam; `kit` (`$SKEIN_KIT`, resolved under `$SKEIN_REPO`) wires the shared store into the clone and
/// runs the bootstrap; `store` (`$SKEIN_STORE`, else the store skein already reads) is mounted so the
/// kit can link it. Runs with cwd `$SKEIN_REPO`, so `.` is the repo workspace.
pub(crate) fn native_launch_command(
    name: &str,
    branch: &str,
    agent_override: Option<&str>,
) -> String {
    // Repo-managed path: if the box belongs to a registered repo, build entirely from `repos.json`
    // + skein's own kit — no `SKEIN_REPO`/`SKEIN_KIT` env, no repo-side script.
    if let Some(repo) = repo_for_box(name) {
        return repo_launch_command_as(name, &repo, branch, agent_override);
    }
    let agent = agent_override
        .map(str::to_string)
        .or_else(|| env::var("SKEIN_AGENT").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "claude".into());
    let mut parts: Vec<String> = vec!["sbx".into(), "create".into(), "--clone".into()];
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
    persistent_launch_command(parts, name, &agent)
}

/// Launch line for a registered repo, built from `repos.json` + skein's embedded kit:
///   `sbx create --clone --kit <home>/kit --name <id>-<branch> <agent> <work> <store>`
/// The agent positional is a registered sbx agent **name** (`sbx create` only accepts the built-in set:
/// claude, codex, …; each has its own image, so it can't be a path or a wrapper command). It's the
/// repo's `agent` (`$SKEIN_AGENT` overrides). `<work>` is the host clone; `<store>` is mounted at its
/// host path so the kit links it in. The kit checks out the branch (from the launch spec) before the
/// agent starts. `sbx create` provisions the image without starting its entrypoint; Skein then uses
/// `sbx exec` to start the agent in the same `skein-agent` tmux session used by every later attach.
/// Closing or reloading the creation terminal therefore cannot kill or fork an in-progress turn.
/// Side effect: writes the launch spec + ensures kit/store (best-effort; a failure only logs, the
/// command still builds).
pub(crate) fn repo_launch_command_as(
    name: &str,
    repo: &Repo,
    branch: &str,
    agent_override: Option<&str>,
) -> String {
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
    let kit = skein_home().join("kit");
    let agent = agent_override
        .map(str::to_string)
        .or_else(|| env::var("SKEIN_AGENT").ok().filter(|s| !s.is_empty()))
        .or_else(|| (!repo.agent.is_empty()).then(|| repo.agent.clone()))
        .unwrap_or_else(|| "claude".into());
    if let Err(e) = write_launch_spec_for_agent(name, &branch, repo, &agent) {
        eprintln!("skein: write_launch_spec: {e}");
    }
    let parts = vec![
        "sbx".to_string(),
        "create".into(),
        "--clone".into(),
        "--kit".into(),
        sh_quote(&kit.to_string_lossy()),
        "--name".into(),
        sh_quote(name),
        sh_quote(&agent), // registered sbx agent name (claude | codex | …)
        sh_quote(&repo.work),
        sh_quote(&repo.store),
    ];
    persistent_launch_command(parts, name, &agent)
}

/// Join a completed `sbx create` argv with the first tmux-backed agent attach. Each argument is
/// shell-quoted because the terminal launch path executes this compound command via `sh -c`.
pub(crate) fn persistent_launch_command(create: Vec<String>, name: &str, agent: &str) -> String {
    let attach = initial_attach_argv_as(name, agent)
        .into_iter()
        .map(|arg| sh_quote(&arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} && sbx {attach}", create.join(" "))
}

/// Resolve a possibly-relative path against `$SKEIN_REPO` (the dir launches run in), so a relative
/// `$SKEIN_KIT` behaves like a relative `$SKEIN_LAUNCH_CMD`.
pub(crate) fn resolve_under_repo(p: &str) -> String {
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
pub(crate) fn launch_store() -> Option<String> {
    if let Some(s) = env::var("SKEIN_STORE").ok().filter(|s| !s.is_empty()) {
        return Some(s);
    }
    store_dir().map(|p| p.to_string_lossy().into_owned())
}

/// A fresh drop-batch id: millis-since-epoch + a process-local counter (no collisions within a run).
pub(crate) fn next_drop_id() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}-{n}")
}

/// Where a dropped file lands in the box: `(dir, path)` under `/tmp/skein-drop-<batch>/`. `rel` is
/// the client's name for it and may carry subdirectories (a dropped folder arrives one file at a
/// time, each with its path relative to the folder); every component is sanitised, so the result is
/// always inside the batch dir. An empty/unusable `batch` gets a fresh id.
pub fn drop_dest(batch: &str, rel: &str) -> Result<(String, String), String> {
    // The batch id is skein's own (the UI mints `<millis>-<n>` in base36), so it's held to a stricter
    // alphabet than a user's filename: no dots at all, which keeps the drop root a plain single name.
    let batch: String = batch
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(40)
        .collect();
    let batch = if batch.trim_matches(['-', '_']).is_empty() {
        next_drop_id()
    } else {
        batch
    };
    let mut parts: Vec<String> = rel
        .split(['/', '\\'])
        .map(safe_component)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() > 24 {
        return Err("drop path too deep".into());
    }
    let file = parts.pop().unwrap_or_default();
    let file = if file.is_empty() {
        "file".to_string()
    } else {
        file
    };
    let mut dir = format!("/tmp/skein-drop-{batch}");
    for p in &parts {
        dir.push('/');
        dir.push_str(p);
    }
    let path = format!("{dir}/{file}");
    if path.len() > 512 {
        return Err("drop path too long".into());
    }
    Ok((dir, path))
}

/// The argv that streams stdin into `path` inside `name`'s sandbox, creating `dir` first (that's how
/// a folder drop recreates its tree). `-i` and *not* `-t`: a pty would mangle the binary bytes.
pub fn box_write_argv(name: &str, dir: &str, path: &str) -> Result<Vec<String>, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let inner = format!("mkdir -p {} && cat > {}", sh_quote(dir), sh_quote(path));
    Ok(vec![
        "exec".into(),
        "-i".into(),
        name.into(),
        "sh".into(),
        "-c".into(),
        inner,
    ])
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
    let boxes = fleet_boxes().ok_or("cannot verify box state (`sbx ls` unavailable)")?;
    let live = boxes
        .iter()
        .find(|b| b.name == name)
        .ok_or_else(|| format!("box {name:?} does not exist"))?
        .live;
    if live != Some(Liveness::Running) {
        return Err(format!(
            "box {name:?} is not running; attach/start it before resuming"
        ));
    }
    let p = if prompt.trim().is_empty() {
        "Yes, please proceed."
    } else {
        prompt
    };
    let agent = agent_for_box(name);
    let runtime =
        runtime_adapter(&agent).ok_or_else(|| format!("box uses unsupported runtime {agent:?}"))?;
    let inner = match env::var("SKEIN_RESUME_CMD") {
        Ok(c) if !c.is_empty() => c
            .replace("{name}", &sh_quote(name))
            .replace("{prompt}", &sh_quote(p))
            .replace("{runtime}", &sh_quote(runtime.info.id)),
        _ => {
            let guest = runtime.headless_resume.replace("{prompt}", &sh_quote(p));
            format!("sbx exec {} bash -lc {}", sh_quote(name), sh_quote(&guest))
        }
    };

    // Keep a durable log and observe the child briefly. The old `nohup … &` only proved that a
    // shell forked, so a missing CLI or rejected resume was reported as success. Here an immediate
    // non-zero exit is surfaced; a healthy long-running agent is reaped by a tiny waiter thread.
    let store = store_for_box(name).ok_or("can't locate the box's shared store")?;
    let status_dir = store.join("status");
    fs::create_dir_all(&status_dir).map_err(|e| format!("mkdir {}: {e}", status_dir.display()))?;
    let log_path = status_dir.join(format!("{name}.resume.log"));
    let mut stdout =
        fs::File::create(&log_path).map_err(|e| format!("create {}: {e}", log_path.display()))?;
    {
        use std::io::Write as _;
        let _ = writeln!(
            stdout,
            "skein resume: ts={} runtime={}",
            Utc::now().to_rfc3339(),
            runtime.info.id
        );
    }
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("clone {}: {e}", log_path.display()))?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&inner)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("resume launch: {e}"))?;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(500) {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let detail = fs::read_to_string(&log_path).unwrap_or_default();
                return Err(format!(
                    "resume exited {}{}; see {}",
                    status.code().unwrap_or(-1),
                    if detail.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", detail.trim())
                    },
                    log_path.display()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("resume status: {e}")),
        }
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// ---------- step 7: rationed, lazy AI enrichment (subscription `claude -p`, no API key) ----------

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
    // A shared box has no sandbox of its own to stop. `sbx stop <box>` would either fail or — worse
    // — stop an unrelated sandbox that happens to carry the same name. Killing its tmux server ends
    // every process in the box, which frees the namespace, and leaves the tree for a later restart.
    if let Some(rec) = shared_record(name) {
        let sock = sh_quote(&rec.sock);
        let script = format!("tmux -S {sock} kill-server 2>/dev/null; rm -f {sock}; exit 0");
        return own_sandbox(&rec.sandbox)
            .exec(&script, Duration::from_secs(30))
            .map(|_| ());
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
pub(crate) fn delist_box(name: &str) -> Result<(), String> {
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
        let mut v = parse_registry(&data).map_err(|e| format!("parsing registry: {e}"))?;
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
    // Drop the box's per-box *live* runtime files, so a destroyed box leaves nothing stale behind:
    // its turn-state probe output and its launch spec. Best-effort — a missing file is fine.
    //
    // Deliberately NOT deleted here: journals/<name>.md, diffs/<name>.*, tasks/<name>.json. A
    // --clone's own working tree (and its .skein/journal.md) dies with the box, so the store copies
    // are the only durable record of what that box did — they feed the cross-run workflow/process
    // learn-loop and must outlive the box, not just its live session.
    for p in [
        store.join("status").join(format!("{name}.json")),
        store.join("status").join(format!("{name}.pane.json")),
        store.join("status").join(format!("{name}.agents")),
        store.join("status").join(format!("{name}.agents.lock")),
        store
            .join("skein")
            .join("launch")
            .join(format!("{name}.json")),
    ] {
        let _ = fs::remove_file(&p);
    }
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
    // Before the box goes: a bearer token outlives the filesystem it was written to, so a destroy
    // that leaves it live hands the fleet a credential nobody is holding any more. Best-effort by
    // design — an unreachable gateway must not strand a box on the board.
    if let Err(e) = sync_revoke_token(name) {
        eprintln!("skein: destroying {name}, but revoking its tracker token failed — revoke it by hand at the gateway: {e}");
    }
    if let Some(rec) = shared_record(name) {
        // Same reasoning as stop_box, and this one REMOVES — `sbx rm -f <box>` aimed at a sandbox
        // that shares the box's name would destroy someone else's work. Kill the server, then the
        // tree: the checkout is VM-local, so this is the destructive step `destroy_command`
        // documents, just aimed at the right thing.
        let sock = sh_quote(&rec.sock);
        let root = sh_quote(&box_root(name));
        let script = format!("tmux -S {sock} kill-server 2>/dev/null; rm -rf {root}; exit 0");
        own_sandbox(&rec.sandbox).exec(&script, Duration::from_secs(120))?;
        forget_place(name);
        if let Err(e) = delist_box(name) {
            eprintln!("skein: destroyed {name}, but delisting it failed (harmless): {e}");
        }
        return Ok(());
    }
    let (_out, err, code) = run_shell(&destroy_command(name))?;
    if code != 0 {
        return Err(format!("teardown failed (exit {code}): {}", err.trim()));
    }
    // The sandbox is gone (`sbx rm` succeeded). Delisting is just bookkeeping, and `sbx ls` is the
    // fleet source of record — so a stale/unparseable registry must NOT fail the destroy, which would
    // leave the box's tab open over a sandbox that no longer exists. Log and move on; the next
    // successful delist (or a registry self-heal) cleans up the leftover entry.
    if let Err(e) = delist_box(name) {
        eprintln!("skein: destroyed {name}, but delisting it from the registry failed (harmless — sbx ls is the source of record): {e}");
    }
    Ok(())
}

pub(crate) fn sbx_guest_output(
    name: &str,
    shell: &str,
    timeout: Duration,
) -> Result<String, String> {
    place_of(name)
        .ok_or("invalid box name")?
        .exec(shell, timeout)
}

// ---------- work tracking: wiring a box to the sync gateway ----------
// Boxes share a backlog through `sync` — Plane as the system of record, behind a gateway that adds
// the one thing Plane cannot do: an atomic claim, so two boxes never work the same item. An agent
// reaches it as an MCP server, which needs two values: the gateway URL and an agent token.
//
// The token is per box, and what that buys is the LEASE, not Plane attribution. The gateway stores
// the minter's Plane token against each agent, so every box provisioned from one PAT writes to Plane
// as that human — `<owner>/<box>` is the gateway's holder string, not a Plane user. What one token
// per box gives you is a distinct *holder*: with a shared token two boxes would both hold every item
// they claimed, which is precisely the failure the gateway exists to prevent. It also makes
// revocation per box, so retiring one box does not disarm the fleet.
//
// A box is never handed the Plane PAT itself: that would let it set `assignees` directly and walk
// around the claim.
//
// Nothing here runs on a tick. Provisioning is an explicit act (`sync_provision_box`), for the same
// reason verification is: it spends a network round trip and mints a real credential.

/// Run a command in a box with `stdin` fed from a string. The captured-output sibling of
/// [`sbx_guest_output`], for the case where the payload must not be an argument.
pub(crate) fn guest_write(
    name: &str,
    shell: &str,
    stdin: &str,
    timeout: Duration,
) -> Result<(), String> {
    place_of(name)
        .ok_or("invalid box name")?
        .write(shell, stdin.as_bytes(), timeout)
}

// ---------- transcript: the conversation as the RECORD has it, not as the screen had it ----------
// A box's rendered terminal is the most fragile copy of its conversation: it dies with the browser
// tab, with a skein-server restart, with the tmux session, and silently with the scrollback limit.
// On 2026-07-31 a box rebooted, `claude --continue` restored the conversation from disk, and the
// screen came back with `history_size 0` — the agent remembered everything, the human could read
// none of it. The runtimes already keep the real record as JSONL inside the box; this reads that.
//
// Discovery is by mtime rather than a probe-recorded path deliberately: it works on a box whose
// probes were never installed, and needs no reattach to start working.

/// The `sbx` argv (sans the leading `sbx`, which the server prepends) that opens box `name`'s agent
/// terminal. `_dir` is unused for the default invocation but kept so the `{dir}` override stays uniform.
///
/// The agent runs inside a persistent `skein-agent` tmux session so its live process survives a
/// browser disconnect — sbx has no live-process attach of its own, so without this a closed tab kills
/// the agent mid-turn. A has-session → detached-create → configure → attach sequence is the seam: it
/// reattaches when alive and creates with the adapter's resume command only when missing. Detached
/// creation lets Skein hide tmux chrome and configure scrolling before the browser attaches. The
/// resume command is per-agent: claude → `claude --continue` (picks the
/// transcript back up); Codex uses `resume --last`. If the tmux process is still alive, this command
/// is not run at all — the client attaches directly to the in-progress process.
/// A missing tmux is a broken Skein box contract, not a reason to start a second direct process.
/// Mirror of [`shell_argv`], which backs the shell tab the same way. Override wholesale with
/// `$SKEIN_ATTACH_CMD`.
pub fn attach_argv(name: &str, _dir: &str) -> Vec<String> {
    let agent = agent_for_box(name);
    attach_argv_as(name, _dir, &agent)
}

/// Attach using an explicit runtime. Normal operation uses the configured runtime in `skein-agent`.
/// A different runtime remains available as a low-level compatibility path; product takeover uses
/// [`replace_box`] so boxes stay single-runtime.
pub fn attach_argv_as(name: &str, _dir: &str, agent: &str) -> Vec<String> {
    let runtime = resolve_runtime(agent);
    let agent = runtime.info.id;
    let tmux_name = agent_session_name(name, agent);
    agent_attach_argv(name, runtime, &tmux_name, runtime.interactive_resume, false)
}

/// The first agent attach after `sbx create`. It deliberately uses the same primary tmux session
/// name as all future reconnects, but starts a new native conversation instead of asking the
/// provider to resume some unrelated prior transcript.
pub(crate) fn initial_attach_argv_as(name: &str, agent: &str) -> Vec<String> {
    let runtime = resolve_runtime(agent);
    agent_attach_argv(
        name,
        runtime,
        "skein-agent",
        runtime.interactive_start,
        true,
    )
}

pub(crate) fn agent_attach_argv(
    name: &str,
    runtime: &RuntimeAdapter,
    tmux_name: &str,
    command: &str,
    wait_for_setup: bool,
) -> Vec<String> {
    let agent = runtime.info.id;
    let executable = runtime.info.executable;
    let setup_wait = if wait_for_setup {
        INITIAL_SETUP_WAIT
    } else {
        ""
    };
    let instruction = agent_instruction_setup(runtime);
    let command = guarded_agent_command(agent, command);
    let place = place_of(name).unwrap_or_else(|| own_sandbox(name));
    // Every `tmux` below is this box's server: bare under the original model, socket-qualified when
    // the sandbox is shared. Session names are identical either way, so without the socket two boxes
    // would both find a live `skein-agent` on the sandbox's one server and attach to each other's.
    let tmux = place.tmux();
    let observer = pane_observer_start(tmux_name, place.tmux_sock());
    let configure = TMUX_CONFIGURE.replace("tmux ", &format!("{tmux} "));
    let shell = format!(
        "{setup_wait}if ! command -v {executable} >/dev/null 2>&1; then echo 'skein: {agent} is not installed in this sandbox image; create a {agent} box or install/authenticate the CLI here to take over'; exec bash -li; fi; \
         if ! command -v tmux >/dev/null 2>&1; then echo 'skein: tmux is required for durable sessions but is missing; recreate this box or install tmux'; exit 1; fi; \
         {setup}; \
         created=0; if ! {tmux} has-session -t {tmux_name} 2>/dev/null; then {instruction}; {update}; {tmux} new-session -d -s {tmux_name} {command:?}; created=1; fi; \
         if [ \"$created\" = 1 ]; then {tmux} set-option -t {tmux_name} @skein-agent-contract {TMUX_AGENT_CONTRACT}; fi; \
         {observer} \
         {configure}exec {tmux} -u attach-session -t {tmux_name}",
        setup = runtime.interactive_setup,
        update = runtime.update_before_start,
    );
    place.interactive_argv(&shell)
}

/// Stop one runtime's persistent tmux process without touching the sandbox or another provider's
/// native session. Reattaching recreates it through that adapter's native resume command.
pub fn restart_agent_session(name: &str, runtime: Option<&str>) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let runtime = runtime
        .map(str::to_string)
        .unwrap_or_else(|| agent_for_box(name));
    let runtime = runtime.as_str();
    if !valid_runtime(runtime) {
        return Err(format!("unsupported runtime {runtime:?}"));
    }
    if box_liveness(name) != Some(Liveness::Running) {
        return Err(format!("box {name:?} is not running"));
    }
    let session = agent_session_name(name, runtime);
    let place = place_of(name).ok_or("invalid box name")?;
    // `session` is built from a validated box name and a validated runtime, so it is safe to spell
    // into a shell string here — and going through the place is what aims kill-session at this
    // box's own server rather than whichever one answers on the sandbox's default socket.
    let argv = place.exec_argv(&format!("{} kill-session -t {session}", place.tmux()));
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let (out, err, code) = run_capture(&argv[0], &args)?;
    if code == 0 {
        Ok(())
    } else {
        let detail = if err.trim().is_empty() { out } else { err };
        Err(if detail.trim().is_empty() {
            format!("could not restart {runtime} session")
        } else {
            detail.trim().to_string()
        })
    }
}

/// The shell command that (re)starts an agent, resuming prior history when the runtime supports it —
/// run inside the `skein-agent` tmux session by [`attach_argv`] (the per-runtime seam). Claude uses
/// `--continue`; Codex uses `resume --last`; future runtimes add their command in the adapter table.
#[cfg(test)]
pub(crate) fn agent_resume_cmd(agent: &str) -> String {
    runtime_adapter(agent)
        .map(|runtime| runtime.interactive_resume.to_string())
        .unwrap_or_else(|| agent.to_string())
}

/// `sbx` argv for an interactive *shell* in the box — a plain terminal to run commands in, separate
/// from the agent session. Uses a persistent `skein-shell` tmux session when tmux is present (so this
/// terminal survives reconnects). tmux is part of the managed-box contract; refusing to open a
/// direct shell avoids presenting a terminal whose process dies on reload. Override the whole
/// command with $SKEIN_SHELL_CMD (`sh -c`).
pub fn shell_argv(name: &str) -> Vec<String> {
    let place = place_of(name).unwrap_or_else(|| own_sandbox(name));
    let tmux = place.tmux();
    let configure = TMUX_CONFIGURE.replace("tmux ", &format!("{tmux} "));
    place.interactive_argv(&format!(
        "if ! command -v tmux >/dev/null 2>&1; then echo 'skein: tmux is required for durable sessions but is missing; recreate this box or install tmux'; exit 1; fi; if ! {tmux} has-session -t skein-shell 2>/dev/null; then {tmux} new-session -d -s skein-shell; fi; {configure}exec {tmux} -u attach-session -t skein-shell"
    ))
}
