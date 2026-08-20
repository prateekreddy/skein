//! The one sandbox that hosts many boxes.
//!
//! An sbx sandbox is a microVM, and its memory is a *reservation*: skein's original model gives each
//! box one, so five boxes reserve five times the peak even though four of them are idle in an editor.
//! On a 36 GB machine that is what makes the host start refusing work. The boxes' needs are spiky and
//! rarely simultaneous — a build wants 5 GB for two minutes, editing wants almost nothing — so the
//! fix is to make memory a pool they share rather than N ceilings that sum.
//!
//! So: one sandbox, many boxes, each in its own bwrap namespace with its own `/tmp`, `$HOME` and
//! checkout (see `box-session.sh`). Inside a VM, cgroup limits are *ceilings* rather than
//! reservations — a box capped at 8 GB that uses 200 MB costs 200 MB — which is the whole reason
//! this shape wins.
//!
//! Everything here is inert until [`crate::place::fleet_sandbox`] names a sandbox. Boxes already
//! running as their own VM keep running that way: [`crate::place::place_of`] follows a box's own
//! record, so turning this on never retroactively reinterprets one.

use crate::config::skein_home;
use crate::config::*;
use crate::kit::KIT_STARTUP_SH;
use crate::place::{anchor_probe, parse_anchor_probe, record_agent_port, recorded_agent_port};
use crate::place::{
    fleet_sandbox, forget_place, own_sandbox, place_of, placed_boxes, record_place, shared_record,
    Place, PlaceRecord,
};
use crate::repos::agent_for_box;
use crate::repos::{
    branch_of, is_git_url, is_ssh_url, launch_spec, load_repos, remote_origin_url, repo_for_box,
    write_launch_spec_for_agent, Repo,
};
use crate::sbx::fleet_boxes;
use crate::util::valid_name;
use crate::util::*;
use chrono::Utc;
use std::io::IsTerminal;
use std::time::Duration;

/// The launcher, embedded so it can be installed into a sandbox that has never seen this repo.
/// The fleet sandbox hosts boxes from *many* repos, so it cannot be served out of any one repo's
/// store — and shipping it through a store would put runtime tooling in shared data besides.
const BOX_SESSION_SH: &str = include_str!("box-session.sh");
const GIT_CREDENTIAL_SH: &str = include_str!("git-credential-skein.sh");
/// The in-sandbox agent, shipped in the binary for the same reason the launcher is: an installer
/// that fetched it would need the network working at exactly the moment things are going wrong.
const FLEET_AGENT_PY: &str = include_str!("fleet-agent.py");

/// Where box roots live inside the fleet sandbox.
///
/// Deliberately not under `$HOME` or `/tmp`: `box-session.sh` binds the box's own directories over
/// both, so a root beneath either would be visible only from inside the box that owns it — and the
/// tmux socket and anchor pidfile that skein reads from outside live in this root.
///
/// `$SKEIN_FLEET_ROOT` overrides it, in the same spirit as `$SKEIN_LS_CMD` and `$SKEIN_LAUNCH_CMD`:
/// `/boxes` needs root to create, so without this seam the launch path could only ever be exercised
/// against a real sandbox — which is precisely the part that kept going untested.
pub fn fleet_root() -> String {
    std::env::var("SKEIN_FLEET_ROOT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/boxes".to_string())
}

/// Where the launcher is installed inside the fleet sandbox.
pub fn box_session_path() -> String {
    format!("{}/.skein/box-session.sh", fleet_root())
}

/// Where the in-sandbox agent is installed. Beside the launcher, for the same reason: the fleet
/// root outlives the sandbox's `/tmp` and belongs to skein rather than to any one box.
pub fn fleet_agent_path() -> String {
    format!("{}/.skein/fleet-agent.py", fleet_root())
}

/// Where git's credential helper is installed. Beside the launcher, because every box's gitconfig
/// names this path and a box that cannot find it falls back to having no credential at all.
pub fn git_credential_helper_path() -> String {
    format!("{}/.skein/git-credential-skein", fleet_root())
}

/// Where the agent's token lives **inside** the sandbox.
///
/// Deliberately not under [`fleet_root`]'s box directories and never in the shared `.claude` store:
/// the store is mounted into every box, and this token authorises running commands in *any* box's
/// namespace. A copy inside one box would hand that box the run of all of them.
pub fn fleet_agent_token_path() -> String {
    format!("{}/.skein/fleet-agent.token", fleet_root())
}

/// The port the agent listens on **inside** the sandbox. Fixed, and deliberately so.
///
/// Only skein's agent listens inside the sandbox, so there is nothing here to collide with — while
/// the *host* side collides with everything else on the machine. Splitting them that way means a
/// host-side conflict is re-published with one call and never has to restart the agent, or reach
/// into a sandbox that may be exactly the thing not answering.
pub const AGENT_SANDBOX_PORT: u16 = 8317;

/// A host port nothing is listening on right now.
///
/// Asked of the OS rather than scanned, which is both faster and honest about what "free" means.
/// It is a hint and not a reservation — the port can be taken between here and the publish — but
/// every caller verifies afterwards by connecting, so a lost race costs one retry and not a lie.
fn free_host_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()?
        .local_addr()
        .ok()
        .map(|a| a.port())
}

/// Publish the agent's port to the host, and keep it published. Returns the working host port.
///
/// The healing is the point, and it is built around a specific sbx behaviour: a port mapping
/// survives `sbx rm` and is still *reported* by `sbx ports` while every connection through it is
/// refused (docker/sbx-releases#297). skein recreates the fleet sandbox on every resize, so it
/// reaches that state routinely. Anything that healed by reading `sbx ports` would therefore call
/// the broken mapping healthy forever — so every candidate here is judged by whether the agent
/// **answers**, never by what sbx says about it.
///
/// Order of preference: the pinned port if the user set one, then the last one that worked, then a
/// fresh one from the OS. A port that is already working is returned untouched — the common case is
/// a no-op with a single connection to prove it.
pub fn ensure_fleet_agent_port(sandbox: &str) -> Result<u16, String> {
    let pinned = load_config().fleet_agent_port;
    // Already working: nothing to publish, and re-publishing a healthy mapping is how a working
    // fleet acquires a broken one.
    for candidate in [pinned, recorded_agent_port().unwrap_or(0)] {
        if candidate != 0 && crate::place::agent_answers(candidate) {
            record_agent_port(candidate);
            return Ok(candidate);
        }
    }

    // Mappings sbx already has for our sandbox port, before making another one. sbx has no
    // unpublish verb, so every new mapping is permanent — publishing one per server restart would
    // accumulate them forever, and each dead one is exactly the phantom that #297 describes. Reuse
    // is the only way not to leak.
    let mut tried: Vec<String> = Vec::new();
    for port in existing_agent_ports(sandbox) {
        if settled_answer(port) {
            record_agent_port(port);
            return Ok(port);
        }
        tried.push(format!("{port}: an existing mapping, still silent"));
    }

    // A pinned port is tried first and only once: the user asked for that number, and quietly
    // serving a different one would make the pin a suggestion.
    let candidates: Vec<u16> = if pinned != 0 {
        vec![pinned]
    } else {
        // Two chances, not more. Each attempt that fails leaves a mapping behind that nothing can
        // remove, so the cost of trying again is permanent clutter in the sandbox's port table.
        std::iter::repeat_with(free_host_port)
            .take(2)
            .flatten()
            .collect()
    };

    for port in candidates {
        match publish_agent_port(sandbox, port) {
            Ok(()) => {
                if settled_answer(port) {
                    record_agent_port(port);
                    return Ok(port);
                }
                // Published and still silent. Either the agent is not up — the caller starts it
                // before this, so that is a real failure — or this is the phantom mapping above.
                tried.push(format!("{port}: published but the agent did not answer"));
            }
            Err(why) => tried.push(format!("{port}: {why}")),
        }
    }
    Err(format!(
        "could not publish the fleet agent's port ({}). \
         The board falls back to `sbx exec`, which still works — only its resilience is reduced.",
        tried.join("; ")
    ))
}

/// Does the agent answer on `port`, allowing a moment for a fresh mapping to come up?
///
/// A publish returns before its forwarder is necessarily accepting, and a single immediate check
/// gets an instant refusal rather than a timeout — so it reads as "broken" and moves on, burning a
/// port that would have worked a second later. Since a burnt port cannot be unpublished, that
/// mistake is permanent, which is what makes the wait worth more than the latency.
fn settled_answer(port: u16) -> bool {
    // No waiting under test: the fixtures either listen already or never will, so the window would
    // only be spent sleeping — it took the suite from 2.7s to 15s, which is how a test file stops
    // being run often enough to be worth having.
    let attempts = if cfg!(test) { 1 } else { 6 };
    for attempt in 0..attempts {
        if crate::place::agent_answers(port) {
            return true;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    false
}

/// Host ports sbx already forwards to [`AGENT_SANDBOX_PORT`] in this sandbox.
///
/// Parsed from the table `sbx ports <sandbox>` prints — `HOST IP / HOST PORT / SANDBOX PORT /
/// PROTOCOL` — because the alternative is publishing a new mapping on every server restart and
/// never being able to remove any of them. Deduplicated: the same host port is listed once per
/// address family (`127.0.0.1` and `::1`), and they are one mapping.
fn existing_agent_ports(sandbox: &str) -> Vec<u16> {
    let Ok((out, _, 0)) = run_capture_for("sbx", &["ports", sandbox], Duration::from_secs(20))
    else {
        return Vec::new();
    };
    let mut found: Vec<u16> = out
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let host_port: u16 = cols.nth(1)?.parse().ok()?;
            let sandbox_port: u16 = cols.next()?.parse().ok()?;
            (sandbox_port == AGENT_SANDBOX_PORT).then_some(host_port)
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// One `sbx ports … --publish` call.
///
/// Its own function so the wire format is in one readable place: `HOST:SANDBOX/PROTOCOL`, which is
/// sbx's spelling and not a guess — an unpublish verb does not exist, which is why healing moves to
/// a new port rather than tidying up the old one.
fn publish_agent_port(sandbox: &str, host_port: u16) -> Result<(), String> {
    let mapping = format!("{host_port}:{AGENT_SANDBOX_PORT}/tcp");
    let (out, err, code) = run_capture_for(
        "sbx",
        &["ports", sandbox, "--publish", &mapping],
        Duration::from_secs(30),
    )?;
    if code == 0 {
        return Ok(());
    }
    let detail = if err.trim().is_empty() { out } else { err };
    Err(detail.trim().to_string())
}

/// The tmux session the agent runs in, on the sandbox's own socket.
///
/// tmux rather than `nohup`/`setsid` because the sandbox already has it (the substrate installs it,
/// since a box without tmux cannot exist) and because it makes the agent *inspectable*: whether it
/// is running is one `has-session`, and its output is a pane someone can read when it misbehaves.
const AGENT_SESSION: &str = "skein-fleet-agent";

/// Install the agent and make sure it is running. Idempotent, and safe to call on every ensure.
///
/// Returns the token, so the caller can record the same secret host-side — the two must agree, and
/// generating it in one place and returning it is how they cannot drift.
///
/// **It publishes the port too**, via [`ensure_fleet_agent_port`], which judges every candidate by
/// whether the agent answers through it rather than by what `sbx ports` claims. This once said the
/// opposite — publish by hand, then pin the number in `fleet_agent_port` — and that instruction
/// outlived the code by long enough to be followed. A pin is still honoured and still useful when
/// something else needs the number in advance; it is no longer required to have a transport at all.
pub fn ensure_fleet_agent(sandbox: &str) -> Result<String, String> {
    let token = crate::place::ensure_agent_token()?;
    let place = own_sandbox(sandbox);

    // The script first, over stdin: it is large, and `sbx exec`'s argv is visible in every process
    // listing on the host.
    let path = fleet_agent_path();
    let dir = fleet_agent_path();
    let dir = dir
        .rsplit_once('/')
        .map(|(d, _)| d)
        .unwrap_or("/boxes/.skein");
    place
        .write(
            &format!(
                "mkdir -p {} && cat > {} && chmod 700 {}",
                sh_quote(dir),
                sh_quote(&path),
                sh_quote(&path)
            ),
            FLEET_AGENT_PY.as_bytes(),
            Duration::from_secs(30),
        )
        .map_err(|e| format!("installing the fleet agent in {sandbox}: {e}"))?;

    // The token over stdin too, and for a stronger reason than size: an argument would put the
    // secret in `ps` on the host and in the shell history of anything that logged the call.
    let token_path = fleet_agent_token_path();
    place
        .write(
            &format!(
                "umask 077 && cat > {} && chmod 600 {}",
                sh_quote(&token_path),
                sh_quote(&token_path)
            ),
            token.as_bytes(),
            Duration::from_secs(30),
        )
        .map_err(|e| format!("installing the fleet agent token in {sandbox}: {e}"))?;

    // The script has just been written; the process serving is whatever started before that, and
    // `start_fleet_agent` leaves a running session alone. So an upgrade would install a newer agent
    // and never run it — the setting on, the port answering, and every new endpoint quietly missing
    // while the host fell back to `sbx exec` for the calls that needed it. Retire it first.
    retire_stale_agent(sandbox);

    // Start before publishing: `ensure_fleet_agent_port` judges a mapping by whether the agent
    // answers through it, so publishing first would fail every candidate and burn all three.
    start_fleet_agent(sandbox)?;
    // And publish only once something is actually behind the mapping.
    //
    // **sbx has no unpublish.** Every mapping made here lasts as long as the sandbox, so publishing
    // to find out whether the agent is up spends a permanent resource on a question that has a
    // cheap answer: ask the sandbox whether the process exists. Without this, a fleet that cannot
    // run the agent at all — no python3, a substrate that never installed, a crash loop — leaks two
    // mappings per attempt, for ever, and each dead one is exactly the phantom sbx keeps reporting
    // as published (docker/sbx-releases#297).
    if !agent_process_is_up(sandbox) {
        return Err(format!(
            "the agent was installed and started in {sandbox} but no python process is running \
             there — nothing was published, since a port mapping cannot be withdrawn. Check \
             `tmux -S … capture-pane` in the sandbox, or that python3 is present"
        ));
    }
    ensure_fleet_agent_port(sandbox)?;
    Ok(token)
}

/// Stop an agent that is older than the one skein has just installed, so the supervisor starts the
/// new one. A no-op when nothing is serving or what is serving is current.
///
/// Silent about failure on purpose: every outcome is recoverable by the code that follows. A kill
/// that did not land leaves the old agent up, which still carries `/exec`; a kill that landed and a
/// start that did not is reported by `ensure_fleet_agent_port`, which connects to find out.
fn retire_stale_agent(sandbox: &str) {
    let Some(port) = recorded_agent_port() else {
        return;
    };
    match crate::place::agent_protocol(port) {
        // Nothing answering, or already current — `start_fleet_agent` handles both.
        None => return,
        Some(version) if version >= crate::place::AGENT_PROTOCOL => return,
        Some(_) => {}
    }
    stop_fleet_agent(sandbox);
}

/// Take the agent away from a fleet that has switched it off. A no-op when nothing is serving.
///
/// The probe first is what keeps this off the hot path: `heal_fleet_agent` runs on every server
/// start and every box start, and a fleet that has never had an agent would otherwise pay an
/// `sbx exec` each time to kill a process that was never there. A local connection to a port with
/// nothing behind it is refused immediately, so the common case costs a syscall.
fn remove_fleet_agent(sandbox: &str) {
    let Some(port) = recorded_agent_port() else {
        return;
    };
    if !crate::place::agent_answers(port) {
        return;
    }
    stop_fleet_agent(sandbox);
}

/// Is the agent's python actually running in the sandbox?
///
/// Asked over `sbx exec` and not over the agent, for the obvious reason: the answer is about whether
/// there is an agent to ask. `pgrep -f` against the same anchored pattern the retirement uses, so
/// the two cannot disagree about what "the agent" is.
///
/// A sandbox that cannot answer at all reads as *not up*: publishing a port to something skein
/// cannot see is exactly the permanent mistake this check exists to avoid.
fn agent_process_is_up(sandbox: &str) -> bool {
    let script = format!(
        "pgrep -f {} >/dev/null 2>&1 && echo up",
        sh_quote(&agent_pkill_pattern(&fleet_agent_path()))
    );
    own_sandbox(sandbox)
        .exec_sbx(&script, Duration::from_secs(20))
        .map(|out| out.trim() == "up")
        .unwrap_or(false)
}

/// Stop the agent and its supervisor. Silent about failure on purpose — see the callers, each of
/// which is recoverable by the code that follows it.
///
/// Killing the session is what stops the agent: the session *is* the `while true` supervisor, so
/// ending it stops the restart as well as the process. `pkill` is for a python that somehow outlived
/// its supervisor, and is allowed to find nothing.
fn stop_fleet_agent(sandbox: &str) {
    let script = format!(
        "tmux kill-session -t {session} 2>/dev/null; pkill -f {pattern} 2>/dev/null; true",
        session = sh_quote(AGENT_SESSION),
        pattern = sh_quote(&agent_pkill_pattern(&fleet_agent_path())),
    );
    // Over `sbx exec` and never the agent: this kills the process that would be carrying the reply,
    // so a successful stop would come back as a transport failure.
    let _ = own_sandbox(sandbox).exec_sbx(&script, Duration::from_secs(30));
}

/// The pattern that matches the agent process and **only** the agent process.
///
/// `pkill -f` matches against a process's whole command line, so the bare path matched far more than
/// intended: the `while true` supervisor that would restart the agent, the tmux session holding that
/// supervisor, and any shell whose command line merely mentions the path — including the one running
/// the `pkill`. Retiring an agent by killing its own supervisor is a stop, not a restart, and that
/// is precisely what happened when this was run outside its usual sandwich: the agent went away and
/// nothing brought it back.
///
/// Anchoring at `python` fixes it, because that is what distinguishes the process from everything
/// that merely refers to it. Dots are escaped since `-f` takes an extended regular expression and an
/// unescaped `.` would match any character.
fn agent_pkill_pattern(path: &str) -> String {
    format!("^python[0-9.]* {}( |$)", path.replace('.', "\\."))
}

/// Start the agent if it is not already up, and leave it supervised.
///
/// The `while true` is the supervision: a Python process that dies — OOM-killed, a bug, a signal —
/// must come back, because everything that depends on it degrades silently to `sbx exec` and the
/// only symptom is the board being as fragile as it was before. The `sleep 2` keeps a crash-loop
/// from becoming a busy loop on a sandbox that is already unwell.
///
/// The port is read back from the host's config so the sandbox and the host agree on one number,
/// and it is the *sandbox-side* port here — what `sbx ports` maps to the host is the host's business.
pub fn start_fleet_agent(sandbox: &str) -> Result<(), String> {
    let port = AGENT_SANDBOX_PORT;
    let script = format!(
        "tmux has-session -t {session} 2>/dev/null && exit 0; \
         tmux new-session -d -s {session} {inner}",
        session = sh_quote(AGENT_SESSION),
        inner = sh_quote(&format!(
            "while true; do python3 {} {} {}; sleep 2; done",
            sh_quote(&fleet_agent_path()),
            port,
            sh_quote(&fleet_agent_token_path()),
        )),
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(30))
        .map(|_| ())
        .map_err(|e| format!("starting the fleet agent in {sandbox}: {e}"))
}

/// Where the provisioning script is installed inside the fleet sandbox.
///
/// Beside the launcher rather than in `~/.local/bin` (where the kit puts it) for two reasons: a box
/// binds its own `$HOME` over the sandbox's, and the fleet sandbox is created without a kit at all,
/// so nothing would have put it there. Under `--dev-bind / /` this path reads the same from inside
/// every box as it does from the sandbox.
pub fn box_provision_path() -> String {
    format!("{}/.skein/skein-startup.sh", fleet_root())
}

/// One box's root inside the fleet sandbox. Callers must have validated `name`; every path skein
/// derives for a box hangs off this, so a name containing `..` would escape the layout entirely.
pub fn box_root(name: &str) -> String {
    format!("{}/{name}", fleet_root())
}

/// The box's tmux socket — the same path inside the namespace and out, which is what lets skein
/// list, attach to and kill a box without entering it first.
pub fn box_sock(name: &str) -> String {
    format!("{}/session.sock", box_root(name))
}

/// The file `box-session.sh` writes the box's anchor pid to. Read from outside, so it must sit in
/// the box root rather than in the box's private `/tmp`.
pub fn box_pidfile(name: &str) -> String {
    format!("{}/anchor.pid", box_root(name))
}

/// Where boxes keep the state that must outlive the sandbox, on the **host**.
///
/// A host path, mounted into the sandbox at that same absolute path — so this one string addresses
/// it from both sides, exactly as a repo store does. The *parent* is mounted, so a box added later
/// needs no recreate.
///
/// Distinct from a repo's store on purpose: this is per-box and skein-owned, not project-scoped
/// shared data, so nothing here crosses between boxes and it is not somewhere the user puts files.
pub fn box_state_root() -> String {
    skein_home().join("boxes").to_string_lossy().into_owned()
}

/// Where one box's "this is the workshop box" answer is kept.
///
/// A file in the box's host state directory, exactly as its disk and git-scope overrides already
/// are. Host-side so the cockpit can set it with the fleet down, and per box because the whole
/// point is that it is true of one box and false of the rest.
fn privileged_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(&box_state(name)).join("privileged")
}

/// Is this the workshop box — the one that may see every box's files and act at fleet scope?
///
/// Ordinary boxes get a mount namespace that hides the other boxes' directories, and an empty file
/// over the fleet agent's token. That is right for a box doing a repo's work and wrong for the box
/// used to debug and extend skein itself, which needs to read the fleet to be any use at all.
///
/// **Off unless the file says exactly `1`.** Anything else — absent, empty, half-written, corrupted
/// — is off, because the two failure directions are nothing like each other: guessing "privileged"
/// hands one box every other box's credentials, and guessing "ordinary" costs a restart.
pub fn box_is_privileged(name: &str) -> bool {
    std::fs::read_to_string(privileged_path(name))
        .unwrap_or_default()
        .trim()
        == "1"
}

/// Make `name` the workshop box, or return it to being ordinary. Takes effect at its **next start**:
/// a namespace is built when a box comes up, and a running box already has the one it was given.
///
/// Deliberately not exclusive — skein does not clear the flag on other boxes when one is set. Two
/// privileged boxes is a thing someone may want and the cockpit shows plainly; silently un-privileging
/// a box someone is working in, because they ticked a box elsewhere, is not.
pub fn set_box_privileged(name: &str, on: bool) -> Result<(), String> {
    if !crate::util::valid_name(name) {
        return Err(format!("unusable box name {name:?}"));
    }
    let path = privileged_path(name);
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let dir = path.parent().ok_or("no state directory")?.to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, &dir, b"1")
}

/// One box's durable host-side state directory.
pub fn box_state(name: &str) -> String {
    format!("{}/{name}", box_state_root())
}

/// The `sbx create` argv for the fleet sandbox.
///
/// `shell` rather than an agent: nothing runs in the sandbox itself — every agent runs inside a box's
/// namespace, started by `box-session.sh`. The mounts are [`fleet_mounts`]: `~/.skein/repos`, the
/// *parent* of every managed repo's store, so adding one later needs no recreate — plus any adopted
/// repo that lives outside it. A box's checkout is not mounted at all, because boxes clone from the
/// remote onto VM-local disk (measured ~5× faster to write and ~14× faster to read than a virtiofs
/// mount, which matters for a build).
///
/// Memory and CPUs come from [`Config`], and both are ceilings the boxes share rather than one
/// reservation each — which is what makes them safe to set generously. CPUs default to every host
/// core but one, so the machine keeps answering while the fleet compiles.
pub fn create_argv(sandbox: &str, mounts: &[String]) -> Vec<String> {
    let config = load_config();
    let mut argv = vec!["create".to_string(), "--name".into(), sandbox.to_string()];
    let memory = config.fleet_memory.trim();
    if !memory.is_empty() {
        argv.push("-m".into());
        argv.push(memory.to_string());
    }
    let cpus = config.fleet_cpus.trim().to_string();
    let cpus = if cpus.is_empty() {
        host_cpus_less_one()
    } else {
        cpus
    };
    if !cpus.is_empty() {
        argv.push("--cpus".into());
        argv.push(cpus);
    }
    argv.push("shell".into());
    argv.extend(mounts.iter().cloned());
    argv
}

/// The environment `sbx create` needs for what its argv cannot carry — today, the sandbox's disk.
///
/// sbx takes memory and CPUs as flags but disk as an environment variable, read from the *create
/// command's* own environment: `DOCKER_SANDBOXES_ROOT_SIZE=40g sbx run claude` is the documented
/// form, and this is the same thing for `sbx create`. Root defaults to 20 GB.
///
/// This used to claim the value came from the *daemon's* environment, so it only landed if the
/// daemon happened to start with the create. That is wrong, and the running fleet is the proof: it
/// carries the configured 60g on a `vdb` of exactly 60G under a daemon that had been up for hours.
/// The correction matters because the false version made the disk setting look unreliable, and
/// invited working around it.
///
/// What is true is the second half: **the size is fixed for the life of the sandbox.** sbx has no
/// resize — its whole verb list is `login run ls stop rm create exec cp ports` — so changing a disk
/// means a new sandbox, exactly as changing memory does. [`resize_fleet`] is that path for both.
///
/// One shared disk is the fleet's real ceiling. Memory stopped summing when boxes started sharing a
/// sandbox; disk started summing for exactly the same reason.
///
/// There is a **second** disk this does not set: `DOCKER_SANDBOXES_DOCKER_SIZE`, the Docker data
/// disk at `/var/lib/docker`, 50 GB by default and sparse. It is where every box's `docker build`
/// output actually lands, so on a fleet — one Docker daemon shared by every box — it sums the same
/// way the root does, and skein neither sizes nor measures it.
pub fn create_env() -> Vec<(String, String)> {
    let disk = load_config().fleet_disk.trim().to_string();
    if disk.is_empty() {
        return Vec::new();
    }
    vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), disk)]
}

/// Every host directory the fleet sandbox must be able to see.
///
/// [`fleet_workspace`] covers repos skein manages, whose work clone and store both live under it.
/// It does **not** cover a repo adopted in place, or one pointed at a store the user already had —
/// `skein add <path> --store …`, which is how this very project is registered. Those sit anywhere on
/// the host, so they are mounted explicitly or the box cannot read its own store, and provisioning
/// fails for a reason that reads as a skein bug rather than a missing mount.
///
/// Deduped against the workspace, and against each other: mounting a path twice is not obviously
/// harmless, and mounting a *parent* of it is what keeps a later repo from needing a recreate.
pub fn fleet_mounts() -> Vec<String> {
    let workspace = fleet_workspace();
    // The box-state parent too: boxes keep their conversation there, on the host, so it survives the
    // sandbox rather than only surviving a planned resize.
    let mut mounts = vec![workspace.clone(), box_state_root()];
    for repo in load_repos() {
        for path in [repo.store.clone(), repo.work.clone()] {
            let path = path.trim().to_string();
            if path.is_empty() {
                continue;
            }
            if mounts.iter().any(|m| under(&path, m)) {
                continue;
            }
            mounts.push(path);
        }
    }
    mounts
}

/// Is `path` inside `dir` (or `dir` itself)? Textual, because both are host absolute paths skein
/// wrote or normalised, and the answer is needed before any sandbox exists to ask.
fn under(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// How the sandbox's memory is divided, in MiB.
///
/// Three claims on one VM, and until this existed only the first was written down:
///
/// * **boxes** — the whole workload, and all of it under `/sys/fs/cgroup/skein`: everything the
///   agents and their builds run, *and* the containers they start, which dockerd is pointed at
///   [`CONTAINER_CGROUP`] so that they land inside the same parent rather than beside it;
/// * **plumbing** — the sandbox's own container: its init, the ssh-agent forwarder, dockerd and
///   containerd themselves;
/// * **reserve** — everything outside the workload: the kernel, and the VM-level services that
///   answer the host. Capped by nobody, because it is what everything else is measured against.
///
/// The workload is **one** share rather than a boxes half and a Docker half, and that is the whole
/// of what "bounded together" means here. Splitting it read like two protections and was one and a
/// half: only `skein` can actually be capped ([`fleet_limits`] says why `docker` cannot), so the
/// Docker half was never a ceiling on Docker — it was memory withheld from the boxes on Docker's
/// behalf. A fleet whose boxes wanted 20 GB with no container running was told no, and the third
/// held back for `docker build` protected nothing, because nothing was written on that cgroup.
///
/// So the pool is shared and taken first-come: a box may fill it when Docker is idle, and a build
/// may fill it when the boxes are. Sharing it does not mean giving up the bound. The containers are
/// nested *inside* the cgroup that carries the ceiling, so the two are held to the total between
/// them by the same one limit that holds the boxes — and an overshoot is an OOM in whichever of
/// them caused it, never in the sandbox's own processes. The reserve and plumbing shares, the ones
/// that keep the sandbox answering at all, are untouched by the merge.
///
/// The reserve is the point of the whole exercise. There is no swap in the sandbox, so reaching the
/// VM's memory is not a slowdown, it is the kernel's global OOM killer choosing a victim — and it
/// picks by badness, not by blame, so the process it kills is as likely to be what answers the host
/// as the build that caused it. A sandbox whose plumbing was killed is exactly a sandbox that
/// "stops responding" and only comes back when it is cycled. Keeping the shares' sum below the
/// total converts that into an OOM *inside* the offending cgroup, which kills a build.
pub struct MemoryPlan {
    pub boxes: u64,
    pub plumbing: u64,
    pub reserve: u64,
}

/// The division above, or `None` when [`Config::fleet_memory`] names no number to divide.
pub fn memory_plan() -> Option<MemoryPlan> {
    let total = parse_mib(&load_config().fleet_memory)?;
    // A fixed gigabyte plus 2%, because what this covers is mostly *fixed*: the VM's own services
    // do not grow with the size of the VM, and only the kernel's own structures (page tables,
    // per-cpu areas, slab) scale at all. A flat percentage therefore reserves far too much of a big
    // fleet and, at 10%, was 4.6× what a live 26 GiB sandbox actually had outside both cgroups —
    // measured at 574 MiB, of which 191 MiB was unreclaimable kernel memory.
    //
    // It is not tighter than that because the failure it prevents is not graceful. With no swap,
    // overshooting is an instant kill rather than a slowdown, and the victim is chosen across the
    // whole VM — so the cost of being wrong is a dead sandbox, not a slow one. Never more than half
    // either, so a tiny configured total still leaves something to work in.
    let reserve = (1024 + total / 50).min(total / 2);
    // Small and flat: the sandbox's own container holds 70 MiB of anonymous memory on a live fleet.
    // The ~1.7 GiB beside it is page cache and dentry slab, which reclaims under pressure rather
    // than needing to be owned. This is headroom for dockerd and containerd growing with the number
    // of containers, not a share of the workload.
    let plumbing = 512.min(total / 8);
    // Everything left over is the workload's, in one share. A third of it used to be set aside for
    // Docker; see [`MemoryPlan`] for why holding it back protected nothing and cost the boxes a
    // third of the fleet whenever no container was running.
    let boxes = total.saturating_sub(reserve + plumbing);
    Some(MemoryPlan {
        boxes,
        plumbing,
        reserve,
    })
}

/// The per-box cgroup limits, as the `key=value,…` spec `box-session.sh` applies.
///
/// Memory only. **CPU is deliberately not capped**: `cpu.weight` is already equal for every box, so
/// they fair-share under contention and a lone box still gets every core — and capping it would
/// leave cores idle while a box waits, which is the exact waste the shared sandbox exists to end.
/// Memory is different because it is not reclaimable on demand: two boxes wanting 20 GB do not each
/// get 13 slowly, they hit the wall and the kernel starts killing things.
///
/// `max` is what stops one box taking the rest of the boxes down with it. `high` sits below it so
/// the kernel throttles and reclaims first — a box that briefly overshoots gets slower rather than
/// losing its turn.
///
/// Defaults are 70% and 55% **of the boxes' share** ([`memory_plan`]), not of the whole VM. They
/// were once fractions of the fleet total, which read like a protection and was not one: 70% of the
/// VM each, with nothing capping the sum, meant any two boxes could exhaust it between them. What
/// bounds the fleet is the ceiling on their shared parent; this bounds one box against the others.
///
/// `pids.max` is the fork-bomb guard; a runaway spawn loop in one box would otherwise exhaust the
/// VM's pid space and no box could start a process.
pub fn box_limits() -> String {
    let config = load_config();
    let share = memory_plan().map(|plan| plan.boxes);
    let pick = |explicit: &str, fraction: u64| -> Option<String> {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Some(explicit.to_string());
        }
        share.map(|boxes| format!("{}M", (boxes * fraction / 100).max(512)))
    };
    let mut parts = Vec::new();
    if let Some(max) = pick(&config.box_memory_max, 70) {
        parts.push(format!("max={max}"));
    }
    if let Some(high) = pick(&config.box_memory_high, 55) {
        parts.push(format!("high={high}"));
    }
    parts.push("pids=8192".to_string());
    parts.join(",")
}

/// The ceilings on the two cgroups that hold everything a box can cause, as the `<cgroup>=max/high`
/// spec `box-session.sh` applies. Empty when no fleet total is configured to divide.
///
/// `skein` is the parent of every box's cgroup, so it is the only place the boxes' *sum* can be
/// bounded — a per-box ceiling never could be. It is the one ceiling here, and it is a number.
///
/// `docker` is named too, but only to be handed `max/max` — an explicit *absence* of a ceiling.
/// Earlier versions capped it, on the reasoning that a box's `docker build` runs there and no
/// per-box limit reaches it. That reasoning was right about the hole and wrong about the patch,
/// because of what else lives in that cgroup: the sandbox's own container is a child of it, so
/// `/sys/fs/cgroup/docker` holds init, `socat`, dockerd and containerd — the machinery that answers
/// `sbx exec`. Adding the plumbing share to its ceiling was an attempt to leave that machinery
/// room, and it does not work, because the failure is not about the size of the number.
///
/// Measured on this fleet while it was wedged, with `memory.high` at 7.57 GiB and the cgroup at
/// 7.73 GiB of *anonymous* memory behind 73 MiB of page cache: `pgscan` 43,232 MiB against
/// `pgsteal` 45 MiB. The kernel scanned 43 GB to recover 45 MB — 1,695 throttle events a second,
/// ten of eleven cores, indefinitely. `memory.high` throttles by stalling the allocator until
/// reclaim catches up, which is humane when the overshoot is brief and there is cache to give back.
/// A linker holding 3.4 GB for ten minutes with no swap satisfies neither: there is nothing to
/// reclaim, so the stall never ends. And because init and `socat` share the cgroup, the stall lands
/// on the sandbox's own service path — new `sbx exec` calls hang while established streams, already
/// faulted in, keep flowing. The VM had 16 GB free throughout.
///
/// `memory.max` is no better placed. It kills rather than stalls, and the OOM it would trigger
/// picks its victim from a cgroup containing pid 1 — trading a stuck build for a dead sandbox.
///
/// So the containers are moved instead of the ceiling. [`install_docker_config`] points dockerd at
/// [`CONTAINER_CGROUP`] — `skein/containers`, a child of the boxes' own parent — and what stays
/// behind in `/docker` is the sandbox itself, which nothing caps and nothing should.
///
/// That is what makes `skein` the one ceiling *and* a real one. It is sized to the whole workload,
/// and once dockerd has been pointed inside it, the whole workload is what it actually holds:
/// boxes and containers under one limit, taken first-come, with an overshoot killed in whichever
/// of them caused it and never in the sandbox's own processes. [`MemoryPlan`] keeps no third back
/// for Docker, because a reservation is the opposite of a shared pool — it was memory withheld
/// from the boxes on behalf of a cgroup nothing was written on, buying no protection and idling
/// real memory every hour no container ran.
///
/// Until a fleet has cycled, its containers are still in `/docker` and outside this ceiling: the
/// setting only decides where the *next* dockerd puts them, and restarting dockerd to hurry it
/// would stop every running container. Uncapped for one more boot is the cheaper wrong.
///
/// Applied on every box start rather than once, because dockerd recreates `/sys/fs/cgroup/docker`
/// from scratch when the sandbox cycles, taking any limit written on it with it. That is also why
/// `max/max` is written rather than simply omitted: a fleet an older skein already capped keeps
/// that cap until something writes over it.
pub fn fleet_limits() -> String {
    let Some(plan) = memory_plan() else {
        return String::new();
    };
    // `high` below `max` for the same reason it is per box: past it the kernel reclaims and
    // throttles, so a fleet that briefly overshoots gets slower instead of losing a box.
    let ceiling = |mib: u64| format!("{mib}M/{}M", (mib * 9 / 10).max(512));
    // `total` is what these are a share OF, and it travels with them because only the sandbox can
    // check it. sbx fixes a sandbox's memory when it is created, so editing Fleet memory without
    // rebuilding leaves this describing a VM that does not exist — and a ceiling worked out for a
    // machine twice the real size is not a ceiling. The launcher scales by what it actually finds.
    format!(
        "total={}M,skein={},docker=max/max",
        plan.boxes + plan.plumbing + plan.reserve,
        ceiling(plan.boxes),
    )
}

/// Why a box has no memory ceiling, or `None` when it has one.
///
/// Read from the file `box-session.sh` leaves behind rather than from its stderr: skein keeps a
/// command's stdout and discards stderr on success, so a warning about a box that started *fine*
/// would be dropped exactly when nothing looked wrong. The fact outlives the launch that produced
/// it, which is what lets anything later — a doctor check, a row on the board — still ask.
pub fn uncapped_reason(name: &str) -> Option<String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return None;
    }
    let path = format!("{}/limits.state", box_root(name));
    let state = own_sandbox(&sandbox)
        .exec(&format!("cat {}", sh_quote(&path)), Duration::from_secs(15))
        .ok()?;
    let mut parts = state.split_whitespace();
    match parts.next()? {
        "uncapped" => Some(parts.next().unwrap_or("reason not recorded").to_string()),
        _ => None,
    }
}

