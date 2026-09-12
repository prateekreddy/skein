//! The `sbx` seam: everything skein does *to* a sandbox.
//!
//! Every call into a box funnels through a handful of helpers here — [`sbx_guest_output`],
//! `guest_write`, the launch/attach argv builders and the lifecycle commands. That is deliberate:
//! it is the one place that knows a box is a sandbox, so changing what backs a box is a change to
//! this module rather than to every feature that touches one.

use crate::ai::ai_says_hold;
use crate::fleet::{box_root, box_state};
use crate::kit::{ensure_kit, ensure_store};
use crate::place::{forget_place, own_sandbox, place_of, shared_record};
use crate::registry::{parse_registry, store_for_box};
use crate::repos::{
    agent_for_box, branch_from_box, launch_spec, repo_for_box, write_launch_spec_for_agent, Repo,
};
use crate::runtime::*;
use crate::sbx::{box_liveness, Liveness};
use crate::tracking::sync_revoke_token;
use crate::util::valid_name;
use crate::util::*;
use chrono::Utc;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
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
    launch_command_as(name, branch, agent, Attach::Yes)
}

/// Whether the launch ends by attaching to the box's agent session.
///
/// `Yes` is a terminal creating a box: the person is watching, and when it comes up they are already
/// in it. `No` is any surface that is not a terminal — it wants the box created and will attach
/// separately, or not at all. The difference is one flag on the command and it is the whole of what
/// made box creation reachable only from a WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    Yes,
    No,
}

/// The command that creates a box, attaching or not.
pub fn launch_command_as(name: &str, branch: &str, agent: Option<&str>, attach: Attach) -> String {
    if let Ok(t) = env::var("SKEIN_LAUNCH_CMD") {
        if !t.is_empty() {
            return t
                .replace("{branch}", &sh_quote(branch))
                .replace("{name}", &sh_quote(name));
        }
    }
    native_launch_command(name, branch, agent, attach)
}

/// skein's own launch command, used when `$SKEIN_LAUNCH_CMD` is unset.
///
/// A box belongs to a **registered repo** — that is the only shape there is. It used to be possible to
/// create one outside repos.json, from `$SKEIN_REPO`/`$SKEIN_KIT` and a `sandboxes.json`, and that
/// path created the box as its own microVM: `sbx create --clone …`. Both went together, because a box
/// in the shared sandbox is built by a sequence of steps that needs a repo to clone from and a store to
/// mount, and the env-var mode supplied neither in a form skein could resolve per box.
///
/// So an unregistered name is refused here, in the command itself, where the message reaches the
/// terminal that asked. `skein add` is the whole of the fix.
pub(crate) fn native_launch_command(
    name: &str,
    branch: &str,
    agent_override: Option<&str>,
    attach: Attach,
) -> String {
    match repo_for_box(name) {
        Some(repo) => repo_launch_command_as(name, &repo, branch, agent_override, attach),
        None => format!(
            "echo 'skein: {} belongs to no registered repo, so there is nothing to create it from. \
             Register one with: skein add <git-url>' >&2; exit 1",
            name
        ),
    }
}

pub(crate) fn repo_launch_command_as(
    name: &str,
    repo: &Repo,
    branch: &str,
    agent_override: Option<&str>,
    attach: Attach,
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
    let agent = agent_override
        .map(str::to_string)
        .or_else(|| env::var("SKEIN_AGENT").ok().filter(|s| !s.is_empty()))
        .or_else(|| (!repo.agent.is_empty()).then(|| repo.agent.clone()))
        .unwrap_or_else(|| "claude".into());
    if let Err(e) = write_launch_spec_for_agent(name, &branch, repo, &agent) {
        eprintln!("skein: write_launch_spec: {e}");
    }
    // One sandbox hosts every box, so there is no `sbx create` for a box at all. Bringing one up is a
    // sequence of round-trips into that sandbox, each consuming the last one's side effects (see
    // `fleet::start_box`) — not something a single shell line can express, and not worth open-coding
    // into one. The launcher runs `skein start`; `skein attach` then does the full agent setup in the
    // same tmux server the box is anchored to.
    //
    // `--attach` rather than `&& sbx <argv>`: the attach argv names the box's PLACEMENT, and this
    // string is built before `skein start` has created one. Precomputed, it addressed a sandbox named
    // after the box — `ERROR: no sandbox named …` the moment the box came up perfectly.
    format!(
        "{} start {} --branch {} --agent {}{}",
        skein_exe(),
        sh_quote(name),
        sh_quote(&branch),
        sh_quote(&agent),
        match attach {
            Attach::Yes => " --attach",
            Attach::No => "",
        },
    )
}

/// How to spell `skein` in a command the server hands to `sh -c`.
///
/// Bare `skein` assumes an install on `$PATH`, and the cockpit is normally run straight out of a
/// build — `cargo run --bin skein-server`, where nothing named `skein` is on `$PATH` at all. So
/// creating a box in the fleet died on `sh: skein: command not found`, and the terminal then
/// reconnected onto `sbx exec` for a sandbox that was never made: `no sandbox named …`, forever.
///
/// The sibling of the running executable is the right answer and needs no configuration: the two
/// binaries are built and installed together, so whichever `skein-server` is running, the `skein`
/// beside it is the matching build. Falls back to the bare name when there is no sibling — an
/// installed-on-PATH layout, which is exactly when bare works.
pub(crate) fn skein_exe() -> String {
    match skein_cli() {
        SkeinCli::Beside(path) => sh_quote(&path),
        SkeinCli::OnPath => "skein".into(),
    }
}

/// Which of [`skein_exe`]'s two answers was used — kept, because a shell that cannot find the
/// program reports both as the same `not found` and they have opposite cures.
///
/// A bare name the shell could not find is a question about the `$PATH` the server was started
/// with, and the reader's own shell will contradict the message. An absolute sibling path the shell
/// could not find is a build that no longer ships both binaries — `$PATH` had no part in it, and
/// naming it sends the reader to check something that is fine. `util::spawn_failure` draws that same
/// line one layer down, for a failed `exec` rather than a shell's refusal.
enum SkeinCli {
    /// The `skein` beside the running executable — this build's matching CLI, by absolute path.
    Beside(String),
    /// No sibling, so the bare name, resolved on the server's `$PATH`.
    OnPath,
}

fn skein_cli() -> SkeinCli {
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("skein")))
        .filter(|p| p.is_file())
        .map(|p| SkeinCli::Beside(p.to_string_lossy().into_owned()))
        .unwrap_or(SkeinCli::OnPath)
}

/// Why a launch ended without `skein` ever running — or `None` when it ran.
///
/// **The exit code is the whole of the evidence, and only two values of it mean this.** A launch is
/// a command line handed to `sh -c`, so a program it cannot start is reported by the *shell*, not by
/// skein and not by the OS: 127 is "not found" and 126 is "found, and could not be executed". Every
/// other code — including 0 — comes from `skein start` itself, which keeps its own reason in
/// `starts/<box>.err`; answering for those would replace what happened with a paraphrase of it.
///
/// The `not-found`-on-`$PATH` sentence is [`crate::util::spawn_failure`]'s and is called rather than
/// copied (SKEIN-429): skein says one thing about a program it could not start.
pub fn launch_never_ran(name: &str, code: u32) -> Option<String> {
    never_ran(name, code, skein_cli())
}

/// [`launch_never_ran`] with the resolution supplied, so both arms can be read without arranging a
/// server that has a sibling binary and one that does not.
fn never_ran(name: &str, code: u32, cli: SkeinCli) -> Option<String> {
    let said = match (code, &cli) {
        // Exactly the fault `spawn_failure` describes, so it says it: the name was resolved on a
        // `$PATH`, and the one that matters is the server's rather than the reader's.
        (127, SkeinCli::OnPath) => crate::util::spawn_failure(
            &Command::new("skein"),
            &std::io::Error::from(std::io::ErrorKind::NotFound),
        ),
        (127, SkeinCli::Beside(path)) => format!(
            "`{path}` is gone. That is the `skein` beside the running `skein-server`, and it was \
             there when this command was built — install or rebuild both binaries together."
        ),
        // Not phrased as an OS error: the OS never returned one. The shell found the file and
        // declined to run it, and inventing an `os error 13` would cite evidence skein does not have.
        (126, cli) => {
            let exe = match cli {
                SkeinCli::Beside(path) => path.clone(),
                SkeinCli::OnPath => "skein".to_string(),
            };
            format!(
                "the shell found `{exe}` and could not execute it — a mode bit, or a script whose \
                 interpreter line is broken. Check `ls -l {exe}`, and reinstall it if it is not a \
                 working binary."
            )
        }
        _ => return None,
    };
    Some(format!(
        "box {name} was never started: its launch runs under `sh -c`, and {said}"
    ))
}

