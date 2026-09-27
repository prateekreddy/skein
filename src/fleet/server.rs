//! The cockpit server in the sandbox: starting and stopping it, its doorway and door, detached
//! runs under tmux, and publishing the cockpit's port.

use super::*;

/// Host ports sbx already forwards to `sandbox_port` in this sandbox, or `None` when the question
/// could not be put at all.
///
/// Parsed from the table `sbx ports <sandbox>` prints — `HOST IP / HOST PORT / SANDBOX PORT /
/// PROTOCOL`. Deduplicated: the same host port is listed once per address family (`127.0.0.1` and
/// `::1`), and they are one mapping.
///
/// **`None` is not an empty list, and that distinction is the whole reason this returns an
/// `Option`.** It used to answer `Vec::new()` when `sbx` could not be run — which in the fleet is
/// always, since `sbx` is host-only — so "nothing forwards this port" and "I cannot see the host
/// from here" were the same answer. A caller reading the first acts; a caller reading the second
/// must not. That is §2.4's `unknown`, and [`publish_cockpit_port`] is where it becomes one.
///
/// **Everything except the spawn is [`forwards_in`]**, so the table can be read in a test without
/// one (SKEIN-747). What is left here is the transport and the one decision that belongs to it:
/// any failure to run `sbx` at all — not on this PATH, no process to be had, killed at the budget —
/// is `None`, because none of them is a reading.
fn existing_forwards(sandbox: &str, sandbox_port: u16) -> Option<Vec<u16>> {
    let Ok((out, _, 0)) = run_capture_for("sbx", &["ports", sandbox], Duration::from_secs(20))
    else {
        return None;
    };
    Some(forwards_in(&out, sandbox_port))
}

/// The host ports in one `sbx ports` table that map to `sandbox_port`, deduplicated and sorted.
///
/// Split out of [`existing_forwards`] because parsing a table is not spawning a process, and the
/// test that owned this had to spawn one to reach it (SKEIN-747). The dedup was the part that paid
/// for the split immediately: `sbx` lists one mapping once per address family — `127.0.0.1` and
/// `::1` — and no fake `sbx` in this file had ever printed the second row, so the line that folds
/// them was never run by a test until this became callable with a table.
fn forwards_in(table: &str, sandbox_port: u16) -> Vec<u16> {
    let mut found: Vec<u16> = table
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let host_port: u16 = cols.nth(1)?.parse().ok()?;
            let mapped: u16 = cols.next()?.parse().ok()?;
            (mapped == sandbox_port).then_some(host_port)
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
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
    format!("^python[0-9.]* {}( |$)", regex_literal(path))
}

/// A path as a regex that matches only itself.
///
/// Only `.` used to be escaped, which is the metacharacter people remember. The path comes from
/// `fleet_root()` — `$SKEIN_FLEET_ROOT`, which an operator sets — so a `+`, `[`, `(`, `*`, `?` or
/// `|` in it left the pattern matching something OTHER than the intended process. That pattern is
/// handed to `pkill -f`, so being wrong there does not mean failing to find the agent; it means
/// killing whatever else matched.
fn regex_literal(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                format!("\\{c}")
            }
            c => c.to_string(),
        })
        .collect()
}

/// A supervisor loop around `script`: run `body` for ever, and stop when `script` itself is gone.
///
/// **The stopping condition is the whole reason this is a function.** Both of skein's supervisors
/// used to say `while true`, and a fleet deleted out from under either of them left a bash
/// restarting a python script that no longer existed, twice a second, until the machine was
/// rebooted. Every integration fixture deletes its fleet root on the way out and so does a fleet a
/// person destroys; 105 doorway loops and one agent loop were alive on one box when somebody
/// finally read `ps`. Fixing one and leaving the other is how the second one was found, which is
/// why the reasoning lives here rather than twice.
///
/// Safe as a *condition* rather than as a race in both places, for different reasons. The doorway
/// is renamed into place ([`install_doorway`]), so its path is never momentarily absent. The agent
/// is written with `cat >`, which truncates rather than unlinks — the file exists throughout, and a
/// half-written one is a python that fails and is retried, exactly as before.
///
/// A script that dies for any *other* reason still has its file and is still restarted. This ends
/// only the case where there is nothing left to restart it with.
fn supervised(script: &str, body: &str) -> String {
    format!("while [ -f {} ]; do {body} done", sh_quote(script))
}

// --- The move (delivery §3 4c): skein-server runs inside the fleet it operates -------------------
//
// Everything below is the mover rather than the moved: it installs the door and the binary, and
// the server it starts is the process that then runs inside the fleet. It used to also tell that
// server so, with `SKEIN_IN_FLEET=1` in its environment, because a second host-driven deployment
// existed to be told apart from this one. SKEIN-521 deleted that alternative — there is one
// deployment now, nothing declares it and nothing detects it, and the variable went with the
// module that read it (SKEIN-643).

/// The socket-holder installed beside the server. `src/server-doorway.py` says why the socket is
/// opened by a process that is not the server: the port must never be free (§9.4), including across
/// server crashes and upgrades, and a supervisor that re-ran a self-binding server would reopen
/// the squat window on every restart.
const SERVER_DOORWAY_PY: &str = include_str!("../server-doorway.py");

/// How the doorway is found and reloaded, the one copy `start-door.sh` also carries — see its header.
const DOOR_SH: &str = include_str!("../door.sh");

/// The tmux session the doorway (and through it the server) runs in. Its own socket file rather
/// than the sandbox's default server, so `fleet-serve` in a test — where the "sandbox" is the
/// machine itself — cannot collide with a real session, and so the pane is findable by path.
const SERVER_SESSION: &str = "skein-server";