/// Apply the current per-box ceilings to every box that is already running.
///
/// Unlike the fleet's own memory, a cgroup limit is **live**: writing `memory.max` changes the cap
/// on a running box immediately, with no restart and nothing to save or restore. So a tighter or
/// looser per-box ceiling is a setting you can simply change, and it would be wrong to make the user
/// rebuild the fleet for it — that is the expensive path, and this is not.
///
/// Returns the boxes whose limits could not be written. Best-effort per box on purpose: one box
/// missing its cgroup (started before delegation existed, say) must not stop the others being
/// corrected.
pub fn apply_box_limits() -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured".into());
    }
    let limits = box_limits();
    let fleet = own_sandbox(&sandbox);
    let mut failed = Vec::new();
    // The shared ceilings first, and reported under a name no box answers to. They are what keeps
    // the sandbox itself alive (see `fleet_limits`), so a run that fixed every box and silently
    // left these unwritten would have skipped the important half.
    //
    // Handed back to the launcher rather than written from here, because the half that matters can
    // only be done inside: these numbers are a share of the *configured* fleet size, and the
    // sandbox is the only thing that knows what it really got. Reinstalled first, so a sandbox
    // built before `--ceilings` existed gets the copy that has it.
    install_launcher(&sandbox)?;
    let ceilings = format!(
        "SKEIN_FLEET_LIMITS={} {} --ceilings",
        sh_quote(&fleet_limits()),
        sh_quote(&box_session_path())
    );
    if fleet.exec(&ceilings, Duration::from_secs(30)).is_err() {
        failed.push("the fleet's shared ceilings".to_string());
    }
    for (name, _) in placed_boxes(&sandbox) {
        let mut writes = Vec::new();
        for kv in limits.split(',') {
            let Some((key, value)) = kv.split_once('=') else {
                continue;
            };
            let file = match key {
                "max" => "memory.max",
                "high" => "memory.high",
                "pids" => "pids.max",
                _ => continue,
            };
            writes.push(format!(
                "printf '%s\\n' {} | sudo tee /sys/fs/cgroup/skein/{}/{file} >/dev/null",
                sh_quote(value),
                name
            ));
        }
        // `test -d` first, so a box with no cgroup is reported rather than counted as adjusted.
        let script = format!(
            "test -d /sys/fs/cgroup/skein/{name} || exit 1; {}",
            writes.join(" && ")
        );
        if fleet.exec(&script, Duration::from_secs(30)).is_err() {
            failed.push(name);
        }
    }
    Ok(failed)
}

/// Bring a fleet that already exists into line with the skein that has just started.
///
/// A fleet sandbox is long-lived and skein is not: the sandbox keeps the launcher and the cgroup
/// ceilings it was last given, and nothing about restarting the server replaced either. So an
/// upgrade landed in a state where the *host* had one skein and the *sandbox* had the last one's
/// idea of how to start a box — and where the two disagree about the spec they pass between them,
/// every box in that fleet stops starting until something reinstalls the launcher. That happened:
/// a launcher that could not read `docker=max/max` exited before tmux, so each reconnect found no
/// session, and the stale anchor pid it then entered read as `nsenter: cannot open
/// /proc/<pid>/ns/user` — an error about namespaces, for a fleet that needed a file copied.
///
/// Repairing it on server start rather than on the settings save that changed the number, because
/// the mismatch is not caused by a setting: it is caused by *this binary* being newer than the copy
/// out there, which is exactly what a restart means and nothing else observes.
///
/// Both halves are idempotent — the launcher is written whole, and the ceilings are values, not
/// deltas — so a fleet that was already current pays two `sbx exec` calls and changes nothing.
///
/// **Only a sandbox already awake.** Waking one costs a VM boot, and starting the cockpit is not a
/// request to run the fleet — `sbx ls` is asked instead of the sandbox itself, so a sleeping fleet
/// is left asleep. It is not left stale either: [`ensure_box_session`] reinstalls the launcher on
/// the path that wakes it, so the repair happens when the fleet is next actually used.
///
/// A fleet that is asleep and a fleet that could not be *asked* are handled the same way and said
/// differently. The second is reported, because everything below this point is skipped on a
/// question that timed out, and a skipped repair that says nothing is indistinguishable from one
/// that succeeded.
pub fn heal_fleet() -> Result<(), String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Ok(()); // one sandbox per box — no shared launcher to be stale
    }
    // "Asleep" and "sbx did not answer" both mean *don't touch it*, and they used to be the same
    // branch. They are not the same thing to say. Leaving a sleeping fleet asleep is the intent
    // above; a fleet skein could not *see* is a repair that quietly did not happen — and this gate
    // stands in front of the launcher, the agent and the docker config alike, so a daemon too busy
    // to answer within `fleet_boxes`'s budget skips all three and reports nothing. That is the same
    // stall the in-sandbox agent exists to survive, deciding whether the agent gets installed.
    let Some(boxes) = crate::sbx::fleet_boxes() else {
        eprintln!(
            "skein: {}, so {sandbox} was not brought into line with this build — its launcher, \
             agent and docker config are whatever the last server left. They are repaired on the \
             next box start.",
            crate::sbx::fleet_failure().unwrap_or_else(|| "sbx did not answer".into())
        );
        return Ok(());
    };
    let awake = boxes
        .iter()
        .any(|b| b.name == sandbox && b.live == Some(crate::sbx::Liveness::Running));
    if !awake {
        return Ok(());
    }
    install_launcher(&sandbox)?;
    // Here as well as in `ensure_fleet`, and this is the call that matters for switching it on: a
    // server restart is when the setting is read, and a fleet that has been up for days would
    // otherwise never install an agent until the next box start.
    heal_fleet_agent(&sandbox);
    // Best-effort and reported rather than fatal: this only decides where the *next* dockerd puts
    // its containers, so failing it costs the merged pool its enforcement, not the fleet its boxes.
    if let Err(e) = install_docker_config(&sandbox) {
        eprintln!(
            "skein: could not point dockerd at the workload cgroup ({e}); containers in {sandbox} \
             stay outside the ceiling"
        );
    }
    // The same call the cockpit's "apply now" makes, and for the same reason it is handed back to
    // the launcher rather than written from here: these numbers are a share of the *configured*
    // fleet size, and only the sandbox knows what it really got (see `fleet_limits`).
    let ceilings = format!(
        "SKEIN_FLEET_LIMITS={} {} --ceilings",
        sh_quote(&fleet_limits()),
        sh_quote(&box_session_path())
    );
    own_sandbox(&sandbox)
        .exec(&ceilings, Duration::from_secs(30))
        .map(|_| ())
        .map_err(|e| format!("reapplying the shared ceilings in {sandbox}: {e}"))
}

/// Where dockerd is told to put the containers it runs: **inside** the workload cgroup, beside the
/// boxes rather than in a tree of its own.
///
/// This is what makes the merged pool real. [`memory_plan`] gives the boxes and the containers they
/// start one share between them, and until dockerd is told this, that share was an intention with
/// nothing enforcing it — `/sys/fs/cgroup/skein` bounded only the boxes, and a container could take
/// as much again beside it. Nested here, the one ceiling on `skein` covers both, first-come, which
/// is what "one pool" was supposed to mean.
///
/// It has to be a *child* of `skein` rather than a sibling with its own ceiling, because a second
/// ceiling would be a second reservation — the boxes idling memory the containers may not have and
/// the other way round, which is the thing the merge removed.
pub const CONTAINER_CGROUP: &str = "/skein/containers";

/// Point the sandbox's dockerd at [`CONTAINER_CGROUP`].
///
/// **Why this can be done at all, when capping `/sys/fs/cgroup/docker` could not.** That cgroup is
/// where dockerd puts its containers *and* where the sandbox's own container lives — init, socat,
/// dockerd, containerd — so every ceiling written there hit the machinery that answers `sbx exec`
/// rather than the build that overshot. `cgroup-parent` moves only the containers. What is left in
/// `/docker` is the sandbox itself, which is what [`MemoryPlan::plumbing`] and the reserve are for
/// and which nothing caps.
///
/// **It takes effect at the next dockerd start, not now.** `cgroup-parent` is not one of the
/// options dockerd re-reads on SIGHUP, and restarting dockerd here would stop every running
/// container — this sandbox has no live-restore, so a database someone is using would go down to
/// apply a memory ceiling. Written and left for the next cycle instead. Containers already running
/// stay where they are, outside the ceiling, until they are next recreated.
///
/// **Merged, never clobbered, and validated before it lands.** A `daemon.json` that does not parse
/// stops dockerd starting at all, so the failure this guards against is a fleet with no Docker: the
/// existing file is read first and kept if it holds other settings, a file that cannot be parsed is
/// reported and left exactly as it is rather than overwritten with something valid, and the new
/// content is re-read from disk before it replaces the old one.
/// Where dockerd keeps its data when it shares the boxes' disk.
///
/// Beside `.skein` in the fleet root rather than under `/var/lib`, for two reasons. It is plainly
/// skein's doing, next to the other thing skein put there; and the leading dot keeps it out of
/// `/boxes/*/`, which is how every box is enumerated — a `docker` directory there would read as a
/// box with no repo, which is a thing `resize_fleet` aborts on.
pub fn docker_data_root() -> String {
    format!("{}/.docker", fleet_root())
}

fn install_docker_config(sandbox: &str) -> Result<(), String> {
    // Empty when Docker keeps its own disk. Passed either way so the script has one shape.
    let root = if load_config().fleet_one_disk {
        docker_data_root()
    } else {
        String::new()
    };
    let script = format!(
        "sudo mkdir -p /etc/docker && sudo python3 - /etc/docker/daemon.json {} {}",
        sh_quote(CONTAINER_CGROUP),
        sh_quote(&root),
    );
    own_sandbox(sandbox)
        .write(
            &script,
            DOCKER_CONFIG_PY.as_bytes(),
            Duration::from_secs(30),
        )
        .map(|_| ())
        .map_err(|e| format!("writing /etc/docker/daemon.json in {sandbox}: {e}"))
}

/// The edit [`install_docker_config`] makes, as a program rather than a shell one-liner: it is a
/// read-modify-write of a file that stops dockerd booting when it is wrong, and that is worth being
/// able to read.
const DOCKER_CONFIG_PY: &str = r#"import json, os, sys
path, parent = sys.argv[1], sys.argv[2]
# Empty means "leave Docker on its own disk" — the argument is always passed, so the absent case is
# a value rather than a different invocation.
root = sys.argv[3] if len(sys.argv) > 3 else ""
try:
    config = json.load(open(path))
    if not isinstance(config, dict):
        raise ValueError("the top level is not an object")
except FileNotFoundError:
    config = {}          # no config at all is the normal case, not a problem
except Exception as e:
    # Deliberately not repaired. Something else wrote this, and replacing it with a valid file of
    # our own would take away settings dockerd is running on.
    sys.exit("skein: %s is not readable as JSON (%s); leaving it alone" % (path, e))
want_root = config.get("data-root") if not root else root
if config.get("cgroup-parent") == parent and config.get("data-root") == want_root:
    sys.exit(0)
config["cgroup-parent"] = parent
# One pool rather than two ceilings: dockerd's data goes on the sandbox's root filesystem, the same
# one the boxes are on, so a single number sizes the lot. Only ever *set*, never cleared — turning
# the setting off leaves dockerd reading the data it already has, because removing the key would
# point it back at an empty disk and make every image and volume vanish without deleting any of it.
if root:
    config["data-root"] = root
# Written beside the real file and re-read before it replaces it, so a half-written or unparseable
# result can never become the file dockerd starts from. `os.replace` is atomic within a filesystem.
scratch = path + ".skein-new"
with open(scratch, "w") as f:
    f.write(json.dumps(config, indent=2) + "\n")
json.load(open(scratch))
os.replace(scratch, path)
print("skein: dockerd will place containers under %s from its next start" % parent)
if root:
    print("skein: and keep its data in %s, on the same disk as the boxes" % root)
"#;

/// A memory size as MiB. Accepts what sbx accepts (`26g`, `512M`, a bare byte count).
///
/// `None` rather than a guess when it cannot be read: a mis-parsed ceiling is worse than no ceiling,
/// because it would silently cap every box at some number nobody chose.
fn parse_mib(value: &str) -> Option<u64> {
    let value = value.trim().to_lowercase();
    // `26gi` and `26g` are the same size, so drop the `i` before looking at the unit — reading it as
    // the unit is exactly the mis-parse this function exists to avoid.
    let value = value.strip_suffix('i').unwrap_or(&value);
    let unit = value.chars().last()?;
    if unit.is_ascii_digit() {
        // A bare number: sbx reads it as bytes.
        return value.parse::<u64>().ok().map(|b| b / (1024 * 1024));
    }
    let digits = value[..value.len() - unit.len_utf8()].trim();
    let n = digits.parse::<u64>().ok()?;
    match unit {
        'g' => Some(n * 1024),
        'm' => Some(n),
        'k' => Some(n / 1024),
        _ => None,
    }
}

/// Every host CPU but one, so the host stays responsive while the fleet is busy. Empty when the
/// count cannot be read, which leaves the flag off and sbx's own default in charge.
/// What this machine actually has, so a fleet can be sized against it rather than against a number
/// someone typed once.
///
/// Every field is the host's, not the sandbox's: these are the quantities `sbx create` is about to
/// take a share of, and the share is invisible from inside afterwards. Reported in MB because that
/// is what the arithmetic below wants; the UI turns them back into GB, which is how the flags are
/// spelled.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HostCapacity {
    pub cpus: u64,
    /// Total RAM. `0` when it could not be read — reported as unknown rather than guessed, because a
    /// proposal derived from a wrong total is worse than no proposal.
    pub memory_mb: u64,
    /// Free space on [`HostCapacity::disk_path`], which is where the sandbox's disk image grows.
    pub disk_free_mb: u64,
    pub disk_total_mb: u64,
    pub disk_path: String,
}

/// Total RAM in MB, or 0 when this platform will not say.
fn host_memory_mb() -> u64 {
    // Linux: the first field of MemTotal, in kB. Read rather than shelled out for, because this runs
    // on the path that draws a dialog and a subprocess per open would be felt.
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                if let Some(kb) = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    return kb / 1024;
                }
            }
        }
    }
    // macOS: bytes, and the only way to ask.
    let mut cmd = std::process::Command::new("sysctl");
    cmd.args(["-n", "hw.memsize"]);
    crate::util::output_with_timeout(&mut cmd, Duration::from_secs(5))
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map(|bytes| bytes / 1024 / 1024)
        .unwrap_or(0)
}

/// Free and total MB on the filesystem holding `path`, via `df`. `(0, 0)` when it cannot be read.
///
/// `df -Pk` rather than a `statvfs` binding: POSIX-portable output, no new dependency, and this is
/// asked once per dialog rather than per tick.
fn disk_space_mb(path: &str) -> (u64, u64) {
    let mut cmd = std::process::Command::new("df");
    cmd.args(["-Pk", path]);
    let Some(out) = crate::util::output_with_timeout(&mut cmd, Duration::from_secs(10))
        .filter(|o| o.status.success())
    else {
        return (0, 0);
    };
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// `(free_mb, total_mb)` from `df -Pk` output.
///
/// Counted from the END of the row, not the start: the columns are Filesystem, 1024-blocks, Used,
/// Available, Capacity, Mounted-on, and a device name longer than the column wraps onto its own
/// line under some `df`s while the mount point can contain spaces. The five numeric columns are
/// always the last six fields minus the mount point, so the tail is the stable end to count from.
fn parse_df(text: &str) -> (u64, u64) {
    let Some(row) = text.lines().nth(1).filter(|l| !l.trim().is_empty()) else {
        return (0, 0);
    };
    let fields: Vec<&str> = row.split_whitespace().collect();
    if fields.len() < 5 {
        return (0, 0);
    }
    let at = |back: usize| -> u64 {
        fields
            .get(fields.len().wrapping_sub(back))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    };
    // …1024-blocks, Used, Available, Capacity, Mounted-on
    (at(3), at(5))
}

/// This machine, measured.
pub fn host_capacity() -> HostCapacity {
    // Where the sandbox's disk actually grows. Docker's own data root would be exact, and asking for
    // it costs a `docker info` on a daemon that may be the very thing that is unwell — so this
    // reports the filesystem it is *on*, and names the path so the number can be checked.
    let disk_path = "/".to_string();
    let (disk_free_mb, disk_total_mb) = disk_space_mb(&disk_path);
    HostCapacity {
        cpus: std::thread::available_parallelism()
            .map(|n| n.get() as u64)
            .unwrap_or(0),
        memory_mb: host_memory_mb(),
        disk_free_mb,
        disk_total_mb,
        disk_path,
    }
}

/// A size for the fleet, proposed from what the host has. Every field is a string in sbx's own
/// spelling, so the dialog shows exactly what will be passed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FleetSize {
    pub memory: String,
    pub cpus: String,
    pub disk: String,
    pub box_disk_max: String,
}

/// What skein would ask for, given this host — the numbers the confirmation dialog starts on.
///
/// Deliberately a *proposal* and not a default that silently applies. A fleet sandbox is the largest
/// thing skein creates on someone's machine, and until now it was created by a side effect of
/// launching a first box, at whatever `fleet_memory` happened to say — 26g, a number chosen for a
/// different machine. On a 16 GB laptop that is most of the RAM, decided by nobody.
///
/// The shares: memory is 70% of the host, which leaves the host itself working while the fleet is
/// busy; CPUs are all but one, so a saturated fleet still leaves a core to type in; disk is half the
/// free space capped at 60 GB, because the image is sparse and grows into what it is given.
pub fn proposed_fleet_size(host: &HostCapacity) -> FleetSize {
    let gb = |mb: u64| format!("{}g", (mb / 1024).max(1));
    // `configured_field`, not the loaded `Config`: `fleet_memory` reads back this build's 26g on a
    // machine nobody has configured, so deferring to it would defer to a number chosen elsewhere.
    let memory =
        crate::config::configured_field("fleet_memory").unwrap_or_else(|| match host.memory_mb {
            0 => default_fleet_memory_hint(),
            total => gb((total * 7 / 10).max(4096)),
        });
    // From the capacity passed in, not a fresh probe: this function's whole contract is "given this
    // machine", and a proposal that measured a different one would be untestable and, on a host
    // whose CPU count skein was told rather than read, wrong.
    let cpus = crate::config::configured_field("fleet_cpus")
        .unwrap_or_else(|| host.cpus.saturating_sub(1).max(1).to_string());
    let disk =
        crate::config::configured_field("fleet_disk").unwrap_or_else(|| match host.disk_free_mb {
            0 => "20g".to_string(),
            free => gb((free / 2).clamp(20 * 1024, 60 * 1024)),
        });
    FleetSize {
        memory,
        cpus,
        disk,
        box_disk_max: load_config().box_disk_max,
    }
}

/// The memory to propose when the host will not say how much it has. Named rather than inlined so
/// the one place a guess survives is obvious.
fn default_fleet_memory_hint() -> String {
    "8g".to_string()
}

fn host_cpus_less_one() -> String {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1).to_string())
        .unwrap_or_default()
}

/// The host directory the fleet sandbox mounts: the parent of every repo's store.
///
/// sbx mounts a workspace at its **host absolute path** (verified in a real sandbox — the host's
/// `/Users/…/.skein/repos` is that same path inside the guest). That is worth more than it looks:
/// `repo.store` is a host path, and it resolves unchanged inside the fleet sandbox, so nothing in
/// skein has to translate one. Mounting the parent rather than each store is what lets a repo be
/// added later without recreating the sandbox.
pub fn fleet_workspace() -> String {
    skein_home().join("repos").to_string_lossy().into_owned()
}

/// Is the fleet sandbox already there?
///
/// `None` when sbx could not be asked at all — which is not the same as "absent", and must not be,
/// or a wedged daemon would have skein try to create a sandbox that already exists.
pub fn fleet_exists(sandbox: &str) -> Option<bool> {
    Some(fleet_boxes()?.iter().any(|b| b.name == sandbox))
}

/// Create the fleet sandbox if it is missing, then install the launcher into it.
///
/// Idempotent: safe to call before every launch, which is how a sandbox the user removed by hand
/// comes back rather than leaving every box unstartable.
pub fn ensure_fleet(sandbox: &str, mounts: &[String]) -> Result<(), String> {
    if !valid_name(sandbox) {
        return Err("invalid fleet sandbox name".into());
    }
    match fleet_exists(sandbox) {
        Some(true) => {}
        Some(false) => {
            let argv = create_argv(sandbox, mounts);
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            // `sbx create` confirms before it mounts host directories, and creating the fleet
            // sandbox mounts several. When a terminal is there, hand it over — the question is for
            // the person running the command. When there isn't (the server), capture it, but on a
            // budget that fits booting a microVM rather than the 30s action timeout.
            let env = create_env();
            let failure = if std::io::stdin().is_terminal() {
                match run_attached_env("sbx", &args, &env)? {
                    0 => None,
                    code => Some(format!("sbx exited {code}")),
                }
            } else {
                let (out, err, code) =
                    run_capture_for_env("sbx", &args, Duration::from_secs(900), &env)?;
                match code {
                    0 => None,
                    _ => Some({
                        let detail = if err.trim().is_empty() { out } else { err };
                        detail.trim().to_string()
                    }),
                }
            };
            if let Some(detail) = failure {
                // The hand-run line must carry the environment too, or a fleet configured for a
                // bigger disk is quietly recreated at the default 20 GB by the very command the
                // error told someone to type.
                let prefix = env
                    .iter()
                    .map(|(k, v)| format!("{k}={} ", sh_quote(v)))
                    .collect::<String>();
                return Err(format!(
                    "creating fleet sandbox {sandbox}: {detail}\n\
                     if that was a confirmation you never saw, create it once by hand:\n  {prefix}sbx {}",
                    args.join(" ")
                ));
            }
        }
        None => {
            return Err(format!(
                "cannot tell whether the fleet sandbox exists: {}",
                crate::sbx::fleet_failure().unwrap_or_else(|| "sbx did not answer".into())
            ))
        }
    }
    ensure_substrate(sandbox)?;
    ensure_fleet_root(sandbox)?;
    // After the substrate (which may have just installed the runtimes) and before any box starts,
    // so a rebuilt sandbox has its login back before the first box seeds from it.
    sync_fleet_login(sandbox);
    ensure_known_hosts(sandbox);
    // Here as well as in `heal_fleet`, because a sandbox this call has just *created* would
    // otherwise run its whole first life with containers outside the ceiling: the server that made
    // it is already running, so the next restart is the earliest healing would reach it. Reported
    // rather than fatal for the same reason as there — it costs the merged pool its enforcement,
    // not the fleet its boxes.
    if let Err(e) = install_docker_config(sandbox) {
        eprintln!(
            "skein: could not point dockerd at the workload cgroup ({e}); containers in {sandbox} \
             stay outside the ceiling"
        );
    }
    heal_fleet_agent(sandbox);
    install_launcher(sandbox)
}

/// Bring the in-sandbox agent into line with this binary and this config, if it is wanted.
///
/// Shared by [`ensure_fleet`] and [`heal_fleet`] so the two cannot drift, and called from *both*
/// because they answer different moments: `ensure_fleet` runs when a box starts, `heal_fleet` when
/// the server does. Wiring it only into the first is a bug this had — turning the setting on did
/// nothing at all until someone happened to start a box, and said nothing about why.
///
/// Wanted by default, and switching it off **takes the agent away** rather than ignoring it. That
/// half used to be missing, and it mattered little while the setting was off by default: an agent
/// only existed if someone had asked for one. Now that every new fleet gets one, `false` is how a
/// fleet declines it — and a decline that leaves the process running, merely routing around it,
/// would be the opposite of what was asked for.
///
/// Reported rather than fatal, because without it every call takes `sbx exec` — which is what it did
/// before the agent existed, so the fleet still works and only its resilience is reduced.
fn heal_fleet_agent(sandbox: &str) {
    if !load_config().fleet_agent {
        remove_fleet_agent(sandbox);
        return;
    }
    match ensure_fleet_agent(sandbox) {
        Ok(_) => {}
        Err(e) => eprintln!(
            "skein: the in-sandbox agent is not serving ({e}); every call falls back to \
             `sbx exec`, which is what it did before the agent existed"
        ),
    }
}

/// Bring the transport up whenever it *can* be brought up, instead of only at server start.
///
/// [`heal_fleet`] runs once, at a moment chosen by when the server happened to start, and every
/// repair it performs sits behind one `sbx ls` with a five-second budget. A daemon that is cold at
/// boot — the ordinary case, since the server usually starts with everything else — misses that
/// window, and then nothing tries again until someone starts a box. A fleet with the setting on can
/// therefore sit on `sbx exec` for days, which is exactly the report that prompted this: "sbx did
/// not answer, so skein-fleet was not brought into line with this build", from a machine where
/// `sbx ls` in a terminal worked perfectly.
///
/// It covers the other two ways a fleet ends up below the transport it asked for, because they are
/// the same question asked later: an agent still speaking v1 after skein was upgraded, and a port
/// that stopped answering after a resize (sbx keeps reporting the dead mapping). In all three the
/// answer is `ensure_fleet_agent`, and the only thing missing was another chance to run it.
///
/// Returns a line worth printing **when the state changed**, `None` when there is nothing new to
/// say. A watcher that spoke every tick would be as unreadable as one that never spoke.
pub fn heal_transport() -> Option<String> {
    if !load_config().fleet_agent {
        return None;
    }
    let state = transport_state();
    if state.speaks >= state.wants {
        transport_attempt_worked();
        return announce(
            &format!("up:{}:{}", state.speaks, state.port),
            format!(
                "the in-sandbox agent is serving on port {} (v{})",
                state.port, state.speaks
            ),
        );
    }
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return None;
    }
    // Only into a sandbox that is up. Creating or waking one is a box start's business — a watcher
    // that booted a fleet nobody had asked for would be a background task with an opinion.
    let up = crate::sbx::fleet_boxes()?
        .iter()
        .any(|b| b.name == sandbox && b.live == Some(crate::sbx::Liveness::Running));
    if !up {
        return None;
    }
    // Backed off, and this is not tidiness — it is the difference between a watcher and a leak.
    //
    // `ensure_fleet_agent` publishes a port when the current one does not answer, and **sbx has no
    // unpublish**: every attempt that fails leaves a mapping behind for the life of the sandbox. A
    // fleet where the agent cannot come up at all — no python3, a wedged daemon, an image without
    // the substrate — therefore accumulated two dead port mappings a minute, permanently, along with
    // four `sbx exec`s to install and start something that was never going to start. That is a fleet
    // being made worse by the thing watching it.
    //
    // Doubling from a minute to an hour keeps the fast recovery that this exists for — a daemon that
    // was merely cold is picked up on the first or second tick — while a fleet that cannot host an
    // agent is asked twice an hour instead of sixty times.
    if !transport_attempt_due() {
        return None;
    }
    match ensure_fleet_agent(&sandbox) {
        Ok(_) => {
            transport_attempt_worked();
            let now = transport_state();
            announce(
                &format!("up:{}:{}", now.speaks, now.port),
                format!(
                    "the in-sandbox agent is serving on port {} (v{})",
                    now.port, now.speaks
                ),
            )
        }
        Err(e) => announce(
            "down",
            format!(
                "the in-sandbox agent is not serving ({e}); every call falls back to `sbx exec`"
            ),
        ),
    }
}

/// Whether enough quiet has passed to try bringing the agent up again.
///
/// The wait doubles per consecutive failure — one minute, two, four — capped by
/// [`TRANSPORT_MAX_WAIT`] so a fleet whose daemon recovers in the afternoon is still picked up.
/// [`transport_attempt_worked`] clears it, so the next problem starts from a minute again rather
/// than from the last one's backoff.
fn transport_attempt_due() -> bool {
    let Ok(mut next) = TRANSPORT_NEXT.lock() else {
        return false;
    };
    if next.is_some_and(|at| std::time::Instant::now() < at) {
        return false;
    }
    let fails = TRANSPORT_FAILS.load(std::sync::atomic::Ordering::Relaxed);
    let wait = Duration::from_secs(60u64 << fails.min(5)).min(TRANSPORT_MAX_WAIT);
    *next = Some(std::time::Instant::now() + wait);
    TRANSPORT_FAILS.store(
        fails.saturating_add(1),
        std::sync::atomic::Ordering::Relaxed,
    );
    true
}

/// The agent is serving: forget the backoff.
fn transport_attempt_worked() {
    TRANSPORT_FAILS.store(0, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut next) = TRANSPORT_NEXT.lock() {
        *next = None;
    }
}

/// However bad it gets, try at least this often.
const TRANSPORT_MAX_WAIT: Duration = Duration::from_secs(30 * 60);
static TRANSPORT_NEXT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
static TRANSPORT_FAILS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Say it once. The same news on the next tick is not news.
///
/// Keyed on the *state* rather than on the sentence, because the sentence carries details that
/// change while the state does not: a failed publish names the ports it tried, and skein picks
/// fresh ones each attempt, so a fleet stuck on `sbx exec` would announce itself every minute with
/// different numbers. The current detail is never lost — the health banner and `skein doctor` read
/// it live. This is only about which lines are worth a log.
fn announce(key: &str, message: String) -> Option<String> {
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let mut last = LAST.lock().ok()?;
    if *last == key {
        return None;
    }
    *last = key.to_string();
    Some(message)
}

/// Install the tools a box needs in order to exist at all.
///
/// Measured in a real sandbox: the `shell` image ships `bwrap` and `git` but **not `tmux`**, and a
/// box without tmux cannot start — `box-session.sh` refuses, because the session *is* the box.
///
/// skein's kit installs jq and tmux for ordinary boxes, but it cannot serve this one: its startup
/// hook returns early in non-clone mode ("already has an in-repo .claude"), and a fleet sandbox is
/// neither a clone nor a mounted repo. So it provisions its own substrate rather than bending a hook
/// written for a different shape. jq comes along because the store probes that run inside boxes need it.
///
/// bwrap is checked but never installed: without it there is no isolation to be had, and quietly
/// continuing would give every box the sandbox's own `/tmp` and `$HOME` — the exact collision this
/// design exists to prevent.
/// The provisioning script itself, at module scope so it can be asserted on without a sandbox.
///
/// apt's output is kept, not discarded: when this step fails it is the only thing that says whether
/// the mirror was unreachable, sudo refused, or the package simply isn't there — and "missing
/// required tools: tmux" with the reason thrown away is a dead end.
const SUBSTRATE_SCRIPT: &str = r#"need='';
         command -v tmux >/dev/null 2>&1 || need="$need tmux";
         command -v jq   >/dev/null 2>&1 || need="$need jq";
         # What this fleet's owner has approved, replayed. A sandbox is rebuilt from an image that
         # knows nothing about it, so without this a rebuild silently comes back missing packages
         # someone already said yes to — and every box starts asking for them again.
         #
         # Kept apart from `need` deliberately: `need` is command-checked at the end, and an
         # approved package need not be a command at all. `libnss3` installs perfectly and would
         # still read as missing, failing a launch over a package that is actually there.
         extra='';
         for p in $SKEIN_APPROVED_APT; do
           dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'ok installed' || extra="$extra $p";
         done;
         # The agent runtimes are substrate too. The `shell` image has neither, and an agent image
         # would only ever carry one of them — so they are installed once, into the sandbox, and
         # every box in it shares them. Measured on a real sandbox: 6s and 3s. What stays per box is
         # the state, which box-session.sh keeps private; only the binaries are shared.
         npm="$SKEIN_RUNTIME_PACKAGES";
         command -v bwrap >/dev/null 2>&1 || { echo 'skein: this sandbox image has no bwrap; boxes cannot be isolated in it' >&2; exit 1; };
         log=/tmp/skein-substrate.log;
         want=""; for p in $npm; do
           case "$p" in
             *claude-code) command -v claude >/dev/null 2>&1 || want="$want $p" ;;
             *codex)       command -v codex  >/dev/null 2>&1 || want="$want $p" ;;
             *)            want="$want $p" ;;
           esac;
         done;
         # Approved npm packages, asked of npm itself rather than of $PATH: a global package need
         # not put a command on it, so `command -v` would reinstall it on every single launch.
         for p in $SKEIN_APPROVED_NPM; do
           npm ls -g --depth=0 "$p" >/dev/null 2>&1 || want="$want $p";
         done;
         npm="$want";
         apt_want="$need$extra";
         [ -n "$apt_want" ] || [ -n "$npm" ] || exit 0;
         [ -n "$apt_want" ] || { timeout 300 sudo npm install -g $npm >>"$log" 2>&1 || true; exit 0; };
         # A freshly created sandbox is still running its own first-boot apt, and apt refuses to run
         # twice. Outlast it rather than failing the launch on a race: measured on a real rebuild,
         # where the retry landed on "Could not get lock ... held by process 281 (apt-get)".
         waited=0;
         while [ "$waited" -lt 120 ]; do
           if sudo fuser /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock >/dev/null 2>&1; then
             sleep 3; waited=$((waited + 3));
           else break; fi;
         done;
         # update FIRST. A fresh image ships an empty index, where install reports "Package 'tmux'
         # has no installation candidate" — which reads as a missing package and is a missing index.
         { timeout 180 sudo apt-get update -qq; \
           timeout 240 sudo apt-get install -y -qq $apt_want \
             || { sleep 5; timeout 180 sudo apt-get update -qq; \
                  timeout 240 sudo apt-get install -y -qq $apt_want; }; } >"$log" 2>&1;
         if [ -n "$npm" ] && command -v npm >/dev/null 2>&1; then
           timeout 300 sudo npm install -g $npm >>"$log" 2>&1 || true;
         fi;
         missing=''; for t in $need; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done;
         [ -z "$missing" ] || {
             echo "skein: the fleet sandbox is missing required tools:$missing";
             echo "skein: apt said (tail of $log inside the sandbox):";
             tail -n 25 "$log" | sed 's/^/  | /';
             exit 1;
         } >&2"#;

pub fn ensure_substrate(sandbox: &str) -> Result<(), String> {
    let script = SUBSTRATE_SCRIPT;
    // The packages are named by the caller, not by the script, so a harness can ask for none.
    // Without that seam the integration test — whose `sbx exec` runs on the developer's own machine
    // — npm-installs an agent runtime onto it, which is both a 50s test and software nobody asked
    // for. $SKEIN_RUNTIME_PACKAGES set to empty means "install no runtimes".
    let packages = std::env::var("SKEIN_RUNTIME_PACKAGES")
        .unwrap_or_else(|_| "@anthropic-ai/claude-code @openai/codex".to_string());
    // Read on the host, because the record of what was approved lives there — see
    // `substrate::manifest_path` for why keeping it in the sandbox would defeat the whole point.
    let (apt, npm) = crate::substrate::approved_packages();
    let script = format!(
        "SKEIN_RUNTIME_PACKAGES={}; SKEIN_APPROVED_APT={}; SKEIN_APPROVED_NPM={}; {script}",
        sh_quote(packages.trim()),
        sh_quote(&apt.join(" ")),
        sh_quote(&npm.join(" ")),
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(900))
        .map(|_| ())
}

/// Claude's transcript directory for a working directory: every `/` and `.` becomes `-`.
///
/// Not a guess — read off a real box: `/Users/you/.skein/repos/sync/work` is stored as
/// `-Users-you--skein-repos-sync-work` (the `/.` giving the doubled dash).
pub fn transcript_slug(dir: &str) -> String {
    dir.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Point a migrated box's conversation at the directory it now works in.
///
/// The runtimes key a transcript by the cwd it was had in, and migrating a box MOVES that cwd: from
/// the old sandbox's checkout to `/boxes/<name>/tree`. So the conversation travels in the snapshot,
/// lands intact — and the agent looks under a slug for its new path, finds nothing, and starts over.
/// Measured on the first real migration: 25MB of transcript under
/// `-Users-you--skein-repos-sync-work`, and an empty `-boxes-example-box-9-tree` beside it.
///
/// Copy rather than move, and only into an empty destination: the old directory is the record of
/// where that conversation actually happened, and a box that already has a conversation of its own
/// must never have someone else's merged into it.
pub fn realign_transcript(name: &str) -> Result<usize, String> {
    let root = std::path::PathBuf::from(box_state(name)).join("claude-projects");
    let target = root.join(transcript_slug(&format!("{}/tree", box_root(name))));
    let jsonl = |dir: &std::path::Path| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect()
    };
    // BYTES, not files. A long conversation is a couple of enormous files, while the junk beside it
    // — a probe, a one-off `claude` run in /tmp, a scratchpad — is many small ones. Counting files
    // ranked a 6-file scratchpad above the 87MB conversation it was supposed to rescue, which is
    // exactly the transcript this function exists for.
    let size = |dir: &std::path::Path| -> u64 {
        jsonl(dir)
            .iter()
            .filter_map(|p| p.metadata().ok())
            .map(|m| m.len())
            .sum()
    };
    let richest = |exclude: &std::path::Path| -> Option<(std::path::PathBuf, u64)> {
        std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p != exclude)
            .map(|p| {
                let bytes = size(&p);
                (p, bytes)
            })
            .filter(|(_, bytes)| *bytes > 0)
            .max_by_key(|(_, bytes)| *bytes)
    };

    let here = size(&target);
    if target.is_dir() && here > 0 {
        // Never merge one conversation into another. But say so when the box is plainly sitting on
        // the wrong one: a migration that failed part-way leaves a few small sessions here and the
        // real history beside it, and silence at this point reads as "there was nothing to carry".
        if let Some((source, bytes)) = richest(&target) {
            if bytes > here.saturating_mul(4) {
                eprintln!(
                    "skein: {name} has a {}MB conversation of its own, but {} holds {}MB — if this \
                     box was migrated, that is the older one and copying its *.jsonl across (cp -p) \
                     restores it",
                    here / 1_000_000,
                    source.display(),
                    bytes / 1_000_000,
                );
            }
        }
        return Ok(0);
    }

    let Some((source, _)) = richest(&target) else {
        return Ok(0);
    };
    std::fs::create_dir_all(&target).map_err(|e| format!("mkdir {}: {e}", target.display()))?;
    let mut moved = 0;
    for from in jsonl(&source) {
        let Some(file) = from.file_name() else {
            continue;
        };
        let to = target.join(file);
        if std::fs::copy(&from, &to).is_err() {
            continue;
        }
        moved += 1;
        // Carry the modification time too: `claude --continue` opens the most recently modified
        // transcript, and a copy stamped `now` makes whichever file landed last look like the
        // newest conversation. Best-effort — a wrong mtime is worse than no copy only in ordering.
        if let Ok(modified) = from.metadata().and_then(|m| m.modified()) {
            if let Ok(handle) = std::fs::File::options().write(true).open(&to) {
                let _ = handle.set_times(std::fs::FileTimes::new().set_modified(modified));
            }
        }
    }
    Ok(moved)
}