/// Keep why a launch never reached `skein`, where the terminal that reconnects will read it.
///
/// **A surface outside `skein` has to do this, because everything that records a failed start is
/// inside the binary that never started.** `fleet::start_box` writes `starts/<box>.err`, and
/// `skein start` writes it for the failures that come before `start_box` is reached — both from
/// inside `skein`. So the one failure that record-keeping cannot see is `skein` not running at all,
/// and `absent_box_reason` answered the person who had just pressed Launch with "There is no record
/// of a start having been attempted" (SKEIN-589).
///
/// Returns the sentence it wrote, or `None` when `skein` did run: it keeps its own reason, which is
/// specific, and this one is not.
pub fn remember_launch_never_ran(name: &str, code: u32) -> Option<String> {
    let why = launch_never_ran(name, code)?;
    crate::fleet::remember_start_failure(name, &why);
    Some(why)
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

/// The argv that streams stdin into `path` inside box `name`, creating `dir` first (that's how a
/// folder drop recreates its tree). `-i` and *not* `-t`: a pty would mangle the binary bytes.
///
/// Through the box's **placement**, because a fleet box is not a sandbox: `sbx exec -i <box>` names
/// something sbx has never heard of, and every paste, drop and file pick into a fleet box failed
/// with `no sandbox named …` reported to the browser as "attach failed".
///
/// Returns the full argv including `sbx`, since where a box lives decides the program as well as its
/// arguments — a legacy box is still `sbx exec`, a fleet box is `sbx exec … nsenter …`.
pub fn box_write_argv(name: &str, dir: &str, path: &str) -> Result<Vec<String>, String> {
    // Validated before the lookup so the refusal says which problem it is: a name that could never
    // be a box is a caller bug, a valid name with no placement is a box that is not there.
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let place = place_of(name).ok_or_else(|| format!("no box named {name}"))?;
    Ok(place.write_argv(&box_write_script(dir, path)))
}

/// The script both write paths run, so the two cannot drift into writing different files.
fn box_write_script(dir: &str, path: &str) -> String {
    format!("mkdir -p {} && cat > {}", sh_quote(dir), sh_quote(path))
}

/// Where the log of a resume skein performed is kept: with skein's own state, never in the box's
/// store.
///
/// **The store is a directory the box writes.** It is bound read-write into the box it belongs to
/// (`box-session.sh`'s `--bind "$SKEIN_BOX_STORE"`), and every sibling box of the same repo shares
/// it — so this log used to be a `File::create` by a privileged process, at a name the subject of
/// the log could predict, on a path the subject could replace with a symlink. Architecture §9.5 R8
/// states the rule it broke: *no privileged actor reads, writes, chowns or follows a path a box can
/// influence*. A link planted at `status/<box>.resume.log` pointing at `config.json`,
/// `git-grants.json` or a decision artifact had skein truncate and then write over it on the next
/// Continue click.
///
/// The store is for what a box produces. This is skein saying what it did, which belongs beside the
/// rest of the box's host-side state — the same directory `tracking` already uses, and one the
/// launcher binds into the box READ-ONLY (bind table row 13).
fn resume_log_path(name: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(box_state(name));
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    Ok(dir.join("resume.log"))
}

/// Open a log skein writes, refusing to follow a symlink standing where the file should be.
///
/// `create_new` is `O_CREAT|O_EXCL`, and the reason it is here rather than `File::create` is that
/// `O_EXCL` is the one open flag the kernel refuses to resolve a final symlink for — a dangling one
/// included. Unlinking first is what makes that usable for a log that is rewritten on every resume:
/// `remove_file` removes the *link* and never the thing it points at, so the two lines together
/// mean this call can only ever create a fresh regular file at exactly this name.
///
/// Someone who wins the gap between the two lines gets `EEXIST` and an error, not a followed link:
/// the failure is closed, and it is reported rather than silently written through.
///
/// Portable, and deliberately so rather than passing the kernel's no-follow open flag by hand:
/// that constant is a different number on Linux and on macOS and skein runs on both, so spelling
/// it here would be two magic numbers where `create_new` is one guarantee.
fn open_log(path: &Path) -> Result<fs::File, String> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("clear {}: {e}", path.display())),
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))
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
    // `box_liveness`, not `sbx ls` directly: a box in the fleet is not a sandbox, so it never
    // appears there and every one of them answered "box does not exist" — the cockpit's Continue
    // button, dead for the entire fleet.
    match box_liveness(name) {
        Some(Liveness::Running) => {}
        Some(_) => {
            return Err(format!(
                "box {name:?} is not running; attach/start it before resuming"
            ))
        }
        None => {
            return Err(format!(
                "cannot tell whether box {name:?} is running; attach/start it before resuming"
            ))
        }
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
            // Through the box's placement, never `sbx exec <box>`: that names a sandbox, and for a
            // fleet box there is none — or worse, an unrelated one wearing the same name.
            //
            // And through `spawning`, because this argv is *spawned* twenty lines below, as text
            // inside a larger `sh -c`. Being text rather than an argv is why it cannot go through
            // `Place::command`, and was why it reached a real box in a test process with neither
            // half of the seam applied (SKEIN-764).
            let place = place_of(name).ok_or_else(|| no_place(name))?;
            place
                .spawning(place.exec_argv(&guest))
                .iter()
                .map(|arg| sh_quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        }
    };

    // Keep a durable log and observe the child briefly. The old `nohup … &` only proved that a
    // shell forked, so a missing CLI or rejected resume was reported as success. Here an immediate
    // non-zero exit is surfaced; a healthy long-running agent is reaped by a tiny waiter thread.
    let log_path = resume_log_path(name)?;
    let mut stdout = open_log(&log_path)?;
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

/// The shell command that stops a box, when something has said what one is.
///
/// `None` unless `$SKEIN_STOP_CMD` names a template (`{name}` is substituted and shell-quoted).
/// **There is no default any more.** It used to be `sbx stop {name}` — the per-VM model, where a box
/// WAS a sandbox named after it. Nothing resolves to that shape now ([`crate::place::place_of`]
/// returns `None` for such a name), and in-fleet there is no `sbx` on `$PATH` to run it with, so the
/// default could only ever miss — or, worse, halt an unrelated sandbox that happened to share the
/// name. The variable survives as the seam this crate's own tests stop a box through.
pub fn stop_command(name: &str) -> Option<String> {
    let t = env::var("SKEIN_STOP_CMD").ok().filter(|t| !t.is_empty())?;
    Some(t.replace("{name}", &sh_quote(name)))
}

/// Stop a box: run `stop_command` to halt the running sandbox. The box stays listed (it goes stale
/// until resumed) — this only frees the compute, it does not delist or destroy.
pub fn stop_box(name: &str) -> Result<(), String> {
    crate::fleet::disturbing_liveness(|| stop_box_inner(name))
}

fn stop_box_inner(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    // A shared box has no sandbox of its own to stop. `sbx stop <box>` would either fail or — worse
    // — stop an unrelated sandbox that happens to carry the same name. Killing its tmux server ends
    // every process in the box, which frees the namespace, and leaves the tree for a later restart.
    if let Some(rec) = shared_record(name) {
        // The tmux server IS this box's liveness, so killing it makes the sweep's picture wrong
        // rather than merely old — and the gate serves its last good answer while it refreshes.
        // Settled by the wrapper above, on this branch and on the one below it: the invalidation
        // used to live here, and the `sbx stop` path underneath returned without one.
        return own_sandbox(&rec.sandbox)
            .exec(
                &crate::fleet::stop_script(name, &rec),
                Duration::from_secs(30),
            )
            .map(|_| ());
    }
    // No placement record, and so no box of skein's to stop. The answer is the one
    // `absent_box_reason` gives every other surface asked about a name skein has not placed: it
    // distinguishes a start that failed from a sandbox somebody else made, which "stop failed" did
    // not. `None` from it means "cannot tell", which here is still not a box this can stop.
    let Some(cmd) = stop_command(name) else {
        return Err(crate::fleet::absent_box_reason(name).unwrap_or_else(|| no_place(name)));
    };
    let (_out, err, code) = run_shell(&cmd)?;
    if code != 0 {
        return Err(format!("stop failed (exit {code}): {}", err.trim()));
    }
    Ok(())
}

/// The shell that destroys a shared box: the same ending, and then the box itself.
///
/// The cgroup is removed after the tree, and `rmdir` refuses one that still holds processes — which
/// used to be a race it could simply lose, because nothing had killed them. It is retried briefly
/// rather than once: `cgroup.kill` signals, and the members are reaped a moment later. Left behind,
/// every destroyed box accumulates an empty cgroup, and a box later given the same name inherits the
/// old one's limits instead of the current settings.
pub(crate) fn destroy_script(name: &str, rec: &crate::place::PlaceRecord) -> String {
    format!(
        "{look}; tmux -S {sock} kill-server 2>/dev/null; {kill}; {sweep}; {containers}; rm -rf {root}; \
         for _ in 1 2 3 4 5; do sudo rmdir {cgroup} 2>/dev/null && break; sleep 0.2; done; \
         sudo rmdir {containers_cgroup} 2>/dev/null; exit 0",
        look = crate::fleet::namespace_kill(rec.ns_pid, &rec.generation, rec.ns_start),
        sock = sh_quote(&rec.sock),
        kill = crate::fleet::box_cgroup_kill(name),
        sweep = crate::fleet::namespace_sweep(),
        containers = crate::fleet::box_containers_kill(name),
        root = sh_quote(&box_root(name)),
        cgroup = sh_quote(&crate::fleet::box_cgroup(name)),
        containers_cgroup = sh_quote(&crate::fleet::box_container_cgroup(name)),
    )
}