/// Run `script` in a detached tmux session, refusing rather than starting a second one.
///
/// The shape the server's doorway already uses, named once because a second copy is where the two
/// start to disagree. **`has-session` first and `exit 0` on a hit** is deliberately not what this
/// does: the deleted agent's supervisor wanted "leave a running one alone", and an update
/// wants "say so", because a person who pressed the button twice needs to be told the first press
/// is still going rather than shown a session that ignores them.
pub fn detach_named(sandbox: &str, session: &str, script: &str) -> Result<(), String> {
    if !crate::util::valid_name(session) {
        return Err(format!("invalid session name {session:?}"));
    }
    let place = own_sandbox(sandbox);
    let path = detached_script_path(session);
    // **The script goes to a file, never into tmux's argv** — see [`detached_script_path`]. Through
    // `Place::write`, which is the trick an in-sandbox install already uses and whose size problem is
    // already solved there: under the cap it is the agent's chunked `/write`, over it `sbx exec -i`,
    // whose stdin has no ceiling at all.
    place.write(
        &format!(
            "mkdir -p {dir} && cat > {path} && chmod 700 {path}",
            dir = sh_quote(path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/tmp")),
            path = sh_quote(&path),
        ),
        script.as_bytes(),
        Duration::from_secs(60),
    )?;
    place
        .exec(&detach_command(session), Duration::from_secs(30))
        .map(|_| ())
}

/// Whether the session [`detach_named`] started is still there.
///
/// **`None` is "could not ask", and a caller must never round it down to "gone".** No fleet agent,
/// a sandbox in the middle of a restart, a tmux that did not answer — every one of those arrives
/// here, and every one of them happens most often during the last minute of a *successful* update,
/// because the thing being installed is the process doing the asking. Reading silence as death is
/// how a build that is going fine gets declared dead in the pane watching it.
///
/// `; echo $?` rather than the exec's own status, because [`crate::place::Place::exec`] reports "it
/// ran and said no" as `Ok` — so "the session is gone" and "the sandbox never answered" would come
/// back indistinguishable, which is exactly the distinction this function exists to make.
pub fn detached_alive(sandbox: &str, session: &str) -> Option<bool> {
    if !crate::util::valid_name(session) {
        return None;
    }
    let said = own_sandbox(sandbox)
        .exec(
            &format!(
                "tmux has-session -t {} >/dev/null 2>&1; echo $?",
                sh_quote(session)
            ),
            Duration::from_secs(15),
        )
        .ok()?;
    // tmux exits 1 both for a session that ended and for a server that is not running at all, and
    // both of those are the same fact to a caller: there is no run in there.
    match said.split_whitespace().last() {
        Some("0") => Some(true),
        Some("1") => Some(false),
        _ => None,
    }
}

/// What [`detach_named`] tells tmux — **and it takes no script, which is the fix.**
///
/// The old version interpolated the whole script here. It cannot now: there is no parameter to put
/// one in, so the command's length depends on the session name alone and `util::valid_name` bounds
/// that. See [`detached_script_path`] for what the length used to be and what tmux said about it.
fn detach_command(session: &str) -> String {
    detach_command_at(&detached_script_path(session), session)
}

/// [`detach_command`] against a script path the caller names, rather than one read from the
/// environment as this runs.
///
/// Split for the reason `box_ready_script_in` was: `fleet_root()` reads `$SKEIN_FLEET_ROOT`, unit
/// tests run as threads of one process, and 711 of them call `set_var`. A test that built the
/// command here and the expected path there read the variable TWICE, and a neighbour setting it in
/// between made the two disagree — which is an order-dependent failure that says nothing about the
/// code under test. Production still has exactly one reader of the variable, one line above.
fn detach_command_at(path: &str, session: &str) -> String {
    format!(
        "tmux has-session -t {name} 2>/dev/null && {{ echo \"a {session} session is already \
         running\" >&2; exit 1; }}; tmux new-session -d -s {name} {run}",
        name = sh_quote(session),
        run = sh_quote(&format!("sh {}", sh_quote(path))),
    )
}

/// tmux's ceiling on one command, measured rather than looked up.
///
/// The client packs a command into a single imsg and refuses anything that will not fit. tmux 3.6,
/// in this sandbox on 2026-08-31: a 35,254-byte argument answered `command too long` and created no
/// session; the same call with a short one created it. `MAX_IMSGSIZE` is 16384 and the header eats
/// some of that, so this is the round number below it rather than a boundary anybody should sit
/// against.
#[cfg(test)]
const TMUX_COMMAND_CEILING: usize = 16_384;

/// Where [`detach_named`] leaves the script it is about to run.
///
/// **Because tmux has a ceiling on a command and skein went through it.** `tmux new-session -d -s
/// <name> <script>` packs the whole script into one argument, and the client sends it to the server
/// in a single imsg — capped at 16 KB. The update's script embeds the whole of `bootstrap.sh`,
/// which passed that mark and reached 35 KB, so tmux answered `command too long`
/// and started nothing. Reproduced on tmux 3.6, 2026-08-31: the same call with a short argument
/// creates the session and the 35 KB one creates none.
///
/// **What made it worse than a failed button.** `update::start` writes an empty log and removes the
/// done marker *before* this call, and `update::running` is "the log is there and the marker is
/// not" — so a launch that never happened left the cockpit reporting an update in progress for
/// ever, with an empty log and the button disabled. That half is fixed where it lives.
///
/// Beside the fleet's other installed pieces rather than in `/tmp`: this is a thing skein put in
/// the sandbox, it is worth being able to read after a run that went wrong, and a sandbox's `/tmp`
/// is shared by everything skein runs in it.
pub(super) fn detached_script_path(session: &str) -> String {
    format!("{}/.skein/detached/{session}.sh", fleet_root())
}

/// What `bootstrap.sh` needs told, and nothing more.
///
/// Only the values that differ from its own defaults are worth sending; the script's job is to work
/// on a sandbox where skein does not exist yet, so every one of these has a default there too.
pub(super) fn bootstrap_env() -> Vec<(&'static str, String)> {
    vec![
        ("SKEIN_FLEET_ROOT", fleet_root()),
        ("SKEIN_SOURCE_URL", skein_source_url()),
        ("SKEIN_SOURCE_REF", skein_source_ref()),
        ("SKEIN_SERVER_PORT", server_sandbox_port().to_string()),
        ("SKEIN_HOME", skein_home().to_string_lossy().to_string()),
    ]
}

/// Install the socket-holder alone, with no binary to put behind it.
///
/// Separate from the server itself because the two arrive at different moments and that is the
/// whole point of the door: it is opened at fleet **create**, when the only thing that exists is a
/// sandbox, and the binary turns up later, when `bootstrap.sh` builds it. A create that had to wait for a
/// binary would leave the port free for exactly the interval in which the first box is launched.
fn install_doorway(sandbox: &str) -> Result<(), String> {
    let doorway = server_doorway_path();
    let dir = doorway.rsplit_once('/').map(|(d, _)| d).unwrap_or("/boxes");
    own_sandbox(sandbox)
        .write(
            // Renamed into place for the half-written half of the reason above: ETXTBSY does
            // not apply to a `#!` script (the kernel opens the interpreter, not this file), but the
            // supervisor's two-second retry can still catch one mid-write.
            &format!(
                "mkdir -p {dir} && cat > {new} && chmod 755 {new} && mv {new} {doorway}",
                dir = sh_quote(dir),
                new = sh_quote(&format!("{doorway}.new")),
                doorway = sh_quote(&doorway),
            ),
            SERVER_DOORWAY_PY.as_bytes(),
            Duration::from_secs(30),
        )
        .map(|_| ())
        .map_err(|e| format!("installing the server doorway in {sandbox}: {e}"))
}

/// Hold the cockpit's port in `sandbox`, whether or not there is a server to put behind it.
///
/// **This is the moment that closes §9.4's squat**, and it is fleet create rather than
/// `fleet-serve`: the mapping a serve publishes outlives skein, so a box that took the port before
/// the doorway did becomes the cockpit, and the browser hands it the fleet token on the first
/// request. `ensure_fleet` runs this before it installs the launcher — that is, before the sandbox
/// has ever been able to start a box — so there is no interval in which a box and a free port
/// coexist.
///
/// The already-open case costs one `exec` and nothing else: a doorway that holds the port is left
/// alone rather than reinstalled, because the launch path is not where an upgrade belongs (that is
/// [`reload_server`], which swaps it deliberately).
pub fn ensure_fleet_door(sandbox: &str) -> Result<(), String> {
    if door_holds_port(sandbox, server_sandbox_port()) {
        return Ok(());
    }
    // A doorway that is ALIVE but unstamped — the stamp deleted by hand, or a pid in it that no
    // longer names this doorway. `start_server` cannot repair that: it finds the tmux session
    // already there and returns, so nothing re-stamps and every later publish refuses
    // to publish, correctly but permanently, until somebody serves twice. A re-exec re-stamps
    // across the same descriptor without ever closing the socket, which is exactly the repair.
    //
    // Only reachable when the stamp already says no, so a box start does not restart the server
    // behind a healthy door — that guard is the early return above, not this branch. A doorway
    // that takes the signal and then fails to stamp within the window falls through to the
    // install-and-start below, which is where an unrecoverable one belongs.
    if reload_server(sandbox) && door_settles(sandbox, server_sandbox_port()) {
        return Ok(());
    }
    install_doorway(sandbox)?;
    start_server(sandbox)
}

/// Is the process holding the cockpit's port **the doorway**, rather than merely something?
///
/// A TCP connect cannot answer this and the difference is the whole attack: a squatter accepts
/// too, so a connect-only judgement publishes the host's mapping — and with it the browser and its
/// token — to whatever got there first. The stamp is read the way `places/` anchors are read: a
/// pid is a number that gets reused, so it counts only when the process is alive **and** is this
/// doorway, which `/proc/<pid>/cmdline` says exactly. A stamp left by a doorway that was killed
/// names a dead pid and reads as closed, which is the honest answer.
fn door_holds_port(sandbox: &str, port: u16) -> bool {
    door_pid(sandbox, port).is_some()
}

/// [`door_holds_port`], saying WHICH doorway: its pid, read and judged exactly as that describes.
///
/// Public for the cockpit's "Restart on new build" (SKEIN-1029), which has to name what holds the
/// port when a reload does not take — and a pid it re-derived some other way would be a second
/// answer to the question this function already answers.
pub fn door_pid(sandbox: &str, port: u16) -> Option<u32> {
    let said = own_sandbox(sandbox)
        .exec(&door_pid_script(port), Duration::from_secs(20))
        .ok()?;
    said.trim().strip_prefix("up ")?.parse().ok()
}

/// [`door_pid`]'s question as a script: `src/door.sh`'s `skein_stamped_door`, answering `up <pid>`.
fn door_pid_script(port: u16) -> String {
    format!(
        "{DOOR_SH}\npid=$(skein_stamped_door {stamp} {port} {doorway}) || exit 1; echo \"up $pid\"",
        stamp = sh_quote(&server_door_stamp_path()),
        doorway = sh_quote(&server_doorway_path()),
    )
}

/// Wait, briefly, for the doorway to take the port and say so.
///
/// A start returns before the python behind it has bound, so an immediate read of the stamp is a
/// question asked too early — and the answer it gets ("no doorway") is the one that refuses to
/// publish. The window is generous because what it guards is a mapping skein will not take back —
/// see [`publish_cockpit_port`], which is where that rule and its reason live.
fn door_settles(sandbox: &str, port: u16) -> bool {
    let attempts = 20;
    for attempt in 0..attempts {
        if door_holds_port(sandbox, port) {
            return true;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    false
}

/// Ask a running doorway to restart the server behind it, without closing the socket.
///
/// `SIGUSR1` and not `SIGHUP`: tmux sends `SIGHUP` to a pane's processes when its session is
/// killed, so a doorway that reloaded on `SIGHUP` would re-exec itself out of every stop.
///
/// **Which process is signalled is `src/door.sh`'s `skein_door_reload`** — the doorway the stamp
/// names, else the one a cockpit supervisor runs — and `start-door.sh` carries the same bytes.
/// It was `pkill -USR1 -f` on a command-line pattern here while `start-door.sh` used the stamp:
/// two answers to "which process is the doorway", and the pattern one is a `pkill -f`.
///
/// Returns whether a doorway was there to signal — false means there is nothing to reload and the
/// caller must start one.
pub fn reload_server(sandbox: &str) -> bool {
    let script = format!("{} >/dev/null 2>&1 && echo reloaded", reload_command());
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(30))
        .map(|out| out.trim() == "reloaded")
        .unwrap_or(false)
}

/// The reload [`reload_server`] sends, as a person would type it — so the cockpit can hand over the
/// exact command when a reload it sent did not take (SKEIN-1029), rather than a paraphrase of it.
pub fn reload_command() -> String {
    format!(
        "{DOOR_SH}\nskein_door_reload {stamp} {port} {doorway} {sock} {old}",
        stamp = sh_quote(&server_door_stamp_path()),
        port = server_sandbox_port(),
        doorway = sh_quote(&server_doorway_path()),
        sock = sh_quote(&server_tmux_sock()),
        old = sh_quote(&pre_move_server_tmux_sock()),
    )
}

/// What the doorway's pane last said — where a server that will not start says why. The same
/// command [`cockpit_port_advice`] gives for a door that is not held.
pub fn doorway_pane_command() -> String {
    format!(
        "tmux -S {} capture-pane -p -t {}",
        server_tmux_sock(),
        SERVER_SESSION
    )
}

/// Start the doorway, which opens the socket and only then runs the server behind it.
///
/// The ordering the item exists for is not enforced here — it is *structural*: there is no
/// spelling of this start that runs skein-server before the socket is open, because the only thing
/// started is the doorway, and the doorway binds before it forks. `SKEIN_HOME` rides in the inner
/// command because the volume is mounted into the sandbox at its host path and the sandbox's own
/// environment has never heard of it.
///
/// **The supervisor's delay is conditional, and that is the point.** Every second between the
/// doorway dying and its replacement binding is a second the cockpit's port stands empty with
/// boxes already running — so a doorway that had been up is restarted in the time python takes to
/// start, and only one that died in its first five seconds is slept on. Unconditional (which this
/// was) meant a two-second window on every crash; unconditionally instant would turn a doorway
/// that cannot start at all into a busy loop on a sandbox that is already unwell.
///
/// **The loop ends when the doorway it supervises is gone** — see [`supervised`], which is where
/// that is decided and why.
pub fn start_server(sandbox: &str) -> Result<(), String> {
    let sock = server_tmux_sock();
    let inner = supervised(
        &server_doorway_path(),
        &format!(
            "began=$(date +%s); \
             SKEIN_HOME={home} python3 {doorway} {port} {server} {stamp}; \
             [ $(($(date +%s) - began)) -lt 5 ] && sleep 2;",
            home = sh_quote(&skein_home().to_string_lossy()),
            doorway = sh_quote(&server_doorway_path()),
            port = server_sandbox_port(),
            server = sh_quote(&server_path()),
            stamp = sh_quote(&server_door_stamp_path()),
        ),
    );
    // The socket lives under `private/`, which — unlike the `.skein` it used to sit in — is not
    // already there on a fleet that has never started a box: `box-session.sh` makes it at box start
    // and `bootstrap.sh` does not make it at all. tmux does not create a socket's parent, so without
    // this the first `new-session` on a fresh fleet dies with "error creating … (No such file or
    // directory)" and the cockpit's port is never held. `start-door.sh` carries the same two lines
    // for the same reason. `700` because the cover is the mount and the mode is the belt beside it.
    //
    // A session on the PRE-move socket is a running cockpit too (SKEIN-1025): a fleet that has not
    // re-run `bootstrap.sh` since SKEIN-529 still supervises its doorway there, and starting one on
    // the new socket beside it gives two supervisors contending for one port — SKEIN-1020's state,
    // reached from Rust. So it is asked, and left alone; moving it is `start-door.sh`'s job.
    let script = format!(
        "mkdir -p {private} && chmod 700 {private}; \
         tmux -S {sock} has-session -t {session} 2>/dev/null && exit 0; \
         tmux -S {old} has-session -t {session} 2>/dev/null && exit 0; \
         tmux -S {sock} new-session -d -s {session} {inner}",
        private = sh_quote(&fleet_private_dir()),
        sock = sh_quote(&sock),
        old = sh_quote(&pre_move_server_tmux_sock()),
        session = sh_quote(SERVER_SESSION),
        inner = sh_quote(&inner),
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(30))
        .map(|_| ())
        .map_err(|e| format!("starting skein-server in {sandbox}: {e}"))
}

/// Stop the doorway and the server it holds. Ending the session ends the supervisor loop, the
/// doorway and — same process group — the server it forked; the `pkill`s are for anything that
/// somehow outlived its session, and are allowed to find nothing.
///
/// **Both sockets** (SKEIN-1025). On a fleet whose supervisor is still on the pre-move socket
/// ([`pre_move_server_tmux_sock`]), a kill-session on the new one found nothing, the `pkill` ended
/// the doorway, and the surviving supervisor loop started it again two seconds later — a stop that
/// did not stop.
pub fn stop_server(sandbox: &str) {
    let script = format!(
        "tmux -S {sock} kill-session -t {session} 2>/dev/null; \
         tmux -S {old} kill-session -t {session} 2>/dev/null; \
         pkill -f {doorway} 2>/dev/null; pkill -f {server} 2>/dev/null; true",
        sock = sh_quote(&server_tmux_sock()),
        old = sh_quote(&pre_move_server_tmux_sock()),
        session = sh_quote(SERVER_SESSION),
        doorway = sh_quote(&agent_pkill_pattern(&server_doorway_path())),
        server = sh_quote(&format!("^{}( |$)", regex_literal(&server_path()))),
    );
    let _ = own_sandbox(sandbox).exec(&script, Duration::from_secs(30));
}

/// Stop the cockpit **without closing its door** — `skein cockpit-stop`.
///
/// Deliberately not [`stop_server`], which is a teardown: ending the session ends the doorway, and
/// a doorway that lets go of the port reopens exactly the hole the doorway exists to close. The
/// `sbx` mapping outlives the process holding it and skein does not withdraw it
/// ([`cockpit_port_advice`]) — a box that binds the freed port becomes the cockpit, and the browser
/// hands it the fleet token on the first request (architecture §9.4). Note the hole is the SANDBOX
/// end of the mapping, which no host-side withdrawal reaches: `--unpublish` would not close this
/// one even if skein called it. A stop that costs you that is not a stop anybody wants.
///
/// So the server is taken away and the door is left standing, using a state the doorway already
/// has rather than a mechanism added beside it: with nothing executable at [`server_path`] it holds
/// the socket and waits, saying so once. That is the **create-time** state — `ensure_fleet` opens
/// the door before any binary exists — so this returns the fleet to a shape it has already been in,
/// and the supervisor starts one again as soon as a binary is back on disk.
///
/// **Removed before stopped**, and the order is the whole correctness of it: the doorway restarts
/// its child two seconds after it exits, so stopping first leaves a window in which the binary is
/// still there to be restarted from.
///
/// The child is ended by [`reload_server`] — the doorway's own `SIGUSR1` — rather than by a `pkill`
/// at the server's path, and that is not a stylistic choice. `pkill -f` matches a command line, and
/// the server's command line is only its own path when the server is a *binary*; anything with a
/// `#!` line runs as `python3 <path>` and the pattern silently matches nothing. The doorway knows
/// its child by pid, so it cannot be wrong about this, and its handler already does exactly the
/// two things wanted: `SIGTERM` the child, then re-exec itself across the same descriptor. Same
/// process, same socket, and the fresh loop finds no binary and settles into holding the port.
///
/// Reports rather than refuses when there was nothing running: a stop that errors on an already
/// stopped cockpit is a stop people stop trusting, and the state afterwards is the same either way.
pub fn stop_serving(sandbox: &str) -> Result<String, String> {
    let script = format!(
        "was=stopped; [ -x {server} ] && was=running; rm -f {server}; echo \"$was\"",
        server = sh_quote(&server_path()),
    );
    let was = own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(30))
        .map(|out| out.trim().to_string())
        .map_err(|e| format!("stopping the server in {sandbox}: {e}"))?;
    reload_server(sandbox);
    Ok(was)
}

/// **Is it safe to tell somebody to publish the cockpit's port?** — §9.4's guard, moved to the
/// actor that now performs the act.
///
/// `ensure_fleet_server` used to hold this: it refused to publish onto a port the doorway did not
/// hold, because a mapping published to a squatter has handed the browser and the fleet token over
/// by the time anyone could take it back — and skein withdraws nothing. Deleting it (SKEIN-576)
/// deleted that refusal, and the person who now runs `sbx ports --publish` is acting on what skein
/// told them. **So the guard did not go with the publisher; it went with the advice.** A guard that
/// is fooled no longer publishes to a squatter itself. It advises somebody else to, and that holds
/// for the warden's `publish` doer too (SKEIN-1130): nothing is put to it for a port this refuses.
///
/// The judgement is the stamp and never a TCP connect ([`door_holds_port`]): a squatter accepts
/// exactly as the doorway does, which is the whole of §9.4.
pub fn cockpit_port_advice(sandbox: &str) -> Result<crate::operation::Operation, String> {
    let port = server_sandbox_port();
    if !door_holds_port(sandbox, port) {
        return Err(door_refusal(sandbox, port));
    }
    Ok(publish_cockpit_port(sandbox))
}

/// Why the cockpit's port is not to be published while the doorway does not hold it — the one
/// refusal both [`cockpit_port_advice`] and [`ask_warden_to_publish_cockpit_port_at_start`] give.
fn door_refusal(sandbox: &str, port: u16) -> String {
    format!(
        "the cockpit's port :{port} in {sandbox} is not held by the doorway, so do not publish \
         it. Either the doorway could not start (check `{look}` in the sandbox, or that \
         python3 is present), or something else in the fleet is already on :{port} — which \
         is architecture §9.4's squat, and a mapping published to it hands the browser and its \
         token to whatever holds it. Nothing takes that back afterwards.",
        look = doorway_pane_command(),
    )
}

/// **Publishing the cockpit's port, as an Operation a person performs** (§2.4, SKEIN-576).
///
/// Skein does not publish this mapping and no longer tries. That is not a gap left by deleting
/// host-driven skein — it is the shape `docs/delivery.md` records for every privileged act: *"an
/// unreachable warden does not fall back to running `sbx` here, because that fallback would be
/// taken on exactly the day something was wrong. The failure names the fix and gives the line to
/// run by hand."* What used to be here was that fallback: two candidate ports, a publish attempt
/// per candidate, and a prompt only once both had failed.
///
/// **The warden has a doer for this act now, and this operation still names none.**
/// [`crate::warden_client::Act::Publish`] is asked of the warden's `publish` doer (SKEIN-1130).
/// Publishing opens a host port into the network namespace every box shares, which is why it stays
/// a prompted act: the warden performs it only after the person types the operation id at its
/// terminal, so the person stays in the decision. `doer` is still `None` here because nothing
/// drives this operation. The owner's decision (SKEIN-1140, "keep Publish for repair") makes a
/// publish a repair: `sbx create` publishes this port itself (`-p` in `create_argv`), and the
/// warden looks at `sbx ports` before it asks. Skein has nothing that observes a missing mapping
/// (this function is the only reader, and it cannot read in-fleet), so this operation is not
/// driven: the repair is asked of the warden once per server start instead, by
/// [`ask_warden_to_publish_cockpit_port_at_start`]. [`crate::operation::Operation::may_drive`] is
/// false here, and the recipe is what skein prints.
///
/// **The check is three-valued because the honest answer usually is.** In the fleet `sbx` cannot be
/// run at all, so [`existing_forwards`] returns `None` and this is `unknown` — not "no mapping",
/// which would be a lie that reads as an instruction to make one. On a host that can still ask, an
/// existing mapping that answers is `satisfied` and one that does not is `unsatisfied`.
///
/// Judged by a TCP connect rather than an HTTP exchange, deliberately: the doorway holds the
/// listening socket whether or not the server behind it is up yet, and the kernel completes the
/// handshake from the backlog — so "connects" is exactly the property the door promises.
///
/// This is the reading and [`cockpit_port_operation`] is the judgement made on it; that split is
/// what lets the judgement be tested without a process (SKEIN-747).
pub fn publish_cockpit_port(sandbox: &str) -> crate::operation::Operation {
    let sandbox_port = server_sandbox_port();
    cockpit_port_operation(sandbox, existing_forwards(sandbox, sandbox_port))
}

/// [`publish_cockpit_port`]'s judgement, with the reading handed to it (SKEIN-747).
///
/// **The mapping is what this decides, and asking `sbx` is how the mapping is learnt.** Those are
/// two jobs and they used to be one function, so the test for the first had to run the second: it
/// installed a fake `sbx` on the `PATH` and read the answer back out of a subprocess. A subprocess
/// is a thing a loaded machine can refuse — `fork` can fail for want of a process, an `exec` of a
/// script this process has just written can come back `ETXTBSY` while a sibling thread holds the
/// write descriptor across its own fork, and a 20s budget is a budget — and every one of those
/// arrives here as `None`, which is `unknown`. The test then read "I could not ask" as "sbx says
/// the mapping is dead", which is the one distinction this whole path exists to keep.
///
/// Measured on this box while SKEIN-747 was open: 300 exec attempts on a freshly written script,
/// with eight threads spawning alongside, gave 28 `ETXTBSY` failures and no other kind.
///
/// So the reading is a parameter. `unknown` is now reachable in a test by passing `None` rather
/// than by arranging for a spawn to fail, and a dead mapping is `Some(vec![port])` rather than a
/// process that has to survive the run.
fn cockpit_port_operation(
    sandbox: &str,
    forwards: Option<Vec<u16>>,
) -> crate::operation::Operation {
    use crate::operation::{Check, Class, Operation};
    let sandbox_port = server_sandbox_port();
    let act = crate::warden_client::Act::Publish {
        sandbox: sandbox.to_string(),
        host_port: sandbox_port,
        sandbox_port,
    };
    let recipe = vec![act.command()];
    let check = match forwards {
        // The question could not be put. `sbx` is host-only and this runs in the fleet.
        None => Check::Unknown(format!(
            "cannot ask this machine which ports {sandbox} forwards — `sbx` runs on the host and \
             skein runs inside the fleet, so the mapping is only visible from out there"
        )),
        Some(forwards) => match forwards.iter().find(|p| cockpit_settled(**p)) {
            Some(port) => Check::Satisfied(format!("127.0.0.1:{port} reaches the cockpit")),
            None if forwards.is_empty() => {
                Check::Unsatisfied(format!("nothing forwards to :{sandbox_port}"))
            }
            None => Check::Unsatisfied(format!(
                "{} forwards to :{sandbox_port}, and nothing answers through any of them",
                forwards
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
    };
    Operation {
        // Derived from the recipe, not minted, so a person who runs it twice is running one
        // operation twice rather than two — the property `warden_client::operation_id` implements
        // and asserts.
        id: crate::warden_client::operation_id("publish-cockpit-port", sandbox, &recipe),
        desired: format!(
            "a browser on this machine reaches the cockpit inside {sandbox} on :{sandbox_port}"
        ),
        check,
        recipe,
        class: Class::Idempotent,
        // Nothing drives this operation, though the warden has a `publish` doer (SKEIN-1130). See
        // `publish_cockpit_port`'s doc: a publish is a repair, asked of the warden once per server
        // start rather than through this operation (SKEIN-1140).
        doer: None,
    }
}

/// Does anything accept on the host side of `port`? Retried briefly, because a publish returns
/// before its forwarder necessarily does — a single immediate check gets an instant refusal, reads
/// it as "broken", and abandons a mapping that would have worked a second later. That mapping is
/// only recoverable by a person, which is what makes the wait worth more than the latency.
fn cockpit_settled(port: u16) -> bool {
    let attempts = if cfg!(test) { 1 } else { 6 };
    for attempt in 0..attempts {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok() {
            return true;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    false
}

/// Whether this process has asked the warden to publish the cockpit's port yet.
static ASKED_AT_START: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long an ask may take before the server log says it is waiting on a person.
///
/// A warden that finds the mapping in place answers within a `sbx ports`, so an ask still open
/// after this is almost always one sitting at the warden's terminal.
const ASK_QUIETLY_FOR: Duration = Duration::from_secs(5);

/// **Ask the warden, once per server start, to publish the cockpit's port** (SKEIN-1130).
///
/// The owner's trigger (2026-09-24). `sbx create` publishes this port itself (`-p` in
/// `create_argv`), so a publish is a repair, and skein cannot see a missing mapping from in-fleet
/// ([`existing_forwards`] is `None` here). A server start is the moment that repair is asked for:
/// the warden looks at `sbx ports` first, so the usual answer is `already` and nobody is asked.
///
/// **Spawned, and never waited on.** The warden's approval blocks on its terminal until a person
/// types, so the ask runs on its own thread and holds nothing the server needs; the call returns
/// as soon as that thread exists. The client's own reply budget (half an hour) is the timeout, and
/// a timed-out ask is logged as the prompt it becomes and not retried until the next start.
///
/// What the log gets:
/// - no warden answering: nothing, as before this existed;
/// - the doorway does not hold the port: [`door_refusal`], and nothing is asked (§9.4);
/// - `already`: nothing, unless the waiting line was already written, when the warden's own line
///   closes it — so the log never ends on a wait that ended;
/// - an ask still open after [`ASK_QUIETLY_FOR`]: the waiting line, then the published line once
///   the person approves;
/// - declined, refused or timed out: the prompt a person would be shown, [`Prompt::render`].
///
/// [`Prompt::render`]: crate::warden_client::Prompt::render
pub fn ask_warden_to_publish_cockpit_port_at_start() {
    let sandbox = fleet_sandbox();
    let port = server_sandbox_port();
    let door = sandbox.clone();
    let _ = ask_at_start(
        &ASKED_AT_START,
        crate::warden_client::Warden::configured(),
        sandbox,
        port,
        ASK_QUIETLY_FOR,
        move || match door_holds_port(&door, port) {
            true => Ok(()),
            false => Err(door_refusal(&door, port)),
        },
        |line: &str| eprintln!("skein: {line}"),
    );
}

/// [`ask_warden_to_publish_cockpit_port_at_start`], with everything it reads handed to it, so a
/// test can put a fake warden, a door and a log behind it. Returns the thread asking, if this call
/// started one.
fn ask_at_start(
    asked: &'static std::sync::atomic::AtomicBool,
    warden: crate::warden_client::Warden,
    sandbox: String,
    port: u16,
    quietly_for: Duration,
    door: impl FnOnce() -> Result<(), String> + Send + 'static,
    say: impl Fn(&str) + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    use crate::warden_client::{perform_through, Act, Answered, Performed};
    if asked.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    std::thread::Builder::new()
        .name("warden-publish".into())
        .spawn(move || {
            // No warden answering is today's behaviour at a server start: nothing to say, and no
            // terminal to say it to. Only a warden that answers is asked to do anything.
            if warden.glance().is_err() {
                return;
            }
            if let Err(refused) = door() {
                say(&refused);
                return;
            }
            let act = Act::Publish {
                sandbox,
                host_port: port,
                sandbox_port: port,
            };
            // The ask on a thread of its own, so this one can tell a quick answer from a person
            // being asked. A thread that cannot be had, or one that dies, leaves nothing to report.
            let (sent, answer) = std::sync::mpsc::channel();
            let asking = std::thread::Builder::new()
                .name("warden-publish-ask".into())
                .spawn(move || {
                    let _ = sent.send(perform_through(&warden, &act));
                });
            if asking.is_err() {
                return;
            }
            let mut waited = false;
            let performed = match answer.recv_timeout(quietly_for) {
                Ok(performed) => performed,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    say("waiting on the warden to approve the port…");
                    waited = true;
                    match answer.recv() {
                        Ok(performed) => performed,
                        Err(_) => return,
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            };
            match performed {
                Performed::Warden(already @ Answered::Already(_)) => {
                    if waited {
                        say(already.detail());
                    }
                }
                Performed::Warden(_) => say(&format!(
                    "the cockpit's port is published — 127.0.0.1:{port} reaches it now."
                )),
                Performed::Prompt(prompt) => say(&prompt.render()),
                Performed::Uncertain(answered) => say(answered.detail()),
            }
        })
        .ok()
}

/// The mount set for a fleet whose sandbox will host the server: [`fleet_mounts`] plus the volume
/// root itself, which the server needs and no box may read.
///
/// This used to refuse unless the caller said `--uncovered-volume`, because the 4a cover skipped
/// any mount that was an *ancestor* of its own covers — and the volume root is an ancestor of the
/// box-state parent, so mounting it handed every box `credentials/`, `api-token`, `github-pats/`
/// and `tokens/`. The launcher covers ancestors now, ahead of the binds that would otherwise be
/// thrown away (SKEIN-219, `src/box-session.sh`), and `tests/isolation_bwrap/` proves it by
/// running bwrap on a volume-shaped fleet and reading those paths back. So the grant is gone and
/// the mount is ordinary: nothing here is taken knowingly any more, because nothing is given away.
pub fn fleet_serve_mounts() -> Vec<String> {
    let home = skein_home().to_string_lossy().into_owned();
    let mut mounts = vec![home.clone()];
    for mount in fleet_mounts() {
        if under(&mount, &home) || mounts.iter().any(|m| under(&mount, m)) {
            continue; // already visible through the volume root
        }
        mounts.push(mount);
    }
    mounts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// The cockpit's port is published by the create, and the README's create line says so too.
    ///
    /// It used to be a fourth line somebody ran by hand, on the stated reason that "a sandbox
    /// cannot publish its own port" — which is true, and was never the question. `sbx create` takes
    /// `-p/--publish` (its own `--help`, read rather than assumed), so the host command that makes
    /// the sandbox can publish for it. A step a person runs separately is a step a person can skip,
    /// and skipping this one leaves a fleet that looks installed, serves nothing the browser can
    /// reach, and says so nowhere.
    ///
    /// Position matters and is asserted: sbx's usage is `sbx create [flags] AGENT PATH [PATH...]`,
    /// so a `-p` after `shell` is an argument to the `shell` subcommand rather than a flag to
    /// `create`. That is a mistake this argv has already made once (SKEIN-456, a second verb and a
    /// second name prepended to every create), which is why the shape is checked and not only the
    /// presence.
    #[test]
    fn the_create_publishes_the_cockpits_port_and_the_readme_agrees() {
        let _env = env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        std::env::remove_var("SKEIN_SERVER_PORT");
        let port = server_sandbox_port();
        let mapping = format!("{port}:{port}");

        let argv = create_argv("skein-fleet", &["/h/.skein".to_string()]);
        let at = argv.iter().position(|a| a == "-p").unwrap_or_else(|| {
            panic!(
                "the create publishes nothing, so the cockpit is unreachable from the browser \
                 however healthy it is: {argv:?}"
            )
        });
        assert_eq!(
            argv.get(at + 1).map(String::as_str),
            Some(mapping.as_str()),
            "the create's -p does not carry the cockpit's port {mapping}: {argv:?}"
        );
        let agent = argv
            .iter()
            .position(|a| a == "shell")
            .expect("the create names no agent");
        assert!(
            at < agent,
            "-p comes after `shell`, where sbx reads it as an argument to the shell subcommand \
             rather than as a flag to create: {argv:?}"
        );

        // And the README's own line, so the text a person copies and the argv skein builds cannot
        // drift. Every `sbx create` the README shows, not just the first — the second is the one
        // for people with repos outside the volume, and it is the one that gets forgotten.
        let readme = include_str!("../../README.md");
        let creates: Vec<&str> = readme
            .lines()
            .filter(|l| l.contains("sbx create --name skein-fleet"))
            .collect();
        assert!(
            !creates.is_empty(),
            "the README no longer shows a create line to check"
        );
        for line in &creates {
            assert!(
                line.contains(&format!("-p {mapping}")),
                "a README create line does not publish the cockpit's port, so following it \
                 produces a fleet the browser cannot reach: {line}"
            );
        }

        // The install block itself must no longer carry a separate publish. Asserted against the
        // fenced block rather than the whole file, because the prose still names `sbx ports` — as
        // the repair for a sandbox created without `-p`, which is a different thing from a step in
        // the install.
        let install = readme
            .split("```sh")
            .nth(1)
            .and_then(|b| b.split("```").next())
            .expect("the README no longer opens with a shell block");
        assert!(
            install.contains("sbx exec -i skein-fleet"),
            "the block read is not the install block: {install}"
        );
        assert!(
            !install.contains("sbx ports"),
            "the install still ends with a publish somebody has to remember to run:\n{install}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// A supervisor runs its script again and again, and stops when the script is gone.
    ///
    /// Run as `bash` rather than asserted as a string, because the claim is about what the loop
    /// *does* — and the failure it guards against does not look like a wrong string, it looks like
    /// a process nobody ever notices. Both of skein's supervisors said `while true`; a fleet
    /// deleted out from under either left a bash restarting a python script that no longer existed,
    /// twice a second, for ever. 105 doorway loops and one agent loop were alive on one box.
    ///
    /// Under `timeout`, because the bug's symptom *is* not-terminating: without it, reintroducing
    /// `while true` would hang this test rather than fail it, and a hang is the one result nobody
    /// reads. Exit 124 is what `timeout` reports when it had to kill, and it is asserted by name.
    ///
    /// The doorway's half of this is also proved end to end, against real tmux and a real fleet
    /// root, by `fleet_move::a_supervisor_whose_fleet_is_gone_stops_rather_than_restarting_for_ever`.
    #[test]
    fn a_supervisor_stops_when_the_script_it_restarts_is_gone() {
        let dir = std::path::PathBuf::from("/var/tmp")
            .join(format!("skein-supervisor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("supervised.py");
        let ticks = dir.join("ticks");

        // Runs while the script is there, and the body removes it — so this must tick exactly once
        // and then end. Ticking for ever is the bug; ticking never would mean the loop was already
        // broken for the ordinary case, which is what the second half below rules out.
        let run = |body: &str| -> std::process::Output {
            std::process::Command::new("timeout")
                .arg("5")
                .arg("bash")
                .arg("-c")
                .arg(supervised(&script.to_string_lossy(), body))
                .output()
                .expect("bash")
        };

        std::fs::write(&script, "").unwrap();
        let out = run(&format!(
            "echo tick >> {t}; rm -f {s};",
            t = sh_quote(&ticks.to_string_lossy()),
            s = sh_quote(&script.to_string_lossy()),
        ));
        assert_ne!(
            out.status.code(),
            Some(124),
            "the supervisor never ended after the script it restarts was deleted — this is the \
             loop that left 105 orphaned bash processes on one box, each spinning at 0.5 Hz with \
             nothing left to run and nothing that will ever reap it"
        );
        assert_eq!(
            std::fs::read_to_string(&ticks)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "a supervisor whose script is present must run it — this one did not, so it would \
             never restart a doorway or an agent that merely crashed either"
        );

        // And with nothing there to begin with, it does not run at all.
        let _ = std::fs::remove_file(&ticks);
        let out = run(&format!(
            "echo tick >> {};",
            sh_quote(&ticks.to_string_lossy())
        ));
        assert_ne!(
            out.status.code(),
            Some(124),
            "it never ended with no script at all"
        );
        assert!(!ticks.exists(), "it ran a script that was not there");

        let _ = std::fs::remove_dir_all(&dir);
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
    /// tmux session by hand. It was survivable in place only because the agent's retirement was
    /// sandwiched between its `tmux kill-session` and its restart, which is a dangerous thing for a
    /// line to depend on.
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

    /// **Skein does not publish the cockpit's port, and says so with the line to run** (SKEIN-576).
    ///
    /// This used to drive `ensure_server_port`, which tried two candidate ports through `sbx` and
    /// prompted only once both had failed. That is the fallback `docs/delivery.md:143` rules out —
    /// *"an unreachable warden does not fall back to running `sbx` here, because that fallback
    /// would be taken on exactly the day something was wrong"* — so the publishing went and what
    /// remains is [`publish_cockpit_port`], an Operation nothing drives. The warden has a
    /// `publish` doer since SKEIN-1130, but it is asked only with the person typing the operation
    /// id at its terminal, and never by `sbx` here.
    ///
    /// **Three properties survived the deletion and all three are here.**
    ///
    /// *A mapping only counts when something answers through it.* sbx's own bug in miniature: a
    /// mapping survives `sbx rm` and is still *reported* by `sbx ports` while every connection
    /// through it is refused (docker/sbx-releases#297), which skein reaches routinely because a
    /// resize recreates the sandbox. A check that believed the listing would report `satisfied` on
    /// a fleet nobody can reach. So a listed mapping that is dead and one real listener decide the
    /// answer between them.
    ///
    /// *Skein withdraws nothing, and now publishes nothing either.* The assertion is on the whole
    /// `sbx` transcript rather than on the return value, because a return value cannot tell a
    /// refusal apart from a refusal that ran the command first.
    ///
    /// *The wire format is sbx's spelling and not a guess* — `HOST:SANDBOX/PROTOCOL`. It used to be
    /// `publish_forward`'s; it is the recipe's now, and a person pastes it, so a typo is worth more
    /// than it was.
    ///
    /// **The listing is passed in, and that is SKEIN-747.** Every reading below used to come back
    /// through a fake `sbx` on the `PATH`, so this test's subject — what skein concludes from a
    /// mapping — rested on a subprocess starting. On a loaded machine one did not: the test failed
    /// in a full `cargo test --all` with `Unknown("cannot ask this machine which ports skein-fleet
    /// forwards…")` against the message *"a dead mapping sbx listed was believed"*, which is not a
    /// dead mapping at all. It is the answer for a question that was never put, and reading it as a
    /// mapping is the exact confusion [`crate::operation::Check`] has three states to prevent.
    /// [`cockpit_port_operation`] takes the reading, so `None` is now written rather than staged,
    /// and the four answers below cost no process at all.
    ///
    /// **And the dead port is held rather than released, which is SKEIN-737.** SKEIN-747 fixed the
    /// `Unknown` half of this test and left the other defect in the same fixture: `dead` was a
    /// number obtained by binding a listener and dropping it, so between the drop and the
    /// assertion the kernel was free to give it to anything, and a neighbour that took it turned
    /// the first judgement below into a statement about what else was running on the machine.
    /// SKEIN-709 and SKEIN-755 are the same defect filed twice more. It is obtained and blocked
    /// now — see the fixture — and the port cannot be re-bound, which is asserted where it is made.
    ///
    /// **What makes this fail**: giving the operation a doer; believing a listing without
    /// connecting through it (the first assertion, and the one the flake was disguised as); or
    /// answering `Some(vec![])` for a question that could not be put, which turns "I cannot see the
    /// host from here" into "nothing forwards this port" — an `unsatisfied` that reads as an
    /// instruction to go ahead.
    #[test]
    fn the_cockpits_port_is_a_recipe_a_person_runs_and_never_a_command_skein_runs() {
        use crate::operation::Check;
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

        // The one port that answers. `cockpit_settled` asks for a TCP connect and nothing more —
        // the doorway holds the socket before the server behind it is up — so a bare listener is
        // the whole fixture.
        let live = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let working = live.local_addr().unwrap().port();
        // A port nothing answers on, **and that nothing can start answering on** (SKEIN-737).
        //
        // The phantom mapping post-resize is a number sbx still lists with nothing behind it, and
        // this used to be built by binding a listener, reading its number and dropping it. The
        // number went straight back into the ephemeral range and nothing held it, so any neighbour
        // — including another test in this binary, several of which bind ephemeral ports — could be
        // handed it between the drop and the assertion. Then `cockpit_settled` connects, the check
        // comes back `Satisfied`, and the failure blames `publish_cockpit_port` for believing a
        // dead mapping. Seen in the wild twice (SKEIN-709 at ff1b7ab5, SKEIN-737) and reproduced on
        // demand: a second thread binding and dropping ephemeral listeners took the port back and
        // this test failed with *"a dead mapping sbx listed was believed:
        // Satisfied(\"127.0.0.1:36293 reaches the cockpit\")"*.
        //
        // So the port is obtained and then BLOCKED rather than obtained and released: `dead` is the
        // local end of a connection this test holds open for its whole length. Nothing listens
        // there, so a connect to it is refused by the kernel with no listener to hand it to; and
        // the port cannot be re-bound, because a socket with no `SO_REUSEADDR` is bound to it — the
        // assertion below is that property, and it is what the old shape could not satisfy.
        let borrower = std::net::TcpStream::connect(("127.0.0.1", working)).unwrap();
        let _accepted = live.accept().unwrap();
        let dead = borrower.local_addr().unwrap().port();
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", dead)).is_err(),
            "the port this test calls dead can be listened on, so a neighbour can make it answer \
             and the first judgement below becomes a coin toss about what else is running"
        );

        // A working `sbx` first on the PATH that records every call it is given. Nothing below
        // asks it anything — the readings are handed in — so the transcript is how "skein ran the
        // command instead of printing it" is caught: an empty log with a runnable `sbx` beside it
        // is a positive statement that no process was started, not the absence of one.
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("ports.log");
        std::fs::write(&log, "").unwrap();
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

        // 1. A mapping sbx lists that nothing answers through: unsatisfied, and NOT acted on.
        let op = cockpit_port_operation("skein-fleet", Some(vec![dead]));
        assert!(
            matches!(op.check, Check::Unsatisfied(_)),
            "a dead mapping sbx listed was believed: {:?}",
            op.check
        );
        assert!(
            op.check.detail().contains(&dead.to_string()),
            "the check did not say which mapping it found: {:?}",
            op.check
        );
        // Nothing may drive it, whatever the check said. This is the clause that keeps the line
        // above true for a caller that has not read §9.4. The warden's `publish` doer asks the
        // person at its own terminal, which is a different thing from being driven from here.
        assert!(
            !op.may_drive(),
            "an operation nothing drives was cleared to run, and the only way to obey is to run `sbx`"
        );
        // sbx's spelling, `HOST:SANDBOX/PROTOCOL`. A typo here is a line a person pastes and that
        // publishes nothing, with `sbx ports` reporting success.
        // Every argument quoted, which is `warden_client::by_hand`'s rule and not an accident: a
        // line a person pastes has to be right for the argument that needs quoting, and quoting
        // uniformly is how that stays true when somebody adds one.
        assert_eq!(
            op.recipe,
            vec![format!(
                "sbx 'ports' 'skein-fleet' '--publish' '{p}:{p}/tcp'",
                p = server_sandbox_port()
            )],
            "the recipe is not the command sbx takes"
        );

        // 2. A mapping that IS working: satisfied.
        let happy = cockpit_port_operation("skein-fleet", Some(vec![working]));
        assert!(
            matches!(happy.check, Check::Satisfied(_)),
            "a working mapping was not recognised: {:?}",
            happy.check
        );

        // 2b. A listing with nothing in it IS an answer — sbx was asked and said there is no
        // mapping — so it is `unsatisfied` and says which port has none. This is the arm that must
        // not be reachable any other way; case 3 is the one that must never land here.
        let bare = cockpit_port_operation("skein-fleet", Some(vec![]));
        assert!(
            matches!(&bare.check, Check::Unsatisfied(d) if d.contains("nothing forwards")),
            "an empty listing was not read as a mapping that is absent: {:?}",
            bare.check
        );

        // 3. No listing at all — which in the fleet is every time, because `sbx` is host-only. The
        // question was not put, so the answer is `unknown` and not "nothing forwards it": the
        // second reads as go-ahead.
        let blind = cockpit_port_operation("skein-fleet", None);
        assert!(
            matches!(blind.check, Check::Unknown(_)),
            "a question that could not be put was answered anyway: {:?}",
            blind.check
        );
        assert_ne!(
            blind.check.word(),
            bare.check.word(),
            "'sbx says there is no mapping' and 'sbx was never asked' came back as the same state, \
             which is the reading that made this test fail on a busy machine: {:?} vs {:?}",
            bare.check,
            blind.check
        );
        assert!(!blind.may_drive());
        // And the recipe is printable with the check unknown — that is the whole of what skein
        // offers here, so a rendering that hid it would leave a person with nothing.
        let said = blind.render();
        assert!(
            said.contains(&format!(
                "sbx 'ports' 'skein-fleet' '--publish' '{p}:{p}/tcp'",
                p = server_sandbox_port()
            )),
            "the recipe was not in what a person reads:\n{said}"
        );
        assert!(
            said.contains("yours to run"),
            "nothing told the person it was theirs to run:\n{said}"
        );

        // Four judgements, and a runnable `sbx` sitting first on the PATH throughout. Not one of
        // them may have started it.
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.trim().is_empty(),
            "judging a mapping ran `sbx`, so the check is a command again and the transcript \
             below is what a person's machine would have been made to do:\n{calls}"
        );

        // The one thing about the *reading* that must not drift, and it is the half this test used
        // to stage a subprocess to reach: a question that could not be put is `None`, never
        // `Some(vec![])`. `sbx` is not on the PATH this process started with — which is what made
        // the old case 3 work — so restoring that PATH makes the spawn fail for certain, whatever
        // the machine is doing. Failing to start and being killed at the budget are the same answer
        // here, so no arm of it depends on timing.
        std::env::set_var("PATH", &path);
        assert_eq!(
            existing_forwards("skein-fleet", server_sandbox_port()),
            None,
            "`sbx` could not be run at all and the reading still came back as a listing, so \"I \
             cannot see the host from here\" now reads as \"nothing forwards this port\""
        );
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        // And the caller that does ask — `publish_cockpit_port`, the whole path — asks and never
        // publishes. The assertion is negative on purpose: a machine that could not start `sbx` at
        // all leaves this log empty and proves nothing here, which is precisely why none of the
        // judgements above depends on it any more.
        let whole = publish_cockpit_port("skein-fleet");
        assert!(
            !whole.may_drive(),
            "the whole path cleared an operation nothing can perform"
        );
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !calls.contains("--publish") && !calls.contains("--unpublish"),
            "skein ran the command instead of printing it, which is the fallback \
             `docs/delivery.md` rules out:\n{calls}"
        );

        drop(live);
        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// One mapping listed once per address family is one mapping (SKEIN-747).
    ///
    /// [`forwards_in`] folds the `127.0.0.1` and `::1` rows `sbx ports` prints for a single
    /// forward, and until this test that line had never run under one: every fake `sbx` in this
    /// file prints one row, so a `dedup` that did nothing looked exactly like a `dedup` that
    /// worked. Reading a doubled mapping as two is not cosmetic — `publish_cockpit_port` names
    /// every port it found in an `unsatisfied`, and a person is being told what to go and look at.
    ///
    /// The table is the shape this file's own fake `sbx` prints, header included, because the
    /// header is a line the parser has to reject: its second column is `IP`, which is not a port.
    ///
    /// **What makes this fail**: dropping the `dedup`, or matching on the host port instead of the
    /// sandbox port — the second returns `9999` for a question about `:7878`.
    #[test]
    fn one_forward_listed_for_two_address_families_is_one_forward() {
        let table = "HOST IP\tHOST PORT\tSANDBOX PORT\tPROTOCOL\n\
                     127.0.0.1\t7878\t7878\ttcp\n\
                     ::1\t7878\t7878\ttcp\n\
                     127.0.0.1\t9999\t22\ttcp\n";
        assert_eq!(
            forwards_in(table, 7878),
            vec![7878],
            "one forward listed for two address families was counted twice"
        );
        assert_eq!(
            forwards_in(table, 22),
            vec![9999],
            "the mapping is read by its sandbox port, and the host port is what it answers"
        );
        assert!(
            forwards_in(table, 5432).is_empty(),
            "a port nothing forwards was said to be forwarded"
        );
    }

    /// **A detached run tells tmux to open a file, and never hands it the script.**
    ///
    /// tmux caps one command at an imsg, and skein went through it: the update's script embeds the
    /// whole of `bootstrap.sh`, which reached 35 KB, so `tmux new-session -d -s skein-update
    /// '<35KB>'` answered `command too long` and started nothing — while the cockpit reported an
    /// update in progress, because `update::start` had already written its log. Reproduced on tmux
    /// 3.6 in this sandbox on 2026-08-31.
    ///
    /// **What would make this fail:** putting the script back in the command. It cannot be done
    /// without giving `detach_command` a parameter to hold one, and the length assertion below is
    /// what catches it growing for any other reason.
    #[test]
    fn a_detached_run_hands_tmux_a_filename_rather_than_a_script() {
        // One read of the fleet root, passed to both halves, so a neighbour thread setting
        // `$SKEIN_FLEET_ROOT` between them cannot make this test disagree with itself — and now
        // the lock and a fixture root as well, because a neighbour that has REMOVED the variable
        // is a refusal rather than a `/boxes` answer (SKEIN-690).
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        let path = detached_script_path("skein-update");
        let command = detach_command_at(&path, "skein-update");
        assert!(
            command.len() < TMUX_COMMAND_CEILING,
            "the command tmux is sent is {} bytes, and tmux refuses one over {TMUX_COMMAND_CEILING}",
            command.len()
        );
        // It runs the file, and it is the same file `detach_named` wrote. Quoted TWICE, which is
        // not a mistake and is worth pinning: the inner quoting is for the `/bin/sh -c` tmux runs
        // the command under, and the outer is for the `bash -lc` that `Place::exec` wraps the whole
        // thing in. One layer short and a fleet root containing a space runs `sh /boxes/my` and
        // reports an update that opened nothing.
        assert!(
            command.ends_with(&sh_quote(&format!("sh {}", sh_quote(&path)))),
            "the detached run does not open the script that was written for it: {command}"
        );
        // Independently of how the line above builds it: the file is where the writer puts it.
        assert!(
            command.contains(".skein/detached/skein-update.sh"),
            "the two halves name different files, so tmux would run nothing: {command}"
        );
        // And it still refuses a second run rather than starting one beside the first — the
        // property this function had before the fix and must not lose to it.
        assert!(command.contains("has-session"));
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// A warden on a port of its own: `/v1/fleet` answers, `/v1/publish` answers with whatever
    /// `publish` returns (`None` holds the request open for good), and every publish is counted.
    fn warden_at_start(
        publish: impl Fn() -> Option<(u16, String)> + Send + Sync + 'static,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let publishes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = publishes.clone();
        let publish = std::sync::Arc::new(publish);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let (counted, publish) = (counted.clone(), publish.clone());
                std::thread::spawn(move || {
                    let mut raw = [0u8; 8192];
                    let read = stream.read(&mut raw).unwrap_or(0);
                    let asked = String::from_utf8_lossy(&raw[..read]).to_string();
                    let (code, body) = match asked.split_whitespace().nth(1).unwrap_or("") {
                        "/v1/fleet" => {
                            (200, r#"{"sandboxes":[],"capabilities":["publish"]}"#.into())
                        }
                        "/v1/publish" => {
                            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            match publish() {
                                Some(answer) => answer,
                                None => loop {
                                    std::thread::sleep(Duration::from_secs(3600));
                                },
                            }
                        }
                        _ => (404, r#"{"error":"no such endpoint"}"#.into()),
                    };
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 {code} Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                });
            }
        });
        (port, publishes)
    }

    /// A once-per-start flag of the test's own, since the real one is the process's.
    fn fresh_start() -> &'static std::sync::atomic::AtomicBool {
        Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false)))
    }

    /// Asks the warden at `port` as a server start would, collecting what the log is given.
    fn start_asking(
        asked: &'static std::sync::atomic::AtomicBool,
        port: u16,
        quietly_for: Duration,
    ) -> (
        Option<std::thread::JoinHandle<()>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let said = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = said.clone();
        let asking = ask_at_start(
            asked,
            crate::warden_client::Warden::at("127.0.0.1", port),
            "skein-test-fleet".into(),
            7878,
            quietly_for,
            || Ok(()),
            move |line: &str| log.lock().unwrap().push(line.to_string()),
        );
        (asking, said)
    }

    const RAN: &str = r#"{"state":"ran","ok":true,"said":"published"}"#;

    /// A warden nobody answers at does not hold the server's start, and the log says why it waits.
    ///
    /// **What makes this fail**: the ask run on the caller's thread, or its thread joined before
    /// returning — the start then waits as long as the warden's terminal does.
    #[test]
    fn a_warden_that_never_answers_does_not_hold_the_server_start() {
        let (port, publishes) = warden_at_start(|| None);
        let (returned, came_back) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = returned.send(start_asking(fresh_start(), port, Duration::from_millis(200)).1);
        });
        let said = came_back
            .recv_timeout(Duration::from_secs(2))
            .expect("the server start waited on a warden that never answers");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while said.lock().unwrap().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the log never said it was waiting"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(publishes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            *said.lock().unwrap(),
            vec!["waiting on the warden to approve the port…".to_string()]
        );
    }

    /// The published line follows the waiting line only after the person approves.
    #[test]
    fn an_ask_a_person_approves_is_waited_on_and_then_announced() {
        let (port, _) = warden_at_start(|| {
            std::thread::sleep(Duration::from_millis(800));
            Some((200, RAN.into()))
        });
        let (asking, said) = start_asking(fresh_start(), port, Duration::from_millis(100));
        asking
            .expect("the first ask of a start was not made")
            .join()
            .unwrap();
        assert_eq!(
            *said.lock().unwrap(),
            vec![
                "waiting on the warden to approve the port…".to_string(),
                "the cockpit's port is published — 127.0.0.1:7878 reaches it now.".to_string(),
            ]
        );
    }

    /// One start asks once, however many times it is told to.
    ///
    /// **What makes this fail**: the once-per-start flag not consulted, so a second call asks the
    /// warden again — and a person at its terminal is asked twice about one port.
    #[test]
    fn a_server_start_asks_the_warden_exactly_once() {
        let (port, publishes) = warden_at_start(|| Some((200, RAN.into())));
        let asked = fresh_start();
        let (first, _) = start_asking(asked, port, Duration::from_secs(30));
        let (second, _) = start_asking(asked, port, Duration::from_secs(30));
        for asking in [first, second].into_iter().flatten() {
            asking.join().unwrap();
        }
        assert_eq!(publishes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A mapping the warden found in place is the usual start, and it leaves the log alone.
    ///
    /// **What makes this fail**: `already` read as a publish that ran, which announces a mapping
    /// nobody made at every start.
    #[test]
    fn a_mapping_already_in_place_is_asked_about_and_announces_nothing() {
        let (port, publishes) = warden_at_start(|| {
            Some((
                200,
                r#"{"state":"already","ok":true,"said":"7878:7878/tcp is already published for skein-test-fleet, so nothing was asked and nothing ran"}"#.into(),
            ))
        });
        let (asking, said) = start_asking(fresh_start(), port, Duration::from_secs(30));
        asking
            .expect("the first ask of a start was not made")
            .join()
            .unwrap();
        assert_eq!(publishes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(*said.lock().unwrap(), Vec::<String>::new());
    }

    /// A refusal is logged as the prompt a person would be shown, naming what the warden said.
    #[test]
    fn a_refused_ask_logs_the_prompt_and_is_not_repeated() {
        let (port, publishes) = warden_at_start(|| {
            Some((
                409,
                r#"{"state":"refused","error":"the person declined"}"#.into(),
            ))
        });
        let asked = fresh_start();
        let (asking, said) = start_asking(asked, port, Duration::from_secs(30));
        asking
            .expect("the first ask of a start was not made")
            .join()
            .unwrap();
        assert!(start_asking(asked, port, Duration::from_secs(30))
            .0
            .is_none());
        let said = said.lock().unwrap();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].contains("The warden was asked first: the person declined"),
            "{said:?}"
        );
        assert_eq!(publishes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// With no warden answering, a start asks nothing and says nothing — today's behaviour.
    ///
    /// **What makes this fail**: asking without first seeing a warden there, which turns the
    /// transport error into a prompt block in the log of every start on a machine with no warden.
    #[test]
    fn with_no_warden_a_server_start_asks_nothing_and_says_nothing() {
        // A port that was just bound and let go, so nothing is listening on it.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (asking, said) = start_asking(fresh_start(), port, Duration::from_secs(30));
        asking
            .expect("the first ask of a start was not made")
            .join()
            .unwrap();
        assert_eq!(*said.lock().unwrap(), Vec::<String>::new());
    }

    /// A doorway that does not hold the port refuses the ask before the warden is asked (§9.4).
    #[test]
    fn a_port_the_doorway_does_not_hold_is_not_asked_for() {
        let (port, publishes) = warden_at_start(|| Some((200, RAN.into())));
        let said = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = said.clone();
        ask_at_start(
            fresh_start(),
            crate::warden_client::Warden::at("127.0.0.1", port),
            "skein-test-fleet".into(),
            7878,
            Duration::from_secs(30),
            || Err("not held".to_string()),
            move |line: &str| log.lock().unwrap().push(line.to_string()),
        )
        .expect("the first ask of a start was not made")
        .join()
        .unwrap();
        assert_eq!(*said.lock().unwrap(), vec!["not held".to_string()]);
        assert_eq!(publishes.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// **The doorway is found, and reloaded, one way** (SKEIN-1199).
    ///
    /// Rust reloaded it with `pkill -USR1 -f` on a command-line pattern while `start-door.sh` sent
    /// `kill -USR1` to the stamp's pid: two answers to "which process is the doorway". Both now run
    /// `src/door.sh`. What each assertion catches:
    /// - a Rust script that stops starting with `door.sh` — a reload re-typed beside it;
    /// - `start-door.sh`'s copy edited, or `door.sh` edited and not pasted into it;
    /// - a `SIGUSR1` sent from anywhere else under `src/` or in `bootstrap.sh`, `pkill` included.
    ///
    /// That the reload reaches the doorway at all, with and without its stamp, is
    /// `tests/fleet_move.rs`, against a real doorway.
    #[test]
    fn the_doorway_is_found_one_way() {
        // The scripts name fleet paths, which refuse to resolve in an unpinned test; nothing is
        // run and nothing is written, so any directory that is not a live fleet will do.
        let _g = crate::testutil::env_lock();
        let tmp = crate::testutil::tempdir();
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_FLEET_ROOT", tmp.join("fleet"))
            .set("SKEIN_HOME", tmp.join("home"));
        for (who, script) in [
            ("door_pid", door_pid_script(server_sandbox_port())),
            ("reload_server", reload_command()),
        ] {
            assert!(
                script.starts_with(DOOR_SH),
                "{who} does not run src/door.sh, so it can pick a different process from start-door.sh"
            );
        }

        const BEGIN: &str = "# >>> door.sh";
        const END: &str = "# <<< door.sh\n";
        let bootstrap = include_str!("../../bootstrap.sh");
        assert_eq!(
            bootstrap.matches(BEGIN).count(),
            1,
            "bootstrap.sh must carry exactly one copy of src/door.sh, inside start-door.sh"
        );
        let begin = bootstrap.find(BEGIN).unwrap();
        let body = begin + bootstrap[begin..].find('\n').unwrap() + 1;
        let end = body
            + bootstrap[body..]
                .find(END)
                .expect("start-door.sh opens its copy of src/door.sh and never closes it");
        assert!(
            bootstrap[body..end] == *DOOR_SH,
            "start-door.sh's copy of src/door.sh differs from the file. Edit src/door.sh and paste \
             it between the markers, whole."
        );

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = vec![root.join("bootstrap.sh")];
        let mut dirs = vec![root.join("src")];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs" || x == "sh") {
                    files.push(p);
                }
            }
        }
        assert!(files.len() > 100, "the walk found {} files", files.len());
        // Spelled in two pieces so this line is not itself one the scan finds.
        let signal = format!("kill -{}", "USR1");
        let mut senders = Vec::new();
        for f in &files {
            let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel == "src/door.sh" {
                continue;
            }
            let text = std::fs::read_to_string(f).unwrap();
            let outside = match (text.find(BEGIN), text.find(END)) {
                (Some(b), Some(e)) => format!("{}{}", &text[..b], &text[e..]),
                _ => text,
            };
            for line in outside.lines() {
                let l = line.trim_start();
                if l.starts_with('#') || l.starts_with("//") {
                    continue;
                }
                if l.contains(&signal) {
                    senders.push(format!("{rel}: {}", line.trim()));
                }
            }
        }
        assert!(
            senders.is_empty(),
            "the doorway is signalled outside src/door.sh — call skein_door_reload instead:\n{}",
            senders.join("\n")
        );
    }

    /// A stamp is believed only about the doorway it names: the pid must be alive, the port its
    /// own, AND its command line the doorway's. Run against the real src/door.sh, with the
    /// supervisor's sockets absent, so the stamp is the only way to a pid.
    ///
    /// A pid is a number the kernel hands out again. A doorway killed without clearing its stamp
    /// leaves one naming whatever process is given that pid next, and SIGUSR1's default action ends
    /// most processes — so a reload trusting the pid alone kills a stranger.
    ///
    /// **What would make this fail:** deleting the command-line check from `skein_stamped_door`
    /// (in both copies, so the copy test above stays green) — the stranger gets the signal. Or
    /// deleting the port check — the doorway stamped for another port gets it. The last case is
    /// the control: the same process, stamped for its own port, IS signalled, so the two refusals
    /// are not a script that signals nothing.
    #[test]
    fn a_stale_stamp_does_not_signal_whatever_now_has_its_pid() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let tmp = crate::testutil::tempdir();
        let doorway = tmp.join("server-doorway.py");
        let stamp = tmp.join("server.door");
        let absent = tmp.join("no-supervisor.tmux");
        let port = 47917;

        // A process that records SIGUSR1 instead of dying of it. `$0` is `name`, so its command
        // line holds `name` as one argument: the doorway's path for a doorway, anything else for a
        // stranger.
        let spawn = |name: &std::path::Path, marker: &std::path::Path| {
            Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "trap 'touch {m}' USR1; while :; do sleep 0.05; done",
                    m = crate::util::sh_quote(&marker.to_string_lossy())
                ))
                .arg(name)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn a stand-in")
        };
        let reload = || {
            Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "{DOOR_SH}\nskein_door_reload {} {port} {} {}",
                    crate::util::sh_quote(&stamp.to_string_lossy()),
                    crate::util::sh_quote(&doorway.to_string_lossy()),
                    crate::util::sh_quote(&absent.to_string_lossy()),
                ))
                .output()
                .expect("run src/door.sh")
        };
        let signalled = |marker: &std::path::Path| {
            let until = Instant::now() + Duration::from_millis(600);
            while Instant::now() < until {
                if marker.exists() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };

        let stranger_marker = tmp.join("stranger.usr1");
        let mut stranger = spawn(&tmp.join("not-the-doorway"), &stranger_marker);
        let door_marker = tmp.join("doorway.usr1");
        let mut door = spawn(&doorway, &door_marker);
        let cases = || -> Result<(), String> {
            // The stranger: alive, right port, not the doorway.
            std::fs::write(&stamp, format!("{} {port}\n", stranger.id())).unwrap();
            let out = reload();
            if signalled(&stranger_marker) {
                return Err(
                    "a stamp naming a live pid whose command line is not the doorway was \
                            believed, and that process was sent SIGUSR1"
                        .into(),
                );
            }
            if out.status.success() {
                return Err(format!(
                    "skein_door_reload reported a reload with no doorway to reload: {}",
                    String::from_utf8_lossy(&out.stdout)
                ));
            }
            // The doorway, stamped for a port it does not hold.
            std::fs::write(&stamp, format!("{} {}\n", door.id(), port + 1)).unwrap();
            let out = reload();
            if signalled(&door_marker) || out.status.success() {
                return Err(
                    "a stamp for another port was believed, and its doorway was sent \
                            SIGUSR1"
                        .into(),
                );
            }
            // The control: the same doorway, stamped for this port.
            std::fs::write(&stamp, format!("{} {port}\n", door.id())).unwrap();
            let out = reload();
            if !signalled(&door_marker) || !out.status.success() {
                return Err(format!(
                    "the doorway holding this port by its stamp was not signalled, so the refusals \
                     above prove nothing: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            Ok(())
        };
        let verdict = cases();
        let _ = stranger.kill();
        let _ = stranger.wait();
        let _ = door.kill();
        let _ = door.wait();
        if let Err(why) = verdict {
            panic!("{why}");
        }
    }
}