/// Trust the SSH hosts a box will clone from, once per sandbox.
///
/// A fleet box clones from the remote itself, and an SSH remote needs the host's key in
/// `known_hosts` first. A legacy box got that from its kit; the fleet sandbox never had it, so the
/// first migration of an SSH-remote repo failed with the least helpful pair of errors git produces:
///
///   ssh_askpass: exec(/usr/bin/ssh-askpass): No such file or directory
///   Host key verification failed.
///
/// which reads as a credentials problem and is a host-trust one — with no known host and no
/// terminal, SSH fell back to asking a human who was not there.
///
/// A real connection with `accept-new`, not `ssh-keyscan`: keyscan is answered with "Connection
/// closed by remote host" here while an ordinary `ssh -T` succeeds and records the key itself.
/// (I read that one keyscan failure as "port 22 is closed" and was wrong — the transport is fine.)
/// `accept-new` trusts an unknown host once and still refuses a CHANGED key, which is the property
/// worth keeping. Best-effort: an HTTPS repo needs none of this, and refusing to launch over it
/// would be absurd.
///
/// Every SSH host any box might reach, from both places a repo names one.
///
/// `source` is where a box CLONES from; a repo adopted in place has a path there and an SSH URL on
/// its `origin`, which is where its boxes PUSH. Reading only `source` meant the four adopted repos
/// contributed no hosts at all — precisely the repos whose boxes now have an SSH origin.
fn ssh_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = load_repos()
        .iter()
        .flat_map(|repo| {
            [
                repo.source.clone(),
                remote_origin_url(&repo.work).unwrap_or_default(),
            ]
        })
        .filter(|url| is_ssh_url(url))
        .filter_map(|url| host_of(&url).map(|h| h.to_string()))
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// The shell that pins those hosts into whichever `$HOME` it runs in.
fn known_hosts_script(hosts: &[String]) -> String {
    format!(
        "mkdir -p \"$HOME/.ssh\" && chmod 700 \"$HOME/.ssh\"; \
         for h in {hosts}; do \
           ssh-keygen -F \"$h\" >/dev/null 2>&1 && continue; \
           timeout 25 ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -T \"git@$h\" \
             >/dev/null 2>&1; \
         done; exit 0",
        hosts = hosts
            .iter()
            .map(|h| sh_quote(h))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub fn ensure_known_hosts(sandbox: &str) {
    let hosts = ssh_hosts();
    if hosts.is_empty() {
        return;
    }
    if let Err(e) = own_sandbox(sandbox).exec(&known_hosts_script(&hosts), Duration::from_secs(120))
    {
        eprintln!("skein: could not pin SSH host keys in {sandbox} ({e}); a box cloning over SSH will fail host key verification");
    }
}

/// The same trust, inside the box — where the agent's own `git push` runs.
///
/// The sandbox's `known_hosts` does not reach a box: every box has a private HOME, and that is the
/// point of it. Cloning never noticed because it runs in the sandbox namespace, so the gap only
/// showed when a box first pushed to a real remote and got `Host key verification failed` — read as
/// a credentials problem, and the credentials were fine the whole time.
pub fn ensure_box_known_hosts(name: &str) {
    let hosts = ssh_hosts();
    if hosts.is_empty() {
        return;
    }
    let Some(place) = place_of(name) else { return };
    if let Err(e) = place.exec(&known_hosts_script(&hosts), Duration::from_secs(120)) {
        eprintln!("skein: could not pin SSH host keys in {name} ({e}); pushing over SSH from it will fail host key verification");
    }
}

/// Create the fleet root and hand it to the sandbox user.
///
/// [`fleet_root`] defaults to `/boxes` — at the filesystem root, where a non-root user cannot mkdir.
/// Everything after this point (the launcher, every box root, every tmux socket) is created with a
/// plain `mkdir -p` by the sandbox user, so all of it fails until this runs once. The integration
/// test never caught it precisely because `$SKEIN_FLEET_ROOT` points it at a writable temp dir —
/// the seam that makes the launch path testable is also the seam that hid its first real step.
pub fn ensure_fleet_root(sandbox: &str) -> Result<(), String> {
    let root = sh_quote(&fleet_root());
    // Escalate only when there is something to escalate for. `/boxes` sits at the filesystem root
    // where an unprivileged mkdir cannot reach, so in production this still falls through to sudo
    // exactly as before — but a fleet root anywhere writable is now made without it.
    //
    // That is not a tidiness argument, it is a testability one. `$SKEIN_FLEET_ROOT` points the
    // integration test at a temp dir specifically so the launch path can be exercised without a
    // sandbox, and reaching for sudo regardless made the whole test unrunnable anywhere sudo is not
    // available — which now includes every box, since a box is a user namespace and sudo cannot work
    // in one. The test that guards `box-session.sh` was therefore red exactly where that file is
    // edited. `mkdir -p` on an existing directory succeeds, so the second `-w` is what keeps an
    // unwritable-but-present root falling through rather than being called done.
    let script = format!(
        "[ -w {root} ] && exit 0; \
         mkdir -p {root} 2>/dev/null && [ -w {root} ] && exit 0; \
         sudo mkdir -p {root} && sudo chown \"$(id -u):$(id -g)\" {root} && chmod 755 {root}"
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(60))
        .map(|_| ())
        .map_err(|e| format!("preparing the fleet root {root}: {e}"))
}

/// Write `box-session.sh` and the provisioning script into the sandbox, over stdin rather than as
/// arguments — both are large and `sbx exec`'s argv is visible in every process listing on the host.
pub fn install_launcher(sandbox: &str) -> Result<(), String> {
    for (path, body) in [
        (box_session_path(), BOX_SESSION_SH),
        (box_provision_path(), KIT_STARTUP_SH),
        (git_credential_helper_path(), GIT_CREDENTIAL_SH),
    ] {
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/boxes");
        let script = format!(
            "mkdir -p {} && cat > {} && chmod 755 {}",
            sh_quote(dir),
            sh_quote(&path),
            sh_quote(&path)
        );
        // The fleet sandbox itself, not a box inside it — no namespace to enter.
        own_sandbox(sandbox).write(&script, body.as_bytes(), Duration::from_secs(30))?;
    }
    Ok(())
}

/// The shell that prepares a box's checkout inside the fleet sandbox.
///
/// Clones from the **remote** at `base`, then creates or checks out the box's branch. Boxes used to
/// be cloned from the host's own clone, which meant a new box inherited whatever was stale or
/// half-committed there; from the remote it starts from the same base the diff is taken against.
///
/// Refuses rather than reuses when the tree is already populated: a box root left behind by a
/// previous box of the same name would otherwise silently give the new one someone else's work.
///
/// `upstream` repairs the one case where cloning and pushing want different sources. A repo adopted
/// in place has no URL, so the clone comes from the host's own checkout ([`clone_source`]) — and
/// `git clone <path>` sets `origin` to that path, discarding the URL the host clone pushes to. The
/// box then depends on the host for something it should never need it for: skein *validates* the
/// host clone's origin ([`crate::repos::remote_warning`] warns when there isn't one, precisely because "a
/// box can't push or open a PR until one exists") and then provisions a box pointing somewhere else
/// entirely. Pushing worked anyway until a box shared the host's checked-out branch, at which point
/// git refused with a message about `receive.denyCurrentBranch` — a remote-side policy error for
/// what is really a mis-pointed remote.
///
/// So: clone locally, which is fast and carries the host's unpushed commits, then point `origin` at
/// the URL and keep the path as `local`. Nothing is lost and the box pushes where every other box
/// does. Empty when the source is already a URL, or when the host clone has no origin to inherit.
pub fn clone_script(name: &str, url: &str, base: &str, branch: &str, upstream: &str) -> String {
    let root = box_root(name);
    let tree = format!("{root}/tree");
    let tree_q = sh_quote(&tree);
    let url_q = sh_quote(url);
    // An empty base is not a missing value to substitute a guess for — it is skein saying it could
    // not learn the remote's default, and a plain `git clone` asks the remote for it directly.
    //
    // When there IS a base, the clone falls back to the same plain form rather than failing. A base
    // that the remote does not have has always been possible — a configured base a given repo does
    // not use, a cached `origin/HEAD` gone stale after a rename — and the cost was severe out of all
    // proportion: a migration that stops the old sandbox, then cannot start the new box, leaves the
    // box in neither place until someone woke the old sandbox by hand. A tree cloned from the wrong base
    // is a non-event by comparison, since the branch is checked out over it immediately.
    let clone = if base.is_empty() {
        format!("git clone {url_q} {tree_q}")
    } else {
        format!(
            "git clone --branch {base_q} {url_q} {tree_q} || \
             {{ echo 'skein: no {base} on the remote; cloning its default branch instead' >&2; \
                rm -rf {tree_q}; git clone {url_q} {tree_q}; }}",
            base_q = sh_quote(base),
        )
    };
    // Best-effort, and deliberately not under `set -e`: a box whose remote could not be re-pointed
    // is a box that pushes to the host clone, which is where it would have pushed anyway. Failing
    // the whole clone over it would turn a working box into no box.
    let remotes = if upstream.is_empty() {
        String::new()
    } else {
        format!(
            "; git remote add local {url_q} 2>/dev/null || true; \
             git remote set-url origin {up_q} || \
             echo 'skein: could not point origin at {upstream}; this box pushes to the host clone' >&2",
            up_q = sh_quote(upstream),
        )
    };
    format!(
        "set -e; \
         if [ -e {tree_q}/.git ]; then echo 'skein: {name} already has a checkout; destroy the box first' >&2; exit 1; fi; \
         mkdir -p {root_q}; \
         {clone}; \
         cd {tree_q}; \
         git checkout -B {branch_q}{remotes}",
        root_q = sh_quote(&root),
        branch_q = sh_quote(branch),
    )
}

/// How much disk each box is using, in MiB — every box at once, in one round trip.
///
/// Measured rather than enforced, and that distinction is the honest part of this: the fleet's disk
/// is a single filesystem shared by every box, so nothing in the kernel stops one from filling it.
/// A limit here is a number skein checks and reports, not a wall the box hits — which is why it can
/// be changed while a box runs, and why it is worth having at all: the alternative is finding out
/// when some *other* box's build dies with ENOSPC and no indication of who took the space.
///
/// `du -sxm`, one process for the lot: measured at 0.2s for a 3.2 GB tree, so a board tick can pay
/// for it. `-x` keeps it on the sandbox's own filesystem — a box's store is a host mount, and
/// walking virtiofs to count bytes that are not on this disk would be both slow and wrong.
pub fn fleet_disk_usage() -> std::collections::HashMap<String, u64> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Default::default();
    }
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_secs(30)
    };
    DISK_GATE
        .get(fresh, move || {
            let out = own_sandbox(&sandbox)
                .exec(&disk_usage_script(&fleet_root()), Duration::from_secs(60))
                .ok()?;
            Some(parse_disk_usage(&out))
        })
        .unwrap_or_default()
}

/// `du` over every box, and the `|| true` is the entire point of this being its own function.
///
/// `du` exits nonzero if it could not read so much as one directory anywhere in the tree, while
/// still printing correct totals for everything it *could* read. `exec` treats a nonzero exit as a
/// failed call, so without this a single unreadable directory — in any box, at any depth — threw
/// away the disk figures for the whole fleet and every box reported nothing.
///
/// That is not hypothetical and not rare: boxes create unreadable directories in the course of
/// ordinary work (a test asserting behaviour on an unreadable store leaves one behind), and one is
/// enough. The failure was invisible for a long time because the row chip stays silent below 80% of
/// a box's allowance, so "no disk figure" and "nothing worth saying" looked identical.
///
/// Partial output is the right answer here. A total is worth having even when one subtree could not
/// be walked, and a `du` that printed nothing at all still parses to an empty map.
fn disk_usage_script(root: &str) -> String {
    format!("du -sxm {root}/*/ 2>/dev/null || true")
}

/// Turn `du -sxm` output into MiB per box. Separate so it can be tested against the real thing.
fn parse_disk_usage(out: &str) -> std::collections::HashMap<String, u64> {
    out.lines()
        .filter_map(|line| {
            let (mb, path) = line.trim().split_once(char::is_whitespace)?;
            let name = path.trim().trim_end_matches('/').rsplit('/').next()?;
            Some((name.to_string(), mb.trim().parse().ok()?))
        })
        .collect()
}

/// See [`crate::util::Gate`]: remembered, asked by one caller at a time, and asked less often while the
/// sandbox is failing to answer — a `du` over every box is the most expensive question skein asks
/// on a tick, and the last thing a struggling sandbox should be handed more of.
static DISK_GATE: crate::util::Gate<std::collections::HashMap<String, u64>> =
    crate::util::Gate::new();

/// What the fleet's one VM is actually using right now — the gauge behind [`fleet_resources`].
///
/// Sized in MiB throughout, because that is what every other number in this module speaks and the
/// browser should not have to know which unit each field arrived in.
///
/// `boxes` and `docker` are the two cgroups [`fleet_limits`] writes ceilings on. They are here
/// rather than a single VM total because a single total cannot answer the question you ask when the
/// sandbox is struggling: *what* is holding it. 12 GB in the boxes is the agents working; 12 GB in
/// `docker` is a container someone forgot, in a cgroup no per-box limit reaches.
///
/// Memory *used* is `MemTotal - MemAvailable`, not `MemTotal - MemFree`. Free is nearly always small
/// and nearly always meaningless — the kernel spends idle memory on page cache and hands it back on
/// demand — so a gauge drawn from it reads as a permanently full machine.
///
/// The two cgroup figures are `anon` from `memory.stat`, **not** `memory.current`, and the two are
/// not interchangeable: `current` counts page cache, which `MemAvailable` has already treated as
/// free. Measured on this fleet, `current` reported 11.0 GB for the boxes and 10.8 GB for docker
/// against a whole-VM `mem_used` of 2.9 GB — two parts of a bar, each four times the bar. `anon`
/// gave 1.2 GB and 0.8 GB, which sum inside the total and leave the VM's own services visible as
/// the difference. It is also the memory that *matters* here, being the part the kernel cannot
/// reclaim its way out of and therefore the part that ends in an OOM kill.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FleetResources {
    pub mem_total: u64,
    pub mem_used: u64,
    pub boxes: u64,
    pub docker: u64,
    pub disk_total: u64,
    pub disk_used: u64,
    /// `/var/lib/docker` — images, volumes and build cache. **A different disk from the one above**,
    /// and that is why it is measured separately rather than folded in: sbx gives a sandbox two, a
    /// root filesystem sized by `DOCKER_SANDBOXES_ROOT_SIZE` and this one by
    /// `DOCKER_SANDBOXES_DOCKER_SIZE`, so filling one says nothing about the other.
    ///
    /// Without it the gauge answered the wrong question confidently. Measured here: the boxes' disk
    /// 37% full while this one was at 76%, so a build that ran out of space did so against a strip
    /// showing two thirds free — the disk that filled was not the disk being drawn.
    ///
    /// Zero when Docker shares the boxes' filesystem, so the same bytes are never drawn twice.
    pub images_total: u64,
    pub images_used: u64,
    pub cpus: u64,
    pub load1: f64,
    pub load5: f64,
    /// What [`memory_plan`] allows the workload — `boxes` and `docker` **together**, because they
    /// share one pool taken first-come rather than holding a slice each. One number, so the gauge
    /// cannot imply two separate allowances where there is one. Zero when no fleet total is
    /// configured to divide.
    ///
    /// It is what the *plan* allows, not what any single cgroup enforces: only `skein` carries a
    /// ceiling (see [`fleet_limits`] for why `docker` cannot), so this is the line the workload is
    /// meant to stay under, and `docker` can cross it without being stopped.
    pub workload_max: u64,
    /// True while the sandbox is failing to answer — see [`crate::util::Gate`]. The figures are then the
    /// last ones that arrived, and saying so is the difference between stale and wrong.
    pub stale: bool,
}

/// The fleet VM's memory, disk and CPU, in one round trip.
///
/// `None` when no fleet sandbox is configured — there is no VM to ask — or when one has never
/// answered. Deliberately coarse and deliberately stale-tolerant: this is a gauge you glance at, not
/// a number anything decides on, so it is worth at most one `sbx exec` every 30 seconds and worth
/// nothing at all when the sandbox is busy. The [`crate::util::Gate`] enforces both, and backs off further
/// while the sandbox is unwell — a struggling VM being asked how it feels every 2 seconds is how
/// skein used to keep it struggling.
///
/// One shell, printing `key value` lines, because the alternative is five round trips to build one
/// strip. `df` is asked about [`fleet_root`] rather than `/`: box roots are the only disk skein can
/// account for, and on a filesystem the boxes do not share the number would be answering about
/// somebody else's storage.
/// Which way skein is actually reaching the fleet, as opposed to which way it was configured to.
///
/// This exists because the difference has been invisible three times running, and each time the
/// symptom was the same: everything works, only less resiliently, so nothing draws attention to it.
/// The transport was wired into one call site and not the other; the port was published but never
/// answered; and the setting was simply off while both of us believed it on. A degraded transport
/// that says nothing is indistinguishable from a healthy one right up until the daemon stalls, which
/// is the one moment it was supposed to help.
///
/// Deliberately not folded into [`fleet_resources`]: that asks the *sandbox* and can fail, and this
/// is a host-side fact — a setting and a loopback connection — that must stay answerable when the
/// sandbox does not. The two only share a poll.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Transport {
    /// The `fleet_agent` setting. False means every call spawns `sbx exec`, as before the agent.
    pub configured: bool,
    /// The host port skein published and verified, 0 when it never got one.
    pub port: u16,
    /// What answers there now — 0 for nothing, 1 for an agent from before versions existed.
    pub speaks: u32,
    /// What this build needs. `speaks < wants` is an agent an upgrade has not yet replaced.
    pub wants: u32,
    /// Why `config.json` could not be read, when it could not be.
    ///
    /// Carried here rather than left to its own endpoint because it is the answer to the question
    /// `configured` provokes. False normally means "you did not ask for the agent"; with this set it
    /// means "skein does not know what you asked for", because one unparseable field discards the
    /// whole file and every setting reverts to a default that looks exactly like a choice. Those two
    /// read identically on the board and are opposite problems.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<String>,
}

pub fn transport_state() -> Transport {
    let wants = crate::place::AGENT_PROTOCOL;
    let settings = crate::config::config_error();
    if !load_config().fleet_agent {
        return Transport {
            wants,
            settings,
            ..Default::default()
        };
    }
    let port = recorded_agent_port().unwrap_or(0);
    Transport {
        configured: true,
        port,
        // A real connection, not the mapping sbx reports: a published port survives `sbx rm` and is
        // still listed while every connection through it is refused, so asking is the only answer
        // that means anything.
        speaks: crate::place::agent_protocol(port).unwrap_or(0),
        wants,
        settings,
    }
}

/// What one box is using right now.
///
/// The fleet gauge answers "is the sandbox in trouble", which is the wrong question when the answer
/// is yes: the next thing you want is *which box*, and nothing could tell you. One box saturating
/// every core is legitimate here — `cpu.weight` is equal and uncapped on purpose, so a lone box gets
/// the whole machine and gives it back under contention — but "legitimate" and "what you wanted"
/// are different, and you cannot judge which without seeing the name.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BoxLoad {
    pub name: String,
    /// CPUs in use, measured rather than reported: `usage_usec` twice over a known interval. A
    /// cgroup only carries a running total, so a single read says how much CPU a box has used since
    /// it started — which is a fine way to rank yesterday's builds and no way at all to find what is
    /// busy now.
    pub cores: f64,
    /// `memory.current` — everything the box is charged for, including page cache it could give
    /// back. `anon` is what the fleet gauge stacks, because that is the part an OOM turns on.
    pub mem: u64,
    pub pids: u64,
    /// MiB on the fleet's shared disk, and this box's share of it. Merged in from
    /// [`fleet_disk_usage`] rather than measured here: counting bytes means walking the tree, which
    /// is seconds per box on a big checkout and has no business inside a half-second CPU sample.
    /// That walk is already done and already gated, so this costs a map lookup.
    ///
    /// Present because "what is eating the machine" is asked about disk at least as often as about
    /// CPU — and unlike memory, disk is the one the fleet actually runs out of: this sandbox hit
    /// 100% mid-build while a single box transiently took 25 GB.
    pub disk_mb: u64,
    /// What this box is allowed, when it has an allowance. Measured, never enforced — one
    /// filesystem serves every box — so this says who took the space, not who may.
    pub disk_limit_mb: Option<u64>,
}

/// Every box's live usage, in one round trip.
///
/// Not behind the resource gate: this is asked for deliberately rather than polled, and a cached
/// answer to "what is eating the machine *now*" is worse than a slow one. The half-second inside
/// the script is the measurement interval, not latency to hide.
pub fn box_loads() -> Vec<BoxLoad> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Vec::new();
    }
    let mut loads = own_sandbox(&sandbox)
        .exec(BOX_LOAD_SCRIPT, Duration::from_secs(20))
        .map(|out| parse_box_loads(&out, BOX_LOAD_INTERVAL_US))
        .unwrap_or_default();
    // Folded in after the sample rather than during it: the disk figures come from their own gate,
    // and making the CPU measurement wait on a tree walk would widen a half-second interval into
    // however long `du` takes over every box.
    let usage = fleet_disk_usage();
    for load in &mut loads {
        load.disk_mb = usage.get(&load.name).copied().unwrap_or(0);
        load.disk_limit_mb = usage.get(&load.name).and(box_disk_limit(&load.name));
    }
    loads
}

const BOX_LOAD_INTERVAL_US: f64 = 500_000.0;

/// Two samples of every box cgroup, separated by the interval above.
///
/// `usage_usec` is cumulative, so the pair is the whole point — and both are taken in one shell so
/// the interval is the sandbox's own clock rather than a round trip that might stall between them.
const BOX_LOAD_SCRIPT: &str = "\
cd /sys/fs/cgroup/skein 2>/dev/null || exit 0; \
for d in */; do n=${d%/}; \
  printf 'a %s %s\\n' \"$n\" \"$(awk '/^usage_usec/{print $2}' \"$d/cpu.stat\" 2>/dev/null)\"; done; \
sleep 0.5; \
for d in */; do n=${d%/}; \
  printf 'b %s %s %s %s\\n' \"$n\" \
    \"$(awk '/^usage_usec/{print $2}' \"$d/cpu.stat\" 2>/dev/null)\" \
    \"$(cat \"$d/memory.current\" 2>/dev/null)\" \
    \"$(cat \"$d/pids.current\" 2>/dev/null)\"; done";