/// Delist a box from the cockpit: remove its registry entry and append it to `<store>/history.jsonl`,
/// and drop the live per-box files the box will never write again.
/// Used after `destroy_box` tears the sandbox down, so a removed sandbox doesn't linger as stale.
/// Touches only skein's own records, never the sandbox.
///
/// **The store is the box's own** — [`store_for_box`], which is how every other per-box read in this
/// crate resolves one. It used to be `locate_registry`, the single legacy store, and on a fleet
/// install that names a `sandboxes.json` the boxes never write: the lookup itself failed, and
/// because the failure was raised before the file cleanup below, every destroy left
/// `status/<name>.json`, its pane and agents files and the launch spec behind. Both callers only
/// log what this returns, so nothing ever said so.
///
/// **The cleanup does not sit behind the registry rewrite's `?`.** The two are independent — one
/// edits a file the whole fleet shares, the other removes files belonging to a box that is already
/// gone — and ordering them the other way is what made the second conditional on the first.
pub(crate) fn delist_box(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let store = store_for_box(name).ok_or_else(|| {
        format!(
            "can't locate {name}'s store ({})",
            crate::registry::registry_origin()
        )
    })?;
    forget_box_files(&store, name);
    delist_from_registry(&store, name)
}

/// The box's own *live* runtime files, so a destroyed box leaves nothing stale behind: its
/// turn-state probe output, its pane observation, its agents list and its launch spec.
///
/// Best-effort — a missing file is fine, and this is called for a box that may already be gone.
///
/// Deliberately NOT deleted here: journals/<name>.md, diffs/<name>.*, tasks/<name>.json. A
/// --clone's own working tree (and its .skein/journal.md) dies with the box, so the store copies
/// are the only durable record of what that box did — they feed the cross-run workflow/process
/// learn-loop and must outlive the box, not just its live session.
fn forget_box_files(store: &Path, name: &str) {
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
}

/// Remove `name` from `<store>/sandboxes.json` and append the removed entry to `history.jsonl`.
///
/// `sandboxes.json` rather than whatever `$SKEIN_REGISTRY` spells, because that is the file every
/// *reader* of a per-store registry opens ([`crate::mailbox::sandboxes_in`], and so `all_sandboxes`
/// and the board through it). Delisting has to remove the entry from the file the board reads.
fn delist_from_registry(store: &Path, name: &str) -> Result<(), String> {
    use fs2::FileExt;
    use std::io::Write as _;
    let path = store.join("sandboxes.json");

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
        write_atomic(&path, store, pretty.as_bytes())
    })();
    let _ = lock.unlock();
    result
}

/// The shell command that tears a box down, when something has said what one is.
///
/// `None` unless `$SKEIN_DESTROY_CMD` names a template (`{name}` is substituted and shell-quoted).
/// **There is no default any more**, and here the missing default is worth more than in
/// [`stop_command`]: it was `sbx rm -f {name}`, DESTRUCTIVE, and aimed at a sandbox named after the
/// box — so on a fleet install, where no such sandbox is skein's, the one thing it could hit is
/// somebody else's sandbox that happens to share the name. Nothing resolves to the per-VM shape any
/// more; the variable survives as the seam this crate's own tests tear a box down through.
pub fn destroy_command(name: &str) -> Option<String> {
    let t = env::var("SKEIN_DESTROY_CMD")
        .ok()
        .filter(|t| !t.is_empty())?;
    Some(t.replace("{name}", &sh_quote(name)))
}

/// Destroy a box: end it, then delist it. The teardown must succeed before we delist, so a failed
/// teardown leaves the box on the board to retry rather than orphaning a box you can no longer see.
/// Destructive — the box's tree goes with it.
pub fn destroy_box(name: &str) -> Result<(), String> {
    // Its disk as well as its liveness: the tree is gone, so the `du` figures now attribute space to
    // a box that is not there and hide the room that just came back.
    let gone = crate::fleet::disturbing(
        &[
            crate::fleet::Remembered::BoxLiveness,
            crate::fleet::Remembered::BoxDisk,
        ],
        || destroy_box_inner(name),
    );
    // Into the log skein does not own (§9.5 R6). This is the most consequential thing skein does
    // without asking the warden to do it — a box and every uncommitted thing in it — and the whole
    // point of a host-side log is that a skein which later goes wrong cannot edit the line saying
    // what it did. Reported after, with the outcome, because "it was destroyed" can be checked
    // against a box that is gone and "it is about to be" can be checked against nothing.
    crate::warden_client::reported(
        &format!("destroy-box-{name}"),
        "destroyed a box",
        &match &gone {
            Ok(()) => format!("{name} and its checkout are gone"),
            Err(why) => format!("{name} was not destroyed: {why}"),
        },
    );
    gone
}

fn destroy_box_inner(name: &str) -> Result<(), String> {
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
        // that shares the box's name would destroy someone else's work, which is why there is no
        // longer a default that could. Kill the server, then the tree: the checkout lives in the
        // box, so this is the destructive step, aimed at the right thing.
        own_sandbox(&rec.sandbox).exec(&destroy_script(name, &rec), Duration::from_secs(120))?;
        forget_place(name);
        // AFTER `forget_place`, and the ordering is what makes it safe rather than tidy: the
        // placement record is one of the two registers `fleet::live_box_names` reads, and the sweep
        // refuses a name that still reads as live.
        forget_what_skein_decided(name);
        // Same reason as `stop_box`, and worse here: the box is not merely stopped, it is gone, and
        // a sweep serving its last good answer would keep a destroyed box on the board.
        if let Err(e) = delist_box(name) {
            eprintln!("skein: destroyed {name}, but delisting it failed (harmless): {e}");
        }
        return Ok(());
    }
    // Same as `stop_box`: no placement record means there is no box here to tear down, and the
    // refusal says which of the two reasons it is rather than reporting a teardown that failed.
    let Some(cmd) = destroy_command(name) else {
        return Err(crate::fleet::absent_box_reason(name).unwrap_or_else(|| no_place(name)));
    };
    let (_out, err, code) = run_shell(&cmd)?;
    if code != 0 {
        return Err(format!("teardown failed (exit {code}): {}", err.trim()));
    }
    forget_what_skein_decided(name);
    // The sandbox is gone (`sbx rm` succeeded). Delisting is just bookkeeping, and `sbx ls` is the
    // fleet source of record — so a stale/unparseable registry must NOT fail the destroy, which would
    // leave the box's tab open over a sandbox that no longer exists. Log and move on; the next
    // successful delist (or a registry self-heal) cleans up the leftover entry.
    if let Err(e) = delist_box(name) {
        eprintln!("skein: destroyed {name}, but delisting it from the registry failed (harmless — sbx ls is the source of record): {e}");
    }
    Ok(())
}

/// The other half of forgetting a box, and the half that is not about disk (SKEIN-736).
///
/// [`forget_box_files`] drops the box's live files in the store. This drops what skein *decided*
/// about it — `privileged`, `git-scope`, `disk`, `identity` — and the two request drop-boxes the
/// launcher makes for it outside its namespace. Neither `destroy_script`'s `rm -rf` nor
/// `forget_box_files` reaches any of them, so before this every destroyed box left all four
/// answers on disk under its name, and a box later created with that name read them as its own.
///
/// Best-effort and reported, never fatal: the box itself is already gone by the time this runs, so
/// failing the destroy over a leftover directory would leave a box on the board that no longer
/// exists. The message says what the leftover *means* rather than which syscall failed, because the
/// consequence — a later box of this name inheriting `privileged` — is the part a reader has to act
/// on. [`crate::fleet::forget_departed_box`] decides whether the box is gone at all.
fn forget_what_skein_decided(name: &str) {
    if let Err(why) = crate::fleet::forget_departed_box(name) {
        eprintln!(
            "skein: destroyed {name}, but what skein had DECIDED about it is still on disk, so a \
             box later created with this name would inherit it — including `privileged`, which \
             skips every isolation bind. Remove it by hand: {why}"
        );
    }
}

/// Why `place_of` said no, phrased for whoever is reading the failure.
///
/// It answers two questions in one `Option`: the name is unusable, or the name is fine and skein has
/// not placed a box by it. Both used to read "invalid box name", which was true of the first and
/// misleading for the second — it sent people checking their typing when the answer was that the box
/// does not exist or its start failed.
fn no_place(name: &str) -> String {
    match crate::util::valid_name(name) {
        false => format!("unusable box name {name:?}"),
        true => format!(
            "skein has not placed a box called {name}, so it does not know where it runs \
             (its start may have failed, or it may be a sandbox skein did not create)"
        ),
    }
}

pub fn sbx_guest_output(name: &str, shell: &str, timeout: Duration) -> Result<String, String> {
    place_of(name)
        .ok_or_else(|| no_place(name))?
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
        .ok_or_else(|| no_place(name))?
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
pub fn initial_attach_argv_as(name: &str, agent: &str) -> Vec<String> {
    let runtime = resolve_runtime(agent);
    // A box that was rebuilt from a snapshot is new to sbx but not new to its user: a fleet resize
    // restores the previous conversation into the fresh checkout, and starting the runtime clean
    // here would leave that transcript sitting on disk, unread, while the agent opened an empty
    // session against a tree full of context it appears not to remember.
    //
    // Safe for the cross-runtime takeover path, which also carries a handoff dir: `interactive_resume`
    // is `<cli> --continue || <cli>`, so a target runtime with no native transcript of its own falls
    // back to a fresh start on its own — the takeover brief is what carries context there.
    let command = if restored_from_snapshot(name) {
        runtime.interactive_resume
    } else {
        runtime.interactive_start
    };
    agent_attach_argv(name, runtime, "skein-agent", command, true)
}

/// Was this box's checkout rebuilt from a snapshot rather than started empty?
///
/// Read from the launch spec — the same record the provisioning script restores from — so the host
/// and the box agree on it without a round-trip into a box that may not be up yet.
fn restored_from_snapshot(name: &str) -> bool {
    repo_for_box(name)
        .and_then(|repo| launch_spec(&repo, name))
        .and_then(|spec| {
            spec.get("handoff")?
                .get("dir")?
                .as_str()
                .map(|d| !d.trim().is_empty())
        })
        .unwrap_or(false)
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
        crate::fleet::initial_setup_wait()
    } else {
        String::new()
    };
    let instruction = agent_instruction_setup(runtime);
    let command = crate::runtime::for_box(command, name);
    let command = guarded_agent_command(agent, &command);
    let Some(place) = place_of(name) else {
        return refusal_argv(name);
    };
    // Every `tmux` below is this box's server, socket-qualified because the sandbox is shared: session
    // names are identical across boxes, so without the socket two boxes would both find a live
    // `skein-agent` on the sandbox's one server and attach to each other's.
    let tmux = place.tmux();
    let observer = pane_observer_start(tmux_name, place.tmux_sock());
    let configure = TMUX_CONFIGURE.replace("tmux ", &format!("{tmux} "));
    let shell = format!(
        "{setup_wait}if ! command -v {executable} >/dev/null 2>&1; then echo 'skein: {agent} is not installed in this sandbox image; create a {agent} box or install/authenticate the CLI here to take over'; exec bash -li; fi; \
         if ! command -v tmux >/dev/null 2>&1; then echo 'skein: tmux is required for durable sessions but is missing; recreate this box or install tmux'; exit 1; fi; \
         {setup}; \
         created=0; if ! {tmux} has-session -t {tmux_name} 2>/dev/null; then {instruction}; {tmux} new-session -d -s {tmux_name} {command:?}; created=1; fi; \
         if [ \"$created\" = 1 ]; then {tmux} set-option -t {tmux_name} @skein-agent-contract {TMUX_AGENT_CONTRACT}; fi; \
         {observer} \
         {configure}exec {tmux} -u attach-session -t {tmux_name}",
        setup = runtime.interactive_setup,
    );
    place.interactive_argv(&shell)
}

/// Stop one runtime's persistent tmux process without touching the sandbox or another provider's
/// native session. Reattaching recreates it through that adapter's native resume command.
///
/// **Its scope is the agent's session and nothing else, deliberately.** This and [`stop_box`] read
/// like the same act and are not: the box keeps running here, and everything else in it keeps
/// running with it — the box's own shell, another runtime's session, and whatever the agent left in
/// the background. A `kill-session` that also swept the box's mount namespace would take a dev
/// server somebody deliberately left up, on a button whose label is "restart the agent".
///
/// The other act has a name now: `skein restart <box>` stops the box — every process in it — and
/// starts it again, which is also what rebuilds its isolation from the current launcher. So the two
/// are distinguishable by what the person asked for rather than by what a signal happens to reach,
/// which is what they were not when this only ever ended the pane's process group.
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
    let place = place_of(name).ok_or_else(|| no_place(name))?;
    // `session` is built from a validated box name and a validated runtime, so it is safe to spell
    // into a shell string here — and going through the place is what aims kill-session at this
    // box's own server rather than whichever one answers on the sandbox's default socket.
    // Through `spawning` for the reason `Place::command` goes through it: the line below runs this
    // argv, and a test that reached here without a stand-in would kill a tmux session on the
    // owner's live fleet (SKEIN-764). `run_capture` rather than `Place::exec` because this caller
    // wants stdout, stderr and the code kept apart, which `exec` folds together.
    let argv =
        place.spawning(place.exec_argv(&format!("{} kill-session -t {session}", place.tmux())));
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

/// What to run instead of an attach when the box has no placement record.
///
/// A terminal has to be handed *something*, and every alternative is worse: an empty argv leaves a
/// blank pane, and addressing `sbx exec <name>` states as fact that skein owns a sandbox it may never
/// have made. This prints one line and exits, so the pane carries the reason.
fn refusal_argv(name: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "echo 'skein: {name} has no placement record, so skein does not know where it runs. \
             If its start failed, run: skein start {name} --branch <branch>. If it is a sandbox you \
             made yourself, reach it with: sbx exec -it {name} bash -l' >&2; exit 1"
        ),
    ]
}