/// Turn the script's two passes into a rate per box. Its own function so the arithmetic is testable
/// without a sandbox — the parser is only correct against exactly the output above.
fn parse_box_loads(out: &str, interval_us: f64) -> Vec<BoxLoad> {
    let mut first: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let mut loads = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.first() {
            Some(&"a") if f.len() >= 3 => {
                if let Ok(v) = f[2].parse() {
                    first.insert(f[1], v);
                }
            }
            Some(&"b") if f.len() >= 5 => {
                let (name, used) = (f[1], f[2].parse::<u64>().unwrap_or(0));
                // A box that appeared between the two passes has no baseline. Reporting it at zero
                // is honest — it has been observed for no time at all — and beats inventing a rate
                // from a total that has been accumulating since it started.
                let before = first.get(name).copied().unwrap_or(used);
                loads.push(BoxLoad {
                    name: name.to_string(),
                    cores: (used.saturating_sub(before) as f64 / interval_us).max(0.0),
                    mem: f[3].parse().unwrap_or(0),
                    pids: f[4].parse().unwrap_or(0),
                    // Filled by `box_loads` from the disk gate; the parser only sees the cgroup
                    // sample, which carries no notion of bytes on disk.
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
    loads.sort_by(|a, b| b.cores.total_cmp(&a.cores).then(a.name.cmp(&b.name)));
    loads
}

pub fn fleet_resources() -> Option<FleetResources> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return None;
    }
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_secs(30)
    };
    let mut resources = RESOURCE_GATE.get(fresh, move || {
        let out = own_sandbox(&sandbox)
            .exec(&resource_script(), Duration::from_secs(20))
            .ok()?;
        parse_resources(&out)
    })?;
    // The ceilings come from the host's own config, not the guest, so they are always current even
    // when the figures beside them are the last ones that arrived.
    if let Some(plan) = memory_plan() {
        resources.workload_max = plan.boxes;
    }
    resources.stale = RESOURCE_GATE.degraded();
    Some(resources)
}

/// The one shell [`fleet_resources`] runs, printing `key value` lines.
///
/// Its own function so the wire format is readable in one place and testable without a sandbox —
/// the parser below is only correct against exactly this output.
fn resource_script() -> String {
    format!(
        "awk '/^MemTotal:/{{t=$2}} /^MemAvailable:/{{a=$2}} \
         END{{print \"mem_total\", int(t/1024); print \"mem_used\", int((t-a)/1024)}}' /proc/meminfo; \
         echo \"cpus $(nproc 2>/dev/null || echo 0)\"; \
         awk '{{print \"load1\", $1; print \"load5\", $2}}' /proc/loadavg; \
         df -Pm {root} 2>/dev/null \
         | awk 'NR==2{{print \"disk_dev\", $1; print \"disk_total\", $2; print \"disk_used\", $3}}'; \
         df -Pm {docker} 2>/dev/null \
         | awk 'NR==2{{print \"images_dev\", $1; print \"images_total\", $2; print \"images_used\", $3}}'; \
         for c in skein skein/containers docker; do \
         awk -v c=$c '/^anon /{{print c, int($2/1048576)}}' \
         /sys/fs/cgroup/$c/memory.stat 2>/dev/null; done",
        root = sh_quote(&fleet_root()),
        // Where dockerd's data actually is, not where it conventionally lives. With one pool,
        // `/var/lib/docker` is still a mounted disk — it is simply the one nothing writes to any
        // more, so measuring it would draw a gauge for an empty disk while the disk that filled up
        // went unreported. Pointed at the pool instead, `disk_dev` and `images_dev` come back as the
        // same device, which is exactly how the strip already knows to draw one row rather than two.
        docker = sh_quote(&effective_docker_root()),
    )
}

/// The directory dockerd keeps its data in, according to the setting that put it there.
fn effective_docker_root() -> String {
    if load_config().fleet_one_disk {
        docker_data_root()
    } else {
        "/var/lib/docker".to_string()
    }
}

/// `key value` lines into a [`FleetResources`], or `None` when the reply carried no memory total.
///
/// That last condition is the point of returning an `Option`: `sbx exec` can succeed while the guest
/// prints nothing usable — a sandbox mid-boot, a `/proc` not yet mounted — and without the check the
/// [`Gate`](crate::util::Gate) would remember a zeroed machine as a good answer and stop asking for 30
/// seconds. Missing individual fields are fine and stay zero; the browser hides a gauge whose
/// denominator is zero rather than drawing a bar against nothing.
fn parse_resources(out: &str) -> Option<FleetResources> {
    let mut r = FleetResources::default();
    // Where the containers are is a question with two answers during a migration, so both homes are
    // read and one is chosen below rather than summed.
    let mut nested: Option<u64> = None;
    let mut outside = 0;
    // Which device each `df` answered about, so the same filesystem is never drawn twice.
    let (mut root_dev, mut images_dev) = (String::new(), String::new());
    for line in out.lines() {
        let Some((key, value)) = line.trim().split_once(' ') else {
            continue;
        };
        let value = value.trim();
        let number = || value.parse::<u64>().unwrap_or(0);
        match key {
            "mem_total" => r.mem_total = number(),
            "mem_used" => r.mem_used = number(),
            "skein" => r.boxes = number(),
            "skein/containers" => nested = Some(number()),
            "docker" => outside = number(),
            "disk_total" => r.disk_total = number(),
            "disk_used" => r.disk_used = number(),
            "disk_dev" => root_dev = value.to_string(),
            "images_total" => r.images_total = number(),
            "images_used" => r.images_used = number(),
            "images_dev" => images_dev = value.to_string(),
            "cpus" => r.cpus = number(),
            "load1" => r.load1 = value.parse().unwrap_or(0.0),
            "load5" => r.load5 = value.parse().unwrap_or(0.0),
            _ => {}
        }
    }
    // Containers live *inside* `skein` once dockerd has been pointed at them, so `skein` counts them
    // and the two figures have to be separated rather than added — added, the strip would draw the
    // same memory twice and a bar of parts would exceed the whole it is drawn against.
    //
    // Chosen on whether the nested cgroup EXISTS, not on whether it holds anything: an empty one is
    // a fleet that has cycled and simply has no container running, and falling back then would put
    // the sandbox's own daemons under the `docker` label. `/sys/fs/cgroup/docker` is not a synonym
    // for the old home — after the move it holds only the sandbox's own container, which belongs to
    // the plumbing share and is counted in `other`.
    match nested {
        Some(containers) => {
            r.docker = containers;
            r.boxes = r.boxes.saturating_sub(containers);
        }
        None => r.docker = outside,
    }
    // sbx gives `/var/lib/docker` a disk of its own, but it does not have to: a sandbox built
    // without one has Docker on the same filesystem as the boxes, and drawing that as a second
    // gauge would show the same bytes twice under two names. Compared by device rather than by
    // path, which is the only comparison that answers "is this the same storage".
    // Both empty means neither `df` answered, which is not the two being the same device — the
    // figures are already zero there, and treating "unknown" as "matched" would be a coincidence
    // waiting to be relied on.
    if !images_dev.is_empty() && images_dev == root_dev {
        r.images_total = 0;
        r.images_used = 0;
    }
    (r.mem_total > 0).then_some(r)
}

/// See [`crate::util::Gate`]. Asked rarely and backed off hard: nothing depends on this answer, so it must
/// never be a reason the sandbox is busy.
static RESOURCE_GATE: crate::util::Gate<FleetResources> = crate::util::Gate::new();

/// This box's disk allowance in MiB: its own if it has one, else the fleet-wide default, `None` for
/// unlimited. Read at every check, so changing it takes effect on the next refresh — no restart.
pub fn box_disk_limit(name: &str) -> Option<u64> {
    let own = std::fs::read_to_string(std::path::Path::new(&box_state(name)).join("disk"))
        .ok()
        .map(|s| s.trim().to_string());
    match own {
        // An empty override is a decision — this box is allowed to use the whole disk.
        Some(v) if v.is_empty() => None,
        Some(v) => parse_mib(&v),
        None => parse_mib(&load_config().box_disk_max),
    }
}

/// Give one box a different allowance, or hand it back to the default. Takes effect immediately.
pub fn set_box_disk_limit(name: &str, limit: Option<&str>) -> Result<(), String> {
    let dir = std::path::PathBuf::from(box_state(name));
    let path = dir.join("disk");
    let Some(limit) = limit else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("clearing {}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    };
    // Three states, and only two of them are a size: `none` says this box may use the whole disk,
    // which is different from having no opinion (that is `None`, and inherits the default).
    let limit = match limit.trim().to_lowercase().as_str() {
        "none" | "unlimited" => "",
        _ => limit.trim(),
    };
    if !limit.is_empty() && parse_mib(limit).is_none() {
        return Err(format!(
            "{limit:?} is not a size — try 10g, 512m, or `none` for unlimited"
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    write_atomic(&path, &dir, limit.as_bytes())
}

/// Who a box commits as: its own choice, else the configured default, else the host clone's.
///
/// Three sources because each answers a different question. A box set at creation is working on
/// someone else's behalf — a shared machine, a different identity per client. The setting is the
/// answer for everything else. And falling back to the host clone means an untouched skein commits
/// as you without anyone configuring anything, because `git -C <work> config user.name` resolves
/// through the host's global gitconfig.
pub fn box_identity(name: &str, repo: &Repo) -> (String, String) {
    let config = load_config();
    let from_host = |key: &str| -> String {
        std::process::Command::new("git")
            .args(["-C", &repo.work, "config", "--get", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let pick = |own: Option<String>, configured: &str, key: &str| -> String {
        own.filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| match configured.trim() {
                "" => from_host(key),
                v => v.to_string(),
            })
    };
    let own = box_identity_override(name);
    (
        pick(own.clone().map(|(n, _)| n), &config.git_name, "user.name"),
        pick(own.map(|(_, e)| e), &config.git_email, "user.email"),
    )
}

/// A box's own committer, recorded at creation. `name\nemail`, beside its other durable state.
pub fn box_identity_override(name: &str) -> Option<(String, String)> {
    let raw =
        std::fs::read_to_string(std::path::Path::new(&box_state(name)).join("identity")).ok()?;
    let mut lines = raw.lines();
    Some((
        lines.next().unwrap_or_default().trim().to_string(),
        lines.next().unwrap_or_default().trim().to_string(),
    ))
}

/// Record (or clear) that committer. `None` returns the box to the configured default.
pub fn set_box_identity(name: &str, who: Option<(&str, &str)>) -> Result<(), String> {
    let dir = std::path::PathBuf::from(box_state(name));
    let path = dir.join("identity");
    let Some((who_name, email)) = who else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("clearing {}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = format!("{}\n{}\n", who_name.trim(), email.trim());
    write_atomic(&path, &dir, body.as_bytes())
}

/// Set that identity inside the box, so its first commit is not `Author identity unknown`.
///
/// `--global` (the box's own HOME), not the repo: the checkout is re-cloned by a rebuild, a resize
/// or a migration, and a repo-local setting goes with it every time. Only what is missing is
/// written, so an identity someone set in the box by hand is never overwritten.
fn identity_script(name: &str, email: &str) -> String {
    let mut steps = Vec::new();
    if !name.trim().is_empty() {
        steps.push(format!(
            "git config --global --get user.name >/dev/null 2>&1 || git config --global user.name {}",
            sh_quote(name.trim())
        ));
    }
    if !email.trim().is_empty() {
        steps.push(format!(
            "git config --global --get user.email >/dev/null 2>&1 || git config --global user.email {}",
            sh_quote(email.trim())
        ));
    }
    steps.join("; ")
}

/// Point an existing box's `origin` at the repo's remote, if it is still the host clone.
///
/// The clone-time version of this ([`clone_script`]) only helps boxes cloned after it landed. This
/// is the same repair for the ones already on disk, and it runs on every start because there is no
/// other moment that would notice.
///
/// The equality test is the whole safety argument: it rewrites only the URL skein itself put there.
/// An origin someone re-pointed by hand — at a fork, at a mirror — is left exactly alone.
pub fn origin_repair_script(name: &str, source: &str, upstream: &str) -> String {
    format!(
        "cd {tree_q} 2>/dev/null || exit 0; \
         cur=\"$(git remote get-url origin 2>/dev/null || true)\"; \
         [ \"$cur\" = {src_q} ] || exit 0; \
         git remote add local {src_q} 2>/dev/null || true; \
         git remote set-url origin {up_q} && \
         echo 'skein: {name} pushed at the host clone; origin now points at {upstream}' >&2",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        src_q = sh_quote(source),
        up_q = sh_quote(upstream),
    )
}

/// Drop what the *previous* session said about this box, as a new one starts.
///
/// Turn state is a claim about a session — "working", "waiting", "ended" — and it is written into
/// the repo's store, which lives on the host and outlives the box entirely. So a migrated box came
/// up reading `ended`: stopping the old sandbox killed its agent, the SessionEnd hook faithfully
/// recorded that, and the new box inherited a dead session's last word and looked terminated while
/// sitting there perfectly alive. The same would greet every box after a resize.
///
/// Only the claim about the current turn goes. The narrative signal, the telemetry and the hook log
/// are history — they describe what happened, not what is happening, and a box that has just moved
/// is exactly when its history is worth keeping. The in-flight sub-agent counter goes too: it counts
/// processes that died with the old session, and a stale one would make the first Notification of
/// the new session read as "still busy" instead of "needs you".
///
/// Absent files are the normal case (a box being created for the first time), so this is silent.
fn forget_turn_state(repo: &Repo, name: &str) {
    let status = std::path::Path::new(&repo.store).join("status");
    for file in [format!("{name}.json"), format!("{name}.agents")] {
        let _ = std::fs::remove_file(status.join(file));
    }
}

/// The shell that starts a box: its namespace, its tmux server, and the agent inside it.
pub fn session_script(name: &str, session: &str, agent_command: &str) -> String {
    format!(
        // The shared ceilings ride in the environment rather than as an eighth positional, because
        // the launcher already installed in a running sandbox does not know about them: a new
        // argument would be read as part of the command, and every box restart would fail until
        // something reinstalled the script. An old launcher ignores an environment variable.
        // `SKEIN_GIT_SCOPE` and `SKEIN_BOX_REPO` ride in the environment for the same reason as the
        // ceilings above, and it is not a stylistic one: a launcher already installed in a running
        // sandbox would read an eighth positional as part of the command, and every box restart
        // would fail until something reinstalled the script. An old launcher ignores an env var.
        "SKEIN_FLEET_LIMITS={fleet_q} SKEIN_GIT_SCOPE={scope_q} SKEIN_BOX_REPO={repo_q} \
         SKEIN_BOX_PRIVILEGED={priv_q} \
         SKEIN_FLEET_MOUNTS={mounts_q} SKEIN_BOX_STORE={store_q} SKEIN_BOX_MIRROR={mirror_q} \
         {launcher} {name_q} {root_q} {pid_q} {session_q} {state_q} {limits_q} bash -lc {cmd_q}",
        launcher = sh_quote(&box_session_path()),
        // The mount set the launcher cannot learn for itself, and the two paths out of it this box
        // is entitled to. The launcher covers every mount and binds these back — an inversion, not
        // a list of things to hide, because `repo.work` and an adopted `repo.store` are arbitrary
        // host paths chosen at repo-add time and no rule written over one root reaches
        // `/home/you/code/thing`.
        //
        // Empty when there is no repo for this box, and that is the safe direction: the box gets a
        // covered view with nothing bound back rather than an uncovered one.
        mounts_q = sh_quote(&mount_manifest(name)),
        store_q = sh_quote(&repo_for_box(name).map(|r| r.store).unwrap_or_default()),
        // Read-only, and it is not a precaution. `sandbox-bootstrap.sh` reads the mirror to surface
        // a repo's gitignored files and already copies every one of them into the store rather than
        // linking at it, precisely so "a box can still never reach the host checkout". Nothing in a
        // box writes here — but skein runs `git -C <repo.work>` on the HOST, so a box that could
        // write `.git/config` would get `core.fsmonitor` executed as the host user. The convention
        // was doing the work; this makes it a mount option.
        mirror_q = sh_quote(&repo_for_box(name).map(|r| r.work).unwrap_or_default()),
        // Off unless the file says otherwise, and an unreadable answer is off. The two directions
        // are not equal: guessing "privileged" hands one box every other box's credentials, and
        // guessing "not" costs the workshop box a restart after someone flips the switch.
        priv_q = sh_quote(if box_is_privileged(name) { "1" } else { "0" }),
        scope_q = sh_quote(if crate::gitgate::box_is_scoped(name) {
            "repo"
        } else {
            "fleet"
        }),
        repo_q = sh_quote(&crate::gitgate::box_repo_slug(name)),
        name_q = sh_quote(name),
        root_q = sh_quote(&box_root(name)),
        pid_q = sh_quote(&box_pidfile(name)),
        session_q = sh_quote(session),
        state_q = sh_quote(&box_state(name)),
        limits_q = sh_quote(&box_limits()),
        fleet_q = sh_quote(&fleet_limits()),
        cmd_q = sh_quote(agent_command),
    )
}

/// The fleet's mount set, one host path per line, for the launcher to cover.
///
/// Newline-separated because these are arbitrary host paths and every other separator can occur in
/// one. A path that contains a newline is *dropped* with a warning rather than passed: dropped, it
/// stays covered and a box loses access to it loudly; passed, it would split into two lines and the
/// launcher would bind back a directory nobody named.
///
/// **Empty when skein cannot name the box's repo**, which leaves the box uncovered rather than
/// covered-with-nothing-back. `repo_for_box` resolves by longest id prefix, so a box named after
/// its repo resolves and a box someone named themselves may not — and a box whose entitlements
/// skein cannot compute is exactly the box that must not have them computed as "none": it would
/// come up with no store, and provisioning gates startup. Said out loud, because a cover that
/// silently did not apply is the failure this whole mechanism exists to prevent.
fn mount_manifest(name: &str) -> String {
    if repo_for_box(name).is_none() {
        eprintln!(
            "skein: {name} matches no repository skein knows, so it cannot be told which mounts \
             are its own — it starts with the sandbox's whole view, as boxes did before covers"
        );
        return String::new();
    }
    let mut out = String::new();
    for mount in fleet_mounts() {
        if mount.contains('\n') {
            eprintln!(
                "skein: {mount:?} has a newline in it, so boxes cannot be told about it — \
                 it stays covered, and a box of that repo will not see it"
            );
            continue;
        }
        out.push_str(&mount);
        out.push('\n');
    }
    out
}

/// The shell that provisions a box: the store link, the branch, the hooks, the guide, the tracker.
///
/// This runs the kit's own startup script — the same bytes sbx runs at startup in a `--clone`
/// sandbox — rather than a fleet-shaped reimplementation of it. Provisioning is a dozen steps and
/// most of them fail *quietly*: a box whose store never got linked looks perfectly healthy and
/// simply never reports. Two implementations of that would be two sets of ways to be silently dark.
///
/// Four env vars carry what the script cannot work out for itself in a shared sandbox, because
/// every signal it normally reads there belongs to the sandbox rather than to the box:
///   * `SKEIN_PROVISION` — say so explicitly, since `/run/sandbox/source` does not exist here;
///   * `SKEIN_BOX`       — the identity, or every box reads one launch spec and one boot report;
///   * `SKEIN_STORE`     — the repo's store, a directory inside the mounted workspace rather than
///     a mount of its own, so the script's scan would find nothing;
///   * `WORKSPACE_DIR`   — the box's checkout, which is not this process's cwd.
///
/// **Must run inside the box's namespace**, not the sandbox: it writes `~/.codex`, `~/.claude` and
/// `~/shared`, and outside the namespace those are the sandbox's, shared by every box.
pub fn provision_script(name: &str, store: &str) -> String {
    format!(
        "SKEIN_PROVISION=1 SKEIN_BOX={name_q} SKEIN_STORE={store_q} WORKSPACE_DIR={tree_q} \
         bash {script_q}",
        name_q = sh_quote(name),
        store_q = sh_quote(store),
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        script_q = sh_quote(&box_provision_path()),
    )
}

/// Bring one box up inside the fleet sandbox, from nothing to a running, provisioned agent.
///
/// The ordering is forced by what each step produces rather than chosen: the anchor pid does not
/// exist until the session runs, the placement is meaningless without the anchor, and provisioning
/// must go *through* the placement or it writes the sandbox's `~/.claude` instead of the box's.
///
///   ensure the sandbox → clone the tree → start the session → read the anchor → record the
///   placement → provision inside it
///
/// Idempotent at the sandbox level and deliberately **not** at the box level: `clone_script` refuses
/// a tree that already exists and `box-session.sh` refuses a second server, because both are how a
/// re-run would otherwise hand a box someone else's uncommitted work or strand its namespace.
pub fn start_box(name: &str, repo: &Repo, branch: &str, agent_command: &str) -> Result<(), String> {
    let out = start_box_inner(name, repo, branch, agent_command);
    // Kept, because the person who needs it is not looking at this terminal. Creating a box from the
    // cockpit runs `skein start` in a PTY; when it fails, that terminal closes, the browser
    // reconnects, and the fresh one has none of the output. What it said instead was "its last start
    // failed, and the error came from that run rather than from this terminal" — an admission that
    // the answer existed and had been thrown away.
    match &out {
        Ok(()) => forget_start_failure(name),
        Err(why) => remember_start_failure(name, why),
    }
    out
}

/// Where the last failed start's reason is kept, per box.
///
/// Under `$SKEIN_HOME` rather than in the fleet, because a start that failed may never have reached
/// the sandbox — the commonest failure of all is not being able to see it.
fn start_failure_path(name: &str) -> Option<std::path::PathBuf> {
    valid_name(name).then(|| skein_home().join("starts").join(format!("{name}.err")))
}

/// Keep why a start failed, for the terminal that will ask later. Public because the CLI fails
/// *before* [`start_box`] too — no repo registered for the name, no branch to start on — and those
/// vanish with the create terminal exactly as the others did.
pub fn remember_start_failure(name: &str, why: &str) {
    let Some(path) = start_failure_path(name) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_ok() {
        let _ = crate::util::write_atomic(&path, dir, why.trim().as_bytes());
    }
}

/// Drop the record once the box starts. A stale reason on a working box is worse than none: it
/// would explain a failure that is over.
fn forget_start_failure(name: &str) {
    if let Some(path) = start_failure_path(name) {
        let _ = std::fs::remove_file(path);
    }
}

/// Why this box's last start failed, if one did and the box still is not there.
pub fn last_start_failure(name: &str) -> Option<String> {
    let text = std::fs::read_to_string(start_failure_path(name)?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| crate::util::clip(text, 400))
}

fn start_box_inner(
    name: &str,
    repo: &Repo,
    branch: &str,
    agent_command: &str,
) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("invalid box name {name:?}"));
    }
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured".into());
    }
    ensure_fleet(&sandbox, &fleet_mounts())?;
    // A fleet box has no `/run/sandbox/source`, so this is how it finds the repo's host files to
    // surface `shared-paths.txt` from — `.env`, and the `CLAUDE.md` some repos keep out of git.
    crate::kit::record_repo_mirror(repo);

    // The store is a HOST path used verbatim inside the sandbox, so this is the one precondition
    // worth paying a round-trip for: unreachable, every later step still "succeeds" and the box
    // comes up with no hooks and no probe — the failure this whole path is least able to see.
    let fleet = own_sandbox(&sandbox);
    let probe = format!("test -d {} && echo ok", sh_quote(&repo.store));
    if fleet
        .exec(&probe, Duration::from_secs(20))
        .ok()
        .as_deref()
        .map(str::trim)
        != Some("ok")
    {
        return Err(format!(
            "the fleet sandbox cannot see {store}, so box {name} would come up with no store.\n\
             Host paths are mounted when the sandbox is created, and this repo was registered after \
             that — a repo added by URL lands under a path that is already mounted, one adopted from \
             a local path does not.\n\
             Rebuild the sandbox with the mounts it needs: `skein resize {memory}` (or Settings → \
             fleet → resize in the cockpit). It carries every existing box across.\n\
             Do NOT `sbx rm {sandbox}` for this: it also works, and it destroys every box in the \
             sandbox along with any work they have not pushed.",
            store = repo.store,
            // The size it is already running at, so the line can be typed as it stands. A resize is
            // the remount; changing the size at the same time is a choice, not a requirement.
            memory = match load_config().fleet_memory.trim() {
                "" => "26g".to_string(),
                size => size.to_string(),
            }
        ));
    }

    // A launch that dies partway leaves a checkout, and sometimes a live session, behind. Carry on
    // from there rather than demand the box be destroyed: cloning is the only step here that is not
    // idempotent, and it is also the only one whose work a repeat would throw away.
    let (has_tree, has_session) = box_progress(&fleet, name, "skein-shell")?;
    let source = clone_source(repo);
    // Only an adopted repo needs this: a URL source already clones from the place it pushes to.
    let upstream = match crate::repos::is_git_url(&repo.source) {
        true => String::new(),
        false => remote_origin_url(&repo.work).unwrap_or_default(),
    };
    if has_tree {
        eprintln!("skein: {name} already has a checkout; keeping it");
        // Every box cloned before origin was re-pointed still pushes at the host, and they are not
        // going to be re-cloned to fix it. Guarded on origin still *being* the host clone, so a box
        // whose remote someone set deliberately keeps it.
        if !upstream.is_empty() {
            let _ = fleet.exec(
                &origin_repair_script(name, &source, &upstream),
                Duration::from_secs(60),
            );
        }
    } else {
        fleet.exec(
            &clone_script(name, &source, &base_branch(repo), branch, &upstream),
            Duration::from_secs(600),
        )?;
    }
    // Before the session, not after: `box-session.sh` reads the token as it comes up to decide what
    // `GH_TOKEN` holds, so a token placed afterwards would leave the box's first turn — the one
    // most likely to push — holding only the read credential. Failures are reported and not fatal:
    // a box that cannot push its own repo yet is recoverable, a box that will not start is not.
    for problem in crate::gitgate::refresh_tokens(name) {
        eprintln!("skein: {name} has no write token yet — {problem}");
    }

    let mut launched: Option<String> = None;
    if has_session {
        eprintln!("skein: {name} already has a live session; keeping it");
    } else {
        forget_turn_state(repo, name);
        launched = Some(fleet.exec(
            &session_script(name, "skein-shell", agent_command),
            Duration::from_secs(120),
        )?);
    }

    // Two different questions, and reading the anchor unconditionally answered the wrong one on the
    // adoption branch: there, no launcher ran, so "the launcher reports it" reached nothing and the
    // only file left to read was the box's own — which the box writes.
    let (ns_pid, generation, ns_start) = match &launched {
        Some(out) => {
            let pid = anchor_from_launch(out)?;
            let (generation, start) = stamp_anchor(&sandbox, name, pid)?;
            (pid, generation, start)
        }
        None => adopt_anchor(&sandbox, name)?,
    };
    record_place(
        name,
        &PlaceRecord {
            sandbox: sandbox.clone(),
            ns_pid,
            home: sandbox_home(&fleet)?,
            tree: format!("{}/tree", box_root(name)),
            sock: box_sock(name),
            generation,
            ns_start,
        },
    )?;

    // The launch spec is how the provisioning script learns the box's branch and runtime — without
    // it the box stays on the clone's default branch, which is a silently wrong box rather than a
    // failed one. The legacy path writes it while building the `sbx create` line; this path had no
    // equivalent. Never overwrite a restore's spec: that one also carries the handoff snapshot.
    if launch_spec(repo, name).is_none() {
        write_launch_spec_for_agent(name, branch, repo, &agent_for_box(name))?;
    }

    // Through the placement, so it lands in the box's private HOME rather than the sandbox's.
    let boxed = place_of(name).ok_or_else(|| format!("box {name} was not placed"))?;
    boxed.exec(
        &provision_script(name, &repo.store),
        Duration::from_secs(300),
    )?;

    // Every start, not just a migration's. `migrate_box` used to be the only caller, and its call
    // sits *after* `start_box` — so a migration that failed here left the conversation under the old
    // sandbox's slug with nothing that would ever move it, and the retry (`skein start`, because the
    // placement already exists and `migrate` now refuses the box) started the agent on an empty
    // transcript. Measured on lattice-feat-design-codex-claude: the work restored, the conversation
    // did not. It is idempotent — a box that already has a conversation at its own slug keeps it —
    // so the honest place for it is wherever a box comes up, not on one path through that.
    // After provisioning, because it writes into the box's private HOME, and on every start because
    // a repo registered since the box was built adds a host it has never trusted.
    ensure_box_known_hosts(name);

    // Same reasoning, same moment: a private HOME starts with no committer, and the box finds out
    // when it tries to commit rather than when it was built.
    let (who, email) = box_identity(name, repo);
    let script = identity_script(&who, &email);
    if !script.is_empty() {
        if let Err(e) = boxed.exec(&script, Duration::from_secs(30)) {
            eprintln!("skein: could not set {name}'s git identity ({e}); its first commit will ask who you are");
        }
    }

    match realign_transcript(name) {
        Ok(0) => {}
        Ok(n) => eprintln!("skein: pointed {n} transcript file(s) at {name}'s working directory"),
        Err(e) => {
            eprintln!("skein: {name} came up, but its conversation could not be located ({e})")
        }
    }

    // A box that started without a ceiling started *successfully*, so nothing else would ever say
    // so — and it is the one condition under which one box's runaway build can kill the others.
    if let Some(why) = uncapped_reason(name) {
        eprintln!(
            "skein: {name} is running WITHOUT a memory ceiling ({why}); a runaway build in it can \
             take down every other box in the fleet"
        );
    }
    Ok(())
}

/// Everything in a box that is not already on a remote, written into the repo's host-mounted store.
///
/// The fleet sandbox's memory and CPUs are fixed when it is created, so changing them means
/// destroying it — and every box's checkout is VM-local, which is exactly what makes builds fast and
/// makes this necessary. Committed-but-unpushed work, staged changes, unstaged changes and untracked
/// files each need their own artifact: a bundle preserves history a patch cannot, and `git diff`
/// covers neither untracked files nor the index/worktree distinction.
///
/// Returns the path *relative to the store*, which is the form the launch spec carries and the
/// provisioning script validates — it refuses anything not under `skein/handoff-snapshots/`.
pub fn snapshot_box(name: &str, store: &str, run: &str) -> Result<String, String> {
    let relative = format!("skein/handoff-snapshots/{name}/{run}");
    let snapshot = format!("{store}/{relative}");
    // Addressed from the SANDBOX, not through the box's namespace.
    //
    // A snapshot exists to rescue work, so requiring the box's *session* to be alive to take one is
    // backwards — and it fails exactly when it is needed most: a fleet box loses its tmux server
    // whenever the sandbox cycles, and the first thing resize did was refuse with `nsenter: cannot
    // open /proc/<pid>/ns/user`, leaving the work it was trying to save unreachable.
    //
    // Nothing here needs the namespace anyway. A box's tree and its private HOME are ordinary
    // directories in the sandbox (`/boxes/<name>/{tree,home}`), so naming them directly reads the
    // same bytes without entering anything. A legacy box keeps the old path: its sandbox IS the box.
    let placed = shared_record(name);
    let (boxed, enter, home) = match &placed {
        Some(record) => (
            own_sandbox(&record.sandbox),
            format!("cd {}; ", sh_quote(&format!("{}/tree", box_root(name)))),
            format!("{}/home", box_root(name)),
        ),
        None => (
            place_of(name).ok_or_else(|| format!("box {name} is not placed"))?,
            String::new(),
            "$HOME".to_string(),
        ),
    };
    let build = format!("{enter}{}", snapshot_script(&snapshot, name, &home));
    boxed.exec(&build, Duration::from_secs(600))?;

    // What the sweep refused to carry, said out loud. A snapshot that quietly leaves things behind
    // is worse than one that carries less: the box comes back looking complete.
    let skipped = std::fs::read_to_string(format!("{snapshot}/{SKIPPED_FILE}")).unwrap_or_default();
    let lines: Vec<&str> = skipped.lines().filter(|l| !l.trim().is_empty()).collect();
    if !lines.is_empty() {
        eprintln!(
            "skein: {name}'s snapshot leaves {} ignored path(s) behind — {}{}. \
             They are build output or dependencies by size; rebuild them in the box.",
            lines.len(),
            lines.iter().take(3).copied().collect::<Vec<_>>().join(", "),
            if lines.len() > 3 { ", …" } else { "" }
        );
    }
    Ok(relative)
}

/// Where the snapshot records the ignored paths it decided not to carry.
const SKIPPED_FILE: &str = "skipped-ignored.txt";

fn ignored_sweep(dir: &str, list: &str, skipped: &str) -> String {
    const FILE_KB: u64 = 10 * 1024;
    const DIR_KB: u64 = 20 * 1024;
    const DIR_FILES: u64 = 2000;
    let d = sh_quote(dir);
    format!(
        "git ls-files --others --ignored --exclude-standard --directory -z \
           -- . ':(exclude).claude' ':(exclude).claude/**' > {d}/ignored.list; \
         : > {skipped_q}; \
         while IFS= read -r -d '' p; do \
           case \"$p\" in \
             */) \
               n=$(find \"$p\" -type f 2>/dev/null | head -n {over} | wc -l); \
               if [ \"$n\" -ge {over} ]; then \
                 printf '%s (over {DIR_FILES} files)\\n' \"$p\" >> {skipped_q}; continue; \
               fi; \
               kb=$(du -sk \"$p\" 2>/dev/null | cut -f1); \
               case \"$kb\" in ''|*[!0-9]*) kb=0 ;; esac; \
               if [ \"$kb\" -gt {DIR_KB} ]; then \
                 printf '%s (%s MB)\\n' \"$p\" \"$((kb/1024))\" >> {skipped_q}; continue; \
               fi ;; \
             *) \
               kb=$(( $(wc -c < \"$p\" 2>/dev/null || echo 0) / 1024 )); \
               if [ \"$kb\" -gt {FILE_KB} ]; then \
                 printf '%s (%s MB)\\n' \"$p\" \"$((kb/1024))\" >> {skipped_q}; continue; \
               fi ;; \
           esac; \
           printf '%s\\0' \"$p\" >> {d}/{list}; \
         done < {d}/ignored.list; \
         rm -f {d}/ignored.list; ",
        over = DIR_FILES + 1,
        skipped_q = sh_quote(skipped),
    )
}

/// Everything a box's work is, written into the store: its commits, its index, its worktree, the
/// files git is not tracking, and the agent's own state.
///
/// **Ignored files are work too.** The sweep used to be `--others --exclude-standard`, which lists
/// untracked files and deliberately omits ignored ones — so `.env`, `.envrc`, local dev config and
/// the box's own `.skein/journal.md` were silently left behind on every migration and every resize.
/// The box came back looking complete and failed at runtime, or came back having forgotten what it
/// had been doing, which is worse than an error because nothing announces it.
///
/// The reason it cannot simply carry everything ignored is `node_modules/` and `target/`: this runs
/// for every box, into a host directory, and a resize does the whole fleet at once. So the rule is
/// **size, not names** — a hand-written list of build directories is a list that is wrong for the
/// next language. An ignored *file* is carried unless it is very large; an ignored *directory* is
/// carried when it is small enough to be config rather than artefacts. `.skein/` and `.env` pass;
/// a dependency tree does not. The file-count probe short-circuits at its threshold, so a directory
/// with 200k files costs one bounded `find` rather than a walk of the whole thing.
///
/// Whatever is refused is written to [`SKIPPED_FILE`] and reported by the caller.
///
/// **The bundle carries what the remote does not have, not the whole history.** `--all` on its own
/// wrote every object the repository has ever held into the store, over a virtiofs mount that is
/// several times slower to write than local disk: one box with a 1.8 GB `.git` took a migration past
/// its ten-minute budget doing nothing but copying history that already exists on the remote. A
/// resize does that for every box at once.
///
/// `--not --remotes` leaves a bundle of exactly the commits that would otherwise be lost, with the
/// rest recorded as prerequisites. That is safe *because of how the box is rebuilt*: the replacement
/// is cloned from the same source this box was, so every prerequisite is already in it before the
/// bundle is opened. Unpushed local commits are by definition not reachable from a remote ref, so
/// they are all still in there — which is the entire job.
///
/// The trimmed bundle is then **checked for the box's own branch**, and that check is the whole
/// safety of this. `--not --remotes` drops any ref whose tip the remote already has, so a box whose
/// checked-out branch is fully pushed gets a bundle without it — and if some *other* local ref is
/// unpushed the bundle is still non-empty, so it looks perfectly healthy. Measured: a 127 KB bundle
/// with no `refs/heads/<branch>` and no HEAD, and a restore that died on `couldn't find remote ref
/// HEAD` after the old sandbox had already been stopped.
///
/// When the branch is missing — or the bundle would be empty, which git refuses to write — the
/// fallback carries the tip commit alone, its parent recorded as a prerequisite. That is a ref the
/// restore can find, and still nothing like the full history. `--all` remains the last resort, for a
/// repository too young to have a parent commit.
fn snapshot_script(snapshot: &str, name: &str, home: &str) -> String {
    let s = sh_quote(snapshot);
    let skipped = format!("{snapshot}/{SKIPPED_FILE}");
    let sweep_ignored = ignored_sweep(snapshot, "untracked.list", &skipped);
    format!(
        "set -e; mkdir -p {s}; \
         b=\"$(git rev-parse --abbrev-ref HEAD)\"; \
         if ! {{ git bundle create {s}/repo.bundle --all --not --remotes 2>/dev/null \
                && git bundle list-heads {s}/repo.bundle 2>/dev/null \
                   | awk -v r=\"refs/heads/$b\" '$2==r{{f=1}} END{{exit !f}}'; }}; then \
           git bundle create {s}/repo.bundle 'HEAD~1..HEAD' 2>/dev/null \
             || git bundle create {s}/repo.bundle --all; \
         fi; \
         git diff --cached --binary HEAD > {s}/index.patch; \
         git diff --binary > {s}/worktree.patch; \
         git ls-files --others --exclude-standard -z -- . ':(exclude).claude' ':(exclude).claude/**' > {s}/untracked.list; \
         {sweep_ignored}\
         {pack}\
         printf '{{\"box\":\"%s\",\"branch\":\"%s\",\"head\":\"%s\"}}\\n' {n} \
           \"$(git rev-parse --abbrev-ref HEAD)\" \"$(git rev-parse HEAD)\" > {s}/manifest.json; \
         {agent_state}",
        n = sh_quote(name),
        pack = pack_carried(snapshot, "untracked.list", "untracked.tgz", &skipped),
        // A box already in the fleet host-binds its transcript; one being migrated in does not.
        agent_state = agent_state_tar(snapshot, home),
    )
}

/// Tar the swept paths, resolving the symlinks that would not survive the move.
///
/// A `--clone` sandbox bind-mounts the host repository at `/run/sandbox/source`, and a box's `.env`
/// is often a symlink into it — that is how the box reads the host's environment file without a
/// copy. tar preserves a symlink *as a symlink*, so the rescue faithfully carried a pointer to a
/// mount that does not exist in the fleet, and the box got a dangling link where its config should
/// be. It looked like the file was there, which is the worst of both outcomes.
///
/// So a symlink is carried as a symlink only while it still points inside the tree, where it will
/// mean the same thing after the move. One that points outside is carried as its *content*, since
/// the thing worth keeping is what it resolves to. One that already resolves to nothing is reported
/// rather than carried — there is nothing behind it to take.
///
/// Two passes into one archive rather than two archives: `-r` appends, `-h` dereferences, and each
/// path appears exactly once, so an extraction that refuses to overwrite still lands the right thing.
fn pack_carried(dir: &str, list: &str, archive: &str, skipped: &str) -> String {
    let d = sh_quote(dir);
    format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; \
         : > {d}/carry.list; : > {d}/deref.list; \
         while IFS= read -r -d '' p; do \
           if [ -L \"$p\" ]; then \
             if [ ! -e \"$p\" ]; then \
               printf '%s (dangling symlink -> %s)\\n' \"$p\" \"$(readlink \"$p\")\" >> {skipped_q}; \
               continue; \
             fi; \
             case \"$(readlink -f \"$p\" 2>/dev/null)\" in \
               \"$root\"/*) ;; \
               *) printf '%s\\0' \"$p\" >> {d}/deref.list; continue ;; \
             esac; \
           fi; \
           printf '%s\\0' \"$p\" >> {d}/carry.list; \
         done < {d}/{list}; \
         if [ -s {d}/carry.list ]; then tar --null -T {d}/carry.list -cf {d}/carry.tar; \
         else tar -cf {d}/carry.tar --files-from /dev/null; fi; \
         if [ -s {d}/deref.list ]; then tar --null -T {d}/deref.list -rhf {d}/carry.tar; fi; \
         gzip -c {d}/carry.tar > {archive_q}; \
         rm -f {d}/carry.tar {d}/carry.list {d}/deref.list {d}/{list}; ",
        skipped_q = sh_quote(skipped),
        archive_q = sh_quote(&format!("{dir}/{archive}")),
    )
}

/// The parts of a box's private `$HOME` that a rebuilt box needs and cannot get any other way.
///
/// An **allowlist**, and that is the whole design. The same argument that made `box-session.sh`
/// private-by-default applies in reverse here: an agent harness keeps state wherever it likes, and
/// with an exclude list anything unanticipated — a new token cache, a new auth file — would be
/// copied into the repo's store, which is host-side shared data. Named paths mean a harness change
/// costs a lost todo list, never a leaked credential.
///
/// `.credentials.json` is therefore not here, and does not need to be: `box-session.sh` seeds the
/// box's `~/.claude` from the sandbox's on first start, so the rebuilt box is already logged in.
///
/// The **transcript is deliberately not here.** A box host-binds `~/.claude/projects`, so the
/// conversation is already durable and already exactly where the rebuilt box will look — tarring it
/// would copy a virtiofs directory out to the store and straight back, twice over the slow path, to
/// arrive at the file that never left.
///
/// This used to be conditional, because a box migrating in from its own VM kept the transcript on
/// VM-local disk and would have lost it. There is no such box any more: every box lives in the shared
/// sandbox with a host-bound home, so the condition had exactly one reachable value.
fn agent_state_tar(snapshot: &str, home: &str) -> String {
    let carried: Vec<&str> = vec![
        ".claude/history.jsonl", // the prompt history
        ".claude/todos",         // in-flight task list
        ".claude.json",          // per-box MCP registration + project state
        ".codex/history.jsonl",
    ];
    let list = carried
        .iter()
        .map(|p| sh_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    // Only the paths that exist: tar fails the whole archive on a missing member, and which of these
    // a box has depends on which runtime it ran.
    // `home` is the box's private HOME as seen from wherever this runs: an absolute path in the
    // sandbox for a fleet box, and literally `$HOME` for a legacy one entered through its own place.
    format!(
        "have=''; for p in {list}; do [ -e \"{h}/$p\" ] && have=\"$have $p\"; done; \
         if [ -n \"$have\" ]; then tar -C \"{h}\" -czf {s}/agent-state.tgz $have; \
         else tar -czf {s}/agent-state.tgz --files-from /dev/null; fi",
        h = home,
        s = sh_quote(snapshot),
    )
}
fn box_archive(name: &str, run: &str) -> String {
    format!("{}/{run}.tar", box_state(name))
}

/// Copy one box out of the sandbox, whole, onto the host.
///
/// A **byte copy, not a reconstruction.** [`snapshot_box`] writes a git bundle, two patches and a
/// tarball of untracked files, from which the box is rebuilt on a fresh clone — that is the right
/// shape for a migration, where the destination is a different sandbox and often a different repo
/// state. It is the wrong shape here, where the destination is the same box in a rebuilt VM: the
/// reconstruction is slower (it re-clones), less faithful (the box comes back reassembled rather
/// than as it was), and it is where the fragility lives — the restore marker, the launch-spec
/// rewrite, the transcript realignment. `tar` has none of that, and carries `/tmp` besides, so a
/// resize is invisible to whatever the agent had half-finished there.
///
/// Uncompressed on purpose. The bulk of a box is git objects and `node_modules`, which are already
/// compressed; gzip would spend minutes of CPU to save little, where the write itself is seconds.
///
/// Sockets need no exclusion — `tar` skips them with a warning and carries on, which is what should
/// happen to a tmux socket whose server is about to die. `anchor.pid` does need one: it names a
/// process in a VM that will not exist, and restoring it would leave a box claiming an anchor that
/// was never there.
fn archive_box(fleet: &Place, name: &str, run: &str) -> Result<String, String> {
    let archive = box_archive(name, run);
    let mb = fleet
        .exec(&archive_script(name, &archive), Duration::from_secs(1800))
        .map_err(|e| format!("could not copy {name} out of the sandbox: {e}"))?;
    eprintln!("skein: {name} copied out ({} MiB)", mb.trim());
    Ok(archive)
}

/// The shell [`archive_box`] runs. Its own function so what the sandbox is asked to do is testable
/// without one — the exclusions here are the difference between a box that comes back and a box that
/// comes back claiming an anchor that does not exist.
///
/// **As root, and that is not a convenience.** A box is a general-purpose machine: it holds files
/// its own user cannot read — a fixture at mode 000, a root-owned build artifact, whatever a
/// container left behind — and `tar` exits non-zero on the first one it cannot open. Under the
/// `set -e` above that aborts the copy, and one unreadable file in one box then refuses the whole
/// fleet's resize. Which is what happened: 6,494 directories left by this project's own test suite,
/// each holding one deliberately unreadable file, and a resize of eight boxes stopped on the
/// seventh with the six already copied left to clean up.
///
/// Root also makes the copy *faithful*, which is the point of a byte copy: ownership and modes come
/// back as they were rather than as whoever ran the resize.
///
/// Deliberately NOT `--ignore-failed-read`. That turns the same situation into an archive missing
/// files nobody was told about, and a box restored short of its own contents is a worse outcome
/// than a resize that refused to start.
///
/// The archive is handed back to the invoking user afterwards, so the rest of the run — `du` here,
/// and the host reading it later — does not need root to touch what root has just written.
fn archive_script(name: &str, archive: &str) -> String {
    format!(
        "set -e; mkdir -p {state}; \
         sudo tar -C {root} --exclude=./anchor.pid --warning=no-file-ignored -cf {archive} . ; \
         sudo chown \"$(id -u):$(id -g)\" {archive}; \
         du -sm {archive} | cut -f1",
        state = sh_quote(&box_state(name)),
        root = sh_quote(&box_root(name)),
        archive = sh_quote(archive),
    )
}

/// Put one box back into a freshly rebuilt sandbox, exactly as it was, and drop the copy.
///
/// The delete is the point of doing it here rather than leaving it to a caller: an archive is the
/// size of the box, so a resize that kept them would leave gigabytes on the host every time it ran —
/// measured, 16 GiB of boxes against 61 GiB free, which is two resizes before the Mac is full. Once
/// `tar -x` has succeeded the bytes are back where they belong and the copy is redundant.
///
/// `set -e` is what makes that safe: the `rm` is only reached if the extraction returned zero, so a
/// resize that fails partway keeps the only copy of the box it could not restore. That copy is then
/// deliberately left behind — the caller says where it is, because at that point it is the box.
fn restore_box(fleet: &Place, name: &str, archive: &str) -> Result<(), String> {
    fleet
        .exec(&restore_script(name, archive), Duration::from_secs(1800))
        .map(|_| ())
        .map_err(|e| format!("could not put {name} back: {e}"))
}

/// The shell [`restore_box`] runs. Its own function for the same reason [`archive_script`] is: the
/// ordering here — extract, *then* delete, under `set -e` — is the whole safety property.
fn restore_script(name: &str, archive: &str) -> String {
    format!(
        // Root on the way back too, and for the matching reason: the archive holds modes and owners
        // the invoking user cannot recreate, and an unprivileged extract would either fail on them
        // or quietly hand every file to whoever ran the resize. As root, `tar` restores the
        // ownership recorded in the archive, which is what makes this a copy rather than a rebuild.
        "set -e; sudo mkdir -p {root}; sudo tar -C {root} -xf {archive}; rm -f {archive}",
        root = sh_quote(&box_root(name)),
        archive = sh_quote(archive),
    )
}

/// Whether a box should have a session again after the rebuild.
///
/// A resize puts the fleet back as it found it, so a box that was deliberately stopped stays
/// stopped — its checkout is restored either way, and `skein start` is then a session start. This
/// used to start everything with a placement record, which woke every stale box on the board.
///
/// An empty map is the sweep saying it could not tell, not that nothing was running, and everything
/// starts. That is the safer way to be wrong: a box wrongly started is a nuisance, a box wrongly
/// left stopped reads as a resize that lost it.
fn should_come_back(was_live: &std::collections::HashMap<String, bool>, name: &str) -> bool {
    was_live.is_empty() || was_live.get(name).copied().unwrap_or(false)
}

/// Refuse a resize that would fill the host disk, before anything is destroyed.
///
/// The archives are the size of the boxes — every checkout, every `node_modules`, every `/tmp` —
/// and they land on the Mac's own disk. Measured here at 16 GiB of boxes against 61 GiB free, which
/// fits and is not comfortable. Running the host out of space *during* a resize would be the worst
/// possible moment for it: the sandbox is gone and the rescue is half-written.
///
/// A fifth over the measured size, because `du` counts what the boxes use and `tar` writes a little
/// more (headers, and no sparse-file handling).
/// What a resize would destroy in `/var/lib/docker`, named so the person running it can decide.
///
/// A resize is `sbx rm -f` and `sbx create`, and `/var/lib/docker` is a **separate disk** made with
/// the sandbox and destroyed with it. [`archive_box`] copies [`fleet_root`] — the boxes' checkouts —
/// and nothing else, so every image and volume goes. That is fine for most of what is in there:
/// a pulled image comes back with `docker pull`, and a build cache is a cache. It is not fine for
/// the two kinds of thing nothing can recreate — an image that was **built here** and never pushed,
/// and a **named volume**, which exists precisely because someone wanted data to outlive a
/// container. Measured on this fleet: 45 GB of `/var/lib/docker`, including two locally-built
/// images and three named volumes.
///
/// Locally built is read as "has no repo digest". A digest is what an image gets by being pulled
/// from or pushed to a registry, so its absence means no registry has a copy. That over-reports a
/// pulled image someone has since retagged, and that is the right way to be wrong: this decides
/// whether to *ask*, and asking about something recoverable costs a sentence.
///
/// Anonymous volumes are excluded — a 64-hex name is one Docker made up for a container that did
/// not ask for a name, and treating those as precious would refuse every resize forever.
///
/// `Err` is "could not ask", not "nothing to lose", and the caller must not read it as the latter:
/// a wedged dockerd answers no question at all, and that is the state this fleet is most often in
/// when someone reaches for a resize.
/// The one shell [`docker_state_at_risk`] runs, printing `volume <name>` and `image <tag>` lines.
///
/// Its own constant so it can be run against a stub `docker` in a test — the filtering *is* the
/// decision here, and an assertion about the Rust that reads the output would prove nothing about
/// which images and volumes actually reach it.
///
/// `echo asked` is the marker that distinguishes "Docker answered, and holds nothing worth saving"
/// from "Docker did not answer". Without it both are the empty string, and the safe reading of one
/// is the unsafe reading of the other.
const DOCKER_PROBE_SH: &str = "docker volume ls --format '{{.Name}}' 2>/dev/null \
     | grep -vx '[0-9a-f]\\{64\\}' | sed 's/^/volume /'; \
     docker image ls --digests --format '{{.Digest}} {{.Repository}}:{{.Tag}}' 2>/dev/null \
     | awk '$1==\"<none>\" && $2!=\"<none>:<none>\" {print \"image\", $2}'; \
     echo asked";

fn docker_state_at_risk(fleet: &Place) -> Result<Vec<String>, String> {
    let out = fleet
        .exec(DOCKER_PROBE_SH, Duration::from_secs(60))
        .map_err(|e| format!("asking Docker what it is holding: {e}"))?;
    if !out.lines().any(|l| l.trim() == "asked") {
        return Err("Docker did not answer".into());
    }
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("volume ") || l.starts_with("image "))
        .map(str::to_string)
        .collect())
}

/// The refusal itself, as text — its own function so the wording is testable without a sandbox.
///
/// Long on purpose. This stops a command the person deliberately typed, so it has to say what would
/// go, why skein cannot carry it, how to carry it by hand, and how to proceed anyway. A refusal that
/// only says no gets worked around by the shortest available route, which here is `sbx rm -f`.
fn docker_refusal(at_risk: &[String]) -> String {
    // Capped: a fleet with forty volumes should not bury the last line, which is the one that says
    // how to proceed.
    const SHOWN: usize = 8;
    let listed: Vec<&str> = at_risk.iter().take(SHOWN).map(String::as_str).collect();
    let more = at_risk.len().saturating_sub(listed.len());
    format!(
        "a resize destroys /var/lib/docker, and it is holding {} thing{} nothing can put back — \
         resize aborted with the sandbox untouched.\n  {}{}\n  \
         That disk is created with the sandbox and destroyed with it, and skein's copy carries the \
         boxes' checkouts only. Images without a repo digest were built here and are on no \
         registry; named volumes are data someone asked to outlive a container.\n  \
         Save them first — `docker save -o <file> <image>` and, per volume, \
         `docker run --rm -v <volume>:/v -v {state}:/out alpine tar -C /v -cf /out/<volume>.tar .` \
         — writing to {state}, which is on the host and survives the rebuild.\n  \
         Or pass --drop-docker to resize anyway and lose them.",
        at_risk.len(),
        if at_risk.len() == 1 { "" } else { "s" },
        listed.join("\n  "),
        match more {
            0 => String::new(),
            n => format!("\n  …and {n} more"),
        },
        state = box_state_root(),
    )
}

fn room_to_copy_out(fleet: &Place) -> Result<(), String> {
    // `key value` lines rather than three bare numbers, because the third is absent whenever no
    // leftovers exist and positional parsing would then read the free space as the leftover size.
    let script = format!(
        "echo \"boxes $(du -sxm {root} 2>/dev/null | cut -f1)\"; \
         echo \"free $(df -Pm {state} | awk 'NR==2{{print $4}}')\"; \
         echo \"stale $(cat {state}/*/resize-*.tar 2>/dev/null | wc -c | awk '{{print int($1/1048576)}}')\"",
        root = sh_quote(&fleet_root()),
        state = sh_quote(&box_state_root()),
    );
    let out = fleet.exec(&script, Duration::from_secs(300))?;
    let read = |key: &str| -> Option<u64> {
        out.lines().find_map(|l| {
            l.trim()
                .strip_prefix(&format!("{key} "))?
                .trim()
                .parse()
                .ok()
        })
    };
    let (Some(boxes), Some(free)) = (read("boxes"), read("free")) else {
        // Unmeasurable is not the same as too small, and refusing on it would make a resize
        // impossible for anyone whose `df` says something unexpected.
        eprintln!("skein: could not measure the space a resize needs; continuing");
        return Ok(());
    };
    let needed = boxes + boxes / 5;
    if free < needed {
        let stale = read("stale").unwrap_or(0);
        return Err(format!(
            "copying the boxes out needs about {needed} MiB and the host has {free} MiB free — \
             resize aborted with the sandbox untouched. The boxes are {boxes} MiB.{}",
            match stale {
                // A successful resize deletes its copies as it restores them, so anything left is
                // from one that did not finish — and that is worth saying, because it is both the
                // space and, for whichever box it belongs to, the only copy of its work.
                0 => " Freeing space, or `skein stop`ping boxes you do not need, makes room."
                    .to_string(),
                mib => format!(
                    " {mib} MiB of that is held by copies from a resize that did not finish, under \
                     {}: check whether those boxes came back before deleting them.",
                    box_state_root()
                ),
            }
        ));
    }
    Ok(())
}

/// Change the fleet sandbox's memory or CPUs, carrying every box across.
///
/// sbx fixes both at creation — on Apple silicon it is Virtualization.framework underneath, where a
/// VM's memory is fixed in its configuration and validated at start — so this destroys the sandbox
/// and rebuilds it. Every box's checkout is VM-local, which is the thing that makes builds fast, so
/// all of it has to come out first and go back after. The sequence is:
///
///   copy every box out → destroy → recreate → copy every box back → restart
///
/// The boxes are copied **whole**, `/tmp` included, rather than reconstructed from a snapshot. See
/// [`archive_box`] for why: the box that comes back is the box that left, so nothing downstream has
/// to know a resize happened.
///
/// **Nothing is destroyed until every box is safely on the host.** A partial copy is not a partial
/// resize, it is lost work, and the boxes that would lose it are exactly the ones that could not be
/// read — so a single failure aborts with the sandbox still standing and every box still in it. That
/// ordering is the entire safety property of this function.
///
/// Restarting is best-effort *by design*: once the archives are written they are durable, on the
/// host, beside each box's other state. A box that fails to come back can be retried with
/// `skein start` — the tree is already there, so that is a session start rather than a rebuild.
/// Failing the whole resize because the fourth box's session timed out would help nobody.
pub fn resize_fleet(
    memory: &str,
    cpus: &str,
    disk: &str,
    drop_docker: bool,
) -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured; nothing to resize".into());
    }
    let boxes = placed_boxes(&sandbox);
    let fleet = own_sandbox(&sandbox);

    // ---- phase 1: get everything out, or change nothing ----
    // Space before work: the archives are the size of the boxes, and discovering the host is full
    // after the sandbox is gone would be the worst possible moment to discover it.
    room_to_copy_out(&fleet)?;
    // Then what the copy does NOT cover. `/var/lib/docker` is a disk of its own, destroyed with the
    // sandbox and carried by nothing, so a resize silently discards every locally-built image and
    // named volume in it — 45 GB of them on this fleet. Refused rather than warned: a warning is
    // read after the fact, and there is no after the fact for `sbx rm -f`.
    if !drop_docker {
        match docker_state_at_risk(&fleet) {
            Ok(at_risk) if !at_risk.is_empty() => return Err(docker_refusal(&at_risk)),
            Ok(_) => {}
            // Could not ask, which is not the same as nothing to lose — and a wedged dockerd is the
            // state this fleet is most often in when someone reaches for a resize. Refusing on
            // silence is the only reading that cannot destroy something nobody was told about.
            Err(why) => {
                return Err(format!(
                    "could not check what Docker is holding ({why}), and a resize destroys \
                     /var/lib/docker — resize aborted with the sandbox untouched.\n  \
                     Restart the daemon and try again, or pass --drop-docker to resize anyway and \
                     lose whatever is in there."
                ))
            }
        }
    }
    // Who was actually running, captured before anything else and never asked again: a resize must
    // put the fleet back as it found it, and starting a box that was deliberately stopped is not
    // that. Asked here rather than in phase 3 because by then every session is gone — after the
    // rebuild, "was this box running?" is a question the sandbox can no longer answer.
    //
    // `invalidate` first because a remembered answer is not good enough for a decision this coarse,
    // and it is exactly what invalidate is for: the next caller waits for the truth (see `Gate`).
    LIVENESS_GATE.invalidate();
    let was_live = fleet_liveness();
    // An empty map means the sweep could not tell, not that nothing was running. Starting
    // everything is the safer failure here — a box wrongly started is a nuisance, a box wrongly
    // left stopped looks like a resize that lost it.
    if was_live.is_empty() {
        eprintln!("skein: could not tell which boxes were running; all of them will be started");
    }
    // The login next, because it lives in the sandbox's HOME and the rebuild destroys it.
    // `ensure_fleet` restores it afterwards — but only if something captured it BEFORE the destroy,
    // and its own call runs after `sbx create`, when the sandbox is empty and there is nothing left
    // to save. Measured the hard way: a login made between two resizes was gone after the second.
    sync_fleet_login(&sandbox);
    let mut carried: Vec<Carried> = Vec::new();
    let run = format!("resize-{}", Utc::now().format("%Y%m%dT%H%M%SZ"));
    for (name, _) in &boxes {
        // The repo and branch are not needed to *save* the box any more — the archive is the whole
        // box — but `start_box` still needs them to bring it back, and finding that out after the
        // sandbox is destroyed would strand it. So they are still checked here, before anything.
        let repo = repo_for_box(name).ok_or_else(|| {
            format!(
                "box {name} belongs to no registered repo, so nothing could start it again — \
                 resize aborted with the sandbox untouched"
            )
        })?;
        let branch = branch_of(name).unwrap_or_default();
        if branch.trim().is_empty() {
            return Err(format!(
                "box {name} has no recorded branch to come back on — resize aborted with the \
                 sandbox untouched"
            ));
        }
        let archive = archive_box(&fleet, name, &run)
            .map_err(|e| format!("{e} — resize aborted with the sandbox untouched"))?;
        carried.push(Carried {
            live: should_come_back(&was_live, name),
            name: name.clone(),
            repo: repo.clone(),
            branch,
            archive,
        });
    }
    // No launch spec is rewritten here, unlike a migration. A restored box needs no instructions:
    // its tree is already on disk when `start_box` looks, so that path keeps the checkout and starts
    // the session rather than cloning and reconstructing.

    // ---- phase 2: the destructive part ----
    let config = load_config();
    save_config(&Config {
        fleet_memory: memory.trim().to_string(),
        fleet_cpus: cpus.trim().to_string(),
        // Empty keeps the configured disk rather than resetting it to sbx's 20 GB: `skein resize
        // 32g` is a memory change, and it must not silently shrink the disk back on the way past.
        fleet_disk: match disk.trim() {
            "" => config.fleet_disk.clone(),
            d => d.to_string(),
        },
        ..config
    })?;
    // Not the 30s action budget: tearing a microVM down is slower than a status query, and a
    // timeout here is reported as a failed destroy while the destroy carries on regardless.
    let (out, err, code) =
        run_capture_for("sbx", &["rm", "-f", &sandbox], Duration::from_secs(300))?;
    if code != 0 {
        let detail = if err.trim().is_empty() { out } else { err };
        return Err(format!(
            "could not destroy {sandbox}: {} — every box's work is saved in its repo store under \
             {run}, and `skein start <box>` restores it once the sandbox is rebuilt",
            detail.trim()
        ));
    }
    // The namespaces died with the sandbox. Forget them before rebuilding, or `place_of` would hand
    // out pids into a VM that no longer exists.
    for (name, _) in &boxes {
        forget_place(name);
    }
    // The one moment when there is no sandbox at all. If the rebuild fails here — most likely a
    // confirmation `sbx create` asked for and nobody could answer — say where the work is, because
    // the boxes are gone and their checkouts went with the VM.
    let again = format!(
        "skein resize {}{}",
        memory.trim(),
        match cpus.trim() {
            "" => String::new(),
            cpus => format!(" {cpus}"),
        }
    );
    ensure_fleet(&sandbox, &fleet_mounts()).map_err(|e| {
        format!(
            "{sandbox} is not usable yet: {e}\n\
             every box is copied out to its own state directory as {run}.tar, and nothing is lost. \
             `{again}` is safe to re-run — creating the sandbox is idempotent, so it retries only \
             the step that failed"
        )
    })?;

    // ---- phase 3: bring them back ----
    // Restore first, start second, per box: `start_box` decides what to do by looking for a tree, so
    // the archive has to be back on disk before it looks. A box that fails to restore is not started
    // at all — starting it would clone a fresh checkout over the top and quietly discard the work
    // this whole function exists to carry.
    let fleet = own_sandbox(&sandbox);
    let mut failed = Vec::new();
    for box_ in &carried {
        if let Err(e) = restore_box(&fleet, &box_.name, &box_.archive) {
            eprintln!(
                "skein: {}: {e} — its copy is intact at {}, so retrying the resize or restoring by \
                 hand still recovers it; NOT starting it, because that would clone over the top",
                box_.name, box_.archive
            );
            failed.push(box_.name.clone());
            continue;
        }
        // Restored but deliberately not started: it was not running when the resize began, and a
        // resize puts the fleet back as it found it. Its checkout is there, so `skein start` is a
        // session start whenever it is wanted.
        if !box_.live {
            eprintln!("skein: {} restored, left stopped as it was", box_.name);
            continue;
        }
        if let Err(e) = start_box(&box_.name, &box_.repo, &box_.branch, "exec bash -l") {
            eprintln!("skein: {} did not come back: {e}", box_.name);
            failed.push(box_.name.clone());
        }
    }
    Ok(failed)
}

/// One box on its way across a rebuild: where its copy is, and what `start_box` needs to bring it
/// back. Deliberately not [`BoxSnapshot`], whose `dir` is a path *relative to a repo store* because
/// that is the form a launch spec carries. A resize writes no launch spec and its archive is an
/// absolute host path, so sharing the type would mean two meanings for one field.
struct Carried {
    name: String,
    repo: Repo,
    branch: String,
    archive: String,
    /// Whether this box had a live session before the rebuild, and so should have one after.
    live: bool,
}

/// What a box clones from. The registered source when it is a URL — a box should start from the
/// same base the diff is taken against, not from whatever is stale or half-committed in the host's
/// clone. For a repo adopted in place there is no URL, so the host clone is it; that is mounted
/// (see [`fleet_mounts`]) at the same path, and git is content to clone a local directory.
///
/// That fallback carries a real consequence, stated here because nothing else would say it: for an
/// adopted repo the host clone's freshness *is* what every new box starts from. Nobody pulling it
/// means every new box quietly starts behind, with a green checkout and no signal at all. That is
/// what [`crate::repos::pull_repo`] is for, and why it did not become redundant when boxes started
/// cloning from remotes.
pub(crate) fn clone_source(repo: &Repo) -> String {
    if is_git_url(&repo.source) {
        repo.source.clone()
    } else {
        repo.work.clone()
    }
}

/// The branch a box's own branch is cut from.
///
/// Asked of the REMOTE skein is about to clone from, because that is the only copy whose answer is
/// necessarily true. `refs/remotes/origin/HEAD` looks like the remote's answer and is not: it is a
/// local cache written when the host clone was made, and it goes stale when the default is renamed
/// on the far side. A box migration failed on exactly that — the cached ref said `main`, the remote
/// had only `master`, and `git clone --branch main` refused, leaving the box's old sandbox stopped
/// with no fleet box to replace it.
///
/// So: `ls-remote --symref HEAD` first, one round trip against the same source the clone will use.
/// The cached ref and the host clone's own branch remain the offline fallbacks, and `main` only when
/// there is nothing left to ask.
pub fn base_branch(repo: &Repo) -> String {
    let git = |args: &[&str]| -> Option<String> {
        let mut argv = vec!["-C", repo.work.as_str()];
        argv.extend_from_slice(args);
        let (out, _, code) = run_capture("git", &argv).ok()?;
        let out = out.trim().to_string();
        (code == 0 && !out.is_empty()).then_some(out)
    };

    // What this repo's base might be called, most specific first. The configured one is the user's
    // own answer and leads; the two local reads are caches of the remote and follow it.
    let mut wanted: Vec<String> = Vec::new();
    let configured = load_config().base_branch.trim().to_string();
    if !configured.is_empty() {
        wanted.push(configured);
    }
    for candidate in [
        git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .and_then(|r| r.rsplit_once('/').map(|(_, b)| b.to_string())),
        git(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"),
    ]
    .into_iter()
    .flatten()
    {
        if !wanted.contains(&candidate) {
            wanted.push(candidate);
        }
    }

    // One round trip settles all of them: `HEAD` comes back as `ref: refs/heads/<default>` — the
    // remote's own name for its default, whatever it is — and each candidate comes back only if the
    // remote really has it. Naming the refs explicitly keeps the reply small on a repo with
    // thousands of branches. A local-path source answers this too, from its own refs.
    let source = clone_source(repo);
    let mut argv: Vec<String> = ["ls-remote", "--symref", &source, "HEAD"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(wanted.iter().map(|b| format!("refs/heads/{b}")));
    let listing = git(&argv.iter().map(String::as_str).collect::<Vec<_>>());

    if let Some(listing) = listing {
        // The first candidate the remote actually has wins — that is how a configured base of
        // `develop` is honoured on the repos that have one without breaking the repos that do not.
        if let Some(found) = wanted.iter().find(|b| {
            listing
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(&format!("refs/heads/{b}")))
        }) {
            return found.clone();
        }
        // None of them exist there — so take the remote's own default, which always does.
        if let Some(default) = listing
            .lines()
            .find_map(|line| line.strip_prefix("ref:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|r| r.rsplit_once('/').map(|(_, b)| b.to_string()))
        {
            return default;
        }
    }

    // Offline, or a source that cannot be reached: fall back to the local guesses in the same order
    // and let the clone's own fallback cover a wrong one. Empty rather than `main` when there is
    // nothing to go on at all — a guess that names a branch fails the clone outright, while naming
    // none asks git for the remote's default and cannot be wrong.
    wanted.into_iter().next().unwrap_or_default()
}

/// Read back the anchor pid `box-session.sh` recorded, so the host can write the box's placement.
///
/// The pid is knowable only inside the sandbox, and only after the session starts — which is why
/// placement is recorded after launch rather than predicted before it.
/// Where the fleet's logins are kept on the HOST, so they outlive the sandbox.
///
/// Not the shared project store — that is data the boxes read, and a credential has no business in
/// it. This is skein's own directory, beside the box state it already keeps there.
fn fleet_home_dir() -> std::path::PathBuf {
    skein_home().join("fleet-home")
}

/// The files that make a login a login, relative to a HOME.
const LOGIN_FILES: [&str; 2] = [".claude/.credentials.json", ".codex/auth.json"];

/// The token keys that mean "signed in", across both runtimes' shapes.
const LOGIN_KEYS: [&str; 5] = [
    "accessToken",
    "refreshToken",
    "access_token",
    "refresh_token",
    "OPENAI_API_KEY",
];

/// Does this credentials file actually contain a login?
///
/// The named blocks and the top level, never `mcpOAuth`: that block holds a per-repo grant for an
/// MCP server, and an MCP token says nothing about whether the *agent* is signed in. Counting it
/// would make every husk look like a login, since the grants survive a logout.
///
/// Unparseable or empty ⇒ `false`, which is the safe direction: it only ever declines to propagate.
fn carries_login(bytes: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let has = |b: Option<&serde_json::Value>| {
        b.and_then(|b| b.as_object()).is_some_and(|b| {
            LOGIN_KEYS.iter().any(|k| {
                b.get(*k)
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| !t.trim().is_empty())
            })
        })
    };
    has(v.get("claudeAiOauth")) || has(v.get("tokens")) || has(Some(&v))
}

/// Keep the fleet's login on the host, and put it back into a sandbox that has none.
///
/// `skein login` writes into the sandbox's own HOME, which is VM-local — so a resize destroyed it
/// along with everything else, and "log in once" quietly became "log in after every resize".
/// Measured: after a rebuild the sandbox came back with `cred=GONE`.
///
/// Newest wins, in one direction at a time: a sandbox that has the credential is the live copy and
/// refreshes the host's; a sandbox without one is freshly built and gets the host's back. Boxes
/// already reconcile into the sandbox at session start, so a re-login anywhere reaches here too.
///
/// Best-effort by design: a fleet running on API keys has no login to carry, and failing a launch
/// over that would be absurd.
///
/// And a credentials file is not a login. `box-session.sh` learned that — a logged-out agent leaves
/// the file in place with its tokens blanked, and that husk is *newer* than the working copy it
/// replaced — but this side was still "non-empty wins", so the sandbox's husk overwrote the host's
/// saved login and the fleet lost the copy it keeps precisely so a rebuild can restore it. Same bug,
/// one layer up. [`carries_login`] is the same test the launcher applies, kept in step by
/// `the_host_and_the_launcher_agree_on_what_a_login_is`.
/// Which runtimes have a login the fleet can hand to a new box.
///
/// Read from the host's own copy under `fleet-home`, not from the sandbox: this answers the first
/// question a new user has ("did `skein login` work?") and it must answer it with the fleet down,
/// during setup, before any box exists. `carries_login` rather than "the file is there", because a
/// logged-out agent leaves the file in place with its tokens blanked.
pub fn signed_in_runtimes() -> Vec<String> {
    let dir = fleet_home_dir();
    LOGIN_FILES
        .iter()
        .filter(|rel| {
            std::fs::read(dir.join(rel))
                .map(|b| carries_login(&b))
                .unwrap_or(false)
        })
        .map(|rel| match rel.starts_with(".codex") {
            true => "codex".to_string(),
            false => "claude".to_string(),
        })
        .collect()
}

pub fn sync_fleet_login(sandbox: &str) {
    let fleet = own_sandbox(sandbox);
    let dir = fleet_home_dir();
    for rel in LOGIN_FILES {
        let host = dir.join(rel);
        let in_sandbox = fleet
            .bytes(
                &format!("cat \"$HOME\"/{} 2>/dev/null || true", sh_quote(rel)),
                Duration::from_secs(20),
            )
            .unwrap_or_default();
        let saved = std::fs::read(&host).ok();
        match login_move(&in_sandbox, saved.as_deref()) {
            LoginMove::Save => {
                if let Some(parent) = host.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&host, &in_sandbox).is_ok() {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o600));
                }
            }
            LoginMove::Restore => {
                let restore = format!(
                    "mkdir -p \"$(dirname \"$HOME\"/{r})\" && cat > \"$HOME\"/{r} && chmod 600 \"$HOME\"/{r}",
                    r = sh_quote(rel)
                );
                let saved = saved.expect("Restore is only returned when the host has a copy");
                if let Err(e) = fleet.write(&restore, &saved, Duration::from_secs(30)) {
                    eprintln!("skein: could not restore the {rel} login into {sandbox}: {e}");
                }
            }
            LoginMove::Neither => {}
        }
    }
}