/// `sbx` argv for an interactive *shell* in the box — a plain terminal to run commands in, separate
/// from the agent session. Uses a persistent `skein-shell` tmux session when tmux is present (so this
/// terminal survives reconnects). tmux is part of the managed-box contract; refusing to open a
/// direct shell avoids presenting a terminal whose process dies on reload. Override the whole
/// command with $SKEIN_SHELL_CMD (`sh -c`).
pub fn shell_argv(name: &str) -> Vec<String> {
    // No placement ⇒ not a box skein made. Refusing in-band rather than addressing a sandbox named
    // after the box: that guess is what turned "your start failed" into sbx's `no sandbox named …`,
    // arriving from the wrong layer. `absent_box_reason` says the same thing earlier and better; this
    // is the backstop for any path that reaches here without asking it.
    let Some(place) = place_of(name) else {
        return refusal_argv(name);
    };
    let tmux = place.tmux();
    let configure = TMUX_CONFIGURE.replace("tmux ", &format!("{tmux} "));
    place.interactive_argv(&format!(
        "if ! command -v tmux >/dev/null 2>&1; then echo 'skein: tmux is required for durable sessions but is missing; recreate this box or install tmux'; exit 1; fi; if ! {tmux} has-session -t skein-shell 2>/dev/null; then {tmux} new-session -d -s skein-shell; fi; {configure}exec {tmux} -u attach-session -t skein-shell"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{load_config, save_config, Config};
    use crate::place::{record_place, PlaceRecord};
    use crate::repos::{launch_spec_agent, save_repos};
    use crate::testutil::*;

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
            read_prs: false,
            id: "thing".into(),
            source: "s".into(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };
        // box name is the slug `thing-feat-auth`; the REAL branch (with the slash) is feat/auth.
        let cmd = repo_launch_command_as("thing-feat-auth", &repo, "feat/auth", None, Attach::Yes);
        // `skein start`, not `sbx create`: a box is assembled inside the shared sandbox by a sequence
        // of round-trips, which no single shell line can express. The attach happens after the box
        // exists, because its argv names a placement that does not exist yet when this string is built.
        assert!(
            cmd.contains(" start 'thing-feat-auth' --branch 'feat/auth' --agent 'claude' --attach"),
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
            read_prs: false,
            id: "skein".into(),
            source: "s".into(),
            store: home.join("store/.claude").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };
        // The launcher carries the runtime choice; what that runtime then *does* on attach — the
        // update, the hook-trust bypass, no `resume --last` on a first start — is
        // `initial_attach_argv_as`'s business and is asserted there. This string used to contain both,
        // because `sbx create … && sbx exec …` was one line; it is now `skein start --attach`.
        let cmd = repo_launch_command_as("skein-codex", &repo, "codex", Some("codex"), Attach::Yes);
        assert!(
            cmd.contains("--agent 'codex'"),
            "the runtime override has to reach the launcher: {cmd}"
        );
        placed("skein-codex");
        let attach = initial_attach_argv_as("skein-codex", "codex");
        let shell = attach.last().unwrap();
        assert!(shell.contains("new-session -d -s skein-agent"), "{shell}");
        // SKEIN-403: a box's very first attach used to spend a network round trip on an update it
        // could not perform. Nowhere on this path reaches out any more.
        assert!(!shell.contains("codex update"), "{shell}");
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
            cmd.contains("belongs to no registered repo") && cmd.contains("skein add <git-url>"),
            "the terminal must be told what to do about it, and with the argument that command \
             actually takes: {cmd}"
        );
        // And never a path (SKEIN-588). `registrable_source` accepts https/http/ssh/git@ and
        // nothing else, so `skein add <git-url|path>` — which this said until then — sent the one
        // reader who is already stuck to the one source `add_repo` is certain to refuse. Matched on
        // the usage slot rather than on the word "path", so a sentence that happens to mention one
        // is still allowed; offering one in the command is not.
        assert!(
            !cmd.contains("|path") && !cmd.contains("<path"),
            "the fix printed to the terminal must not offer a local path: {cmd}"
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
            read_prs: false,
            id: "web".into(),
            source: "git@github.com:o/web.git".into(),
            store: home
                .join("repos/web/store/.claude")
                .to_string_lossy()
                .into(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
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
            read_prs: false,
            id: "web".into(),
            source: "git@github.com:o/web.git".into(),
            store: home
                .join("repos/web/store/.claude")
                .to_string_lossy()
                .into(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
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

    /// A placement standing only for its socket — the anchor deliberately unverifiable, so the
    /// namespace sweep skips itself and what is left under test is the cgroup half.
    ///
    /// Empty `generation` is not a shortcut: it is the shape of every record written before the
    /// stamp existed, and [`crate::fleet::namespace_kill`] treats it as "cannot be checked", which
    /// is the only safe reading. A test that handed it a *verifiable* anchor would be a test that
    /// sweeps this machine's own namespace.
    fn placed_at(sock: &std::path::Path) -> crate::place::PlaceRecord {
        crate::place::PlaceRecord {
            sock: sock.to_string_lossy().into_owned(),
            ..Default::default()
        }
    }

    /// Run the stop script for real, and watch it reach a process that left the tmux tree.
    ///
    /// The cgroup step is pointed at a scratch directory, because **a box cannot make a real
    /// cgroup** — `sudo` inside one is a user namespace mapping a single uid, and refuses by design.
    /// So what is proved here is the half a test can prove: the script writes `1` to *this box's*
    /// `cgroup.kill` and to nothing else. That the write kills every member is the kernel's
    /// contract, and it is the contract precisely because membership is not something a process can
    /// shed by forking — which is what `tmux kill-server` could not say.
    ///
    /// `sudo` is shimmed to run its argument, the same way the launcher's ceiling test does.
    #[test]
    fn stopping_a_box_reaches_the_processes_that_left_its_tmux_tree() {
        let dir = tempdir();
        let cg = dir.join("cgroup/skein/thing-x");
        fs::create_dir_all(&cg).unwrap();
        fs::write(cg.join("cgroup.kill"), "").unwrap();
        let sock = dir.join("session.sock");
        fs::write(&sock, "").unwrap();

        let ran = run_as_root(
            &crate::fleet::stop_script("thing-x", &placed_at(&sock)),
            &dir,
        );
        assert_eq!(ran, 0, "stopping a box must not fail on the way out");
        assert_eq!(
            fs::read_to_string(cg.join("cgroup.kill")).unwrap().trim(),
            "1",
            "nothing ended the processes that had been reparented out of the tmux tree"
        );
        assert!(
            !sock.exists(),
            "the socket is what liveness reads, so a stop has to remove it"
        );

        // A box with no cgroup at all — delegation missing, which the launcher records as
        // `uncapped no-cgroup-delegation` — must still stop exactly the way it did before.
        let bare = tempdir();
        let bare_sock = bare.join("session.sock");
        fs::write(&bare_sock, "").unwrap();
        assert_eq!(
            run_as_root(
                &crate::fleet::stop_script("thing-x", &placed_at(&bare_sock)),
                &bare
            ),
            0,
            "a box without a cgroup must stop rather than report an error"
        );
        assert!(!bare_sock.exists());
    }

    /// Destroying does the same and then some, and a cgroup that will not go away is not a failure.
    ///
    /// The scratch cgroup here holds a real file, so `rmdir` cannot succeed — which is the point:
    /// the retry gives up and the destroy still reports success, because the box's tree is gone and
    /// that is what destroying it means. A leftover cgroup is untidy; a destroy that reports failure
    /// leaves the box on the board.
    #[test]
    fn destroying_a_box_ends_it_and_survives_a_cgroup_that_will_not_go() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_FLEET_ROOT", dir.join("boxes"));
        let cg = dir.join("cgroup/skein/thing-x");
        fs::create_dir_all(&cg).unwrap();
        fs::write(cg.join("cgroup.kill"), "").unwrap();
        let tree = std::path::PathBuf::from(crate::fleet::box_root("thing-x"));
        fs::create_dir_all(tree.join("tree")).unwrap();
        let sock = dir.join("session.sock");
        fs::write(&sock, "").unwrap();

        let ran = run_as_root(&destroy_script("thing-x", &placed_at(&sock)), &dir);
        env::remove_var("SKEIN_FLEET_ROOT");
        assert_eq!(
            ran, 0,
            "a cgroup that will not rmdir must not fail the destroy"
        );
        assert_eq!(
            fs::read_to_string(cg.join("cgroup.kill")).unwrap().trim(),
            "1",
            "a destroyed box left processes running"
        );
        assert!(
            !tree.exists(),
            "the box's tree survived its own destruction"
        );
    }

    /// Run a launcher-side script with the cgroup root pointed at `root` and `sudo` made harmless.
    ///
    /// The path rewrite rather than an environment seam: `/sys/fs/cgroup/skein` is a kernel path,
    /// not a configurable one, and adding an override to production so a test can run would be a way
    /// for something else to point the kill somewhere it should not go.
    fn run_as_root(script: &str, root: &std::path::Path) -> i32 {
        let harness = format!(
            "sudo() {{ \"$@\"; }}\n{}\n",
            script.replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
        );
        std::process::Command::new("bash")
            .arg("-c")
            .arg(&harness)
            .status()
            .expect("run the stop script")
            .code()
            .unwrap_or(-1)
    }

    #[test]
    fn delist_box_removes_records_and_guards() {
        let _g = env_lock();
        let dir = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
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
        env::remove_var("SKEIN_HOME");
    }

    /// Delisting reads the box's OWN store, and cleans up even when the registry rewrite fails.
    ///
    /// Two failures in one, and they are the same failure twice. `delist_box` used to resolve its
    /// store with `locate_registry()` — the single legacy store — so on a fleet install, where each
    /// repo has its own store and the legacy `sandboxes.json` is a file nothing writes, it errored
    /// before it did anything. And the per-box file cleanup sat *after* that error's `?`, so it was
    /// skipped: every destroy left `status/<name>.json`, its pane and agents files and its launch
    /// spec behind, for ever, with both callers only logging the error.
    ///
    /// The two arms below fail for those two reasons separately: the first if the store is resolved
    /// the old way (the repo store's registry keeps the entry), the second if the cleanup is put
    /// back behind the registry rewrite's `?` (a rewrite that legitimately fails leaves the files).
    #[test]
    fn delisting_uses_the_boxs_own_store_and_cleans_up_even_when_the_registry_will_not() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        // The legacy store, present but with no registry in it — a fleet install exactly: the
        // boxes write into their repo's store and nothing ever writes this file.
        let legacy = home.join("legacy");
        fs::create_dir_all(&legacy).unwrap();
        env::set_var("SKEIN_REGISTRY", legacy.join("sandboxes.json"));
        env::remove_var("SKEIN_SHARED");

        let store = home.join("repo-store").join(".claude");
        fs::create_dir_all(store.join("status")).unwrap();
        fs::create_dir_all(store.join("skein").join("launch")).unwrap();
        save_repos(&[Repo {
            read_prs: false,
            id: "demo".into(),
            source: "s".into(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();

        let reg = store.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"demo-task":{"branch":"x","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""}}"#,
        )
        .unwrap();
        let files = |name: &str| {
            [
                store.join("status").join(format!("{name}.json")),
                store.join("status").join(format!("{name}.pane.json")),
                store.join("status").join(format!("{name}.agents")),
                store
                    .join("skein")
                    .join("launch")
                    .join(format!("{name}.json")),
            ]
        };
        for f in files("demo-task") {
            fs::write(&f, "{}").unwrap();
        }

        delist_box("demo-task").expect("delisting reads the box's own repo store");
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        assert!(
            after.get("demo-task").is_none(),
            "delisting rewrote some other store's registry, not the one this box's repo owns: {after}"
        );
        for f in files("demo-task") {
            assert!(!f.exists(), "a destroyed box left {} behind", f.display());
        }

        // The second arm: a registry rewrite that fails for a real reason must NOT take the
        // cleanup with it. `demo-ghost` belongs to the same repo (so the same store) but is not in
        // that store's registry, so the rewrite errors — and its files must still be gone when it does.
        for f in files("demo-ghost") {
            fs::write(&f, "{}").unwrap();
        }
        assert!(
            delist_box("demo-ghost").is_err(),
            "a box that is not in the registry must still report that it could not be delisted"
        );
        for f in files("demo-ghost") {
            assert!(
                !f.exists(),
                "the registry rewrite's failure skipped the per-box cleanup again: {}",
                f.display()
            );
        }

        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn stop_box_runs_command_without_delisting() {
        let _g = env_lock();
        let dir = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
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
        env::remove_var("SKEIN_HOME");
    }

    /// With nothing placed and no override, stopping or destroying a box REFUSES — it does not
    /// reach for `sbx`.
    ///
    /// `stop_box` used to fall back to `sbx stop <name>` and `destroy_box` to `sbx rm -f <name>`:
    /// the per-VM model by definition, a box being a sandbox named after it. Nothing resolves to
    /// that shape any more, in-fleet there is no `sbx` on `$PATH` at all, and `sbx rm -f` aimed at
    /// a name skein does not own is a destructive command pointed at somebody else's sandbox. So
    /// the default is gone and what is left is the refusal `absent_box_reason` already gives every
    /// other surface, which says WHICH of the two reasons applies rather than "stop failed".
    #[test]
    fn stopping_or_destroying_an_unplaced_box_refuses_instead_of_running_sbx() {
        let _g = env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::remove_var("SKEIN_STOP_CMD");
        env::remove_var("SKEIN_DESTROY_CMD");
        // sbx answers, and lists nothing — so the refusal is the "no such box" one rather than
        // "cannot tell", and this test does not depend on whether the machine running it has sbx.
        env::set_var("SKEIN_LS_CMD", "printf '[]'");

        assert!(
            stop_command("web-main").is_none() && destroy_command("web-main").is_none(),
            "there is no per-VM default left to run: a box is not a sandbox named after it"
        );

        for why in [
            stop_box("web-main").unwrap_err(),
            destroy_box("web-main").unwrap_err(),
        ] {
            assert!(
                why.contains("box web-main does not exist"),
                "the refusal does not say the box is not there: {why}"
            );
            assert!(
                !why.contains("stop failed") && !why.contains("teardown failed"),
                "an `sbx` command was run for a box skein never placed, and its exit code is being \
                 reported as though the box were real: {why}"
            );
        }

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn destroy_box_runs_teardown_then_delists() {
        let _g = env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let dir = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
        // And the fleet root, since SKEIN-736: a destroy now asks `fleet::live_box_names` whether
        // the box is really gone before removing what skein decided about it, and `util::fleet_root`
        // refuses an unpinned test rather than answering with `/boxes` — the owner's live fleet.
        // Pinned through `env_pins` so it goes back on a failing assertion too (SKEIN-696).
        let fleet = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_FLEET_ROOT", &fleet);
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
        env::remove_var("SKEIN_HOME");
    }

    /// **A box created with a destroyed box's name inherits nothing skein decided about the box
    /// that had it** (SKEIN-736).
    ///
    /// That sentence is the property; removing three directories is only how it is reached, which
    /// is why the assertion below is on `fleet::box_is_privileged` and not on a path. Four answers
    /// live in `fleet::box_declared` — `privileged`, `git-scope`, `disk`, `identity` — and every one
    /// of them used to survive a destroy under a name anybody may create a box with again.
    /// `privileged` is the one with teeth: `box-session.sh` skips every isolation bind for the
    /// workshop box and leaves the fleet agent's token readable, so a box created through the
    /// ordinary route came up able to read every other box's credentials because a file four
    /// directories away still said `1`.
    ///
    /// **What makes this fail**: dropping `box_declared` from `fleet::box_side_state`, or not
    /// calling `forget_what_skein_decided` from `destroy_box_inner` at all. Both were tried before
    /// this was believed; the first reports *"a box created with a destroyed box's name came up
    /// PRIVILEGED"* and the second the same.
    ///
    /// The three `assert!`s taken **before** the destroy are not decoration. An absence that was
    /// never a presence proves nothing (CONTRIBUTING, "Before you change anything", rule 3), and a
    /// fixture that quietly failed to write `privileged` would let every assertion below pass over
    /// a box that had never been privileged at all.
    #[test]
    fn a_box_created_with_a_destroyed_boxs_name_inherits_none_of_its_decisions() {
        let _g = env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = tempdir();
        let fleet = tempdir();
        // BOTH, always. `util::fleet_root` falls back to `/boxes` — the owner's live fleet — and
        // this test destroys things (SKEIN-530, SKEIN-626, SKEIN-685).
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);
        env.set("SKEIN_REGISTRY", home.join("sandboxes.json"));
        env.unset("SKEIN_SHARED");
        fs::write(
            home.join("sandboxes.json"),
            r#"{"web-main":{"branch":"x","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""}}"#,
        )
        .unwrap();

        // A box exists: its tree in the fleet root is one of the two registers
        // `fleet::live_box_names` reads.
        fs::create_dir_all(fleet.join("web-main/tree")).unwrap();
        // It is the workshop box, and it has asked both queues for something — the drop-boxes the
        // launcher makes outside its namespace at every start (`src/box-session.sh`, the
        // `for asking in substrate gitgate` loop).
        crate::fleet::set_box_privileged("web-main", true).unwrap();
        for asking in ["substrate", "gitgate"] {
            let drop = fleet.join(format!(".skein/{asking}/requests/web-main"));
            fs::create_dir_all(&drop).unwrap();
            fs::write(drop.join("ask.json"), "{}").unwrap();
        }
        assert!(
            crate::fleet::box_is_privileged("web-main"),
            "the fixture never made the box privileged, so nothing below is a test of anything"
        );
        for asking in ["substrate", "gitgate"] {
            assert!(
                fleet
                    .join(format!(".skein/{asking}/requests/web-main/ask.json"))
                    .exists(),
                "the fixture never made the {asking} drop-box"
            );
        }

        // Destroyed through the seam this crate's own tests tear a box down with, doing what a real
        // teardown does to the register above: `destroy_script`'s `rm -rf <fleet root>/<box>`.
        env.set(
            "SKEIN_DESTROY_CMD",
            format!("rm -rf {}/{{name}}", sh_quote(&fleet.display().to_string())),
        );
        destroy_box("web-main").unwrap();
        assert!(
            !fleet.join("web-main").exists(),
            "the teardown did not remove the box's tree, so this test is asserting against a box \
             that is still live and the sweep is right to refuse it"
        );

        // The property. A box of the same name, created now, is NOT the workshop box.
        assert!(
            !crate::fleet::box_is_privileged("web-main"),
            "a box created with a destroyed box's name came up PRIVILEGED — every isolation bind \
             skipped, off a decision made about a box that no longer exists"
        );
        assert!(
            !crate::fleet::box_declared("web-main").exists(),
            "the destroyed box's declared answers are still on disk at {}",
            crate::fleet::box_declared("web-main").display()
        );
        for asking in ["substrate", "gitgate"] {
            let drop = fleet.join(format!(".skein/{asking}/requests/web-main"));
            assert!(
                !drop.exists(),
                "the destroyed box's {asking} drop-box outlived it: {}",
                drop.display()
            );
        }
        // And the queue roots themselves are untouched — the sweep is per box, not per queue.
        for asking in ["substrate", "gitgate"] {
            assert!(
                fleet.join(format!(".skein/{asking}/requests")).exists(),
                "the sweep took the whole {asking} queue with it, not one box's drop-box"
            );
        }
    }

    #[test]
    fn destroy_succeeds_even_when_registry_is_unparseable() {
        let _g = env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let dir = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
        // The fleet root too, since SKEIN-736 — see `destroy_box_runs_teardown_then_delists`.
        let fleet = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_FLEET_ROOT", &fleet);
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
        env::remove_var("SKEIN_HOME");
    }

    // A box in a shared sandbox is attached to through its namespace, and every tmux call names its
    // own server. Both matter for the same reason: session names are identical across boxes, so a
    // bare `tmux has-session -t skein-agent` on the sandbox's default socket would find a NEIGHBOUR's
    // agent and attach the user straight into someone else's turn.
    // The three lifecycle calls that used to name a SANDBOX after the box. For a shared box no such
    // sandbox exists, so `sbx ls` reported it dead, `sbx stop` missed, and `sbx rm -f` would have
    // aimed a destructive command at whatever sandbox happened to share the name.
    //
    // **Driven against a fleet root of its own, with a real socket.** The three states used to come
    // from a fake `sbx` on `$PATH`, standing in for the sweep's `sbx exec` hop. There is no hop
    // (SKEIN-576), so that fake was bypassed and `local_liveness` read the REAL root — `/boxes` by
    // default, which on a developer machine is somebody's live fleet (SKEIN-530). Pinning
    // `$SKEIN_FLEET_ROOT` fixes the aim, and binding an actual listener is a better oracle than the
    // fake ever was: the question the sweep asks is whether something accepts on that socket, and
    // this answers it by being the thing that accepts.
    #[test]
    fn a_shared_boxs_lifecycle_never_names_a_sandbox_after_the_box() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        let root = dir.join("boxes");
        env::set_var("SKEIN_FLEET_ROOT", &root);
        fs::create_dir_all(root.join("thing-x")).unwrap();
        let sock = root.join("thing-x/session.sock");

        record_place(
            "thing-x",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 4242,
                home: "/home/agent".into(),
                tree: "/boxes/thing-x/tree".into(),
                sock: sock.to_string_lossy().into_owned(),
                // A generation that is not this boot, deliberately: it sends the sweep down the
                // socket path, which is the half this test is about. The anchor half has its own
                // test (`place::the_sweep_verifies_the_anchor_and_falls_back_only_when_it_cannot`).
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();

        // Running: something accepts on the box's own socket, which is exactly what its tmux server
        // does and exactly what no `sbx ls` row can tell you — no sandbox carries this box's name.
        let listening = std::os::unix::net::UnixListener::bind(&sock).expect("a listener");
        assert_eq!(
            box_liveness("thing-x"),
            Some(Liveness::Running),
            "the box's own tmux server IS its liveness — sbx ls knows nothing about a shared box"
        );

        // Dead session: stopped, not missing. The tree is still there to restart from — and the
        // socket FILE is still there too, which is the case that has to be told apart from a live
        // one. Closing the listener leaves the file behind; a connect to it now refuses.
        drop(listening);
        assert!(
            sock.exists(),
            "the stale socket file is the whole subject here"
        );
        assert_eq!(box_liveness("thing-x"), Some(Liveness::Stopped));

        // Nothing to answer for: unknown, which must not be reported as stopped. Reporting a box
        // skein cannot see as gone invites somebody to start a second one over its work.
        fs::remove_dir_all(root.join("thing-x")).unwrap();
        assert_eq!(box_liveness("thing-x"), None);

        forget_place("thing-x");
        env::remove_var("SKEIN_FLEET_ROOT");
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
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();

        for argv in [attach_argv("thing-x", "/d"), shell_argv("thing-x")] {
            // The whole argv, program included: it is not `sbx` at all, so a builder that returned
            // arguments for a program it did not name could not say so. This used to open with
            // `sbx exec -it skein-fleet` — the host's hop, with the pty flags on it. Skein is
            // inside that sandbox now (SKEIN-576) and the argv starts at the crossing; the pty is
            // the caller's, since the caller is a terminal either way.
            assert!(
                !argv.iter().any(|a| a == "sbx"),
                "a hop into the sandbox came back, and there is no sbx here to run it: {argv:?}"
            );
            // **The PATH is pinned before anything runs** (ISO-1, SKEIN-832). An attach is the
            // start path that makes this matter most: `skein attach <box>` is spawned by
            // `run_attach` with `Command::new(program)` (`src/bin/skein.rs:1346`), so without the
            // pin the outer `bash` and the `nsenter` below resolve from the PATH of the person who
            // typed it — `~/.local/bin` at its head, and bound read-write into every box.
            assert_eq!(
                argv[0], "env",
                "an attach no longer pins PATH before it crosses: {argv:?}"
            );
            assert!(
                argv[1].starts_with("PATH="),
                "an attach's pin is not a PATH: {argv:?}"
            );
            assert_eq!(
                &argv[2..4],
                ["bash", "-c"],
                "the attach begins with the shell that checks the anchor before it crosses"
            );
            assert!(
                argv.iter().any(|a| a.contains("--preserve-credentials")),
                "the attach itself runs in the namespace — its setup writes the box's HOME and tree"
            );
            // The crossing is a shell now, so the flag is inside an argument rather than being one:
            // the anchor check has to run in the process that execs `nsenter`, one line before it.
            assert!(
                argv.iter().any(|a| a.contains("skein_start=")),
                "and it proves the anchor is still this box before entering it: {argv:?}"
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
        // No hop in front of the crossing (SKEIN-576) — asserted as an absence, because an index
        // that shifted back would pass a prefix check while the hop was back.
        assert!(!a.iter().any(|x| x == "sbx"), "{a:?}");
        assert!(a.last().unwrap().contains("new-session -d -s skein-agent"));
        assert!(a.last().unwrap().contains("claude --name"));
        assert!(a.last().unwrap().contains("--continue"));
        // **Starting an agent reaches for nothing over the network** (SKEIN-403). This used to run
        // `claude update` first, on every session start: measured 1.9-3.2s, and it FAILED every
        // time — the CLI is root-owned in the sandbox and a box maps only its own uid, so npm
        // cannot write it, and the `|| echo` on the same line swallowed the error. The way to
        // actually move the version is `skein update-agents` (SKEIN-404), which runs where sudo
        // works. Asserted as an absence because that is what the cost was: nobody would notice
        // this coming back except by timing a box.
        assert!(
            !a.last().unwrap().contains("claude update"),
            "starting an agent runs an update first — that is two seconds of network on every \
             session start, and inside a box it can only ever fail: {}",
            a.last().unwrap()
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
        // The same absence for the other adapter (SKEIN-403): both carried an update that could
        // not write the file it was updating, and a fix applied to one of two runtimes is half a
        // fix that reads as a whole one.
        assert!(
            !codex.last().unwrap().contains("codex update"),
            "starting a codex agent still runs an update first: {}",
            codex.last().unwrap()
        );
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
        assert!(!sh.iter().any(|x| x == "sbx"), "{sh:?}");
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
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let argv = box_write_argv(
            "thing-x",
            "/tmp/skein-drop-b1",
            "/tmp/skein-drop-b1/a b.pdf",
        )
        .unwrap();
        // It used to open `sbx exec -i skein-fleet`; there is no hop to carry the `-i` (SKEIN-576)
        // and the pipe is the spawning process's own. What still has to be true is the next line.
        assert!(
            !argv.iter().any(|a| a == "sbx"),
            "a hop into the sandbox came back: {argv:?}"
        );
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

    /// Resume and restart-the-agent-session build their argv here and spawn it themselves, so they
    /// go through the seam too (SKEIN-764).
    ///
    /// Neither can use `Place::command`: [`resume_box`] needs the argv as *text*, shell-quoted
    /// inside a larger `sh -c`, and [`restart_agent_session`] wants stdout, stderr and the exit
    /// code kept apart, which `Place::exec` folds together. So both took the argv from
    /// [`crate::place::Place::exec_argv`] and spawned it past both halves of the seam — no
    /// substitution and no refusal — which in a test process is a `kill-session` and a headless
    /// agent launch on the owner's live fleet.
    ///
    /// **Both directions for each.** With a stand-in the crossing is answered by it and the call
    /// succeeds; with none it is refused. A test that only checked the refusal would still pass
    /// with `spawning` wired in *instead of* the builder rather than around it.
    ///
    /// **What makes it fail:** taking `place.spawning(..)` back off either call site. The matching
    /// `catch_unwind` then comes back `Ok` — having run the real command.
    #[test]
    fn resuming_and_restarting_a_box_go_through_the_seam() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let root = home.join("fleet");
        env::set_var("SKEIN_FLEET_ROOT", &root);
        // `fleet_liveness` answers nothing at all when no fleet sandbox is configured, and both
        // calls below refuse a box they cannot call running — before they ever build a crossing.
        save_config(&Config {
            fleet_sandbox: "skein-fleet".into(),
            ..Config::default()
        })
        .unwrap();
        // A running box, said the way `place::local_liveness` reads one: a directory under the
        // fleet root with something listening on its `session.sock`. (The anchor half of that
        // function is skipped here — `placed` stamps a generation that is not this boot's — which
        // is what leaves the socket as the answer.)
        fs::create_dir_all(root.join("thing-x")).unwrap();
        let _listening =
            std::os::unix::net::UnixListener::bind(root.join("thing-x").join("session.sock"))
                .unwrap();
        placed("thing-x");
        // Or `resume_box` takes the override branch and never builds a crossing at all, which is
        // how `resume_box_guards_name_and_launches` below stays clear of this.
        env::remove_var("SKEIN_RESUME_CMD");
        assert_eq!(
            box_liveness("thing-x"),
            Some(Liveness::Running),
            "the fixture box does not read as running, so neither call below reaches a crossing"
        );

        // Answered by the stand-in, both of them.
        {
            let _stood_in =
                crate::place::seam::install(Box::new(|_: &[String]| Some(vec!["true".into()])));
            resume_box("thing-x", "go").expect("the stand-in answered the resume");
            restart_agent_session("thing-x", None).expect("the stand-in answered the restart");
        }

        // And refused with none installed.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let resumed = std::panic::catch_unwind(|| resume_box("thing-x", "go"));
        let restarted = std::panic::catch_unwind(|| restart_agent_session("thing-x", None));
        std::panic::set_hook(hook);
        forget_place("thing-x");
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");

        for (what, answered) in [("resume", resumed), ("restart", restarted)] {
            let said = match answered {
                Ok(outcome) => panic!(
                    "{what} with no stand-in installed ran the real crossing instead of being \
                     refused: {outcome:?}"
                ),
                Err(e) => e
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "<not a string>".into()),
            };
            assert!(
                said.contains("no stand-in is installed"),
                "{what}'s refusal has to say what is missing, or it tells a contributor nothing: \
                 {said}"
            );
        }
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
        // Where skein's own state is, and therefore where the log goes. Named here rather than
        // inherited, or the test writes the log into whoever's real `~/.skein` ran it.
        env::set_var("SKEIN_HOME", &dir);
        let result = resume_box("thing-x", "");
        assert!(result.is_ok(), "{result:?}");
        assert!(dir.join("boxes/thing-x/resume.log").is_file());
        // Not in the store, which is the half of the move that matters: the store is bound
        // read-write into the box this log is about (ISO-8).
        assert!(!dir.join("status/thing-x.resume.log").exists());
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_RESUME_CMD");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    /// A symlink standing where skein's log goes is not written through (ISO-8, architecture
    /// §9.5 R8).
    ///
    /// The log has moved out of the box-writable store, so this is the second lock rather than the
    /// first — and it is the one that keeps holding if some later change puts a skein-written file
    /// back somewhere a box can reach. `File::create` follows a link; [`open_log`] cannot.
    ///
    /// **What would make it fail**: replacing `open_log`'s `remove_file` + `create_new` pair with
    /// `fs::File::create`. The canary then comes back empty and the assertion below names it.
    #[test]
    fn a_link_planted_where_skein_logs_is_not_written_through() {
        let dir = tempdir();
        let canary = dir.join("git-grants.json");
        fs::write(&canary, "{\"grants\":[]}").unwrap();
        let log = dir.join("resume.log");
        std::os::unix::fs::symlink(&canary, &log).unwrap();

        // Before the absence: the link really is a link to the canary, so a follow WOULD reach it.
        assert_eq!(fs::read_link(&log).unwrap(), canary);

        let opened = open_log(&log).expect("a fresh regular file in place of the link");
        drop(opened);
        assert_eq!(
            fs::read_to_string(&canary).unwrap(),
            "{\"grants\":[]}",
            "skein truncated the file the planted link pointed at"
        );
        assert!(
            fs::symlink_metadata(&log).unwrap().file_type().is_file(),
            "the log is still a link, so the next writer follows it"
        );
    }

    // A stub standing in for the `claude` CLI: it reads the prompt (its last arg) and echoes a
    // canned reply, so the AI paths are exercised without a real model call.
    #[cfg(unix)]
    #[test]
    #[cfg(unix)]
    fn resume_batch_holds_real_decisions_when_ai_on() {
        // No guard on a shell here, deliberately. The `claude` stub this test installs is a
        // `#!/bin/sh` script, so a shell is needed — but so it is by some twenty siblings in this
        // same binary, which spawn `bash` and `.unwrap()` the result. On a machine without one
        // those twenty fail loudly and this one would have been the single quiet pass, which is
        // what SKEIN-790 was about. The probe it replaces asked PATH for `sh` while what is needed
        // is `/bin/sh` behind a shebang, so it did not even read the surface it guarded (SKEIN-825).
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
        // Where skein's own state is, and therefore where the resume log goes — the same line the
        // test above carries, and the one this test did not. Without it `config::skein_home` fell
        // through to the volume marker and this fixture wrote `boxes/box-route/resume.log` into the
        // owner's live `~/.skein`, beside the state of sixteen real boxes (SKEIN-626). The guard in
        // `skein_home` now refuses that rather than doing it, so this line is what keeps the test
        // running at all.
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_CLAUDE_BIN", write_claude_stub(&dir));
        // `ai` remembers a refusal about the setup so a broken `claude` is asked once rather than
        // once per row. It is process-global, like the env this test already locks — so a sibling's
        // failure would otherwise make this stub never run, in a parallel run only.
        crate::ai::forget_refusal();
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

        // The resumed row's log is inside the fixture, which is the property that was false: it
        // used to land in the real home, and only the real home. Asserted on the *resumed* row
        // because that is the one that writes — `box-decide` was held and writes nothing.
        assert!(
            dir.join("boxes/box-route/resume.log").is_file(),
            "the resume log is not in the fixture, so it went wherever `skein_home` pointed"
        );

        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_CLAUDE_BIN");
        crate::ai::forget_refusal();
        env::remove_var("SKEIN_RESUME_CMD");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    /// A launch that never reached `skein` says which program the shell could not run, and where it
    /// looked — and does not answer for a launch that *did* run.
    ///
    /// **The failure worth checking is not an ENOENT.** It is the day the two resolutions
    /// [`skein_exe`] can produce are collapsed into one sentence: a bare name and an absolute
    /// sibling path both come back from the shell as `not found`, and their cures are opposite — one
    /// is the `$PATH` the server was started with, the other is a build missing half its binaries.
    /// Naming `$PATH` for the second sends the reader to check something that is fine, which is the
    /// mistake `act`'s own not-started test guards in the other direction.
    ///
    /// The `$PATH` arm asserts the sentence still ENDS in `util::spawn_failure`'s, whatever that has
    /// become — the SKEIN-429 rule: one thing said about a program skein could not start, in one
    /// place. Checked against a resolution supplied by hand rather than a real failed launch,
    /// because arranging one means a server with no sibling binary and no `skein` on `$PATH`.
    #[test]
    fn a_launch_that_never_reached_skein_names_the_program_and_where_the_shell_looked() {
        let _env = env_lock();

        let bare = never_ran("web-main", 127, SkeinCli::OnPath)
            .expect("a 127 from the launcher is a launch that never ran");
        assert!(
            bare.starts_with("box web-main was never started: its launch runs under `sh -c`, and"),
            "the reader is not told which box, or what its launch runs under: {bare}"
        );
        assert!(
            bare.ends_with(&crate::util::spawn_failure(
                &Command::new("skein"),
                &std::io::Error::from(std::io::ErrorKind::NotFound)
            )),
            "sandbox has gone back to writing its own version of util's sentence: {bare}"
        );
        // The PATH itself. By the time the reader goes to look, they are looking at their own
        // shell's, which is the one that works — so it cannot be recovered afterwards.
        let path = env::var("PATH").unwrap_or_default();
        assert!(
            !path.is_empty() && bare.contains(&path),
            "the message never says which PATH the launch looked on: {bare}"
        );

        // A sibling that is gone is not a PATH problem, and reporting it as one sends the reader to
        // the wrong file entirely.
        let beside = never_ran("web-main", 127, SkeinCli::Beside("/opt/skein/skein".into()))
            .expect("a 127 is a launch that never ran however `skein` was spelled");
        assert!(
            beside.contains("/opt/skein/skein") && !beside.contains("PATH"),
            "a launch that named an absolute binary was reported as a missing PATH entry: {beside}"
        );

        // 126 is a file the shell FOUND. Saying it was not found sends the reader to install
        // something they already have.
        let denied = never_ran("web-main", 126, SkeinCli::OnPath)
            .expect("126 is the shell declining to execute what it found");
        assert!(
            denied.contains("found") && !denied.contains("os error"),
            "the shell's refusal was dressed up as an error the OS never returned: {denied}"
        );

        // Everything else is `skein start` reporting on itself, and it keeps its own reason.
        for ran in [0, 1, 2, 130, 255] {
            assert_eq!(
                never_ran("web-main", ran, SkeinCli::OnPath),
                None,
                "exit {ran} came from `skein start`, and answering for it hides what it said"
            );
        }
    }

    /// The reason survives the terminal, and does not overwrite the one `skein` wrote for itself.
    ///
    /// Both halves are the bug. `absent_box_reason` reads `starts/<box>.err` and is the only thing a
    /// reconnecting terminal has to go on; when the launch could not run `skein` at all, nothing
    /// wrote that file and the person who had just pressed Launch was told "There is no record of a
    /// start having been attempted" (SKEIN-589). And the reverse, once something outside `skein`
    /// writes there: a generic sentence landing on top of the specific one `skein start` recorded
    /// would replace the answer with a summary of it.
    #[test]
    fn a_launch_that_never_ran_is_recorded_and_leaves_skeins_own_reason_alone() {
        let _env = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);

        let wrote = remember_launch_never_ran("web-main", 127).expect("127 is recorded");
        let kept = crate::fleet::last_start_failure("web-main")
            .expect("the reconnect has something to read");
        assert!(
            kept.contains("web-main") && kept.contains("never started"),
            "what a reconnecting terminal reads does not name the box or say it never started: {kept}"
        );
        assert!(
            wrote.starts_with(&kept[..40]),
            "the sentence handed back is not the one that was kept: {wrote} / {kept}"
        );

        // `skein` ran and said why it failed. Nothing here may stand on top of that.
        crate::fleet::remember_start_failure("web-main", "no registered repo for box web-main");
        assert_eq!(
            remember_launch_never_ran("web-main", 1),
            None,
            "a launch whose `skein` ran was answered for by the layer above it"
        );
        assert_eq!(
            crate::fleet::last_start_failure("web-main").as_deref(),
            Some("no registered repo for box web-main"),
            "the reason `skein start` recorded was overwritten by a generic one"
        );

        env::remove_var("SKEIN_HOME");
    }
}