/// Which way a login should move between the sandbox and the host's kept copy.
#[derive(Debug, PartialEq, Eq)]
enum LoginMove {
    /// The sandbox has the live login; the host's copy is refreshed from it.
    Save,
    /// The sandbox has none; the host puts its copy back.
    Restore,
    /// Nobody has a login to move. Not an error — a fleet on API keys never has one.
    Neither,
}

/// The rule, as a decision rather than as control flow — because getting it wrong is silent, and
/// costs the fleet the copy it keeps precisely so a rebuild can restore it.
///
/// "The sandbox has a file" was the old test, and a husk is a file. A logged-out sandbox therefore
/// overwrote a perfectly good saved login, and after that there was nothing left to heal from.
fn login_move(in_sandbox: &[u8], on_host: Option<&[u8]>) -> LoginMove {
    if carries_login(in_sandbox) {
        return LoginMove::Save;
    }
    match on_host {
        Some(saved) if carries_login(saved) => LoginMove::Restore,
        _ => LoginMove::Neither,
    }
}

/// Make sure a placed box has a live session, restarting it from its own tree if not.
///
/// The box is its **tree**; the session is disposable. A fleet box's tmux server does not survive
/// the sandbox stopping — measured: `skein start` brought a box up at 14:12 with a live server, and
/// after the sandbox cycled the checkout, the private HOME and the cgroup ceiling were all intact
/// while the server was gone. Without this, every such box is unreachable until someone re-runs
/// `skein start`, and what they see first is `nsenter: cannot open /proc/<pid>/ns/user` — an error
/// about a namespace, for a box that simply needs starting again.
///
/// A no-op for a box with a live session, and for a box that isn't placed (its sandbox is its box,
/// and sbx starts that itself). Never clones: a missing tree is a different problem and saying so is
/// more useful than silently rebuilding one.
/// Why this box cannot be attached to at all, when that is knowable. `None` ⇒ go ahead and try.
///
/// A box with no placement used to be assumed legacy — one that owns a sandbox named after itself,
/// whose lifecycle sbx manages. That is one of two possibilities. The other is a box whose start
/// **failed**, which has no placement for the same reason it has no checkout: it was never created.
///
/// Treating the second as the first is what produced the loop: the terminal addressed it as its own
/// sandbox, sbx answered `no sandbox named …`, the browser reconnected, and the real error from
/// `skein start` — printed once, at the top — scrolled away behind an endless repeat of a message
/// about a sandbox that was never meant to exist. Worse, sbx's advice there is `sbx create AGENT
/// WORKSPACE`, which builds exactly the per-box VM the fleet exists to replace.
pub fn absent_box_reason(name: &str) -> Option<String> {
    if shared_record(name).is_some() {
        return None;
    }
    // Only speak when sbx has answered at least once. `None` is "cannot tell", and refusing a
    // terminal on that would be worse than letting sbx speak for itself.
    //
    // Note what this does *not* guarantee: [`crate::util::Gate`] serves the last good snapshot while sbx is
    // failing, so this can be reading a stale list. Safe in the direction that matters — a box created
    // since the snapshot has a placement record, which is checked first.
    let boxes = crate::sbx::fleet_boxes()?;
    // A sandbox that exists and skein did not place: someone's own `sbx` box, or one made by a skein
    // old enough to give every box its own VM. Both are read-only as far as skein is concerned. It
    // used to attach to these, which worked by accident for the per-VM ones and was always a guess for
    // the rest — skein has no checkout, no store and no tmux contract in a sandbox it did not build.
    if boxes.iter().any(|b| b.name == name) {
        return Some(format!(
            "{name} is a sandbox skein did not create, so there is nothing here to attach to.\r\n\
             skein runs boxes inside one shared sandbox and knows a box by the placement record it \
             wrote; this one has none.\r\n\
             Reach it directly with `sbx exec -it {name} bash -l`, or let skein own it: register its \
             repo with `skein add`, then create the box from the cockpit.\r\n"
        ));
    }
    Some(format!(
        "box {name} does not exist: skein has no placement for it, and sbx has no sandbox by that \
         name.\r\n\
         {}\r\n\
         Run `skein start {name} --branch <branch>` on the host to try again.\r\n\
         Do not run `sbx create` — sbx suggests it, and it would build the per-VM box skein no longer \
         supports, reserving a whole VM's memory whether or not the box is working.\r\n",
        match last_start_failure(name) {
            Some(why) => format!("Its last start failed: {why}"),
            None => "There is no record of a start having been attempted.".to_string(),
        }
    ))
}

pub fn ensure_box_session(name: &str) -> Result<(), String> {
    let Some(record) = shared_record(name) else {
        return Ok(()); // not skein's to start — see `absent_box_reason`
    };
    if fleet_liveness().get(name).copied().unwrap_or(false) {
        return Ok(());
    }
    let fleet = own_sandbox(&record.sandbox);
    let (has_tree, has_session) = box_progress(&fleet, name, "skein-shell")?;
    if has_session {
        return Ok(()); // raced with someone else's restart, or the sweep was stale
    }
    if !has_tree {
        return Err(format!(
            "box {name} has no checkout in {} — `skein start {name}` to build one",
            record.sandbox
        ));
    }
    // The launcher first, and this is not belt-and-braces. A sandbox keeps whichever copy of
    // `box-session.sh` was installed when it was last provisioned, so a fleet that predates the
    // running skein starts its boxes with an older launcher — and the failure is total rather than
    // partial, because a launcher that cannot parse what this skein passes it exits before tmux and
    // leaves the anchor pid naming a process from the last boot. What anyone sees then is
    // `nsenter: cannot open /proc/<pid>/ns/user` on every reconnect, forever, since nothing on this
    // path ever replaced the copy that could not start. [`heal_fleet`] does this at server start
    // too; here it also covers a sandbox that was asleep then and is being woken now.
    if let Err(e) = install_launcher(&record.sandbox) {
        eprintln!("skein: could not refresh the launcher in {} ({e}); {name} starts with whichever copy is already there", record.sandbox);
    }
    let out = fleet.exec(
        &session_script(name, "skein-shell", "exec bash -l"),
        Duration::from_secs(120),
    )?;
    // The anchor is a new process, so the old record addresses nothing. Re-record before anyone
    // tries to enter the namespace — that is the whole point of doing this here.
    let ns_pid = anchor_from_launch(&out)?;
    let (generation, ns_start) = stamp_anchor(&record.sandbox, name, ns_pid)?;
    record_place(
        name,
        &PlaceRecord {
            ns_pid,
            generation,
            ns_start,
            ..record.clone()
        },
    )?;
    // The sweep just became wrong in the other direction; a stale "dead" answer would send the very
    // next caller through this again.
    LIVENESS_GATE.invalidate();
    // What was observed is that the tmux server was gone, not *why*. A cycled sandbox is the common
    // cause and the one this exists for, but it is not the only one — a killed server or an OOM'd
    // box reach here identically — and asserting it sends anyone debugging to look for a restart
    // that never happened. The tree, the private HOME and the ceiling are all intact either way.
    eprintln!("skein: {name} had no live session, so it was restarted (its work is untouched)");
    Ok(())
}

/// Micro-cache over the fleet's liveness sweep, for the same reason [`crate::sbx::fleet_boxes`] has one:
/// the board asks per box, and a refresh must not become one `sbx exec` per box per tick. A
/// [`crate::util::Gate`] for the same reason too — see the note there — since this is the `sbx exec` skein
/// runs most often, and the one that kept a slow daemon slow.
static LIVENESS_GATE: crate::util::Gate<std::collections::HashMap<String, bool>> =
    crate::util::Gate::new();

/// Which boxes in the fleet sandbox have a live session — asked of the sandbox, in one round-trip.
///
/// A shared box's liveness *is* its tmux server: box alive ⇔ server alive ⇔ namespace joinable. That
/// question cannot be answered from the host. The anchor pid belongs to the sandbox's pid namespace,
/// so `/proc/<pid>` on the host asks about an unrelated process — and on macOS there is no `/proc`
/// at all, which reported every running box as stopped.
///
/// Every box at once because the board refreshes all of them, and a stopped sandbox answers for none
/// of them: an empty map means "cannot tell", which the caller reports rather than inventing.
/// Forget the remembered sweep, so the next caller waits for the truth instead of being handed the
/// last picture.
///
/// skein already does this internally wherever it changes what the sweep would see — starting a box,
/// restarting a dead session, resizing the fleet. This exposes it for the one case that is outside
/// skein: something *else* stopped a box, so the gate is holding an answer that is not merely stale
/// but wrong, and a warm gate serves that answer immediately while refreshing behind the caller.
/// That behaviour is deliberate and worth keeping — it is what stops the board blanking on a slow
/// tick — which is exactly why the caller who knows better has to say so.
///
/// The integration test needs it because `cfg!(test)` is false from `tests/`: the library it links
/// was built without it, so the "no gate under test" escape inside this module does not apply there,
/// and one test was being served the previous test's fleet.
pub fn forget_fleet_liveness() {
    LIVENESS_GATE.invalidate();
}

pub fn fleet_liveness() -> std::collections::HashMap<String, bool> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Default::default();
    }
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_millis(1500)
    };
    LIVENESS_GATE
        .get(fresh, move || {
            let script = format!(
                "for d in {root}/*/; do n=${{d%/}}; n=${{n##*/}}; s=\"$d/session.sock\"; \
                 if [ -S \"$s\" ] && tmux -S \"$s\" has-session 2>/dev/null; then echo \"$n 1\"; \
                 else echo \"$n 0\"; fi; done",
                root = fleet_root()
            );
            let out = own_sandbox(&sandbox)
                .exec(&script, Duration::from_secs(15))
                .ok()?;
            Some(
                out.lines()
                    .filter_map(|line| {
                        let (name, live) = line.trim().split_once(' ')?;
                        Some((name.to_string(), live == "1"))
                    })
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// The command that authenticates `runtime` inside the fleet sandbox, and why it differs per runtime.
///
/// There is no uniform spelling to guess at: `codex login` exists, `claude login` does not.
///
/// Claude's headless-looking option, `setup-token`, was tried here and does not do what this needs:
/// it returns a long-lived token to export as an environment variable and leaves no credential
/// behind, so the sandbox still answered "Not logged in · Please run /login" and `~/.claude` held
/// nothing but `backups`. Seeding a box copies FILES, so the flow that writes one is the flow that
/// works — `/login` inside the TUI. An unknown runtime gets a plain shell rather than a command
/// that fails in an unhelpful way.
pub fn fleet_login_command(runtime: &str) -> String {
    match runtime {
        "codex" => "codex login".into(),
        "claude" => "claude".into(), // then /login inside it
        _ => "exec bash -l".into(),
    }
}

/// Log in to a runtime once, in the sandbox's own HOME, so every box inherits it.
///
/// The sandbox is deliberately not on the board — it is not a box — so there is otherwise no way to
/// reach the one HOME that seeds all the others. Interactive by construction: every one of these
/// flows prints a URL and waits, so the terminal has to be the user's.
pub fn fleet_login(runtime: &str) -> Result<(), String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured".into());
    }
    let argv = [
        "exec".to_string(),
        "-it".into(),
        sandbox.clone(),
        "bash".into(),
        "-lc".into(),
        fleet_login_command(runtime),
    ];
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    match run_attached("sbx", &args)? {
        0 => Ok(()),
        code => Err(format!("login in {sandbox} exited {code}")),
    }
}

/// The sandbox user's `$HOME`, which is the HOME every command in a box must run with.
///
/// Not a private directory and not empty: `box-session.sh` binds the box's own `home` *over* this
/// path, so entering the namespace with it is what gives the box its private view. Recording an
/// empty string here instead made `Place::wrap` export `HOME=`, and every provisioning step then
/// resolved `$HOME/x` to `/x` — which is how a box tried to symlink `/shared` at the filesystem root
/// and reported "shared home unavailable" and a bare "mkdir: Permission denied".
///
/// Asked of the sandbox rather than assumed to be `/home/agent`: the image chooses the user.
fn sandbox_home(fleet: &Place) -> Result<String, String> {
    let home = fleet
        .exec("printf %s \"$HOME\"", Duration::from_secs(20))?
        .trim()
        .to_string();
    if home.is_empty() || !home.starts_with('/') {
        return Err(format!(
            "the fleet sandbox reported no usable HOME ({home:?}); every box command would run with \
             HOME unset and write to the filesystem root"
        ));
    }
    Ok(home)
}

/// How far a previous launch of this box got: does it have a checkout, and is its session alive?
///
/// One round-trip rather than two, and asked of the sandbox rather than inferred from a placement
/// record — after a failed launch the record is exactly what may be missing.
fn box_progress(fleet: &Place, name: &str, session: &str) -> Result<(bool, bool), String> {
    let script = format!(
        "tree=0; sess=0; \
         [ -e {tree_q}/.git ] && tree=1; \
         tmux -S {sock_q} has-session -t {session_q} 2>/dev/null && sess=1; \
         echo \"$tree$sess\"",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        sock_q = sh_quote(&box_sock(name)),
        session_q = sh_quote(session),
    );
    let out = fleet.exec(&script, Duration::from_secs(30))?;
    let out = out.trim();
    Ok((out.starts_with('1'), out.ends_with('1')))
}

/// What the launcher printed on stdout, or an error naming what it printed instead.
///
/// The marker rather than "the last line": the launcher runs a login shell inside the box, and a
/// profile that echoes anything at all would otherwise become the pid skein enters.
pub fn anchor_from_launch(out: &str) -> Result<u32, String> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("SKEIN_ANCHOR "))
        .next_back()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .ok_or_else(|| {
            format!(
                "the launcher did not report an anchor pid; it said: {}",
                crate::util::clip(out.trim(), 300)
            )
        })
}

/// Ask the sandbox what `pid` is, and stamp it.
///
/// One `sbx exec` on the launch path, which is not a hot path — and it has to be a separate one
/// from the launch itself, because the answer has to come from OUTSIDE every box namespace. A box
/// holds `CAP_SYS_ADMIN` in its own user namespace and can mount over its view of `/proc`.
fn stamp_anchor(sandbox: &str, name: &str, ns_pid: u32) -> Result<(String, u64), String> {
    let out = own_sandbox(sandbox).exec(&anchor_probe(ns_pid), Duration::from_secs(10))?;
    parse_anchor_probe(&out).ok_or_else(|| {
        format!("the sandbox could not say what pid {ns_pid} is, so {name} has no usable address")
    })
}

/// The anchor for a box whose session is already live, taken from what skein recorded and *checked*.
///
/// Never from the box. The pidfile under a box's own root is bound read-write, so a box can put a
/// sibling's tmux server pid there — and this is the path where reading it would matter most,
/// because no launcher runs here to report anything.
///
/// Refuses rather than guesses. A live session skein cannot address is a real state and a rare one
/// (it means the record predates this check, or was lost), and the honest answer is a sentence
/// naming the fix — not an address that might be another box.
fn adopt_anchor(sandbox: &str, name: &str) -> Result<(u32, String, u64), String> {
    let record = shared_record(name).ok_or_else(|| {
        format!(
            "{name} has a live session but no placement record, so skein has no address for it \
             that did not come from the box; restart it with `skein restart {name}`"
        )
    })?;
    let seen = stamp_anchor(sandbox, name, record.ns_pid)?;
    anchor_matches(name, &record, &seen)?;
    Ok((record.ns_pid, seen.0, seen.1))
}

/// Is the process at the recorded pid still the one skein recorded?
///
/// Pure, and separate from the reading, because this is the decision: every way of answering "no"
/// means **the box is gone**, and none of them means "enter this instead". Getting that backwards
/// is the whole vulnerability — a wrong address is not a degraded address, it is another box.
fn anchor_matches(name: &str, record: &PlaceRecord, seen: &(String, u64)) -> Result<(), String> {
    let restart = format!("restart it with `skein restart {name}`");
    if record.generation.is_empty() || record.ns_start == 0 {
        return Err(format!(
            "{name}'s placement record predates the anchor check, so skein cannot prove the \
             session it would enter is {name}'s; {restart}"
        ));
    }
    if record.generation != seen.0 {
        return Err(format!(
            "{name}'s anchor belongs to an earlier boot of the sandbox, so that pid now names \
             some other process; {restart}"
        ));
    }
    if record.ns_start != seen.1 {
        return Err(format!(
            "{name}'s anchor pid has been reused by a different process since skein recorded it; \
             {restart}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The anchor is whatever the launcher *marked*, not whatever it printed last.
    ///
    /// The launcher runs a login shell inside the box, so the box's own `.profile` gets to write to
    /// that stream first. Taking the last line would let a box choose the pid skein enters by
    /// echoing a number on login — the same confused deputy the pidfile gave it, through the
    /// channel that replaced the pidfile.
    #[test]
    fn the_launcher_report_is_read_by_its_marker_and_not_by_position() {
        assert_eq!(anchor_from_launch("SKEIN_ANCHOR 4242\n").unwrap(), 4242);
        assert_eq!(
            anchor_from_launch("welcome to your box\nSKEIN_ANCHOR 4242\n1234\n").unwrap(),
            4242,
            "a profile that prints a number after the report must not become the anchor"
        );
        let said = anchor_from_launch("1234\n").unwrap_err();
        assert!(
            said.contains("did not report") && said.contains("1234"),
            "an unmarked stream is a failure that quotes what it saw: {said}"
        );
    }

    /// Half an answer is no answer. Both halves are required, and a missing one must not read as a
    /// match against a record that also has a missing one.
    #[test]
    fn an_anchor_probe_that_could_not_answer_is_not_an_identity() {
        assert_eq!(
            parse_anchor_probe("abc-123 987\n"),
            Some(("abc-123".to_string(), 987))
        );
        assert_eq!(parse_anchor_probe(" 987\n"), None, "no boot id");
        assert_eq!(parse_anchor_probe("abc-123 0\n"), None, "no start time");
        assert_eq!(parse_anchor_probe("abc-123\n"), None, "one field");
        assert_eq!(parse_anchor_probe(""), None);
    }

    /// Every way of failing to match means the box is GONE, and says which way.
    ///
    /// Never "enter this instead": a pid that no longer names what skein recorded names something
    /// else in the same sandbox, and every other candidate is another box.
    #[test]
    fn an_anchor_that_does_not_match_is_a_dead_box_not_a_different_one() {
        let good = PlaceRecord {
            ns_pid: 42,
            generation: "boot-a".into(),
            ns_start: 900,
            ..Default::default()
        };
        assert!(anchor_matches("web-main", &good, &("boot-a".into(), 900)).is_ok());

        let cycled = anchor_matches("web-main", &good, &("boot-b".into(), 900)).unwrap_err();
        assert!(cycled.contains("earlier boot"), "{cycled}");

        let reused = anchor_matches("web-main", &good, &("boot-a".into(), 901)).unwrap_err();
        assert!(reused.contains("reused"), "{reused}");

        // A record from before the stamp existed cannot be checked, so it cannot be trusted — the
        // upgrade path is a restart, not a shrug.
        let old = PlaceRecord {
            ns_pid: 42,
            ..Default::default()
        };
        let said = anchor_matches("web-main", &old, &("boot-a".into(), 900)).unwrap_err();
        assert!(said.contains("predates"), "{said}");
        assert!(
            said.contains("skein restart web-main"),
            "every refusal names the fix: {said}"
        );
    }

    use crate::repos::save_repos;
    use crate::testutil::*;

    /// A box's CPU is a *rate*, and the cgroup only offers a running total.
    ///
    /// Reading `usage_usec` once and reporting it ranks boxes by how much CPU they have burned since
    /// they started, which puts yesterday's long build permanently at the top and never shows what
    /// is busy now. The difference between two samples over a known interval is the whole
    /// measurement.
    #[test]
    fn a_boxs_cpu_is_the_difference_between_two_samples() {
        // Half a second of wall clock; `busy` burns two full cores in it, `idle` none.
        let out = "\
a busy 1000000
a idle 5000000
b busy 2000000 4294967296 312
b idle 5000000 1048576 4
";
        let loads = parse_box_loads(out, 500_000.0);
        assert_eq!(loads.len(), 2);
        // Sorted by what you opened this to find out.
        assert_eq!(loads[0].name, "busy");
        assert_eq!(loads[0].cores, 2.0);
        assert_eq!(loads[0].mem, 4_294_967_296);
        assert_eq!(loads[0].pids, 312);
        assert_eq!(loads[1].name, "idle");
        assert_eq!(loads[1].cores, 0.0);
    }

    /// A box that appears between the two passes has no baseline, and must not be handed one.
    ///
    /// Treating a missing first sample as zero would subtract from it — reporting a box's entire
    /// lifetime of CPU as if it had all happened in half a second, which is both enormous and
    /// exactly the box that just started doing nothing.
    #[test]
    fn a_box_that_arrives_mid_measurement_is_reported_at_zero() {
        let loads = parse_box_loads("b newcomer 900000000 1048576 3\n", 500_000.0);
        assert_eq!(loads.len(), 1);
        assert_eq!(loads[0].cores, 0.0, "not 1800 cores");
    }

    /// The agent's install must put the token on **stdin**, never in the argv.
    ///
    /// `sbx exec`'s argv is visible in `ps` on the host and in the shell history of anything that
    /// logs the call, and this secret authorises running commands as the sandbox in any box's
    /// namespace. A fake `sbx` records exactly what it was handed, so the assertion is about what
    /// crossed the process boundary rather than about how the caller was written.
    #[test]
    fn installing_the_agent_never_puts_the_token_in_an_argument() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": true }).to_string(),
        )
        .unwrap();

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ncat >/dev/null\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        // The publish half cannot succeed here — nothing is listening for `agent_answers` to find,
        // which is exactly right: a mapping is only real if the agent answers through it. The
        // install and start still have to have happened, and that is what this asserts.
        let token = crate::place::ensure_agent_token().unwrap();
        let _ = ensure_fleet_agent("skein-fleet");
        let argv = std::fs::read_to_string(&log).unwrap_or_default();

        assert!(
            !argv.contains(&token),
            "the token reached the argv, where `ps` can read it:\n{argv}"
        );
        // It did get *sent*, just not as an argument: the write that carries it names its path.
        assert!(
            argv.contains(&fleet_agent_token_path()),
            "the token was never installed at all:\n{argv}"
        );
        // And the agent is started rather than merely dropped on disk — an installed agent nothing
        // launched is indistinguishable from no agent, except that it looks like it worked.
        assert!(
            argv.contains(AGENT_SESSION),
            "the agent was installed but never started:\n{argv}"
        );
        // Started only when it is not already up, so an ensure on a healthy fleet is a no-op rather
        // than a second Python process fighting for the port.
        assert!(argv.contains("has-session"), "unconditional start:\n{argv}");

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// Run the launcher's own credential sync over two fixture homes, handing back what each holds
    /// afterwards as `(box, sandbox)`.
    ///
    /// `newer` names the side that gets the later mtime — the tiebreak the rule used to apply to
    /// everything — so each case can say what should happen *despite* it. Stamped rather than slept
    /// into order: writing the two a second apart cost the suite ten seconds, and a suite slow
    /// enough to skip stops catching things.
    fn credential_sync(
        root: &std::path::Path,
        box_has: Option<&str>,
        sandbox_has: Option<&str>,
        newer: &str,
    ) -> (String, String) {
        let block = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("login_life() {"))
            .take_while(|l| !l.starts_with("done"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\ndone";
        let rel = ".claude/.credentials.json";
        let home = root.join("boxhome");
        let sandbox = root.join("sandboxhome");
        for base in [&home, &sandbox] {
            let _ = std::fs::remove_dir_all(base);
            std::fs::create_dir_all(base.join(".claude")).unwrap();
        }
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let new = old + Duration::from_secs(10);
        for (which, body) in [("box", box_has), ("sandbox", sandbox_has)] {
            let Some(body) = body else { continue };
            let base = if which == "box" { &home } else { &sandbox };
            std::fs::write(base.join(rel), body).unwrap();
            let when = if which == newer { new } else { old };
            std::fs::File::options()
                .write(true)
                .open(base.join(rel))
                .and_then(|f| f.set_times(std::fs::FileTimes::new().set_modified(when)))
                .unwrap();
        }
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -uo pipefail\nhome={home}\nexport HOME={sandbox}\n{block}\n",
                home = home.display(),
                sandbox = sandbox.display(),
            ))
            .output()
            .expect("bash to run the launcher's credential sync");
        assert!(
            out.status.success(),
            "the sync itself failed, which would abort the box start: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            std::fs::read_to_string(home.join(rel)).unwrap_or_default(),
            std::fs::read_to_string(sandbox.join(rel)).unwrap_or_default(),
        )
    }

    /// Sharing a login must neither share nor destroy a box's MCP grants.
    ///
    /// `.credentials.json` holds an `mcpOAuth` block per MCP server as well as the agent's login,
    /// and those are per-repo: a box's work-tracking gateway belongs to its repository, which is the
    /// same reason `~/.claude.json` is kept private. The sync copied the file whole, so one box's
    /// grants landed in another and — the half that actually breaks things — the receiving box's own
    /// grants were *discarded* rather than merged. The symptom is an MCP server asking to be
    /// authorised again for no reason, a long way from this code.
    ///
    /// Latent today, because every box in the fleet happens to point at one gateway. The second repo
    /// with its own is when it would bite.
    #[test]
    fn syncing_a_login_leaves_each_boxs_own_mcp_grants_alone() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        // Two boxes, two repos, two gateways — the shape the fleet does not have yet.
        let with_mcp = |token: &str, server: &str, grant: &str| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"r"}},"mcpOAuth":{{"{server}":{{"accessToken":"{grant}"}}}}}}"#
            )
        };
        let (box_after, sandbox_after) = credential_sync(
            root,
            Some(&with_mcp("sk-old", "sync|aaaa", "mcp-for-my-repo")),
            Some(&with_mcp("sk-new", "sync|bbbb", "mcp-for-another-repo")),
            "sandbox",
        );

        // The login travels, which is the point of the sync.
        assert!(
            box_after.contains("sk-new"),
            "the newer login did not reach the box:\n{box_after}"
        );
        // And the box keeps its own gateway grant, which is the point of this test.
        assert!(
            box_after.contains("mcp-for-my-repo"),
            "the box's own MCP grant was destroyed by a login sync:\n{box_after}"
        );
        assert!(
            !box_after.contains("mcp-for-another-repo"),
            "another repo's MCP grant was handed to this box:\n{box_after}"
        );
        // Symmetrically: nothing of the sandbox's moved but its login.
        assert!(
            sandbox_after.contains("mcp-for-another-repo")
                && !sandbox_after.contains("mcp-for-my-repo"),
            "the sandbox's MCP grants were disturbed:\n{sandbox_after}"
        );
    }

    /// A logout must not propagate, however new it is.
    ///
    /// Credentials sync both ways between a box and the sandbox so that logging in once is enough.
    /// "Newest wins" was the whole rule, and it cannot see the difference that matters: a logged-out
    /// agent leaves the file in place with its tokens blanked, and that husk is *newer* than the
    /// working copy it replaced. So a single logged-out box flowed its emptiness up on the next
    /// start, seeded every box created after it, and pulled it back down over logins that were fine
    /// — turning "log in once" into "log in to each box separately", which is the bug this was
    /// built to prevent. Found in the live fleet as boxes holding `"accessToken": ""`.
    ///
    /// Direction is not the fix and neither is order; the fix is that a file without a login never
    /// wins. These are the four states that can meet, driven through the launcher's own code.
    #[test]
    fn a_logged_out_box_cannot_log_out_the_fleet() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        // A real login, and the husk a logout leaves: identical structure, blanked tokens. The husk
        // is what the fleet actually had, so it is written as it was found rather than invented.
        let login = r#"{"claudeAiOauth":{"accessToken":"sk-live","refreshToken":"sk-ref","expiresAt":1786308957532},"mcpOAuth":{"sync|a":{"accessToken":"mcp-token"}}}"#;
        let husk = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0},"mcpOAuth":{"sync|a":{"accessToken":"mcp-token"}}}"#;
        let run = |box_has: Option<&str>, sandbox_has: Option<&str>, newer: &str| {
            credential_sync(root, box_has, sandbox_has, newer)
        };

        // The regression: a fresh logout must not overwrite an older, working login.
        let (box_side, sandbox_side) = run(Some(husk), Some(login), "box");
        assert!(
            sandbox_side.contains("sk-live"),
            "a newer logout overwrote the sandbox's login — one box logs out the fleet:\n{sandbox_side}"
        );
        assert!(
            box_side.contains("sk-live"),
            "the logged-out box was not healed from the sandbox's login:\n{box_side}"
        );

        // And the same in the other direction: the sandbox being the one that went stale.
        let (box_side, sandbox_side) = run(Some(login), Some(husk), "sandbox");
        assert!(
            box_side.contains("sk-live") && sandbox_side.contains("sk-live"),
            "a login was lost to a newer husk on the sandbox side:\nbox {box_side}\nsandbox {sandbox_side}"
        );

        // Two real logins still resolve by recency, which is what makes "log in anywhere" work.
        let fresher = login.replace("sk-live", "sk-fresh");
        let (box_side, sandbox_side) = run(Some(login), Some(&fresher), "sandbox");
        assert!(
            box_side.contains("sk-fresh") && sandbox_side.contains("sk-fresh"),
            "the newer of two logins did not win:\nbox {box_side}\nsandbox {sandbox_side}"
        );

        // Two husks are nothing to choose between, and neither is worth copying anywhere.
        let (box_side, sandbox_side) = run(Some(husk), Some(husk), "box");
        assert!(
            !box_side.contains("sk-live") && !sandbox_side.contains("sk-live"),
            "invented a login from two logouts"
        );

        // A box that has never run seeds from the sandbox — the original "log in once".
        let (box_side, _) = run(None, Some(login), "sandbox");
        assert!(
            box_side.contains("sk-live"),
            "a new box did not inherit the login:\n{box_side}"
        );
    }

    /// mtime says when a file was written; `expiresAt` says which credential is better.
    ///
    /// They come apart exactly where it costs: a box that starts rewrites its own copy, so it holds
    /// the newer mtime whether or not its token is the older one — and recency then replaces a valid
    /// login with a stale one. Found in the live fleet as boxes sitting on tokens that had expired
    /// days earlier while other boxes held good ones.
    ///
    /// Ordering by expiry can only ever prefer the longer-lived credential, so unlike recency it
    /// cannot lose a working login. That is the property here, driven both directions.
    #[test]
    fn the_longer_lived_login_wins_however_recently_the_other_was_written() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let cred = |tok: &str, exp: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{tok}","refreshToken":"r","expiresAt":{exp}}}}}"#
            )
        };
        let live = cred("sk-live", 1_900_000_000_000);
        let stale = cred("sk-stale", 1_700_000_000_000);

        let (box_side, sandbox_side) = credential_sync(root, Some(&live), Some(&stale), "sandbox");
        assert!(
            box_side.contains("sk-live") && sandbox_side.contains("sk-live"),
            "a newer *write* of an expired token beat a live login:\nbox {box_side}\nsandbox {sandbox_side}"
        );

        let (box_side, sandbox_side) = credential_sync(root, Some(&stale), Some(&live), "box");
        assert!(
            box_side.contains("sk-live") && sandbox_side.contains("sk-live"),
            "same loss in the other direction:\nbox {box_side}\nsandbox {sandbox_side}"
        );
    }

    /// The host's copy of the fleet login is the last resort, so a husk must never reach it.
    ///
    /// `fleet-home` exists for one job: a sandbox rebuild destroys its HOME, and this is what puts
    /// the login back. The test was "the sandbox has a file", and a logged-out sandbox has a file —
    /// so a single logout overwrote the saved login and there was then nothing left to restore
    /// from. Same shape as the bug the launcher was already fixed for, one layer up and untested.
    #[test]
    fn a_logged_out_sandbox_cannot_destroy_the_fleets_kept_login() {
        let login = br#"{"claudeAiOauth":{"accessToken":"sk-live","refreshToken":"r"}}"#;
        let husk = br#"{"claudeAiOauth":{"accessToken":"","refreshToken":""}}"#;

        // The regression, and the only case that loses data.
        assert_eq!(
            login_move(husk, Some(login)),
            LoginMove::Restore,
            "a logged-out sandbox overwrote the host's saved login"
        );
        // A live sandbox is the live copy; the host follows it, including across a token refresh.
        assert_eq!(login_move(login, Some(husk)), LoginMove::Save);
        assert_eq!(login_move(login, None), LoginMove::Save);
        // A freshly rebuilt sandbox: empty HOME, and the host has the answer.
        assert_eq!(login_move(b"", Some(login)), LoginMove::Restore);
        // Nothing anywhere is normal on API keys, and writing a husk into a sandbox helps nobody.
        assert_eq!(login_move(b"", None), LoginMove::Neither);
        assert_eq!(login_move(husk, Some(husk)), LoginMove::Neither);
        assert_eq!(login_move(husk, None), LoginMove::Neither);
    }

    /// The host and the launcher must not disagree about what a login is.
    ///
    /// Two implementations of one rule, in two languages, on either side of the same file: the
    /// launcher's `login_life` decides what propagates between a box and the sandbox, and the host's
    /// [`carries_login`] decides what is kept in `fleet-home` for a rebuild to restore. A drift
    /// between them is a fleet that heals in one direction and poisons in the other, which is
    /// exactly what "shared login doesn't work" looks like from outside.
    ///
    /// The mcpOAuth case is the one worth having: those grants SURVIVE a logout, so counting them
    /// would make every husk look like a login and put the original bug straight back.
    #[test]
    fn the_host_and_the_launcher_agree_on_what_a_login_is() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let block = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("login_life() {"))
            .take_while(|l| !l.starts_with("better_login() {"))
            .collect::<Vec<_>>()
            .join("\n");
        let cases: [(&str, bool); 8] = [
            (
                r#"{"claudeAiOauth":{"accessToken":"sk","refreshToken":"r"}}"#,
                true,
            ),
            (
                r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#,
                false,
            ),
            // A logout leaves the MCP grants behind. They are not a login.
            (r#"{"mcpOAuth":{"sync|a":{"accessToken":"grant"}}}"#, false),
            (r#"{"claudeAiOauth":{"accessToken":"   "}}"#, false),
            (
                r#"{"tokens":{"access_token":"a","refresh_token":"b"}}"#,
                true,
            ),
            (r#"{"OPENAI_API_KEY":"sk-x"}"#, true),
            (r#"{}"#, false),
            ("not json at all", false),
        ];
        for (body, want) in cases {
            let p = root.join("cred.json");
            std::fs::write(&p, body).unwrap();
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "set -uo pipefail\n{block}\nlogin_life {}",
                    p.display()
                ))
                .output()
                .expect("bash to run the launcher's login test");
            assert_eq!(
                out.status.success(),
                want,
                "the launcher disagrees about `{body}`"
            );
            assert_eq!(
                carries_login(body.as_bytes()),
                want,
                "the host disagrees about `{body}`"
            );
        }
    }

    /// Retiring the agent must not kill the thing that restarts it.
    ///
    /// `pkill -f` matches a process's entire command line, and the bare path appears in the command
    /// line of every process in the chain: the python agent, the `while true` supervisor that
    /// restarts it, the tmux session holding that supervisor, and any shell that so much as names
    /// the path — including the one running the `pkill` itself.
    ///
    /// This is not theoretical. Run on its own, the old pattern took down the supervisor along with
    /// the agent, so nothing came back and the transport stayed dead until someone started a new
    /// tmux session by hand. It was survivable in place only because `retire_stale_agent` is
    /// sandwiched between `tmux kill-session` and `start_fleet_agent`, which is a dangerous thing
    /// for a line to depend on.
    ///
    /// Checked with `grep -E`, which is the same extended-regex engine `pkill -f` uses, against the
    /// real command lines taken from `ps` on a live fleet.
    #[test]
    fn retiring_the_agent_matches_the_agent_and_nothing_that_restarts_it() {
        let path = "/boxes/.skein/fleet-agent.py";
        let pattern = agent_pkill_pattern(path);
        let matches = |cmdline: &str| -> bool {
            std::process::Command::new("grep")
                .arg("-E")
                .arg(&pattern)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .and_then(|mut c| {
                    use std::io::Write;
                    c.stdin.take().unwrap().write_all(cmdline.as_bytes())?;
                    c.wait()
                })
                .map(|s| s.success())
                .unwrap_or(false)
        };

        assert!(
            matches("python3 /boxes/.skein/fleet-agent.py 8317 /boxes/.skein/fleet-agent.token\n"),
            "the agent itself is no longer matched, so a stranded python survives: {pattern}"
        );
        for spared in [
            // The supervisor. Killing this is what turned a retirement into an outage.
            "bash -c while true; do python3 '/boxes/.skein/fleet-agent.py' 8317 '/boxes/.skein/fleet-agent.token'; sleep 2; done\n",
            // The tmux session that holds it.
            "tmux new-session -d -s skein-fleet-agent while true; do python3 '/boxes/.skein/fleet-agent.py' 8317 'x'; sleep 2; done\n",
            // A shell that merely mentions the path — such as the one running this very pkill.
            "bash -c pkill -f /boxes/.skein/fleet-agent.py\n",
            "bash -c install -m 700 src/fleet-agent.py /boxes/.skein/fleet-agent.py\n",
            // A different file that happens to share the prefix.
            "python3 /boxes/.skein/fleet-agent.python-backup 1 2\n",
        ] {
            assert!(
                !matches(spared),
                "would be killed and must not be: {spared:?} against {pattern}"
            );
        }
    }

    /// One unreadable directory must not blank the disk figures for the whole fleet.
    ///
    /// This is the bug as it actually happened, reproduced against real `du`. A box left a directory
    /// it could not read — an ordinary thing for a box to do — and `du` exited 1 while still
    /// printing correct totals for every other box. `exec` reads a nonzero exit as a failed call, so
    /// the totals were discarded and every box on the board reported no disk usage at all.
    ///
    /// It hid for a long time because the row chip stays silent below 80% of a box's allowance:
    /// "skein has no disk figure for this box" and "this box is nowhere near its limit" render
    /// identically. It only surfaced once the figure was shown unconditionally on hover.
    #[test]
    fn a_directory_it_cannot_read_does_not_erase_everyone_elses_disk_usage() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        for b in ["web-main", "api"] {
            std::fs::create_dir_all(root.join(b).join("tree")).unwrap();
            std::fs::write(root.join(b).join("tree/f"), vec![0u8; 4096]).unwrap();
        }
        // The shape that broke it: readable enough to be descended into, then a directory that is
        // not. `du` reports what it can and exits nonzero.
        let shut = root.join("web-main/secret");
        std::fs::create_dir_all(&shut).unwrap();
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(disk_usage_script(&root.display().to_string()))
            .output()
            .expect("sh to run the disk script");
        // Restored before any assertion can fail, or the temp dir cannot be cleaned up.
        let _ = std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755));

        assert!(
            out.status.success(),
            "a nonzero exit is read as a failed call, and the whole fleet's figures are dropped"
        );
        let got = parse_disk_usage(&String::from_utf8_lossy(&out.stdout));
        assert!(
            got.contains_key("api"),
            "the box with nothing wrong lost its figure too: {got:?}"
        );
        assert!(
            got.contains_key("web-main"),
            "the box with the unreadable directory still has a total: {got:?}"
        );
    }

    #[test]
    fn disk_usage_reads_dus_own_output_and_ignores_anything_else() {
        let got = parse_disk_usage(
            "3483\t/boxes/example-box-1/\n21\t/boxes/bridge-a-b-master/\ndu: cannot access 'x'\n\n",
        );
        assert_eq!(got.get("example-box-1"), Some(&3483));
        assert_eq!(got.get("bridge-a-b-master"), Some(&21));
        assert_eq!(got.len(), 2, "a stray line became a box: {got:?}");
    }

    /// An approved package is installed but never command-checked, and the distinction is the
    /// difference between a fleet that starts and one that does not.
    ///
    /// The provisioning script ends by proving its work: for every name it asked apt for, it checks
    /// `command -v` and fails the launch if the name is not on `$PATH`. That is exactly right for
    /// tmux and jq, which are commands. It is exactly wrong for the packages this gate exists to
    /// install — `libnss3` is the reason chromium cannot start in a box, it installs perfectly, and
    /// it puts no command anywhere. Folding approved packages into `$need` would therefore have
    /// taken down every fleet launch after the first approval, reporting a package as missing while
    /// it sat installed. So they go in `$extra`, and only `$need` is ever verified by command.
    #[test]
    fn an_approved_package_is_installed_without_being_mistaken_for_a_command() {
        let s = SUBSTRATE_SCRIPT;
        assert!(
            s.contains("apt-get install -y -qq $apt_want"),
            "approved packages are never installed: {s}"
        );
        assert!(
            s.contains(r#"missing=''; for t in $need;"#),
            "the command check must iterate $need alone"
        );
        assert!(
            !s.contains("for t in $apt_want") && !s.contains("for t in $extra"),
            "a library package would be reported missing and fail the launch"
        );
        // And it must not reinstall on every launch: a fleet start that always runs apt is a fleet
        // start that always waits for the dpkg lock.
        assert!(
            s.contains("dpkg-query -W") && s.contains("npm ls -g"),
            "already-installed approved packages are re-installed on every launch: {s}"
        );
    }

    /// The approved list reaches the script as one quoted value, whatever is in it.
    #[test]
    fn the_approved_packages_cannot_break_out_of_the_assignment() {
        // `sh_quote` is what stands between the manifest — an ordinary file on the host — and a
        // root command line, so this asserts the join is quoted rather than interpolated bare.
        let quoted = sh_quote("libnss3 libatk1.0-0");
        assert!(
            quoted.starts_with('\'') && quoted.ends_with('\''),
            "{quoted}"
        );
        assert_eq!(sh_quote("a'; rm -rf /; '"), r#"'a'\''; rm -rf /; '\'''"#);
    }

    /// `sudo` in a box explains itself instead of failing incomprehensibly — and only in a box.
    ///
    /// Two halves, and shipping either alone is worse than shipping neither:
    ///
    /// 1. **Written but never bound** is this repo's most-repeated bug — a thing installed on disk
    ///    that nothing ever reaches. The shim would sit in `$root/bin` while every agent kept
    ///    reading "owned by uid 65534, should be 0" and kept trying to chown it.
    /// 2. **Bound too widely** would be far worse than the problem it fixes. The sandbox's own
    ///    `sudo` is what `ensure_substrate` installs tmux and jq with — the very substrate this
    ///    message tells people to ask for. Shadowing it fleet-wide would stop new sandboxes being
    ///    provisioned at all, and the shim's own advice would become impossible to follow.
    #[test]
    fn sudo_in_a_box_says_why_rather_than_failing_in_hex() {
        let launcher = BOX_SESSION_SH;
        // Anchored on the heredoc that carries the shim's body rather than on the redirect that
        // writes it: the body is now preceded by a generated preamble (the box name and the
        // launcher's path, which cannot be known until a box starts), so the redirect is no longer
        // the line the body follows.
        let shim = launcher
            .lines()
            .skip_while(|l| !l.contains("cat <<'SHIM'"))
            .take_while(|l| *l != "SHIM")
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            shim.contains("does not work inside a box"),
            "the shim no longer says what happened"
        );
        assert!(
            shim.contains("--user") && shim.contains("substrate"),
            "a refusal with no way forward is the error message it replaced: {shim}"
        );
        assert!(
            launcher.contains(r#"binds+=(--ro-bind "$root/bin/sudo" "$sudo_real")"#),
            "the shim is written but never bound, so nothing in a box would ever run it"
        );
        // Inside the namespace only. `binds` is applied by the box's bwrap and by nothing else, so
        // being in that array is exactly the scope this needs — and `--dev-bind / /` above it means
        // a bind added anywhere outside it would reach the whole sandbox.
        assert!(
            !launcher.contains("chmod 755 /usr/bin/sudo")
                && !launcher.contains("rm -f /usr/bin/sudo"),
            "the sandbox's real sudo must be left alone — ensure_substrate provisions with it"
        );
        // The launcher is bash (it uses arrays); a shim that only parses under bash would still be
        // run by /bin/sh as `sudo`, so it is checked with the shell that will actually execute it.
        let checked = std::process::Command::new("sh")
            .arg("-n")
            .arg("/dev/stdin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                let body = shim
                    .split_once('\n')
                    .map(|x| x.1)
                    .unwrap_or_default()
                    .to_string();
                c.stdin.take().unwrap().write_all(body.as_bytes())?;
                c.wait()
            });
        assert!(
            checked.map(|s| s.success()).unwrap_or(false),
            "the shim is not a valid POSIX shell script, so `sudo` would fail on a syntax error \
             instead of explaining anything"
        );
    }

    /// The shim is skipped rather than allowed to fail, because failing here stops the box starting.
    ///
    /// bwrap cannot mount a file onto a symlink, and on Debian `sudo` is one
    /// (`/usr/bin/sudo` → `/etc/alternatives/sudo` → `/usr/bin/sudo.ws`). Binding the name instead
    /// of the resolved binary makes bwrap try to *create* the destination, which fails on a
    /// `/usr/bin` no unprivileged user can write — and takes the whole box down with it:
    ///
    /// ```text
    /// bwrap: Can't create file at /usr/bin/sudo: No such file or directory
    /// ```
    ///
    /// That is the trade this shim must never make. A worse error message is a nuisance; a box that
    /// will not start is an outage. So every uncertain step skips, and this proves it skips on the
    /// shape that actually broke it.
    #[test]
    fn a_sudo_it_cannot_shim_is_left_alone_rather_than_breaking_the_box() {
        // Ended at the block's last statement rather than at the first bare `fi`: the shim's own
        // body contains one now (it asks the launcher to file a request before explaining itself),
        // and stopping there cut the extraction off inside the heredoc — which failed as a syntax
        // error that looked like the launcher was broken when only this extraction was.
        let block = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("sudo_real=$(command -v sudo"))
            .take_while(|l| !l.contains(r#"binds+=(--ro-bind "$root/bin/sudo""#))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n  binds+=(--ro-bind \"$root/bin/sudo\" \"$sudo_real\")\nfi";
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let bin = root.join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();

        // A PATH with no system `sudo` on it at all, so "there is no sudo here" is a state this can
        // actually reach. Inheriting the real PATH makes every case find /usr/bin/sudo — including
        // the ones meant to find nothing, which is how a test like this passes while proving
        // nothing. The block still needs the handful of tools it runs, so they are linked in.
        let tools = root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        for tool in ["readlink", "mkdir", "chmod", "cat"] {
            let from = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| std::path::Path::new(d).join(tool))
                .find(|p| p.exists())
                .unwrap_or_else(|| panic!("{tool} is needed to run the launcher's sudo block"));
            std::os::unix::fs::symlink(from, tools.join(tool)).unwrap();
        }

        // `binds` printed at the end is the whole assertion: it is what the box's bwrap is handed,
        // so an entry here is a mount attempted and an empty array is the shim declining.
        let run = |sudo_is: &dyn Fn(&std::path::Path)| -> String {
            let _ = std::fs::remove_file(bin.join("sudo"));
            sudo_is(&bin);
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    // `box` because the shim now bakes the box's name into itself, and under
                    // `set -u` an unset one would fail the block for a reason that has nothing to
                    // do with what this test is about.
                    "set -uo pipefail\nbinds=()\nroot={root}\nbox=testbox\nexport PATH={bin}:{tools}\n\
                     {block}\nprintf '%s\\n' \"${{binds[@]:-}}\"\n",
                    root = root.display(),
                    bin = bin.display(),
                    tools = tools.display(),
                ))
                .output()
                .expect("bash to run the launcher's sudo block");
            assert!(
                out.status.success(),
                "the block itself failed, which would abort the box start: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        // The shape that broke it: a name that resolves to nothing. `command -v` still finds it —
        // which is why the original check was not enough.
        let dangling = run(&|bin| {
            std::os::unix::fs::symlink("/nonexistent/sudo.ws", bin.join("sudo")).unwrap();
        });
        assert!(
            !dangling.contains("--ro-bind"),
            "a dangling sudo was bound anyway; bwrap would refuse and the box would not start:\n{dangling}"
        );

        // No sudo at all: nothing to shim, and nothing to say about it.
        let absent = run(&|_| {});
        assert!(
            !absent.contains("--ro-bind"),
            "bound a sudo that is not there:\n{absent}"
        );

        // And the case it is actually for — bound, and bound at the RESOLVED binary rather than at
        // the symlink, which is the fix itself.
        let real = bin.join("sudo.ws");
        std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
        let present = run(&|bin| {
            std::os::unix::fs::symlink(bin.join("sudo.ws"), bin.join("sudo")).unwrap();
        });
        assert!(
            present.contains("--ro-bind") && present.contains(&real.to_string_lossy().to_string()),
            "the shim did not reach a sudo that is genuinely there:\n{present}"
        );
        assert!(
            std::fs::read_to_string(root.join("bin/sudo"))
                .unwrap_or_default()
                .contains("does not work inside a box"),
            "the shim was bound but its body was never written"
        );
    }

    /// An `sbx` that records what it was handed and succeeds. Returns the log and the PATH to put
    /// back, so an assertion can be about what crossed the process boundary rather than about how
    /// the caller was written.
    fn recording_sbx(home: &std::path::Path) -> (std::path::PathBuf, String) {
        use std::os::unix::fs::PermissionsExt;
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ncat >/dev/null\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        (log, path)
    }

    /// A stand-in agent that answers every request with `body`. Enough for the only question
    /// [`retire_stale_agent`] asks — how old is the thing currently serving — without needing a real
    /// one, which would be the wrong fixture anyway: the case worth testing is an agent this build
    /// cannot produce.
    fn fake_agent(body: String) -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut seen = [0u8; 1024];
                let _ = stream.read(&mut seen);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        port
    }

    /// The board says which transport is actually carrying calls, not which one was asked for.
    ///
    /// Three times now the transport has been silently off — wired into one call site and not the
    /// other, published on a port that never answered, and simply not switched on — and every time
    /// the symptom was identical: everything works, only fragile again, which nothing draws
    /// attention to until the stall it was meant to survive. So the indicator has to report what
    /// skein will *do*, never what was configured or what happens to be listening.
    #[test]
    fn the_board_reports_the_transport_it_will_actually_use() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let current = crate::place::AGENT_PROTOCOL;

        // Off: a fleet that declined it. No port, no probe, no claim.
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": false }).to_string(),
        )
        .unwrap();
        let off = transport_state();
        assert!(!off.configured && off.speaks == 0, "{off:?}");
        assert_eq!(
            off.wants, current,
            "the board must say which version it needs"
        );

        // On because nobody said otherwise — the default — but nothing was ever published, so the
        // honest answer is still `sbx exec`.
        std::fs::write(home.join("config.json"), serde_json::json!({}).to_string()).unwrap();
        let unpublished = transport_state();
        assert!(unpublished.configured && unpublished.speaks == 0 && unpublished.port == 0);

        // An agent answering, and an older one: the difference the board has to show, because a v1
        // carries the frequent calls while everything newer silently falls back.
        for (spoke, label) in [
            (1u32, "an agent from before versions existed"),
            (current, "current"),
        ] {
            let body = if spoke == 1 {
                "skein-fleet-agent".to_string()
            } else {
                format!("skein-fleet-agent {spoke}")
            };
            let port = fake_agent(body);
            std::fs::write(home.join("fleet-agent.port"), port.to_string()).unwrap();
            let seen = transport_state();
            assert_eq!(seen.speaks, spoke, "{label}: {seen:?}");
            assert_eq!(seen.port, port);
        }

        // And the switch wins over the wire: an agent may be sitting there answering, but with the
        // setting off skein is not using it, so the board must not say that it is.
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": false }).to_string(),
        )
        .unwrap();
        let ignored = transport_state();
        assert!(
            !ignored.configured && ignored.speaks == 0,
            "reported an agent skein will not call: {ignored:?}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// The agent lives in the sandbox and outlives the skein that installed it, and
    /// `start_fleet_agent` leaves a running session alone — so installing a newer script does not
    /// mean a newer agent is serving. Without this, an upgrade lands on disk and never runs: the
    /// setting on, the port answering, and every new endpoint quietly missing while the host falls
    /// back to `sbx exec` for exactly the calls that needed it.
    #[test]
    fn an_agent_older_than_this_skein_is_retired_so_the_new_one_can_start() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": true }).to_string(),
        )
        .unwrap();
        let (log, path) = recording_sbx(&home);

        // What is running in the fleet today: an agent from before versions existed, which answers
        // with its name alone.
        let old = fake_agent("skein-fleet-agent".into());
        std::fs::write(home.join("fleet-agent.port"), old.to_string()).unwrap();

        let _ = ensure_fleet_agent("skein-fleet");
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            argv.contains("kill-session"),
            "an agent too old to serve this build was left running:\n{argv}"
        );
        // And retired over `sbx exec` into the sandbox, never over the agent — that call kills the
        // process that would carry its own reply, so a successful retirement would report as a
        // failure and the healer would go on to report the fleet broken.
        assert!(
            argv.lines()
                .any(|l| l.contains("kill-session") && l.starts_with("exec skein-fleet")),
            "the retirement did not go through sbx into the sandbox:\n{argv}"
        );

        // An agent that already speaks this build is left alone. Restarting a healthy one on every
        // ensure would drop the held connection — and the board's liveness with it — for nothing.
        std::fs::write(&log, "").unwrap();
        let current = fake_agent(format!(
            "skein-fleet-agent {}",
            crate::place::AGENT_PROTOCOL
        ));
        std::fs::write(home.join("fleet-agent.port"), current.to_string()).unwrap();
        retire_stale_agent("skein-fleet");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap_or_default(),
            "",
            "a current agent was restarted for no reason"
        );

        // Nothing recorded means nothing is known to be serving; starting is the next step either
        // way, and killing on a guess would take out an agent that was working.
        std::fs::remove_file(home.join("fleet-agent.port")).unwrap();
        retire_stale_agent("skein-fleet");
        assert_eq!(std::fs::read_to_string(&log).unwrap_or_default(), "");

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// A box that was never created must say so, not be addressed as its own sandbox.
    ///
    /// The three states are genuinely different and were collapsed into one: **placed** (a fleet
    /// box, attach normally), **legacy** (no placement but a sandbox of its own, also fine), and
    /// **absent** (neither — its start failed). The third was being treated as the second, so the
    /// terminal ran `sbx exec <name>`, sbx said it had never heard of the sandbox, the browser
    /// reconnected, and the real error scrolled away behind the repeat.
    #[test]
    fn a_box_that_was_never_created_says_so_instead_of_looping() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("sbx");
        // sbx knows about the fleet and about one legacy box, and nothing else.
        std::fs::write(
            &fake,
            "#!/bin/sh\necho '{\"sandboxes\":[\
             {\"name\":\"skein-fleet\",\"status\":\"running\"},\
             {\"name\":\"old-box\",\"status\":\"running\"}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        // Absent: no placement, and sbx has never heard of it.
        let why = absent_box_reason("example-box-1").expect("an absent box to be named as absent");
        assert!(why.contains("does not exist"), "{why}");
        assert!(why.contains("skein start example-box-1"), "no way forward: {why}");
        // sbx's own advice here is `sbx create`, which would build the per-VM box skein dropped.
        assert!(why.contains("Do not run `sbx create`"), "{why}");
        // A sandbox skein did not create: no placement, but sbx knows the name. It used to attach —
        // correct by accident when every box was its own VM, a guess for anything else, since skein
        // has no checkout, no store and no tmux contract in a sandbox it did not build.
        let foreign =
            absent_box_reason("old-box").expect("a foreign sandbox to be named as foreign");
        assert!(
            foreign.contains("skein did not create"),
            "it must say whose sandbox this is: {foreign}"
        );
        assert!(
            foreign.contains("sbx exec -it old-box"),
            "and how to reach it anyway: {foreign}"
        );
        assert!(
            foreign.contains("skein add"),
            "and how to let skein own it: {foreign}"
        );

        // Placed: a fleet box attaches through its placement.
        record_place(
            "placed-box",
            &crate::place::PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 42,
                home: "/home/agent".into(),
                tree: "/boxes/placed-box/tree".into(),
                sock: "/boxes/placed-box/session.sock".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(absent_box_reason("placed-box").is_none());

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// Healing a running fleet must install the agent, not just the launcher.
    ///
    /// This is the regression that shipped: `ensure_fleet_agent` was wired only into `ensure_fleet`,
    /// which runs when a *box* starts. `heal_fleet` runs when the *server* starts, and that is the
    /// moment the setting is actually read — so turning it on did nothing at all until someone
    /// happened to start a box, and said nothing about why. Observed on a live fleet: the launcher
    /// had a fresh timestamp from the restart and `fleet-agent.py` was simply absent.
    ///
    /// A test of the shared helper would not have caught it, because the defect was `heal_fleet`
    /// never reaching the helper. So this drives `heal_fleet` itself.
    #[test]
    fn healing_a_running_fleet_installs_the_agent_too() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": true, "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        // Reports the fleet as running so `heal_fleet` does not skip it, and records everything else.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
                 if [ \"$1\" = ls ]; then \
                   echo '{{\"sandboxes\":[{{\"name\":\"skein-fleet\",\"status\":\"running\"}}]}}'; \
                   exit 0; fi\ncat >/dev/null\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        let _ = heal_fleet();
        let argv = std::fs::read_to_string(&log).unwrap_or_default();

        assert!(
            argv.contains(&fleet_agent_path()),
            "a server restart healed the launcher but never installed the agent:\n{argv}"
        );
        assert!(
            argv.contains(AGENT_SESSION),
            "the agent was installed but never started:\n{argv}"
        );
        // The launcher still gets healed — the agent is an addition, not a replacement.
        assert!(argv.contains(&box_session_path()), "{argv}");

        // And with the setting explicitly off, none of it happens. Explicitly: absence means *on*
        // now, so a fixture that simply omitted the field would be testing the opposite state while
        // reading as if it tested this one.
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": false, "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();
        std::fs::write(&log, "").unwrap();
        let _ = heal_fleet();
        let off = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !off.contains(&fleet_agent_path()),
            "installed when off:\n{off}"
        );
        assert!(
            off.contains(&box_session_path()),
            "healing stopped entirely:\n{off}"
        );

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// A fresh install gets the in-sandbox transport without anyone having to find the setting.
    ///
    /// The default was `false` for as long as the agent has existed, on the reasoning that a fleet
    /// which had not been given a second way in should not acquire one by upgrading. True of an
    /// upgrade, and never true of a first run — so every new install started on `sbx exec`, which is
    /// the call that hangs when the daemon stalls, and stayed there until someone read a doc comment
    /// about a field in a file they had no reason to open.
    ///
    /// No `config.json` at all here, because that is what a first run actually is. A fixture that
    /// wrote `{"fleet_agent": true}` would pass on any default.
    #[test]
    fn a_new_install_gets_the_faster_transport_without_being_asked() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
                 if [ \"$1\" = ls ]; then \
                   echo '{{\"sandboxes\":[{{\"name\":\"skein-fleet\",\"status\":\"running\"}}]}}'; \
                   exit 0; fi\ncat >/dev/null\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        let _ = heal_fleet();
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            argv.contains(&fleet_agent_path()),
            "a first run was left on `sbx exec`:\n{argv}"
        );
        assert!(
            argv.contains(AGENT_SESSION),
            "installed but never started:\n{argv}"
        );

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// Switching it off removes the agent instead of routing around it.
    ///
    /// This half barely mattered while the default was off: an agent existed only where someone had
    /// asked for one, and unasking was rare. Now that every fleet gets one, `false` is the *only*
    /// way to decline — and declining used to mean skein stopped calling the agent while the agent
    /// kept running, which reads as "off" from the host and is not off inside the sandbox.
    ///
    /// The second half is why this is not simply an unconditional kill: `heal_fleet_agent` runs on
    /// every server start and every box start, so a fleet that has never had an agent must not spend
    /// an `sbx exec` per call to kill a process that was never there.
    #[test]
    fn switching_the_agent_off_takes_it_away_rather_than_ignoring_it() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": false, "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
                 if [ \"$1\" = ls ]; then \
                   echo '{{\"sandboxes\":[{{\"name\":\"skein-fleet\",\"status\":\"running\"}}]}}'; \
                   exit 0; fi\ncat >/dev/null\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        // An agent that is up and answering, on the port skein last recorded.
        let live = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let serving = live.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in live.incoming().flatten() {
                let mut stream = stream;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 19\r\n\r\nskein-fleet-agent 2");
            }
        });
        record_agent_port(serving);

        let _ = heal_fleet();
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            argv.contains(&format!("kill-session -t '{AGENT_SESSION}'")),
            "declined, and left running anyway:\n{argv}"
        );
        // And not started again in the same pass. The path itself appears in the kill (it is the
        // `pkill` pattern), so what distinguishes install-and-start from stop is the start's own
        // `has-session` guard.
        assert!(
            !argv.contains("has-session"),
            "removed and started again in the same pass:\n{argv}"
        );

        // Nothing answering: no call at all, on the path that runs on every start.
        let gone = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let silent = gone.local_addr().unwrap().port();
        drop(gone);
        record_agent_port(silent);
        std::fs::write(&log, "").unwrap();
        let _ = heal_fleet();
        let quiet = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !quiet.contains("kill-session"),
            "a fleet with no agent still paid for a kill:\n{quiet}"
        );

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// The reason a box was never created outlives the terminal that was told it.
    ///
    /// Creating a box from the cockpit runs `skein start` in a PTY. When it fails, that terminal
    /// closes with the error in it, the browser reconnects, and the fresh terminal knows only that
    /// there is no box — so it said "its last start failed, and the error came from that run rather
    /// than from this terminal", which is an admission that the answer existed and was discarded.
    /// Reported as: the box will not create, and the message is about a box that does not exist.
    #[test]
    fn the_reason_a_box_never_started_survives_the_terminal_that_was_told_it() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // sbx answers, and has no sandbox by that name: the "box does not exist" branch.
        std::env::set_var("SKEIN_LS_CMD", "echo '[]'");

        let bare = absent_box_reason("web-main").expect("an unplaced box has a reason");
        assert!(
            bare.contains("no record of a start"),
            "a box nobody tried to start must not claim a failure: {bare}"
        );

        remember_start_failure(
            "web-main",
            "cannot tell whether the fleet sandbox exists: `sbx` is not on this process's PATH",
        );
        let told = absent_box_reason("web-main").expect("still unplaced");
        assert!(
            told.contains("not on this process's PATH"),
            "the reason was recorded and then not said: {told}"
        );

        // Cleared when a start works, because a stale reason explains a failure that is over — and
        // it would be read the next time any box of that name is missing for an unrelated reason.
        forget_start_failure("web-main");
        assert_eq!(last_start_failure("web-main"), None);

        std::env::remove_var("SKEIN_LS_CMD");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The fleet is sized against the machine it is going onto.
    ///
    /// It used to be sized by `fleet_memory`, whose default is a number chosen for the machine this
    /// was written on. On a 16 GB laptop that default is most of the RAM, applied by a first box
    /// launch, to a sandbox whose memory cannot be changed afterwards without rebuilding it.
    #[test]
    fn a_fleet_is_proposed_from_what_the_machine_actually_has() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let host = HostCapacity {
            cpus: 8,
            memory_mb: 16 * 1024,
            disk_free_mb: 100 * 1024,
            disk_total_mb: 500 * 1024,
            disk_path: "/".into(),
        };
        let want = proposed_fleet_size(&host);
        // 70% of 16 GB, leaving the host itself able to work while the fleet is busy.
        assert_eq!(want.memory, "11g", "{want:?}");
        // All but one: a saturated fleet still leaves a core to type in.
        assert_eq!(want.cpus, "7", "{want:?}");
        // Half the free space, and the cap: sparse or not, 300 GB of headroom is not a proposal.
        assert_eq!(want.disk, "50g", "{want:?}");

        // A machine that will not say how much memory it has gets a modest number rather than a
        // number derived from zero — which is what "70% of unknown" would be.
        let blind = HostCapacity {
            memory_mb: 0,
            disk_free_mb: 0,
            ..host.clone()
        };
        let want = proposed_fleet_size(&blind);
        assert_eq!(want.memory, default_fleet_memory_hint());
        assert_eq!(want.disk, "20g", "sbx's own default, not a guess: {want:?}");

        // What is already configured wins over any proposal: this dialog also opens on a fleet that
        // has been sized before, and overwriting that with an arithmetic default would silently undo
        // a decision someone made.
        let mut config = load_config();
        config.fleet_memory = "26g".into();
        config.fleet_cpus = "3".into();
        save_config(&config).unwrap();
        let want = proposed_fleet_size(&host);
        assert_eq!((want.memory.as_str(), want.cpus.as_str()), ("26g", "3"));

        std::env::remove_var("SKEIN_HOME");
    }

    /// `df` output, read from the end.
    ///
    /// The columns are fixed but the first and last can both be awkward: a long device name wraps
    /// onto its own line under some `df`s, and a mount point may contain spaces. Counting from the
    /// front gets the wrapped case wrong, which is how a fleet on an ordinary LVM host would have
    /// been proposed a 20 GB disk with 900 GB free.
    #[test]
    fn free_space_is_read_from_the_end_of_the_row() {
        let plain = "Filesystem 1024-blocks     Used Available Capacity Mounted on\n                     /dev/vda1     62914560 21495808  41418752      35% /\n";
        assert_eq!(parse_df(plain), (40448, 61440));

        let wrapped = "Filesystem 1024-blocks Used Available Capacity Mounted on\n                       /dev/mapper/ubuntu--vg-ubuntu--lv 1048576 524288 524288 50% /\n";
        assert_eq!(parse_df(wrapped), (512, 1024));

        assert_eq!(parse_df(""), (0, 0), "no output is not zero free");
        assert_eq!(parse_df("Filesystem 1024-blocks\n"), (0, 0));
    }

    /// The transport gets another chance after the one at startup.
    ///
    /// This is the state a real fleet was found in: `fleet_agent` on, and the board still on
    /// `sbx exec` because the server's single `heal_fleet` had printed "sbx did not answer, so
    /// skein-fleet was not brought into line with this build" and nothing ever tried again. The
    /// five-second `sbx ls` that gates every repair in `heal_fleet` is easy to miss at boot, when the
    /// daemon is cold and the server is starting alongside everything else.
    ///
    /// So the watcher is driven directly, with a fleet that answers *now*, and has to install what
    /// the startup pass did not.
    #[test]
    fn a_transport_that_missed_its_chance_at_startup_is_brought_up_later() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // No `fleet_agent` line at all: the default is on, and a fleet in this state has said
        // nothing about the transport either way.
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
                 if [ \"$1\" = ls ]; then \
                   echo '{{\"sandboxes\":[{{\"name\":\"skein-fleet\",\"status\":\"running\"}}]}}'; \
                   exit 0; fi\ncat >/dev/null\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        let said = heal_transport();
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            argv.contains(&fleet_agent_path()),
            "the fleet stayed on `sbx exec` with nothing trying again:\n{argv}"
        );
        assert!(
            argv.contains(AGENT_SESSION),
            "installed but not started:\n{argv}"
        );
        // Nothing is listening in a test, so the publish half cannot succeed — and the watcher must
        // say so rather than claiming a transport it does not have.
        let said = said.expect("a state change is worth one line");
        assert!(
            said.contains("sbx exec"),
            "the fallback must be named: {said}"
        );

        // The same news next minute is not news: a watcher on a one-minute tick that repeated itself
        // would bury the line that matters under sixty copies an hour. The backoff is cleared first,
        // so this tests the announcement and not the wait — otherwise it would pass for the wrong
        // reason the moment the backoff was introduced, which is exactly what happened.
        transport_attempt_worked();
        assert_eq!(heal_transport(), None, "the watcher repeated itself");

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// A port is never published to something that is not there.
    ///
    /// sbx has **no unpublish**: a mapping lasts as long as the sandbox. So publishing in order to
    /// find out whether the agent came up spends a permanent resource on a question with a cheap
    /// answer, and a fleet that cannot run the agent at all — no python3, a substrate that never
    /// installed, a crash loop — leaked two mappings per attempt. With the watcher retrying every
    /// minute that was 120 dead mappings an hour on a fleet already in trouble, each one a phantom
    /// sbx keeps reporting as published.
    #[test]
    fn a_port_is_not_published_for_an_agent_that_never_started() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        // Everything succeeds and nothing is running: `pgrep` finds no agent, so `exec` prints
        // nothing. This is the shape of a sandbox with no python3 — every step "works" and the
        // agent is not there.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ncat >/dev/null\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        let why = ensure_fleet_agent("skein-fleet").expect_err("no agent is running");
        assert!(why.contains("no python process"), "{why}");
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !argv.contains("--publish"),
            "a permanent port mapping was spent on an agent that is not there:\n{argv}"
        );
        // It still tried to start it — the check is about what happens *after* that fails, not about
        // skipping the attempt.
        assert!(argv.contains(AGENT_SESSION), "{argv}");

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// The watcher backs off, because its retry is not free.
    ///
    /// Each attempt writes the agent, writes its token, restarts it and may publish a port that can
    /// never be withdrawn. Run every minute against a fleet that cannot host an agent, the thing
    /// watching the fleet becomes the thing degrading it.
    #[test]
    fn a_failing_transport_is_asked_less_and_less_often() {
        let _g = env_lock();
        transport_attempt_worked();
        assert!(transport_attempt_due(), "the first attempt must go ahead");
        assert!(
            !transport_attempt_due(),
            "a minute has not passed, so this is the retry that must not happen"
        );
        // A success puts it back to trying promptly: the next problem is a new problem, and starting
        // it at the last one's backoff would leave a recovered fleet waiting half an hour.
        transport_attempt_worked();
        assert!(transport_attempt_due(), "a recovery must reset the wait");
        transport_attempt_worked();
    }

    /// A transport that is already current costs one loopback connection and no `sbx` at all.
    ///
    /// The watcher runs every minute for the life of the server, so the healthy case has to be
    /// nearly free — otherwise a fleet that is working pays forever for a repair aimed at one that
    /// is not. It is also what stops the watcher reinstalling an agent that is serving, which would
    /// retire a live one on a timer.
    #[test]
    fn a_current_transport_is_left_entirely_alone() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        let live = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let serving = live.local_addr().unwrap().port();
        let body = format!("skein-fleet-agent {}", crate::place::AGENT_PROTOCOL);
        std::thread::spawn(move || {
            for stream in live.incoming().flatten() {
                let mut stream = stream;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        record_agent_port(serving);

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("argv.log");
        let fake = bin.join("sbx");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        let said = heal_transport().expect("the boring state is still worth saying once");
        assert!(said.contains(&serving.to_string()), "{said}");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap_or_default(),
            "",
            "a healthy transport was repaired anyway"
        );

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// A published port only counts when the agent **answers** through it.
    ///
    /// This is the whole healing contract, and it exists because sbx keeps reporting a mapping as
    /// published after the sandbox it belonged to is gone, while every connection through it is
    /// refused (docker/sbx-releases#297). skein recreates the fleet sandbox on every resize, so it
    /// meets that state routinely — and a healer that believed `sbx ports` would sit on the broken
    /// mapping forever. Driven with a fake `sbx` that "publishes" everything successfully and a
    /// real listener on exactly one port, which is that bug in miniature.
    #[test]
    fn a_port_is_only_healed_onto_when_the_agent_actually_answers() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // A listener that answers /health exactly as the agent does. This is the ONLY working port;
        // the fake `sbx` below claims success for every one of them.
        let live = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let working = live.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in live.incoming().flatten() {
                let mut stream = stream;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\nskein-fleet-agent");
            }
        });

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("ports.log");
        let fake = bin.join("sbx");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        // A recorded port that no longer answers is precisely the post-resize phantom. Healing must
        // leave it, not trust it.
        let phantom = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let dead = phantom.local_addr().unwrap().port();
        drop(phantom);
        record_agent_port(dead);

        // A pin is honoured and never silently replaced — otherwise the pin is a suggestion.
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": true, "fleet_agent_port": working }).to_string(),
        )
        .unwrap();
        assert_eq!(ensure_fleet_agent_port("skein-fleet").unwrap(), working);
        assert_eq!(recorded_agent_port(), Some(working));

        // A pin that does not answer fails rather than wandering onto another port, and says so.
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": true, "fleet_agent_port": dead }).to_string(),
        )
        .unwrap();
        record_agent_port(dead);
        let err = ensure_fleet_agent_port("skein-fleet").unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
        assert!(
            err.contains("sbx exec"),
            "the fallback must be stated: {err}"
        );

        // The publish actually went through sbx, in sbx's own spelling.
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains(&format!(
                "ports skein-fleet --publish {dead}:{AGENT_SANDBOX_PORT}/tcp"
            )),
            "wrong publish wire format:\n{calls}"
        );

        // A working port that is ALREADY recorded is returned without republishing: re-publishing a
        // healthy mapping is how a working fleet acquires a broken one.
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": true }).to_string(),
        )
        .unwrap();
        record_agent_port(working);
        let before = std::fs::read_to_string(&log).unwrap_or_default().len();
        assert_eq!(ensure_fleet_agent_port("skein-fleet").unwrap(), working);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap_or_default().len(),
            before,
            "a healthy port was republished"
        );

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// Verbatim output of [`resource_script`] on a live fleet, so the parser is tested against what
    /// the guest actually prints rather than against what this file assumes it prints.
    const LIVE_REPLY: &str = "mem_total 26377\nmem_used 11215\ncpus 11\nload1 6.89\nload5 5.48\n\
                              disk_dev overlay\ndisk_total 60168\ndisk_used 20986\n\
                              images_dev /dev/vdd\nimages_total 50089\nimages_used 45433\n\
                              skein 1440\ndocker 8880\n";

    #[test]
    fn the_gauge_reads_a_live_reply_and_the_shares_fit_inside_the_total() {
        let r = parse_resources(LIVE_REPLY).expect("a reply with a memory total is a reply");
        assert_eq!((r.mem_total, r.mem_used), (26377, 11215));
        assert_eq!((r.boxes, r.docker), (1440, 8880));
        assert_eq!((r.disk_total, r.disk_used, r.cpus), (60168, 20986, 11));
        assert_eq!((r.load1, r.load5), (6.89, 5.48));
        // Docker's own disk, which the strip did not draw at all until this: the reply above is a
        // fleet whose boxes' disk is a third full while the one Docker writes to is at 91%. A build
        // that ran out of space there did so against a gauge showing two thirds free.
        assert_eq!((r.images_total, r.images_used), (50089, 45433));
        // The stacked memory bar draws boxes + docker + everything-else against the total, so a
        // reading where the parts exceed the whole is one that renders as a bar past its own end.
        // This is exactly what `memory.current` produced — 11.0 GB and 10.8 GB against 2.9 GB used —
        // and the reason those two figures are `anon` from `memory.stat` instead.
        assert!(
            r.boxes + r.docker <= r.mem_used,
            "the cgroups' share must fit inside what the VM is using: \
             {} + {} against {}",
            r.boxes,
            r.docker,
            r.mem_used
        );
    }

    /// A resize destroys `/var/lib/docker`, so it has to know what is in there worth keeping.
    ///
    /// The filtering is the decision, so it is run for real against a stub `docker` rather than
    /// asserted about: what must survive the filter is an image nothing can re-pull and a volume
    /// someone named, and what must not is everything a `docker pull` or a rebuild puts back.
    /// Getting the second half wrong is not harmless — a resize that refuses over a dangling image
    /// refuses forever, and the way round it is `sbx rm -f`, which loses the boxes too.
    #[test]
    fn the_resize_asks_docker_only_about_what_it_could_not_put_back() {
        let dir = tempdir();
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // Real shapes: a pulled image carries a digest, one built here does not, a dangling layer
        // has neither name nor tag, and an anonymous volume is 64 hex characters Docker chose.
        std::fs::write(
            bin.join("docker"),
            "#!/bin/sh\ncase \"$1 $2\" in\n\
             \"volume ls\") printf 'thing-cargo\\nthing-target\\n\
             3f2a91b8c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1\\n' ;;\n\
             \"image ls\") printf 'sha256:aa11 pgvector/pgvector:pg16\\n\
             <none> thing-rust:local\\n<none> thing-ocr:local\\n<none> <none>:<none>\\n' ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(
            bin.join("docker"),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(DOCKER_PROBE_SH)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .expect("run the probe");
        let lines: Vec<&str> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| Box::leak(l.to_string().into_boxed_str()) as &str)
            .collect();

        assert_eq!(
            lines,
            vec![
                "volume thing-cargo",
                "volume thing-target",
                "image thing-rust:local",
                "image thing-ocr:local",
                "asked",
            ],
            "kept: named volumes and images no registry has a copy of. dropped: the anonymous \
             volume, the pulled image, the dangling layer"
        );
    }

    /// The refusal has to be worth reading, because the alternative to reading it is `sbx rm -f`.
    #[test]
    fn the_refusal_names_what_would_go_and_how_to_proceed_anyway() {
        let at_risk: Vec<String> = ["image thing-rust:local", "volume thing-cargo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let said = docker_refusal(&at_risk);
        assert!(said.contains("thing-rust:local") && said.contains("thing-cargo"));
        assert!(
            said.contains("sandbox untouched"),
            "the first thing to know is that nothing has happened yet: {said}"
        );
        assert!(
            said.contains("docker save") && said.contains("tar -C /v"),
            "refusing without saying how to keep them just moves the problem: {said}"
        );
        assert!(
            said.contains("--drop-docker"),
            "a refusal with no way past it gets worked around outside skein: {said}"
        );
        // A fleet with forty volumes must not bury the line that says how to proceed.
        let many: Vec<String> = (0..40).map(|i| format!("volume v{i}")).collect();
        let long = docker_refusal(&many);
        assert!(long.contains("…and 32 more") && long.contains("--drop-docker"));
    }

    /// Two disks or one, drawn honestly either way.
    ///
    /// sbx gives a sandbox a root filesystem and a separate `/var/lib/docker`, sized by two
    /// different create-time variables — so filling one says nothing about the other, and a single
    /// disk gauge answered the wrong question confidently. But it does not *have* to be two: a
    /// sandbox built without the second has Docker on the boxes' own filesystem, and drawing that
    /// as a second gauge would show the same bytes twice under two names. Told apart by device,
    /// which is the only comparison that answers "is this the same storage".
    #[test]
    fn dockers_disk_is_drawn_when_it_is_its_own_and_never_drawn_twice() {
        let head = "mem_total 26377\nmem_used 900\n";

        let two = parse_resources(&format!(
            "{head}disk_dev overlay\ndisk_total 60168\ndisk_used 20986\n\
             images_dev /dev/vdd\nimages_total 50089\nimages_used 45433\n"
        ))
        .unwrap();
        assert_eq!((two.disk_used, two.images_used), (20986, 45433));

        // One filesystem answering both questions: the boxes' gauge already counts these bytes.
        let one = parse_resources(&format!(
            "{head}disk_dev overlay\ndisk_total 60168\ndisk_used 20986\n\
             images_dev overlay\nimages_total 60168\nimages_used 20986\n"
        ))
        .unwrap();
        assert_eq!(one.disk_used, 20986);
        assert_eq!(
            (one.images_total, one.images_used),
            (0, 0),
            "a zero denominator is how the strip drops a row, which is what one disk should draw"
        );
    }

    /// Once dockerd is pointed at [`CONTAINER_CGROUP`], the containers are counted *inside* `skein`
    /// — so reading both cgroups and adding them would draw the same memory twice and put the parts
    /// of the stacked bar past the whole they are drawn against, which is the exact fault the
    /// `anon`-instead-of-`current` fix was for.
    ///
    /// Both layouts are live at once during a migration, because the setting only takes effect at
    /// the next dockerd start: a fleet that has not cycled still has its containers in `/docker`.
    /// So the choice is made on whether the nested cgroup EXISTS, not on whether it holds anything.
    /// An empty one means a fleet that has cycled and has no container running — falling back then
    /// would label the sandbox's own daemons, which is all that is left in `/docker`, as Docker.
    #[test]
    fn containers_are_counted_once_wherever_dockerd_has_put_them() {
        let head = "mem_total 26377\nmem_used 2947\n";

        // Cycled: `skein` is boxes AND containers, and the nested figure separates them.
        let moved = parse_resources(&format!(
            "{head}skein 2100\nskein/containers 855\ndocker 106\n"
        ))
        .unwrap();
        assert_eq!(
            (moved.boxes, moved.docker),
            (1245, 855),
            "the containers' share belongs to them, not to the boxes that started them"
        );
        assert!(
            moved.boxes + moved.docker <= moved.mem_used,
            "counted twice, the parts of the bar exceed the whole"
        );

        // Cycled, nothing running: the empty nested cgroup is still the answer. Falling back here
        // would report the sandbox's own init and dockerd — all `/docker` holds now — as Docker.
        let idle = parse_resources(&format!(
            "{head}skein 1252\nskein/containers 0\ndocker 106\n"
        ))
        .unwrap();
        assert_eq!((idle.boxes, idle.docker), (1252, 0));

        // Not yet cycled: no nested cgroup at all, so the old home is where they still are.
        let legacy = parse_resources(&format!("{head}skein 1252\ndocker 855\n")).unwrap();
        assert_eq!((legacy.boxes, legacy.docker), (1252, 855));
    }

    /// One pool: dockerd's data moved onto the disk the boxes are on, so one number sizes both.
    ///
    /// Run for real, because what this is really testing is a file that stops dockerd booting when
    /// it is wrong — and `data-root` is the one key in it that can make every image and volume on
    /// the machine disappear from view. Three properties matter, and the last is the sharp one:
    ///
    /// 1. It lands, alongside the cgroup setting rather than instead of it.
    /// 2. It is idempotent, since this runs on every server start.
    /// 3. **Turning the setting off never removes it.** Removing the key would point dockerd back
    ///    at a disk it has not written to since, and every image and volume would vanish — none of
    ///    them deleted, all of them gone as far as anything asking Docker is concerned. Off must
    ///    mean "stop moving it", not "move it back".
    #[test]
    fn sharing_one_disk_with_docker_is_set_once_and_never_silently_undone() {
        let dir = tempdir();
        let path = std::path::Path::new(&dir).join("daemon.json");
        let run = |root: &str| -> std::process::Output {
            use std::io::Write;
            let mut child = std::process::Command::new("python3")
                .args(["-", &path.to_string_lossy(), CONTAINER_CGROUP, root])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("python3");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(DOCKER_CONFIG_PY.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        };
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };
        let pool = "/boxes/.docker";

        assert!(run(pool).status.success());
        assert_eq!(
            read()["data-root"],
            pool,
            "docker was not moved onto the pool"
        );
        assert_eq!(
            read()["cgroup-parent"],
            CONTAINER_CGROUP,
            "moving the data must not cost the memory ceiling"
        );

        let again = run(pool);
        assert!(again.status.success());
        assert!(
            String::from_utf8_lossy(&again.stdout).is_empty(),
            "a config already pointed at the pool is not news, and this runs every server start"
        );

        // The sharp one. Off means stop moving it, not move it back.
        assert!(run("").status.success());
        assert_eq!(
            read()["data-root"],
            pool,
            "turning the setting off pointed dockerd back at an empty disk, and every image and \
             volume on the fleet would read as gone"
        );

        // And it is added to a config someone else owns, not substituted for it.
        std::fs::write(&path, r#"{"dns":["1.1.1.1"]}"#).unwrap();
        assert!(run(pool).status.success());
        assert_eq!(read()["dns"][0], "1.1.1.1");
        assert_eq!(read()["data-root"], pool);
    }

    /// The pool directory must not read as a box.
    #[test]
    fn dockers_pool_is_hidden_from_the_box_enumeration_it_sits_beside() {
        let root = docker_data_root();
        assert!(
            root.rsplit('/').next().is_some_and(|n| n.starts_with('.')),
            "every box is enumerated with `{}/*/`, which a non-dot directory would match — and a \
             box with no repo is a thing `resize_fleet` refuses to proceed past: {root}",
            fleet_root()
        );
        assert!(
            root.starts_with(&fleet_root()),
            "the pool has to be on the boxes' own filesystem or it is not one pool: {root}"
        );
    }

    /// `/etc/docker/daemon.json` is a file that stops dockerd starting *at all* when it is wrong, so
    /// the failure being guarded against is a fleet with no Docker. Run for real rather than
    /// asserted about, because what matters is what Python does to the file, not what this file
    /// believes it does.
    #[test]
    fn pointing_dockerd_at_the_workload_cgroup_never_costs_an_existing_config() {
        let dir = tempdir();
        let path = std::path::Path::new(&dir).join("daemon.json");
        // The empty third argument is Docker keeping its own disk — the shape every existing fleet
        // runs in, and the one this test has always been about.
        let run = || -> std::process::Output {
            use std::io::Write;
            let mut child = std::process::Command::new("python3")
                .args(["-", &path.to_string_lossy(), CONTAINER_CGROUP, ""])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("python3");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(DOCKER_CONFIG_PY.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        };
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };

        // No file is the normal case, not an error.
        assert!(run().status.success());
        assert_eq!(read()["cgroup-parent"], CONTAINER_CGROUP);

        // Settings someone else put there survive: this adds a key, it does not own the file.
        std::fs::write(&path, r#"{"log-driver":"json-file","dns":["1.1.1.1"]}"#).unwrap();
        assert!(run().status.success());
        assert_eq!(read()["log-driver"], "json-file");
        assert_eq!(read()["dns"][0], "1.1.1.1");
        assert_eq!(read()["cgroup-parent"], CONTAINER_CGROUP);

        // Idempotent — this runs on every server start.
        let again = run();
        assert!(again.status.success());
        assert!(
            String::from_utf8_lossy(&again.stdout).is_empty(),
            "a config already pointed at the right place is not news"
        );

        // And a file that cannot be parsed is LEFT ALONE. Replacing it with a valid file of our own
        // would take away whatever dockerd is currently running on; refusing costs only the ceiling.
        let broken = "{ this is not json";
        std::fs::write(&path, broken).unwrap();
        let refused = run();
        assert!(!refused.status.success());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            broken,
            "a config skein cannot read is one it must not overwrite"
        );
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("leaving it alone"),
            "and it has to say so, or the ceiling is silently absent"
        );
    }

    #[test]
    fn a_reply_that_names_no_memory_is_not_remembered_as_a_zeroed_machine() {
        // A sandbox mid-boot answers `sbx exec` successfully and prints nothing useful. Taking that
        // as an answer would park a machine of zero bytes behind the Gate for the next 30 seconds.
        assert!(parse_resources("").is_none());
        assert!(parse_resources("cpus 8\nload1 0.10\n").is_none());
        // Missing pieces of a real reply are fine — a fleet root on a filesystem `df` cannot see
        // loses the disk gauge, not the memory one.
        let partial = parse_resources("mem_total 4096\nmem_used 900\n").unwrap();
        assert_eq!((partial.mem_total, partial.disk_total), (4096, 0));
    }

    /// A resize copies the box, it does not rebuild it — so the archive has to be the whole box, and
    /// it has to land somewhere that outlives the VM being destroyed.
    #[test]
    fn a_resize_copies_the_whole_box_to_a_place_the_rebuild_cannot_reach() {
        // `box_state` reads $SKEIN_HOME, which is process-global: without the lock this races any
        // other test that points it somewhere, and fails for a reason having nothing to do with resize.
        let _g = env_lock();
        let archive = box_archive("web-main", "resize-x");

        // On the host, under the box's own state directory — mounted into the sandbox precisely so
        // it survives one. A copy written anywhere under the fleet root would die with the very
        // thing it exists to outlive.
        assert!(
            archive.starts_with(&box_state("web-main")),
            "the copy belongs beside the box's other durable host state: {archive}"
        );
        assert!(
            !archive.starts_with(&fleet_root()),
            "never under {}, which `sbx rm -f` destroys",
            fleet_root()
        );

        let script = archive_script("web-main", &archive);
        // The whole box, `/tmp` and all: `tar -C <root> … .` rather than naming `tree` and `home`,
        // so a resize is invisible to whatever the agent had half-finished in scratch space.
        assert!(
            script.contains(&format!("tar -C {} ", sh_quote(&box_root("web-main")))),
            "the archive is taken from the box root, whole: {script}"
        );
        // The one exclusion that matters. `anchor.pid` names a process in the VM about to be
        // destroyed; restored, it would have the box claim a namespace that was never recreated.
        // Sockets need no exclusion — tar skips them and warns, which is right for a tmux socket
        // whose server is about to die, and `--warning=no-file-ignored` keeps that off the console.
        assert!(
            script.contains("--exclude=./anchor.pid"),
            "a stale anchor pid must not survive the rebuild: {script}"
        );
        assert!(
            !script.contains("--exclude=./tmp") && !script.contains("--exclude=./home"),
            "nothing else is excluded — an exact copy is the point: {script}"
        );
    }

    /// An archive is the size of the box, so keeping them is how a resize fills the host: measured,
    /// 16 GiB of boxes against 61 GiB free is two resizes before the Mac is full. It must go once
    /// its bytes are back — and *only* then, or a failed restore would delete the only copy.
    #[test]
    fn the_copy_is_deleted_once_it_is_back_and_never_before() {
        let archive = box_archive("web-main", "resize-x");
        let script = restore_script("web-main", &archive);

        let (Some(extract), Some(delete)) = (script.find("tar -C"), script.find("rm -f")) else {
            panic!("a restore must both extract and delete: {script}");
        };
        assert!(
            extract < delete,
            "the copy is deleted after the extraction, never before: {script}"
        );
        assert!(
            script.starts_with("set -e;"),
            "and only if the extraction succeeded — without `set -e` a failed tar still reaches \
             the rm, which would delete the only copy of a box that did not come back: {script}"
        );
    }

    /// A box holds files its own user cannot read, so both halves of the copy run as root.
    ///
    /// `tar` exits non-zero on the first file it cannot open, and under the `set -e` that makes the
    /// delete safe, that aborts the copy — so one unreadable file in one box refuses the whole
    /// fleet's resize. Which is exactly what happened: this project's own test suite had left 6,494
    /// directories under a box's `/tmp`, each holding one file at mode 000, and a resize of eight
    /// boxes stopped on the seventh with six already copied out.
    ///
    /// Root on the way back too, or the extract either fails on those same modes or quietly hands
    /// every file to whoever ran the resize — and ownership surviving is what makes this a copy
    /// rather than a rebuild.
    #[test]
    fn the_copy_runs_as_root_at_both_ends_because_a_box_is_not_all_readable_by_one_user() {
        let archive = box_archive("web-main", "resize-x");
        let out = archive_script("web-main", &archive);
        let back = restore_script("web-main", &archive);

        assert!(
            out.contains("sudo tar -C"),
            "an unreadable file anywhere in the box would abort the resize: {out}"
        );
        assert!(
            back.contains("sudo tar -C"),
            "the archive holds owners and modes an unprivileged extract cannot restore: {back}"
        );
        // Root wrote it, so the rest of the run — `du` here, the host reading it later — would
        // otherwise be touching a file it does not own.
        assert!(
            out.contains("sudo chown"),
            "an archive left owned by root is one the invoking user cannot clean up: {out}"
        );
        // Never `--ignore-failed-read`: that trades a resize that refused to start for a box
        // restored short of its own contents, with nobody told which files went missing.
        for script in [&out, &back] {
            assert!(
                !script.contains("ignore-failed-read"),
                "a copy that silently drops what it could not read is worse than one that stops: \
                 {script}"
            );
        }
    }

    /// A resize puts the fleet back as it found it. Starting every box with a placement record woke
    /// every stale box on the board, which is not the same fleet.
    #[test]
    fn a_resize_restores_every_box_but_only_restarts_the_ones_that_were_running() {
        let swept = std::collections::HashMap::from([
            ("busy".to_string(), true),
            ("idle".to_string(), false),
        ]);
        assert!(should_come_back(&swept, "busy"));
        assert!(!should_come_back(&swept, "idle"));
        // A box the sweep never mentioned has no session to have been in.
        assert!(!should_come_back(&swept, "unheard-of"));

        // But an empty sweep is "cannot tell", not "nothing was running" — and being wrong that way
        // round loses a box rather than merely waking one.
        let blind = std::collections::HashMap::new();
        assert!(should_come_back(&blind, "busy"));
        assert!(should_come_back(&blind, "idle"));
    }

    // The layout is load-bearing rather than cosmetic: box-session.sh binds the box's own /tmp and
    // $HOME over the sandbox's, so anything skein must read from OUTSIDE the box — the tmux socket
    // and the anchor pid — has to live somewhere neither bind covers.
    #[test]
    fn a_boxs_paths_avoid_everything_its_namespace_binds_over() {
        for path in [
            box_root("web-main"),
            box_sock("web-main"),
            box_pidfile("web-main"),
            box_session_path(),
        ] {
            assert!(path.starts_with("/boxes/"), "{path} escaped the layout");
            assert!(
                !path.starts_with("/tmp/"),
                "{path} is under the private /tmp"
            );
            assert!(!path.contains("/home/"), "{path} is under a private HOME");
        }
        assert_eq!(box_sock("web-main"), "/boxes/web-main/session.sock");
        assert_eq!(box_pidfile("web-main"), "/boxes/web-main/anchor.pid");
    }

    #[test]
    fn a_repos_store_is_reachable_at_the_same_path_inside_the_fleet_sandbox() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let workspace = fleet_workspace();
        let store = home.join("repos/web/store/.claude");
        assert!(
            store.starts_with(&workspace),
            "{} must sit under the mounted workspace {workspace}",
            store.display()
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // The sandbox is agentless and mounts the store parent, not any one repo — the two properties
    // that let it host boxes from every repo without being recreated when one is added.
    #[test]
    fn the_fleet_sandbox_is_agentless_and_mounts_the_store_parent() {
        let _g = env_lock();
        std::env::set_var("SKEIN_HOME", tempdir());
        let argv = create_argv("skein-fleet", &["/h/.skein/repos".to_string()]);
        assert_eq!(&argv[..3], ["create", "--name", "skein-fleet"]);
        assert_eq!(
            &argv[argv.len() - 2..],
            ["shell", "/h/.skein/repos"],
            "agentless, and the workspace is the store parent"
        );
        // Both are ceilings the boxes SHARE rather than one reservation each, which is what makes
        // them safe to set generously — and why a default is better here than deferring to sbx's.
        let flags = argv.join(" ");
        assert!(flags.contains("-m 26g"), "{flags}");
        assert!(
            flags.contains("--cpus "),
            "CPUs default to every host core but one, so the host keeps answering: {flags}"
        );
        assert!(
            !argv.contains(&"--clone".to_string()),
            "the sandbox is not a checkout; boxes clone from the remote onto VM-local disk"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // A repo skein manages keeps its work clone and its store under the one mounted workspace, and
    // that is the case the design was built around. A repo ADOPTED in place — or pointed at a store
    // the user already had, which is how this very project is registered — sits anywhere on the host.
    // Missing that mount does not fail loudly: the clone succeeds, the session starts, and the box
    // comes up with no store to link, no hooks, and no probe, looking entirely healthy.
    #[test]
    fn a_repo_that_lives_outside_the_workspace_is_still_mounted() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let managed = home.join("repos/web");
        let mounts = {
            let workspace = fleet_workspace();
            // Managed: both paths are already covered by the workspace, so neither is mounted twice.
            assert!(under(
                &managed.join("store/.claude").to_string_lossy(),
                &workspace
            ));
            assert!(under(&managed.join("work").to_string_lossy(), &workspace));
            // Adopted: outside it, so it must be named explicitly.
            assert!(!under("/Users/y/dev/skein-shared/.claude", &workspace));
            vec![workspace]
        };
        assert_eq!(mounts.len(), 1, "the workspace is always mounted first");
        std::env::remove_var("SKEIN_HOME");
    }

    // A mis-parsed ceiling is worse than no ceiling: it would silently cap every box at a number
    // nobody chose, and the symptom is builds dying with no explanation. So an unreadable size
    // yields None and the box runs uncapped-but-loud, rather than capped-and-wrong.
    #[test]
    fn a_memory_size_is_read_or_refused_never_guessed() {
        assert_eq!(parse_mib("26g"), Some(26624));
        assert_eq!(parse_mib(" 26G "), Some(26624));
        assert_eq!(
            parse_mib("26gi"),
            Some(26624),
            "the i suffix is the same size"
        );
        assert_eq!(parse_mib("512m"), Some(512));
        assert_eq!(
            parse_mib("2097152"),
            Some(2),
            "a bare number is bytes, as sbx reads it"
        );
        for bad in ["", "lots", "26 gigs", "g", "-4g"] {
            assert_eq!(parse_mib(bad), None, "{bad:?} must not parse to a number");
        }
    }

    // The ceiling exists so ONE runaway box cannot take the fleet down with it. That means max sits
    // below the fleet total (or it protects nothing) and high sits below max (or the kernel kills
    /// The fleet's disk is one shared filesystem, and sbx takes its size from the environment rather
    /// than from `sbx create`'s argv — so a knob that only reached the argv would set nothing at all.
    #[test]
    fn the_fleets_disk_size_travels_in_the_environment_not_the_argv() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        save_config(&Config::default()).unwrap();
        assert!(
            create_env().is_empty(),
            "unset must leave sbx on its own default rather than pinning one skein invented"
        );

        save_config(&Config {
            fleet_disk: "60g".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            create_env(),
            vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), "60g".to_string())]
        );
        let argv = create_argv("skein-fleet", &[]);
        assert!(
            !argv.iter().any(|a| a.contains("60g")),
            "sbx create has no disk flag; putting one in the argv would be rejected: {argv:?}"
        );
        // Restored, or the next test to take `env_lock` inherits a SKEIN_HOME naming a
        // directory this test's guard has already removed — and writes through it, which
        // recreates the tree as a leak nobody owns.
        std::env::remove_var("SKEIN_HOME");
    }

    // the box instead of throttling it, turning a slow build into a lost turn).
    #[test]
    fn a_boxs_ceiling_protects_the_fleet_and_throttles_before_it_kills() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Config::default()
        })
        .unwrap();
        let spec = box_limits();
        let get = |k: &str| -> u64 {
            let raw = spec
                .split(',')
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .unwrap_or_else(|| panic!("{k} missing from {spec}"));
            parse_mib(raw).unwrap()
        };
        let (max, high) = (get("max"), get("high"));
        let share = memory_plan().unwrap().boxes;
        assert!(
            max < share,
            "a cap at or above what all the boxes share protects nothing: {spec}"
        );
        assert!(high < max, "high must throttle before max kills: {spec}");
        assert!(
            max > share / 2,
            "a cap this tight makes a normal build fail; the point is one box CAN be big: {spec}"
        );
        assert!(
            spec.contains("pids="),
            "a fork bomb in one box starves every other: {spec}"
        );
        // CPU is deliberately absent — see box_limits.
        assert!(
            !spec.contains("cpu"),
            "capping CPU idles cores while a box waits, which is the waste this design ends: {spec}"
        );

        // An explicit value always wins over the derivation.
        save_config(&Config {
            fleet_memory: "26g".into(),
            box_memory_max: "4g".into(),
            ..Config::default()
        })
        .unwrap();
        assert!(box_limits().contains("max=4g"), "{}", box_limits());
        std::env::remove_var("SKEIN_HOME");
    }

    /// The invariant a per-box ceiling never expressed and could not: what everything adds up to.
    ///
    /// Boxes were capped at 70% of the VM *each* with nothing capping their sum, and the sandbox's
    /// Docker daemon — where a box's `docker build` actually runs — was capped at nothing at all.
    /// Two busy boxes, or one docker-heavy one, could reach the VM's memory; with no swap that is
    /// the global OOM killer choosing a victim by badness rather than by blame, and what it kills
    /// is as readily the thing that answers the host as the build that caused it. That is the
    /// sandbox "not responding" until someone cycles it.
    #[test]
    fn every_claim_on_the_sandbox_together_leaves_it_room_to_answer() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        for total in ["4g", "8g", "26g", "64g"] {
            save_config(&Config {
                fleet_memory: total.into(),
                ..Config::default()
            })
            .unwrap();
            let total_mib = parse_mib(total).unwrap();
            let plan = memory_plan().unwrap();
            assert_eq!(
                plan.boxes + plan.plumbing + plan.reserve,
                total_mib,
                "the shares must account for the whole VM at {total}"
            );
            // Measured at 574 MiB outside both cgroups on a live 26 GiB fleet. A gigabyte is the
            // floor because what it covers barely scales with the size of the VM.
            assert!(
                plan.reserve >= (1024).min(total_mib / 2),
                "the VM's own services are what stop answering first at {total}"
            );

            // The spec the launcher applies has to name both cgroups, but it caps only one of them.
            let spec = fleet_limits();
            let pair = |cgroup: &str| -> &str {
                spec.split(',')
                    .find_map(|p| p.strip_prefix(&format!("{cgroup}=")))
                    .unwrap_or_else(|| panic!("{cgroup} missing from {spec}"))
            };
            let (boxes_max, boxes_high) = pair("skein").split_once('/').expect("max/high");
            let (boxes_max, boxes_high) = (
                parse_mib(boxes_max).unwrap(),
                parse_mib(boxes_high).unwrap(),
            );
            assert_eq!(boxes_max, plan.boxes);
            assert!(
                boxes_high < boxes_max,
                "throttle before killing, the same way a box does: {spec}"
            );
            // One ceiling over the whole workload — the boxes and the containers they start share a
            // pool now rather than each being handed a slice. What it must still leave untouched is
            // the plumbing and the reserve, because those are what answers the host: the merge is
            // between the two workload shares, never into the sandbox's own.
            assert!(
                boxes_max + plan.plumbing < total_mib,
                "the workload's ceiling has to leave the sandbox its own share: {spec}"
            );
            assert!(
                total_mib - boxes_max >= plan.reserve,
                "the reserve survives the merge, or the VM has nothing to answer with: {spec}"
            );
            // Docker is named in order to be left uncapped, and `max` is the only value that says
            // so — a number here throttles the sandbox's own init and socat, which share that
            // cgroup, and hangs `sbx exec` while established streams keep flowing. Withholding the
            // ceiling has to be written rather than omitted, or a fleet an older skein capped keeps
            // that cap for as long as it lives.
            assert_eq!(
                pair("docker"),
                "max/max",
                "a bounded docker cgroup throttles the machinery that answers sbx: {spec}"
            );
            // And what they are a share OF, so the sandbox can check the share against itself.
            assert!(
                spec.starts_with(&format!("total={total_mib}M,")),
                "without the total, a sandbox smaller than the config says gets ceilings that \
                 cannot bound it: {spec}"
            );
        }
        std::env::remove_var("SKEIN_HOME");
    }

    /// The gap between what skein is *configured* for and what the sandbox actually got. sbx fixes
    /// a sandbox's memory when it is created, so editing Fleet memory without rebuilding leaves the
    /// config describing a VM that does not exist — and ceilings worked out for a machine twice the
    /// real size bound nothing at all. Run against the real launcher, with a fake cgroup tree and a
    /// fake `/proc/meminfo`, because the scaling lives in shell and an assertion about the Rust
    /// half would prove nothing about it.
    #[test]
    fn ceilings_shrink_to_the_memory_the_sandbox_really_has() {
        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        for cgroup in ["skein", "docker"] {
            std::fs::create_dir_all(root.join("cgroup").join(cgroup)).unwrap();
        }
        // A sandbox with MORE than skein was told about keeps the ceilings as computed: the reserve
        // is deliberate, and a surplus nobody configured is not an invitation to spend it.
        std::fs::write(root.join("meminfo"), "MemTotal:       41943040 kB\n").unwrap();
        // The launcher's own function, with `sudo` and the cgroup root redirected at the fixture.
        let harness = format!(
            // Drops the `sh -c <script> _` the real call passes, leaving the value and the path.
            "sudo() {{ shift 4; sh -c 'echo \"$1\" > \"$2\"' _ \"$1\" \"$2\"; }}\n\
             {body}\n\
             fleet_limits='total=26624M,skein=15975M/14377M,docker=max/max'\n\
             apply_fleet_ceilings\n",
            body = BOX_SESSION_SH
                .lines()
                .skip_while(|l| !l.starts_with("apply_fleet_ceilings() {"))
                .take_while(|l| *l != "}")
                .collect::<Vec<_>>()
                .join("\n")
                .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
                .replace("/proc/meminfo", &root.join("meminfo").to_string_lossy())
                + "\n}",
        );
        let run = || -> String {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&harness)
                .output()
                .expect("run the launcher's ceiling logic");
            String::from_utf8_lossy(&out.stderr).into_owned()
        };
        let read = |cgroup: &str, file: &str| -> String {
            std::fs::read_to_string(root.join("cgroup").join(cgroup).join(file))
                .unwrap_or_else(|e| panic!("{cgroup}/{file}: {e}"))
                .trim()
                .to_string()
        };
        let quiet = run();
        assert_eq!(read("skein", "memory.max"), "15975M");
        // Written, not left alone: this is how a fleet an older skein capped gets uncapped.
        assert_eq!(read("docker", "memory.max"), "max");
        assert_eq!(read("docker", "memory.high"), "max");
        assert!(
            !quiet.contains("scaling"),
            "nothing to scale, so nothing to say: {quiet}"
        );

        // Half the configured size — every ceiling comes out halved, and says so.
        std::fs::write(root.join("meminfo"), "MemTotal:       13631488 kB\n").unwrap();
        let noisy = run();
        assert_eq!(read("skein", "memory.max"), "7987M", "half of 15975");
        assert_eq!(read("skein", "memory.high"), "7188M");
        // Half of no ceiling is still no ceiling — scaling must not turn `max` into a number.
        assert_eq!(read("docker", "memory.max"), "max");
        assert!(
            noisy.contains("not the 26624M"),
            "a sandbox smaller than skein was told must say so, not silently differ: {noisy}"
        );
    }

    /// A ceiling this launcher cannot read costs the ceiling, not the box.
    ///
    /// The sandbox keeps whichever `box-session.sh` it was last given, so the launcher applying a
    /// spec is routinely OLDER than the skein that sent it. When that gap first opened it took the
    /// whole fleet down: skein began sending `docker=max/max`, the installed launcher fed `max` to
    /// `$(( ))`, and `set -u` aborted the shell before it reached tmux — so every box stopped
    /// starting and each reconnect reported `nsenter: cannot open /proc/<pid>/ns/user`, an error
    /// about namespaces for a fleet that needed a file copied.
    ///
    /// `heal_fleet` narrows that window; it cannot close it, because the next unfamiliar token will
    /// reach some sandbox before the launcher that understands it does. So the launcher has to
    /// degrade rather than die, and this asserts the three properties that means: an unreadable
    /// cgroup is skipped and says so, a readable one beside it is still applied, and neither half
    /// of an unreadable pair is written — a `high` with no `max` above it is the throttle-forever
    /// shape these two limits exist together to avoid.
    ///
    /// Run against the real launcher for the same reason as the test above: the logic is shell, and
    /// a Rust assertion about it would prove nothing.
    #[test]
    fn an_unreadable_ceiling_is_skipped_rather_than_fatal() {
        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        for cgroup in ["skein", "docker"] {
            std::fs::create_dir_all(root.join("cgroup").join(cgroup)).unwrap();
        }
        std::fs::write(root.join("meminfo"), "MemTotal:       27262976 kB\n").unwrap();
        let body = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("apply_fleet_ceilings() {"))
            .take_while(|l| *l != "}")
            .collect::<Vec<_>>()
            .join("\n")
            .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
            .replace("/proc/meminfo", &root.join("meminfo").to_string_lossy())
            + "\n}";
        // `set -uo pipefail` as the real launcher has it — without it this proves nothing, since
        // the failure being guarded against is precisely what `set -u` does to an unread word.
        let run = |spec: &str| -> (String, bool) {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "set -uo pipefail\n\
                     sudo() {{ shift 4; sh -c 'echo \"$1\" > \"$2\"' _ \"$1\" \"$2\"; }}\n\
                     {body}\n\
                     fleet_limits='total=26624M,skein=15975M/14377M,{spec}'\n\
                     apply_fleet_ceilings\n\
                     echo REACHED-THE-END\n"
                ))
                .output()
                .expect("run the launcher's ceiling logic");
            (
                String::from_utf8_lossy(&out.stderr).into_owned(),
                String::from_utf8_lossy(&out.stdout).contains("REACHED-THE-END"),
            )
        };
        let wrote = |cgroup: &str, file: &str| -> Option<String> {
            std::fs::read_to_string(root.join("cgroup").join(cgroup).join(file))
                .ok()
                .map(|s| s.trim().to_string())
        };

        // A word no launcher of this vintage knows — `max` was one of these once.
        let (said, finished) = run("docker=somethingnew/somethingnew");
        assert!(
            finished,
            "the launcher died on a ceiling it could not read, so no box in this fleet starts"
        );
        assert!(
            said.contains("somethingnew"),
            "a skipped ceiling has to name itself, or the fleet runs unbounded and silently: {said}"
        );
        assert_eq!(
            wrote("docker", "memory.max"),
            None,
            "a ceiling that could not be read must leave the cgroup as it found it"
        );
        assert_eq!(
            wrote("skein", "memory.max").as_deref(),
            Some("15975M"),
            "one unreadable cgroup must not cost the others theirs — skein is the ceiling that \
             actually bounds the workload"
        );

        // Half-readable is the dangerous one: `high` alone throttles against a ceiling that is not
        // there, which is the wedge that started all of this.
        for cgroup in ["skein", "docker"] {
            for file in ["memory.max", "memory.high"] {
                let _ = std::fs::remove_file(root.join("cgroup").join(cgroup).join(file));
            }
        }
        let (_, finished) = run("docker=notasize/7188M");
        assert!(finished, "still not fatal when only one half is unreadable");
        assert_eq!(
            wrote("docker", "memory.high"),
            None,
            "a `high` written without the `max` above it is the throttle-forever shape: both halves \
             are read before either is written"
        );
    }

    /// The launcher is the only thing that runs on every box start, which is what the Docker
    /// ceiling needs: dockerd rebuilds its cgroup from scratch when the sandbox cycles and takes
    /// any limit written on it along with it.
    #[test]
    fn the_launcher_is_handed_the_shared_ceilings_as_well_as_the_boxs_own() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Config::default()
        })
        .unwrap();
        let script = session_script("web-main", "skein-agent", "claude");
        assert!(
            script.contains(&fleet_limits()),
            "the box would start under a ceiling nobody had applied: {script}"
        );
        assert!(
            script.contains(&box_limits()),
            "and its own ceiling still has to get there: {script}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // The transcript must NOT be carried, and the HOME must be named absolutely. Both used to depend
    // on a `transcript_is_vm_local` flag that only a migration ever set true; with the per-VM model
    // gone there is one shape left, and this pins it so a rebuild does not start copying a host-bound
    // directory out to the store and back.
    #[test]
    fn a_rebuilt_box_leaves_its_host_bound_conversation_where_it_is() {
        let already_in = agent_state_tar("/snap", "/boxes/web-main/home");
        assert!(
            !already_in.contains(".claude/projects") && !already_in.contains(".codex/sessions"),
            "the transcript is host-bound already; copying it is pure virtiofs waste: {already_in}"
        );
        assert!(
            already_in.contains(".claude/todos") && already_in.contains(".claude.json"),
            "the state that is NOT host-bound still has to travel: {already_in}"
        );

        // A fleet box's HOME is named absolutely, because the tar runs in the SANDBOX rather than
        // inside the box — that is what lets a box whose session has died still have its work saved.
        assert!(
            already_in.contains("/boxes/web-main/home") && !already_in.contains("$HOME"),
            "a dead box's private HOME must still be addressable: {already_in}"
        );

        // The allowlist rule — this lands in host-side shared data, so a credential must never be in it.
        assert!(
            !already_in.contains("credentials"),
            "a credential would be copied into the repo store: {already_in}"
        );
    }

    // A migrated box works in a new directory, and the runtimes key a transcript by that directory.
    // Without this the conversation arrives intact and invisible.
    #[test]
    fn a_migrated_boxs_conversation_follows_it_to_the_new_checkout() {
        use std::{env, fs};
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        assert_eq!(
            transcript_slug("/Users/you/.skein/repos/sync/work"),
            "-Users-you--skein-repos-sync-work",
            "the slug rule is read off a real box, not invented"
        );

        let projects = std::path::PathBuf::from(box_state("example-box-9")).join("claude-projects");
        let old = projects.join("-Users-you--skein-repos-sync-work");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("a.jsonl"), "{}").unwrap();
        fs::write(old.join("b.jsonl"), "{}").unwrap();
        // an empty slug from a one-off command elsewhere must not win
        fs::create_dir_all(projects.join("-tmp")).unwrap();

        assert_eq!(realign_transcript("example-box-9").unwrap(), 2);
        let now = projects.join("-boxes-example-box-9-tree");
        assert!(now.join("a.jsonl").exists() && now.join("b.jsonl").exists());
        assert!(
            old.join("a.jsonl").exists(),
            "copied, not moved — the old directory is the record of where it happened"
        );

        // A box with its own conversation must never have another merged into it.
        fs::write(now.join("own.jsonl"), "{}").unwrap();
        fs::write(old.join("c.jsonl"), "{}").unwrap();
        assert_eq!(realign_transcript("example-box-9").unwrap(), 0);
        assert!(!now.join("c.jsonl").exists());

        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_HOME");
    }

    /// Which sibling is "the conversation" is a question about BYTES, not files.
    ///
    /// Read off lattice-feat-design-codex-claude, whose real history was 2 files totalling 87MB
    /// while the scratchpad slugs beside it held 6 small ones each. Ranking by file count picked a
    /// scratchpad — the one outcome this function exists to prevent, arrived at silently.
    #[test]
    fn the_conversation_carried_across_is_the_biggest_one_not_the_busiest_directory() {
        use std::{env, fs};
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", "/boxes");

        let projects = std::path::PathBuf::from(box_state("lattice-main")).join("claude-projects");
        let real = projects.join("-Users-you-work-lattice");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("long.jsonl"), vec![b'x'; 60_000]).unwrap();
        fs::write(real.join("second.jsonl"), vec![b'x'; 20_000]).unwrap();
        // More files, a fraction of the content — a scratchpad, not a conversation.
        let junk = projects.join("-tmp-scratchpad");
        fs::create_dir_all(&junk).unwrap();
        for i in 0..6 {
            fs::write(junk.join(format!("{i}.jsonl")), vec![b'x'; 500]).unwrap();
        }

        assert_eq!(realign_transcript("lattice-main").unwrap(), 2);
        let now = projects.join("-boxes-lattice-main-tree");
        assert!(
            now.join("long.jsonl").exists(),
            "the 80KB conversation, not the 3KB spread over six files"
        );
        assert!(
            !now.join("0.jsonl").exists(),
            "the scratchpad must not arrive"
        );

        // `claude --continue` opens the most recently modified transcript, so a copy stamped `now`
        // would make whichever file landed last look like the newest conversation.
        let src = fs::metadata(real.join("long.jsonl"))
            .unwrap()
            .modified()
            .unwrap();
        let dst = fs::metadata(now.join("long.jsonl"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            src, dst,
            "the copy must keep the original's modification time"
        );

        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_HOME");
    }

    /// A box pushes to the repo's remote. For a repo adopted in place the clone comes from the
    /// host's checkout — fast, and it carries commits the host has not pushed — but `git clone
    /// <path>` names that path `origin`, and a box whose origin is a directory on someone's laptop
    /// is a box that cannot open a PR. It also fails outright the moment the box works on the branch
    /// the host has checked out, which is the normal state of affairs for skein's own box.
    #[test]
    fn a_box_cloned_from_the_host_still_pushes_to_the_remote() {
        let script = clone_script(
            "web-main",
            "/Users/you/work/web",
            "main",
            "feat/auth",
            "git@github.com:o/r.git",
        );
        assert!(
            script.contains("git clone --branch 'main' '/Users/you/work/web'"),
            "still cloned locally — the point is the push target, not the fetch: {script}"
        );
        assert!(
            script.contains("git remote set-url origin 'git@github.com:o/r.git'"),
            "origin must be the remote the repo actually pushes to: {script}"
        );
        assert!(
            script.contains("git remote add local '/Users/you/work/web'"),
            "the host clone stays reachable by name; re-pointing origin must not lose it: {script}"
        );
        let after_checkout = script.split("git checkout -B").nth(1).unwrap_or_default();
        assert!(
            after_checkout.contains("set-url"),
            "re-pointing before the branch exists would leave a box with no checkout: {script}"
        );

        // A URL source already clones from where it pushes; touching origin there could only break it.
        let direct = clone_script(
            "web-main",
            "git@github.com:o/r.git",
            "main",
            "feat/auth",
            "",
        );
        assert!(
            !direct.contains("remote set-url") && !direct.contains("remote add"),
            "nothing to repair when the source is the remote: {direct}"
        );

        // And the same repair for the boxes already on disk, which will never be re-cloned.
        let repair =
            origin_repair_script("web-main", "/Users/you/work/web", "git@github.com:o/r.git");
        assert!(
            repair.contains("[ \"$cur\" = '/Users/you/work/web' ] || exit 0"),
            "only the URL skein put there may be rewritten; a hand-set origin is someone's \
             deliberate choice: {repair}"
        );
        assert!(repair.contains("git remote set-url origin 'git@github.com:o/r.git'"));
    }

    /// The hosts to trust come from where boxes PUSH as well as where they clone. An adopted repo
    /// has a path for a source and its remote only on `origin` — so reading `source` alone left the
    /// box with no `known_hosts` entry for the one host it actually talks to.
    #[test]
    fn host_trust_covers_the_remote_a_box_pushes_to() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let work = home.join("adopted");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .expect("git");
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@gitlab.example.com:o/r.git"]);

        save_repos(&[Repo {
            id: "adopted".into(),
            // Adopted in place: the source is the checkout, not a URL.
            source: work.to_string_lossy().into_owned(),
            work: work.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        assert_eq!(
            ssh_hosts(),
            vec!["gitlab.example.com".to_string()],
            "a repo whose only SSH URL is on origin still needs its host trusted"
        );
        assert!(
            known_hosts_script(&ssh_hosts()).contains("StrictHostKeyChecking=accept-new"),
            "trust an unknown host once; still refuse a CHANGED one"
        );
        // Restored, or the next test to take `env_lock` inherits a SKEIN_HOME naming a
        // directory this test's guard has already removed — and writes through it, which
        // recreates the tree as a leak nobody owns.
        std::env::remove_var("SKEIN_HOME");
    }

    /// A box has a private HOME and a freshly cloned tree, so it starts with no committer at all —
    /// and finds out at `git commit`, which is after the work, not before it.
    #[test]
    fn a_box_is_told_who_it_commits_as_before_it_needs_to_know() {
        let _g = env_lock();
        let home = tempdir();
        // Deliberately NOT setting SKEIN_FLEET_ROOT: nothing here reads it, and a test that sets a
        // global other tests read is a test that breaks them from another thread.
        std::env::set_var("SKEIN_HOME", &home);
        let work = home.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .expect("git");
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Host Default"]);
        git(&["config", "user.email", "host@example.com"]);
        let repo = Repo {
            id: "web".into(),
            source: work.to_string_lossy().into_owned(),
            work: work.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };

        save_config(&Config::default()).unwrap();
        assert_eq!(
            box_identity("web-main", &repo),
            ("Host Default".into(), "host@example.com".into()),
            "with nothing configured, the host clone already knows — asking the user would be a \
             question skein can answer itself"
        );

        save_config(&Config {
            git_name: "Fleet".into(),
            git_email: "fleet@example.com".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            box_identity("web-main", &repo).0,
            "Fleet",
            "the setting is the answer for every box that did not choose one"
        );

        set_box_identity("web-main", Some(("Client A", "a@client.example"))).unwrap();
        assert_eq!(
            box_identity("web-main", &repo),
            ("Client A".into(), "a@client.example".into()),
            "a box created on someone else's behalf commits as them"
        );
        assert_eq!(
            box_identity("web-other", &repo).0,
            "Fleet",
            "and only that box — its siblings keep the default"
        );

        set_box_identity("web-main", None).unwrap();
        assert_eq!(box_identity("web-main", &repo).0, "Fleet");

        // --global, because the checkout is re-cloned by every rebuild, resize and migration; and
        // never over an identity already set inside the box.
        let script = identity_script("Fleet", "fleet@example.com");
        assert!(script.contains("git config --global user.name 'Fleet'"));
        assert!(
            script.contains("--get user.name >/dev/null 2>&1 ||"),
            "only what is missing: an identity set in the box by hand is someone's choice: {script}"
        );
        assert!(
            identity_script("", "").is_empty(),
            "nothing configured and nothing on the host ⇒ nothing to run"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// Memory has a kernel ceiling per box; disk has one filesystem and no ceiling at all. So the
    /// allowance is a number skein measures against — which is exactly what lets it change under a
    /// running box, and why it must never be described as a quota.
    #[test]
    fn a_boxs_disk_allowance_is_its_own_and_changes_without_a_restart() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        save_config(&Config::default()).unwrap();
        assert_eq!(
            box_disk_limit("web-main"),
            Some(10 * 1024),
            "10g by default — a fleet where every box may take the whole disk has no default at all"
        );

        set_box_disk_limit("web-main", Some("40g")).unwrap();
        assert_eq!(box_disk_limit("web-main"), Some(40 * 1024));
        assert_eq!(
            box_disk_limit("web-other"),
            Some(10 * 1024),
            "one box's allowance is not a decision about the rest"
        );

        // `none` is a decision — this box may use the whole disk — and distinct from having no
        // opinion, which is what a cleared field means and inherits the default again.
        set_box_disk_limit("web-main", Some("none")).unwrap();
        assert_eq!(box_disk_limit("web-main"), None);
        set_box_disk_limit("web-main", Some("unlimited")).unwrap();
        assert_eq!(box_disk_limit("web-main"), None);
        set_box_disk_limit("web-main", None).unwrap();
        assert_eq!(box_disk_limit("web-main"), Some(10 * 1024));

        // A size that parses to nothing would read as "limited" and behave as "unlimited".
        assert!(set_box_disk_limit("web-main", Some("plenty")).is_err());
        assert_eq!(box_disk_limit("web-main"), Some(10 * 1024));

        save_config(&Config {
            box_disk_max: String::new(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            box_disk_limit("web-main"),
            None,
            "blank default ⇒ unlimited"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn one_mount_covers_everything_beneath_it() {
        assert!(under("/a/b", "/a"), "a child is covered");
        assert!(under("/a", "/a"), "the directory itself is covered");
        assert!(
            under("/a/b", "/a/"),
            "a trailing slash is not a different path"
        );
        assert!(
            !under("/ab", "/a"),
            "a prefix is not a parent — /ab would go unmounted while looking covered"
        );
    }

    // A box that already has a checkout is refused, not reused. Silently adopting the tree left by a
    // previous box of the same name would hand the new one someone else's uncommitted work.
    #[test]
    fn preparing_a_checkout_starts_from_the_remote_base_and_never_reuses_a_tree() {
        let script = clone_script(
            "web-main",
            "git@github.com:o/r.git",
            "main",
            "feat/auth",
            "",
        );
        // The quoting closes before `/.git`, which the shell concatenates back into one word.
        assert!(script.contains("if [ -e '/boxes/web-main/tree'/.git ]"));
        assert!(script.contains("exit 1"));
        assert!(
            script.contains("git clone --branch 'main' 'git@github.com:o/r.git'"),
            "from the remote at the base branch — the same base the diff is taken against"
        );
        assert!(
            script.contains("git checkout -B 'feat/auth'"),
            "a branch with a slash is one argument, not a path"
        );
        // A base the remote does not have must not be fatal. It cost a real migration: the old
        // sandbox was already stopped, the clone refused `--branch main` on a repo whose default is
        // `master`, and the box existed in neither place until someone woke the old sandbox by hand.
        assert!(
            script.contains("|| {") && script.matches("git clone").count() == 2,
            "a wrong base must fall back to the remote's default, not strand the box: {script}"
        );
        // And when skein could not learn the base at all, it asks the remote instead of guessing.
        let blind = clone_script("web-main", "git@github.com:o/r.git", "", "feat/auth", "");
        assert!(
            blind.contains("git clone 'git@github.com:o/r.git'") && !blind.contains("--branch"),
            "no base means let git use the remote's default: {blind}"
        );
    }

    // The snapshot carries what the remote does not have. `--all` copied every object the repo had
    // ever held into the store, over a mount several times slower than local disk: a box with a
    // 1.8 GB .git took a migration past its ten-minute budget copying history the remote already
    // had, and a resize would do that for every box at once.
    #[test]
    fn a_snapshot_bundles_the_unpushed_work_not_the_whole_history() {
        use std::fs;
        let dir = tempdir();
        let origin = dir.join("origin");
        let tree = dir.join("tree");
        let home = dir.join("home");
        fs::create_dir_all(&origin).unwrap();
        fs::create_dir_all(&home).unwrap();
        let sh = |cwd: &std::path::Path, script: &str| -> std::process::Output {
            std::process::Command::new("bash")
                .current_dir(cwd)
                .arg("-c")
                .arg(script)
                .env("HOME", &home)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
        };
        // History the remote already has: one large blob, the stand-in for a 1.8 GB .git.
        let out = sh(&origin, "git init -q -b main .");
        assert!(out.status.success(), "{out:?}");
        fs::write(origin.join("big.bin"), vec![7u8; 6 * 1024 * 1024]).unwrap();
        let out = sh(&origin, "git add -A && git commit -qm history");
        assert!(out.status.success(), "{out:?}");

        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), tree.display()),
        );
        assert!(out.status.success(), "{out:?}");
        // The only thing that would actually be lost: one small unpushed commit.
        fs::write(tree.join("work.txt"), "the unpushed work\n").unwrap();
        let out = sh(&tree, "git add -A && git commit -qm unpushed");
        assert!(out.status.success(), "{out:?}");

        let snapshot = dir.join("snap");
        let script = snapshot_script(
            &snapshot.to_string_lossy(),
            "demo-main",
            &home.to_string_lossy(),
        );
        let out = sh(&tree, &script);
        assert!(out.status.success(), "snapshot failed: {out:?}");

        let bundle = snapshot.join("repo.bundle");
        let size = fs::metadata(&bundle).unwrap().len();
        assert!(
            size < 1024 * 1024,
            "the bundle re-copied history the remote already has: {size} bytes"
        );
        // Small, and still complete: the unpushed commit is in there, and a fresh clone of the same
        // origin — which is exactly what the box is rebuilt from — can open it.
        let restored = dir.join("restored");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), restored.display()),
        );
        assert!(out.status.success(), "{out:?}");
        let out = sh(
            &restored,
            &format!(
                "git fetch -q {} 'refs/heads/*:refs/remotes/snapshot/*' && \
                 git checkout -q -B main refs/remotes/snapshot/main && cat work.txt",
                bundle.display()
            ),
        );
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("unpushed work"),
            "a thin bundle must still restore the work it was taken for: {out:?}"
        );

        // A box with nothing unpushed asks git for an empty bundle, which git refuses to write. The
        // fallback carries the tip alone — the restore needs *a* bundle, and the kit treats a
        // missing one as a failed snapshot.
        let clean = dir.join("clean");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), clean.display()),
        );
        assert!(out.status.success(), "{out:?}");
        let snapshot2 = dir.join("snap2");
        let out = sh(
            &clean,
            &snapshot_script(
                &snapshot2.to_string_lossy(),
                "demo-main",
                &home.to_string_lossy(),
            ),
        );
        assert!(
            out.status.success(),
            "snapshot of a clean box failed: {out:?}"
        );
        let size2 = fs::metadata(snapshot2.join("repo.bundle")).unwrap().len();
        assert!(
            size2 > 0,
            "the kit reads a missing bundle as a failed snapshot"
        );
        assert!(
            size2 < 1024 * 1024,
            "a clean box must not fall back to the whole history: {size2} bytes"
        );

        // The shape that actually broke a migration: the checked-out branch is fully pushed, but
        // ANOTHER local ref is not. `--not --remotes` drops the pushed branch's ref while the other
        // keeps the bundle non-empty — so it looked healthy and carried nothing the restore could
        // find, and the box's old sandbox had already been stopped by the time that surfaced.
        let out = sh(
            &clean,
            "git checkout -q -b side && git commit -q --allow-empty -m side && git checkout -q main",
        );
        assert!(out.status.success(), "{out:?}");
        let snapshot3 = dir.join("snap3");
        let out = sh(
            &clean,
            &snapshot_script(
                &snapshot3.to_string_lossy(),
                "demo-main",
                &home.to_string_lossy(),
            ),
        );
        assert!(out.status.success(), "{out:?}");
        let restored2 = dir.join("restored2");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), restored2.display()),
        );
        assert!(out.status.success(), "{out:?}");
        // The invariant the restore depends on: a ref it can actually check the branch out from.
        let bundle3 = snapshot3.join("repo.bundle");
        let out = sh(
            &restored2,
            &format!(
                "git fetch -q {b} 'refs/heads/*:refs/remotes/snapshot/*' 2>/dev/null && \
                 git rev-parse --verify -q refs/remotes/snapshot/main >/dev/null || \
                 git fetch -q {b} HEAD",
                b = bundle3.display()
            ),
        );
        assert!(
            out.status.success(),
            "the restore must find a ref for the box's branch: {out:?}"
        );
    }

    // Ignored files are work too — `.env`, `.envrc`, local dev config, and the box's own
    // `.skein/journal.md`. The sweep omitted every one of them, so a migrated box came back looking
    // complete and either failed at runtime or had forgotten what it was doing. What it must still
    // refuse is `node_modules/` and `target/`, which is why the rule is size rather than a list of
    // names that would be wrong for the next language.
    //
    // Runs the real script against a real repository: it is shell, and shell is where the bug was.
    #[test]
    fn a_snapshot_carries_ignored_config_but_not_the_build_output() {
        use std::fs;
        let dir = tempdir();
        let tree = dir.join("tree");
        let snapshot = dir.join("snap");
        let home = dir.join("home");
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&home).unwrap();
        let sh = |cwd: &std::path::Path, script: &str| -> std::process::Output {
            std::process::Command::new("bash")
                .current_dir(cwd)
                .arg("-c")
                .arg(script)
                .env("HOME", &home)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
        };
        fs::write(
            tree.join(".gitignore"),
            ".env\n.skein/\nnode_modules/\nbig.bin\n",
        )
        .unwrap();
        fs::write(tree.join("src.rs"), "fn main() {}").unwrap();
        let out = sh(
            &tree,
            "git init -q -b main . && git add -A && git commit -qm one",
        );
        assert!(out.status.success(), "{out:?}");

        // Ignored, and all of it work: local config and the box's own journal.
        fs::write(tree.join(".env"), "SECRET=1\n").unwrap();
        fs::create_dir_all(tree.join(".skein")).unwrap();
        fs::write(tree.join(".skein/journal.md"), "what I did\n").unwrap();
        // Ignored, and none of it work: reproducible output, too big to keep copying.
        fs::create_dir_all(tree.join("node_modules/pkg")).unwrap();
        for i in 0..2100 {
            fs::write(tree.join(format!("node_modules/pkg/f{i}")), "x").unwrap();
        }
        fs::write(tree.join("big.bin"), vec![0u8; 11 * 1024 * 1024]).unwrap();
        // Plain untracked files must still be carried, exactly as before.
        fs::write(tree.join("notes.txt"), "scratch\n").unwrap();

        // The three symlink shapes, which is where the rescue went wrong. A `--clone` box's `.env`
        // is a link into `/run/sandbox/source` — the host repo's bind mount — and carrying the LINK
        // hands the fleet box a pointer to a mount that does not exist there.
        use std::os::unix::fs::symlink;
        let outside = dir.join("host-repo");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("env.real"), "FROM_HOST=1\n").unwrap();
        symlink(outside.join("env.real"), tree.join(".env.host")).unwrap();
        symlink("notes.txt", tree.join("notes.link")).unwrap();
        symlink("/run/sandbox/source/.env.gone", tree.join(".env.dangling")).unwrap();
        fs::write(
            tree.join(".gitignore"),
            ".env\n.env.host\n.env.dangling\n.skein/\nnode_modules/\nbig.bin\n",
        )
        .unwrap();

        let script = snapshot_script(
            &snapshot.to_string_lossy(),
            "demo-main",
            &home.to_string_lossy(),
        );
        let out = sh(&tree, &script);
        assert!(out.status.success(), "snapshot failed: {out:?}");

        let listing = sh(&snapshot, "tar -tzf untracked.tgz");
        let carried = String::from_utf8_lossy(&listing.stdout);
        for want in [".env", ".skein/journal.md", "notes.txt"] {
            assert!(
                carried.lines().any(|l| l.trim_end_matches('/') == want),
                "{want} was left behind: {carried}"
            );
        }
        assert!(
            !carried.contains("node_modules"),
            "a dependency tree does not belong in the store: {carried}"
        );
        assert!(!carried.contains("big.bin"), "{carried}");

        // And it says so, rather than leaving them behind quietly.
        let skipped = fs::read_to_string(snapshot.join(SKIPPED_FILE)).unwrap();
        assert!(skipped.contains("node_modules/"), "{skipped}");
        assert!(skipped.contains("big.bin"), "{skipped}");

        // A link out of the tree is carried as its CONTENT: what it points at will not be there
        // after the move, and the content is the thing worth keeping.
        let unpacked = dir.join("unpacked");
        fs::create_dir_all(&unpacked).unwrap();
        let out = sh(
            &unpacked,
            &format!("tar -xzf {}/untracked.tgz", snapshot.display()),
        );
        assert!(out.status.success(), "{out:?}");
        assert!(
            !unpacked.join(".env.host").is_symlink(),
            "a link into a mount the fleet does not have is a dangling link there"
        );
        assert_eq!(
            fs::read_to_string(unpacked.join(".env.host")).unwrap(),
            "FROM_HOST=1\n"
        );
        // A link INSIDE the tree still means the same thing after the move, so it stays a link.
        assert!(
            unpacked.join("notes.link").is_symlink(),
            "an in-tree symlink must not be flattened into a copy"
        );
        // And one that already resolves to nothing is reported, not carried: there is nothing there.
        assert!(
            skipped.contains(".env.dangling"),
            "a broken link must be named, not silently dropped: {skipped}"
        );
        assert!(!unpacked.join(".env.dangling").exists());
    }

    // What counts as work worth carrying is one rule, used by the snapshot and by the sweep it calls.
    // Two copies of it would drift, and the drift would be silent — a box would come back missing a
    // file nobody noticed it had.
    #[test]
    fn the_snapshot_carries_exactly_what_the_sweep_says_is_worth_carrying() {
        let sweep = ignored_sweep("/snap", "list", "/snap/skipped");
        let snapshot = snapshot_script("/snap", "demo-main", "/home/agent");
        assert!(
            snapshot.contains("--others --ignored --exclude-standard --directory"),
            "the snapshot must sweep ignored files at all"
        );
        for rule in ["over 2000 files", "-gt 20480", "-gt 10240"] {
            assert!(sweep.contains(rule), "the sweep lost a rule: {rule}");
            assert!(snapshot.contains(rule), "the snapshot lost a rule: {rule}");
        }
    }

    // A box's turn state describes a SESSION, and the store it is written to outlives the box. So a
    // migrated box came up reading `ended` — the old sandbox's agent died on the way out, its
    // SessionEnd hook recorded that faithfully, and the new box inherited it and looked terminated
    // while sitting there alive. What the box did stays; what it is doing right now does not.
    #[test]
    fn a_new_session_does_not_inherit_the_previous_ones_turn_state() {
        use std::fs;
        let store_tmp = tempdir();
        let store = store_tmp.join("store").join(".claude");
        for dir in ["status", "sessions", "hook-log", "telemetry"] {
            fs::create_dir_all(store.join(dir)).unwrap();
        }
        let repo = Repo {
            id: "bridge".into(),
            source: String::new(),
            work: String::new(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        let write = |rel: &str, body: &str| fs::write(store.join(rel), body).unwrap();
        write(
            "status/bridge-main.json",
            r#"{"status":"ended","detail":"session ended: other"}"#,
        );
        write("status/bridge-main.agents", "2");
        write(
            "sessions/bridge-main.json",
            r#"{"lastMessage":"shipped it"}"#,
        );
        write("hook-log/bridge-main.jsonl", "{\"event\":\"ended\"}\n");
        write("telemetry/bridge-main.jsonl", "{\"total\":1}\n");
        // Another box's state must be untouched: these all live in one shared directory.
        write("status/other-box.json", r#"{"status":"working"}"#);

        forget_turn_state(&repo, "bridge-main");

        assert!(
            !store.join("status/bridge-main.json").exists(),
            "a dead session's last word must not greet the new one"
        );
        assert!(
            !store.join("status/bridge-main.agents").exists(),
            "the counter counts processes that died with the old session"
        );
        for kept in [
            "sessions/bridge-main.json",
            "hook-log/bridge-main.jsonl",
            "telemetry/bridge-main.jsonl",
            "status/other-box.json",
        ] {
            assert!(store.join(kept).exists(), "{kept} is history, not a claim");
        }

        // A box being created for the first time has none of this, and that is not an error.
        forget_turn_state(&repo, "brand-new");
    }

    // The base is a ladder, not a guess: the branch the user configured, then the local caches of
    // the remote's default — and every rung is checked against the remote before it is used, so a
    // configured `develop` is honoured on the repos that have one without breaking those that don't.
    #[test]
    fn the_base_branch_is_whatever_the_remote_actually_has() {
        use std::fs;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        // A real repository whose default branch is `master`, which is the case that failed.
        let origin = home.join("origin");
        fs::create_dir_all(&origin).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {ok:?}");
        };
        git(&origin, &["init", "-b", "master"]);
        fs::write(origin.join("f"), "x").unwrap();
        git(&origin, &["add", "-A"]);
        git(&origin, &["commit", "-m", "one"]);

        let repo = Repo {
            id: "bridge".into(),
            source: origin.to_string_lossy().into_owned(),
            work: origin.to_string_lossy().into_owned(),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };

        let mut config = load_config();
        config.base_branch = String::new();
        save_config(&config).unwrap();
        assert_eq!(
            base_branch(&repo),
            "master",
            "the remote's own default, never an assumed `main`"
        );

        // A configured base the repo does not have must not be taken at face value: it is one
        // setting shared by every repo, so trusting it blindly reintroduces the same failure.
        let mut config = load_config();
        config.base_branch = "main".into();
        save_config(&config).unwrap();
        assert_eq!(
            base_branch(&repo),
            "master",
            "no `main` here — fall through"
        );

        // A configured base the repo DOES have wins, ahead of the remote's default.
        git(&origin, &["branch", "develop"]);
        let mut config = load_config();
        config.base_branch = "develop".into();
        save_config(&config).unwrap();
        assert_eq!(base_branch(&repo), "develop", "the user's own answer leads");

        std::env::remove_var("SKEIN_HOME");
    }

    // Every argument is quoted: a branch like `feat/auth` or a name with a space must reach the
    // launcher whole, and the launcher does its own refusing from there.
    #[test]
    fn starting_a_box_hands_the_launcher_quoted_arguments() {
        // Takes the env lock and pins its own SKEIN_HOME: `session_script` reads the box's cgroup
        // limits out of the config, so without this it can be handed another test's home mid-run
        // and fail on an assertion about a string it never built. Latent for a long time; it only
        // started firing once there were more env-setting tests to race with.
        let _g = env_lock();
        std::env::set_var("SKEIN_HOME", tempdir());
        let script = session_script("web-main", "skein-agent", "claude --continue");
        assert!(
            script.contains("'/boxes/.skein/box-session.sh' 'web-main'"),
            "{script}"
        );
        // The shared ceilings lead, in the environment: an already-installed launcher that knows
        // nothing about them ignores a variable, where it would have read an argument as part of
        // the agent's command line.
        assert!(
            script.starts_with("SKEIN_FLEET_LIMITS="),
            "the ceilings must not be positional: {script}"
        );
        assert!(script.contains("'/boxes/web-main' '/boxes/web-main/anchor.pid' 'skein-agent'"));
        // The host-side state dir the box binds its conversation from — a HOST path, not a /boxes
        // one, because the point of it is to outlive the sandbox that /boxes lives in.
        assert!(
            script.contains(&format!("'{}'", box_state("web-main"))),
            "the box must be told where its durable state lives: {script}"
        );
        assert!(
            !box_state("web-main").starts_with("/boxes"),
            "box state on VM-local disk would defeat the entire point"
        );
        assert!(
            script.ends_with("bash -lc 'claude --continue'"),
            "the agent command stays one argument: {script}"
        );
        std::env::remove_var("SKEIN_HOME");
    }
}
