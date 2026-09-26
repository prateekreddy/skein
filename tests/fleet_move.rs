//! The move (delivery §3 4c), end to end against a fake `sbx`: skein-server is installed into the
//! fleet sandbox over stdin, started behind a socket that was opened first, and published — with
//! the host path untouched, one unset variable away.
//!
//! Same shape as `tests/fleet_launch/` and for the same reason: no `sbx` exists here, but a
//! sandbox is a Linux machine with `tmux` and `python3` — and so is this one — so `sbx exec` can
//! mean "run it here" and everything except sbx's own behaviour is genuinely exercised: a real
//! byte-for-byte install through the stdin pipe, a real doorway process holding a real listening
//! socket, a real fork-and-exec handover checked from the inheriting side.
//!
//! What no test here can claim, said plainly: the sandbox lifecycle itself. `sbx create` with the
//! volume mounted, the published mapping reaching the sandbox's address, and a real skein-server
//! answering through it need a live fleet — the runnable checklist for that lives with SKEIN-109.
//!
//! Four more that need a live fleet, added with SKEIN-105 (the door at create, and the doorway
//! surviving its own restart). Each is a line to run on a real one, not a claim made here:
//!
//!   1. **Create, then look before serving.** Destroy the fleet through the warden, start a box,
//!      and inside the sandbox check `ss -ltnp | grep :7878` names `python3 …/server-doorway.py` —
//!      the door is held on a fleet nobody has run `skein fleet-serve` against yet.
//!   2. **Upgrade with the door open.** With the cockpit serving and a browser tab on it, run
//!      `skein fleet-serve` again; note the doorway's pid before and after
//!      (`pgrep -f server-doorway.py`) and `readlink /proc/<pid>/fd/3` — both unchanged, and the
//!      tab reconnects without the URL changing.
//!   3. **The port survives a killed doorway.** `kill -9` the doorway inside the sandbox and,
//!      from the host, connect to the published port in a loop: it should refuse for well under a
//!      second. `PR_SET_PDEATHSIG` is a Linux syscall the sandbox image has to honour, and the
//!      symptom if it does not is an orphaned `skein-server` still on :7878 that nothing replaces.
//!   4. **The squat refusal on a real image.** Bind :7878 inside the sandbox from a box, then run
//!      `skein fleet-serve` from the host: it must refuse naming §9.4 and publish nothing —
//!      `sbx ports <fleet>` unchanged, because skein never calls `--unpublish` (the argument is
//!      carried once, at `fleet::stop_serving`).

mod common;

use common::{env_lock, env_pins, have, skip, EnvPins, Scratch};
use skein::fleet::{
    cockpit_port_advice, ensure_fleet, ensure_fleet_door, fleet_serve_mounts, reload_server,
    server_door_stamp_path, server_path, server_tmux_sock, server_tmux_sock_in, start_server,
    stop_server, stop_serving,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const FLEET: &str = "test-fleet";

/// A stand-in for `sbx` that runs the guest command locally and RECORDS every invocation, so the
/// start sequence can be asserted as an ordering rather than trusted as a comment.
fn write_fake_sbx(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    fs::write(
        &p,
        r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SBX_LOG"
verb="$1"; shift
case "$verb" in
  create) exit 0 ;;
  rm) exit 0 ;;
  # `ports <sandbox>` lists nothing (a fresh fleet has no mappings); a `--publish` succeeds and is
  # already on the log line above, which is how the test sees it happened and in what order.
  ports) exit 0 ;;
  exec)
    while [ $# -gt 0 ]; do case "$1" in -*) shift ;; *) break ;; esac; done
    shift          # the sandbox name
    exec "$@" ;;
  *) echo "fake sbx: unsupported verb $verb" >&2; exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// **What skein ran at fleet scope, recorded — through the execution seam, not a fake on `$PATH`.**
///
/// The recorder used to be an `sbx` shell script: skein's every crossing went through `sbx exec`,
/// so a fake there saw the whole transcript. There is no hop to intercept (SKEIN-576), and a fake
/// `sbx` is simply never invoked — so the log stayed empty and the assertions over it became
/// assertions about nothing. `place::seam` is where a test says what a fleet-scope command runs, and
/// nothing outside this process can select it (SKEIN-592).
///
/// `run` decides whether the command is also PERFORMED. `false` is for the tests whose claim is an
/// order of operations rather than an effect: `ensure_fleet` runs apt through the substrate and
/// writes `/etc/docker` through `install_docker_config`, and performing those would do both to the
/// machine running the suite. Stdin is still drained in that mode rather than ignored — an install
/// writes megabytes down this pipe, and a reader that exits first turns the write into EPIPE, which
/// the caller reports as its own failure.
fn record_fleet_scope(log: PathBuf, run: bool) -> skein::place::seam::Installed {
    skein::place::seam::install(Box::new(move |argv: &[String]| {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .expect("the transcript");
        writeln!(f, "{}", argv.join(" ")).expect("record the crossing");
        match run {
            true => None,
            false => Some(vec![
                "sh".to_string(),
                "-c".into(),
                "cat >/dev/null 2>&1".into(),
            ]),
        }
    }))
}

/// Not under `/tmp` for the same reason as `fleet_launch`'s scratch, and per-pid so two cargo
/// invocations cannot collide. The prefix is unchanged on purpose: the leaked-process gate counts
/// `ps` lines matching `skein-move-it-`.
///
/// **A teardown that outlives a panic**, which is what `common::Scratch` is for — and this file is
/// where the need was found. Every test here ended by removing its root, and a failing assertion
/// unwinds straight past that. What is left behind is not an idle directory: the supervisor these
/// tests start is `while [ -f <root>/boxes/.skein/server-doorway.py ]; do … done`, so its exit
/// condition is a file inside the very directory the teardown was going to remove. It keeps
/// restarting itself, and the server with it, for as long as that file survives. Four such
/// processes were found by the leaked-process gate on 2026-08-31, after an intermittent failure
/// here — and that gate reports a NUMBER rather than pass or fail, so a leak nobody clears makes
/// every later run's count wrong: the leak does not merely persist, it hides the next one.
///
/// So the quiesce below runs on **every** path, panic included, while only the directory's removal
/// is skipped when the test failed. **The order is load-bearing**: the doorway script first,
/// because removing it is the loop's own exit condition; then the tmux server; then a beat for the
/// supervisor to notice. Killing tmux while the script is still on disk is how a supervisor started
/// by an `sbx exec` somewhere else comes back.
///
/// Both paths are derived from the root rather than from `$SKEIN_FLEET_ROOT`, because by the time
/// this runs the environment is whatever the test last set — and a teardown that reads a variable
/// the failure may have left wrong is a teardown that cleans up somebody else's fleet.
///
/// **Derived from `fleet::server_tmux_sock_in` and not spelled here** (SKEIN-529). This was
/// `skein.join("server.tmux")`, which is the shape that makes a socket move a silent leak rather
/// than a failure: a `kill-server` aimed at a path nothing listens on exits non-zero into a `let _`,
/// and every test in this file then leaves a tmux server, a supervisor shell and a python behind.
/// The parameter exists so there is one definition of the path and this is not a second one.
fn scratch() -> Scratch {
    Scratch::boxes("skein-move-it").quiesce_with(|root| {
        let fleet_root = root.join("boxes");
        let skein = fleet_root.join(".skein");
        let _ = fs::remove_file(skein.join("server-doorway.py"));
        let _ = Command::new("tmux")
            .args([
                "-S",
                &server_tmux_sock_in(fleet_root.to_string_lossy().as_ref()),
                "kill-server",
            ])
            .status();
        // And the socket every fleet had before SKEIN-529, which the upgrade tests at the end of
        // this file start a supervisor on — see `legacy_sock`. Usually finds nothing.
        let _ = Command::new("tmux")
            .args(["-S", &legacy_sock(&fleet_root), "kill-server"])
            .stderr(std::process::Stdio::null())
            .status();
        std::thread::sleep(Duration::from_millis(250));
    })
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn connects(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// The server behind the door is handed the doorway's own socket, and the door stays open while
/// that server never accepts.
///
/// **This used to be two tests in one body** (SKEIN-576). The first half drove `install_server` —
/// a 1 MiB payload down a stdin pipe, landing 0755, then an install → start → publish ordering —
/// and every part of that belonged to a skein OUTSIDE the sandbox carrying a binary in. There is
/// no such skein now: `bootstrap.sh` builds the server where it runs, and the publish is a
/// person's. That half is gone with the thing it tested.
///
/// This half is the handover, and it survives untouched because it was never about the installer:
/// the observer adopts descriptor 3 exactly as `doorway.rs` would, records what it was handed, and
/// then sleeps without ever accepting — so the connect at the end succeeding is proof the DOORWAY
/// holds the socket and not the server. The binary arrives by being written to `server_path()`,
/// which is what an install does from inside and what the neighbouring reload tests already do.
#[test]
fn the_server_behind_the_door_inherits_the_doorways_socket() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());
    let home = root.join("skein");

    ensure_fleet_door(FLEET).expect("the door opens before there is a server to put behind it");
    assert!(wait_for_door(port), "the door never opened");

    // ---- the handover, checked from the inheriting side ----
    stop_server(FLEET);
    let obs = format!("{}/obs.json", root.join("boxes").join(".skein").display());
    fs::write(
        server_path(),
        "#!/usr/bin/env python3\n\
         import json, os, socket, sys, time\n\
         s = socket.socket(fileno=3)\n\
         out = os.path.join(os.path.dirname(sys.argv[0]), 'obs.json')\n\
         json.dump({\n\
             'listen_fds': os.environ.get('LISTEN_FDS'),\n\
             'listen_pid': os.environ.get('LISTEN_PID'),\n\
             'pid': os.getpid(),\n\
             'inherited_only': os.environ.get('SKEIN_LISTEN_INHERITED_ONLY'),\n\
             'skein_home': os.environ.get('SKEIN_HOME'),\n\
             'bound': s.getsockname()[1],\n\
         }, open(out, 'w'))\n\
         time.sleep(120)\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(server_path(), fs::Permissions::from_mode(0o755)).unwrap();
    }
    start_server(FLEET).expect("start behind the held door");
    let mut open = false;
    for _ in 0..100 {
        if connects(port) {
            open = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        open,
        "the cockpit's door is not open: the server was started without the doorway opening the \
         socket first, which is the §9.4 race run by the process meant to close it"
    );
    let mut report = String::new();
    for _ in 0..100 {
        report = fs::read_to_string(&obs).unwrap_or_default();
        if !report.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!report.is_empty(), "the server behind the door never ran");
    let v: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(v["listen_fds"], "1", "the convention names one descriptor");
    assert_eq!(
        v["listen_pid"].as_str().unwrap().parse::<i64>().unwrap(),
        v["pid"].as_i64().unwrap(),
        "LISTEN_PID must be the server's own pid — exec keeps it — or doorway.rs refuses the \
         descriptor as somebody else's"
    );
    assert_eq!(
        v["inherited_only"], "1",
        "a fleet server that lost its descriptor must refuse to bind, not run the race itself"
    );
    assert_eq!(
        v["skein_home"].as_str().unwrap(),
        home.to_string_lossy(),
        "the server reads the volume at its mounted path"
    );
    assert_eq!(
        v["bound"].as_u64().unwrap(),
        port as u64,
        "descriptor 3 is the cockpit's socket, not some other fd"
    );

    // While the observer sleeps, the door still answers: the socket belongs to the doorway.
    assert!(
        connects(port),
        "the door must stay open while the server never accepts — the doorway holds the socket"
    );
}

/// Mounting the volume is the move's one create-time difference: the volume root, and then only
/// the mounts it does not already contain.
///
/// It was a stated grant until SKEIN-219 — the launcher skipped covering ancestors of its own
/// binds, so a volume-mounted fleet was readable from every box, and `--uncovered-volume` was the
/// only way to take that. The launcher covers ancestors first now, and
/// `tests/isolation_bwrap/cover.rs::a_box_on_a_mounted_volume_cannot_read_the_fleets_credentials` is
/// where that is proved against a real namespace; here the claim is only about the mount SET.
#[test]
fn the_volume_mount_is_the_volume_root_plus_the_strays_outside_it() {
    let _env = env_lock();
    let root = scratch();
    let home = root.join("skein");
    fs::create_dir_all(&home).unwrap();
    // After `root`, so the pin goes back before the directory it names is removed.
    let mut pins = env_pins();
    pins.set("SKEIN_HOME", &home);

    let mounts = fleet_serve_mounts();
    assert_eq!(
        mounts.first().map(String::as_str),
        Some(home.to_string_lossy().as_ref()),
        "the volume root is the mount"
    );
    assert!(
        mounts
            .iter()
            .skip(1)
            .all(|m| !m.starts_with(&*home.to_string_lossy())),
        "everything under the volume is already visible through it; mounting a path twice is not \
         obviously harmless: {mounts:?}"
    );
}

// --- SKEIN-105: the port is never free ------------------------------------------------------
//
// Two things the doorway did not do when it landed, and each is a way the port stands empty with
// boxes already running. It opened at `skein fleet-serve` — so a box created before that could
// take the port, become the cockpit, and be handed the fleet token on the browser's first request
// (architecture §9.4). And it did not survive its own restart: an upgrade was a stop and a start,
// which closes the socket and re-binds it, running the race from the process that exists to close
// it.

/// The doorway's pid, read the way skein reads it: off the stamp it writes once it holds the port.
fn door_pid() -> Option<u32> {
    let stamp = fs::read_to_string(server_door_stamp_path()).ok()?;
    stamp.split_whitespace().next()?.parse().ok()
}

/// Which socket descriptor 3 of `pid` is. Identical before and after an upgrade means the listener
/// was carried across rather than closed and re-opened — the one observation that distinguishes a
/// handover from a fast re-bind, and it is available because the "sandbox" here is this machine.
fn door_socket(pid: u32) -> String {
    fs::read_link(format!("/proc/{pid}/fd/3"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Put a server at the installed path, the way an install leaves one.
fn write_server(body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = PathBuf::from(server_path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Did a server tagged `tag` run behind the door? The doorway polls for a missing binary, so this
/// waits rather than asking once.
fn ran_says(tag: &str, out: &Path) -> bool {
    for _ in 0..100 {
        if fs::read_to_string(out).unwrap_or_default().contains(tag) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn wait_for_door(port: u16) -> bool {
    for _ in 0..100 {
        if connects(port) && door_pid().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Takes a staged fleet down **on the way out of the test, however it leaves**.
///
/// `unstage` used to be the last line of each test, which is precisely where cleanup does not
/// happen: a failing test panics before it and leaves a doorway holding a socket against a fixture
/// nobody will ever delete. `fleet::supervised` does not reach these and cannot — the doorway is
/// *healthy*, so its loop never comes back round to notice its fleet is gone. Two sabotage runs
/// while writing the test above left six such processes alive, which is how this was noticed.
///
/// Declared after the two locks in each test and so dropped before them: the fleet comes down and
/// the environment is unset while this test still holds the turn.
///
/// **The second field says this suite's crossings are real** (SKEIN-530). `Place::spawning` refuses
/// a fleet-scope command in a test process that has installed no stand-in, and this is one of the
/// two suites whose subject is the command itself: the door tests below assert that a doorway keeps
/// its socket across a re-exec, and no substitution can demonstrate that. What keeps it inside the
/// fixture is `stage`'s `$SKEIN_FLEET_ROOT`, at the scratch tree this drop removes.
///
/// It rides on the staging rather than on each test because `unstage` crosses too — `stop_server`
/// is a fleet-scope command — and a declaration held by the test body alone would already be gone
/// by the time this ran, on the unwind path where it matters most. The field is dropped after the
/// body, which is what makes that true. A test that wants a stand-in still installs one:
/// [`record_fleet_scope`] is consulted first, so it wins wherever it is asked for.
struct Staged(EnvPins, skein::place::seam::Real);

impl Staged {
    /// Pin one more variable for the life of this staging, restored with the rest.
    ///
    /// Here rather than on the [`EnvPins`] `stage` returns, so that the restore happens **after**
    /// `unstage` rather than before it: the one caller pins `$SKEIN_LS_CMD`, and `unstage` removes
    /// that name itself only once `stop_server` has run.
    fn pin(&mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) -> &mut Staged {
        self.0.set(name, value);
        self
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        // The body runs before the field, so the environment is still staged while `unstage` uses
        // it — `stop_server` reads `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` to find what it is
        // stopping. The pins go back on the line after, before the scratch directory they name is
        // removed.
        unstage();
    }
}

/// Stage a fake fleet: a recording `sbx` on PATH, a scratch volume, and a free cockpit port.
fn stage(root: &Path) -> (u16, EnvPins) {
    write_fake_sbx(&root.join("bin"));
    fs::write(root.join("sbx.log"), "").unwrap();
    let mut pins = env_pins();
    pins.set("SBX_LOG", root.join("sbx.log"));
    // **`$PATH` is prepended to, which is why it has to be RESTORED and not removed.** `unstage`
    // never touched it, so before this every test in the binary added another fake-`sbx` directory
    // to the front of the same `$PATH` and left it there — nine of them by the end of a run, all
    // naming scratch directories that had been deleted.
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let home = root.join("skein");
    fs::create_dir_all(&home).unwrap();
    pins.set("SKEIN_HOME", &home);
    pins.set("SKEIN_FLEET_ROOT", root.join("boxes"));
    // **Which fleet this skein is standing in.** In-fleet, `Place` refuses to address any sandbox
    // but its own — `sbx` is host-only, so there is no second hop to reach another with — and it
    // decides that by comparing the address against `config.fleet_sandbox`. Without this the
    // fixture asks an in-fleet skein to reach a sandbox called `test-fleet` from inside a sandbox
    // it believes is called something else, and every door test fails on the refusal rather than
    // on what it is about. Written to the staged config rather than assumed, because that is where
    // production reads it from.
    fs::write(
        home.join("config.json"),
        format!("{{\n  \"fleet_sandbox\": \"{FLEET}\"\n}}\n"),
    )
    .unwrap();
    let port = free_port();
    pins.set("SKEIN_SERVER_PORT", port.to_string());
    (port, pins)
}

fn unstage() {
    stop_server(FLEET);
    let _ = Command::new("tmux")
        .args(["-S", &server_tmux_sock(), "kill-server"])
        .status();
    for var in [
        "SKEIN_SERVER_BINARY",
        "SKEIN_SERVER_PORT",
        "SBX_LOG",
        "SKEIN_LS_CMD",
        "SKEIN_RUNTIME_PACKAGES",
    ] {
        std::env::remove_var(var);
    }
}

/// An observer that adopts descriptor 3 the way `doorway.rs` does, records which socket it was
/// handed, and then holds it — so what ran behind the door is readable afterwards.
fn observer(tag: &str, out: &Path) -> String {
    format!(
        "#!/usr/bin/env python3\n\
         import os, socket, time\n\
         s = socket.socket(fileno=3)\n\
         open({out:?}, 'a').write('{tag} %d %d\\n' % (os.getpid(), s.getsockname()[1]))\n\
         time.sleep(300)\n",
        out = out.to_string_lossy(),
    )
}

/// The door is open with nothing behind it — which is what makes opening it at fleet **create**
/// possible at all, since the binary does not arrive until somebody runs `skein fleet-serve`.
#[test]
fn the_door_opens_before_there_is_a_server_to_put_behind_it() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());

    // No `install_server`, and no binary anywhere: `server_path()` does not exist.
    ensure_fleet_door(FLEET).expect("the door opens with no server installed");
    assert!(
        wait_for_door(port),
        "the cockpit's port is free on a fleet that exists — the first box to bind :{port} \
         becomes the cockpit (architecture §9.4)"
    );
    assert!(
        !Path::new(&server_path()).exists(),
        "the point of this test is that nothing was installed behind the door"
    );
    let pid = door_pid().expect("the doorway stamps the port it holds");
    assert!(
        door_socket(pid).starts_with("socket:"),
        "the stamped process is not holding a socket at descriptor 3"
    );

    // And a second ensure is a no-op rather than a restart: the launch path must not close the
    // door it is there to keep open.
    ensure_fleet_door(FLEET).expect("a second ensure");
    assert_eq!(
        door_pid(),
        Some(pid),
        "an ensure on an open door replaced the doorway, which closes the port to re-open it"
    );
}

/// A doorway whose stamp went missing is repaired by the next box start, not only by a serve
/// (SKEIN-226).
///
/// The stamp is how `door_holds_port` tells "the doorway holds the port" from "something does",
/// and without it every publish refuses — correctly, since a squatter accepts a connect too. But
/// `start_server` cannot put it back: the tmux session is already there, so it returns having done
/// nothing. A re-exec re-stamps across the same descriptor, so the repair costs neither the socket
/// nor the port.
#[test]
fn a_door_that_lost_its_stamp_is_re_stamped_without_closing() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());

    ensure_fleet_door(FLEET).expect("the door opens");
    assert!(wait_for_door(port), "the door never opened");
    let pid = door_pid().expect("the doorway stamps the port it holds");
    // The socket the door is holding, by identity. A re-exec keeps the PID — that is what `exec`
    // means — so the pid says nothing about whether the descriptor survived, and the inode says
    // everything: same socket, never closed.
    let socket = door_socket(pid);

    // The stamp, and only the stamp, goes. The doorway is still alive and still holding :port.
    fs::remove_file(server_door_stamp_path()).expect("the stamp was there to remove");
    assert!(
        connects(port),
        "removing the stamp closed the port, which is not what this test is about"
    );

    ensure_fleet_door(FLEET).expect("a box start repairs the door");
    assert!(
        wait_for_door(port),
        "the door was left unstamped, so every later publish refuses until somebody serves twice"
    );
    let after = door_pid().expect("the repair re-stamped");
    assert_eq!(
        after, pid,
        "the repair replaced the doorway instead of re-execing it, which closes the port to \
         re-open it — the squat window itself"
    );
    assert_eq!(
        door_socket(after),
        socket,
        "descriptor 3 is a different socket after the repair, so the door was closed and re-bound"
    );
    assert!(connects(port), "the port is not answering after the repair");
}

/// `ensure_fleet` opens the door **before** it installs the launcher — and the launcher is what
/// makes a box in this sandbox possible at all, so that ordering is the whole item: there is no
/// interval in which a box and a free cockpit port coexist.
///
/// Driven with an `sbx` that records and does nothing, because what is under test is an order of
/// operations and not their effects — `ensure_fleet` otherwise runs apt and writes `/etc/docker`.
#[test]
fn the_door_is_open_before_the_launcher_that_makes_boxes_possible() {
    let _env = env_lock();
    let root = scratch();
    let (port, pins) = stage(&root);
    let mut teardown = Staged(pins, skein::place::seam::real_crossings());
    // Recording only: every fleet-scope command is logged and nothing is run.
    let _recorder = record_fleet_scope(root.join("sbx.log"), false);
    // The fleet already exists, so nothing is created and the warden is never asked. Pinned through
    // the staging rather than beside it, so `unstage` still removes this name with `stop_server`
    // behind it before the prior value comes back.
    teardown.pin(
        "SKEIN_LS_CMD",
        format!("echo '[{{\"name\":\"{FLEET}\",\"status\":\"running\"}}]'"),
    );
    skein::sbx::forget_fleet_boxes();

    let _ = ensure_fleet(FLEET);

    let seq = fs::read_to_string(root.join("sbx.log")).unwrap();
    let door = seq
        .lines()
        .position(|l| l.contains("server-doorway.py"))
        .unwrap_or_else(|| panic!("the door was never opened by ensure_fleet:\n{seq}"));
    let launcher = seq
        .lines()
        // The install, not a mention: the substrate script names `box-session.sh` in a comment,
        // and matching that would find the launcher before anything installed it.
        .position(|l| l.contains("cat >") && l.contains("box-session.sh"))
        .unwrap_or_else(|| panic!("the launcher was never installed:\n{seq}"));
    assert!(
        door < launcher,
        "the launcher was installed before the cockpit's port was held: a box started in that \
         window binds :{port} and becomes the cockpit (architecture §9.4)\n{seq}"
    );
    // Nothing reached for `sbx` at all — which subsumes the `create` this used to look for, and
    // catches the rest of the host tool with it. `sbx` is host-only and skein is not on the host.
    assert!(
        !seq.split_whitespace().any(|w| w == "sbx"),
        "skein reached for `sbx`, which is host-only and not here:\n{seq}"
    );
}

/// A reload upgrades the server **across the same listening socket**. The doorway keeps its pid
/// (an `exec` replaces the image, not the process) and descriptor 3 keeps its socket — which is
/// the difference between a handover and a fast re-bind, and only the first leaves no window.
///
/// The observers are `#!` scripts rather than the carried ELF, so they are written straight to the
/// installed path: what is under test is the *socket* across an upgrade, and running two
/// distinguishable servers is the only way to see that the second one got the first one's.
#[test]
fn a_reload_upgrades_the_server_without_ever_closing_the_door() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());
    let ran = root.join("ran.txt");

    ensure_fleet_door(FLEET).expect("the door opens with no server behind it");
    assert!(wait_for_door(port), "the door never opened");
    let pid = door_pid().expect("the doorway stamps the port it holds");
    let socket = door_socket(pid);

    // The binary arrives after the door, which is the create-then-serve order in miniature.
    write_server(&observer("first", &ran));
    assert!(
        ran_says("first", &ran),
        "the first server never ran behind the door"
    );

    write_server(&observer("second", &ran));
    assert!(
        reload_server(FLEET),
        "there was no doorway to reload, so a re-serve would have to start one from nothing"
    );
    assert!(
        ran_says("second", &ran),
        "the reload did not replace the server behind the door"
    );

    // A *second* reload, because the first one re-execs and could rewrite the command line skein
    // finds this process by. `pkill -f` matches an anchored `^python[0-9.]* <doorway>`, so a
    // re-exec spelled with the interpreter's full path would leave a doorway nothing can signal —
    // and the symptom would be a re-serve that silently starts a second one.
    write_server(&observer("third", &ran));
    assert!(
        reload_server(FLEET),
        "the reloaded doorway is no longer findable by the pattern skein signals it with"
    );
    assert!(
        ran_says("third", &ran),
        "the second reload did not replace the server behind the door"
    );

    assert_eq!(
        door_pid(),
        Some(pid),
        "the doorway was replaced rather than reloaded — a new process means a new bind, and the \
         gap between the old close and it is architecture §9.4's window"
    );
    assert_eq!(
        door_socket(pid),
        socket,
        "descriptor 3 is a different socket after the upgrade: the listener was closed and \
         re-opened, which is the race the doorway exists to close"
    );
    // Both were handed the same port, so the second is serving through the first's socket.
    let who = fs::read_to_string(&ran).unwrap();
    assert!(
        who.lines().all(|l| l.ends_with(&format!(" {port}"))),
        "a server behind the door was handed a socket that is not the cockpit's: {who:?}"
    );
}

/// An upgrade against a live fleet takes that path: it reloads the running doorway rather than
/// stopping and starting one, which is what makes an upgrade windowless.
///
/// **The driver changed and the property did not** (SKEIN-576). It used to be `ensure_fleet_server`
/// — a skein outside the sandbox carrying a new binary in — and it is now the binary arriving on
/// disk and `reload_server` being asked to swap it, which is what `bootstrap.sh` does from inside.
/// The transcript is the assertion rather than the outcome: `-USR1` to the doorway that is already
/// there, and no `new-session`, because a second doorway can only mean the first gave up its
/// socket.
#[test]
fn an_upgrade_reloads_the_running_doorway_rather_than_restarting_it() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());
    let ran = root.join("ran.txt");

    // The door first, as `ensure_fleet` opens it at create.
    ensure_fleet_door(FLEET).expect("the door opens");
    assert!(wait_for_door(port), "the door never opened");
    let pid = door_pid().expect("a doorway");
    let socket = door_socket(pid);

    // Logged AND run: this test's doorway is a real process and the reload has to reach it.
    let _recorder = record_fleet_scope(root.join("sbx.log"), true);
    let before = fs::read_to_string(root.join("sbx.log")).unwrap().len();
    write_server(&observer("upgraded", &ran));
    assert!(
        reload_server(FLEET),
        "there was no doorway to reload, so an upgrade would have to start one from nothing"
    );
    let log = fs::read_to_string(root.join("sbx.log")).unwrap();
    let serve = &log[before..];
    assert!(
        serve.contains("-USR1"),
        "the upgrade did not reload the running doorway:\n{serve}"
    );
    assert!(
        !serve.contains("new-session"),
        "the upgrade started a second doorway, which can only mean the first one gave up its \
         socket:\n{serve}"
    );
    assert_eq!(
        door_pid(),
        Some(pid),
        "the doorway did not survive the upgrade"
    );
    assert_eq!(
        door_socket(pid),
        socket,
        "the cockpit's socket was closed and re-opened by the upgrade"
    );
}

/// A doorway that dies is replaced *at once*, because the gap is the port standing empty. The
/// supervisor used to sleep two seconds unconditionally before re-running it.
///
/// With a server behind the door, because that is what makes the death interesting: the server
/// inherited the listener, so a doorway killed on its own would leave an orphan still holding the
/// port — and then no replacement could ever bind it. The server's death is part of the doorway's.
#[test]
fn a_doorway_that_dies_takes_the_server_with_it_and_is_replaced_at_once() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());
    let ran = root.join("ran.txt");

    ensure_fleet_door(FLEET).expect("the door opens");
    assert!(wait_for_door(port), "the door never opened");
    let first = door_pid().expect("a doorway");
    write_server(&observer("first", &ran));
    assert!(ran_says("first", &ran), "no server ran behind the door");
    let served: u32 = fs::read_to_string(&ran)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();

    // Past the supervisor's crash-loop threshold, which is what tells "this one was working" from
    // "this one cannot start at all" — the second still backs off, deliberately.
    std::thread::sleep(Duration::from_secs(6));

    let killed = std::time::Instant::now();
    let _ = Command::new("kill")
        .args(["-9", &first.to_string()])
        .status();
    let mut back = None;
    let mut orphan = true;
    while killed.elapsed() < Duration::from_secs(10) {
        orphan = Path::new(&format!("/proc/{served}")).exists();
        match door_pid() {
            Some(pid) if pid != first && !orphan && connects(port) => {
                back = Some(killed.elapsed());
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(
        !orphan,
        "the server outlived the doorway that forked it and still holds the inherited listener — \
         no replacement doorway can ever bind :{port} again"
    );
    let took =
        back.unwrap_or_else(|| panic!("the door never came back after the doorway was killed"));
    eprintln!("the door was re-opened after {took:?}");
    assert!(
        took < Duration::from_millis(1500),
        "the cockpit's port stood empty for {took:?} after the doorway died — every millisecond \
         of that is a box's chance to bind it (architecture §9.4)"
    );
}

/// The other end of the supervisor: a doorway that dies is replaced, and a doorway that **cannot**
/// be replaced ends the loop instead of retrying it for ever.
///
/// Found rather than reasoned about. Every fixture here and in `tests/ui/onboarding.mjs` deletes
/// its fleet root on the way out and none of them stopped the supervisor first, so each run left a
/// bash restarting a python script that no longer existed, twice a second, until the box was
/// rebooted: 105 of them were alive when somebody finally looked at `ps`. A fleet a person destroys
/// leaks one the same way — the fixtures only made the rate visible.
///
/// The doorway is killed as well as deleted, and both halves are the point. Killing it is what
/// makes the loop take another turn at all (a live doorway holds its socket and never returns), and
/// deleting the fleet is what that turn finds. Under `while true` the session survives both.
#[test]
fn a_supervisor_whose_fleet_is_gone_stops_rather_than_restarting_for_ever() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());

    ensure_fleet_door(FLEET).expect("the door opens");
    assert!(wait_for_door(port), "the door never opened");
    let doorway = door_pid().expect("a doorway");
    // Asserted before the deletion, because an absence that was never a presence proves nothing —
    // and that is precisely how the first draft of this test passed against the bug.
    let before = supervisor_procs(&root);
    assert!(
        !before.is_empty(),
        "nothing on this machine names {}, so there is no supervisor to outlive its fleet",
        root.join("boxes").display()
    );

    // The fleet goes, doorway script and all — `remove_dir_all` is what every fixture's last line
    // does, and what `skein` does to a sandbox it destroys.
    fs::remove_dir_all(root.join("boxes")).expect("the fleet is deleted");
    let _ = Command::new("kill")
        .args(["-9", &doorway.to_string()])
        .status();

    let died = std::time::Instant::now();
    while died.elapsed() < Duration::from_secs(15) && !supervisor_procs(&root).is_empty() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let left = supervisor_procs(&root);
    assert!(
        left.is_empty(),
        "{} of the {} process(es) supervising this fleet are still alive {:?} after it was \
         deleted (pids {left:?}) — each is a bash restarting a doorway that no longer exists, \
         twice a second, and nothing will ever reap it",
        left.len(),
        before.len(),
        died.elapsed()
    );
}

/// Every live process whose command line names this fixture's fleet — the tmux server holding the
/// session and the bash spinning inside it.
///
/// **Not `tmux has-session`**, which is how the first draft of this test passed against the bug it
/// was written for. The session's socket lives at `<fleet>/.skein/private/server.tmux`, *inside* the
/// directory the test deletes, so `has-session` answered "No such file or directory" — a missing
/// socket, read as a dead session, with the supervisor still spinning behind it. The leak is a
/// process, so a process is what has to be counted.
///
/// The fleet root is `/var/tmp/skein-move-it-<pid>`, so the needle cannot match another test's.
fn supervisor_procs(root: &Path) -> Vec<u32> {
    let needle = root.join("boxes").to_string_lossy().into_owned();
    let mut found = vec![];
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&raw).contains(&needle) {
            found.push(pid);
        }
    }
    found
}

/// **A test that fails still takes its supervisor down.**
///
/// The leak of 2026-08-31, in a test. Every test here used to remove its fixture on the last line,
/// which is exactly where cleanup does not happen: a failing assertion panics and unwinds straight
/// past it. What survives is not an idle directory — the supervisor is
/// `while [ -f <root>/boxes/.skein/server-doorway.py ]; do … done`, so its exit condition is a file
/// inside the directory the teardown was going to remove, and it restarts itself and its server for
/// as long as that file lives. Four such processes were found by the leaked-process gate, which is
/// the one gate that reports a NUMBER: a leak nobody clears makes every later run's count wrong, so
/// it does not merely persist, it hides the next one.
///
/// The panic is real rather than simulated, because the thing under test is what `Drop` does while
/// unwinding — and a fixture that merely returned early would exercise the ordinary path instead.
///
/// **What would make this fail:** making `scratch` return a bare `PathBuf` again. The supervisor
/// then outlives the panic and the first assertion below counts it. The second assertion is the
/// one that says why it outlives it: with the doorway script still on disk, the loop has something
/// to come back to.
#[test]
fn a_test_that_panics_still_takes_its_supervisor_down() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    // The same path `scratch` builds, named here because the fixture that owns it is about to be
    // destroyed by the panic and this has to outlive it.
    let path = PathBuf::from("/var/tmp").join(format!("skein-move-it-{}", std::process::id()));

    // The panic below is deliberate, and the default hook would print a backtrace that reads like
    // a real failure in a passing run.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let root = scratch();
        // The pins, but deliberately no `Staged`: `unstage` stops the server and kills the tmux
        // server, and this test is about the supervisor being taken down by `Scratch`'s quiesce.
        // Binding a teardown here would make it pass for the wrong reason.
        let (port, _pins) = stage(&root);
        ensure_fleet_door(FLEET).expect("the door opens");
        assert!(wait_for_door(port), "the door never opened");
        // **Asserted BEFORE the panic**, because an absence that was never a presence proves
        // nothing — the lesson from `a_supervisor_whose_fleet_is_gone_…`, which once passed in
        // 0.26s with 105 processes still spinning behind it.
        assert!(
            !supervisor_procs(&root).is_empty(),
            "the fixture never started a supervisor, so what this test asserts afterwards is \
             about nothing"
        );
        panic!("what a failing assertion does");
    }));
    std::panic::set_hook(hook);
    assert!(
        out.is_err(),
        "the fixture did not panic, so nothing was proved"
    );

    // The supervisor is a `while` loop with a two-second beat; give it the moment `Drop` gave it.
    std::thread::sleep(Duration::from_millis(750));
    let left = supervisor_procs(&path);
    assert!(
        left.is_empty(),
        "a failing test left {} supervisor process(es) alive: {left:?} — they restart themselves \
         and make every later leaked-process count wrong",
        left.len()
    );
    assert!(
        !path
            .join("boxes")
            .join(".skein")
            .join("server-doorway.py")
            .exists(),
        "the loop's own exit condition is still on disk, so anything that re-runs the supervisor \
         brings it back"
    );

    for var in [
        "SKEIN_SERVER_BINARY",
        "SKEIN_SERVER_PORT",
        "SBX_LOG",
        "SKEIN_LS_CMD",
        "SKEIN_RUNTIME_PACKAGES",
    ] {
        std::env::remove_var(var);
    }
}

/// `skein fleet-serve --stop`: the server goes and **the door stays open**.
///
/// The door is the whole assertion. Ending the tmux session would be the obvious stop and it is the
/// wrong one: skein never withdraws the host mapping (`fleet::stop_serving` carries the argument),
/// so it outlives whatever holds the port —
/// let go of it and the next box to bind :port inherits the browser and the fleet token with it
/// (architecture §9.4). A stop that costs you that is not a stop anybody would run twice.
///
/// So this asserts three things in the order they can each be false: the server stopped and stayed
/// stopped, the doorway is the *same process* it was (not a replacement that re-bound, which would
/// have left the window open however briefly), and the port still answers. Then it serves again,
/// because a stop you cannot come back from is a different bug.
#[test]
fn stopping_the_server_leaves_the_door_open_behind_it() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());
    let ran = root.join("ran.txt");

    ensure_fleet_door(FLEET).expect("the door opens");
    assert!(wait_for_door(port), "the door never opened");
    let before = door_pid().expect("a doorway");
    write_server(&observer("serving", &ran));
    assert!(ran_says("serving", &ran), "no server ran behind the door");
    let served: u32 = fs::read_to_string(&ran)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();

    assert_eq!(stop_serving(FLEET).expect("the stop"), "running");

    // Past the doorway's two-second restart, because asking once would pass against a stop that
    // killed the server without removing the binary it would be restarted from.
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !Path::new(&format!("/proc/{served}")).exists(),
        "the server behind the door is still running after the stop"
    );
    assert!(
        !Path::new(&server_path()).exists(),
        "the binary is still installed, so the doorway will put it back in two seconds — a stop \
         that kills without removing is a restart with extra steps"
    );

    // And the part that makes this the right stop at all.
    assert_eq!(
        door_pid(),
        Some(before),
        "the doorway was replaced rather than kept — whatever re-bound the port, there was an \
         interval in which it was free, and skein never unpublishes the mapping that points at \
         it (`fleet::stop_serving`; architecture §9.4)"
    );
    assert!(
        connects(port),
        "nothing answers on :{port} after the stop — the door was closed, and the next thing to \
         bind it inherits the browser and the fleet token"
    );

    // A second stop is not an error: the state afterwards is the same, and a stop that refuses on
    // an already stopped cockpit is a stop people stop trusting.
    assert_eq!(stop_serving(FLEET).expect("a second stop"), "stopped");

    // Serving again comes back, through the same door.
    write_server(&observer("again", &ran));
    assert!(
        ran_says("again", &ran),
        "the cockpit never came back after a stop"
    );
    assert_eq!(door_pid(), Some(before), "coming back re-opened the door");
}

/// Something already holds the cockpit's port inside the sandbox, and skein does not mistake it
/// for its own doorway.
///
/// **What changed here and what did not** (SKEIN-576). The publishing went: skein makes no host
/// mapping at all now, so "nothing is published to a squatter" is true by construction and would
/// be a test that cannot fail. What is left is the half that was always load-bearing — *the guard
/// cannot be a TCP connect*, because a squatter accepts exactly as the doorway does, and it is the
/// doorway's own stamp that tells them apart (`door_holds_port`).
///
/// The stakes moved rather than shrank. A person is now the one who runs `sbx ports --publish`,
/// on the strength of skein telling them the door is open — so a guard fooled by a connect no
/// longer publishes to a squatter itself, it *advises somebody else to*.
#[test]
fn a_squatter_on_the_cockpits_port_is_not_mistaken_for_the_door() {
    let _env = env_lock();
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot hold the door");
    }
    let root = scratch();
    let (port, pins) = stage(&root);
    let _teardown = Staged(pins, skein::place::seam::real_crossings());

    // The squat: a box got there first and is answering on the cockpit's number.
    let squatter = std::net::TcpListener::bind(("0.0.0.0", port)).expect("the squatter binds");
    assert!(
        connects(port),
        "the squatter accepts, exactly as the doorway would"
    );

    // Opening the door is attempted and does not succeed against a bound port — but it does not
    // *report* failure either, which is exactly why the guard cannot live there: `start_server`
    // finds its tmux session started and returns, and the doorway inside it dies unheard.
    let _ = ensure_fleet_door(FLEET);
    let why = cockpit_port_advice(FLEET)
        .expect_err("skein offered a person the line that publishes to a squatter");
    assert!(
        why.contains("§9.4") && why.contains("not held by the doorway"),
        "the refusal must name the squat, or a person reads it as a transient fault and retries: \
         {why}"
    );
    // The stamp is what decided it, and the stamp is absent: nothing may report this door as held.
    assert_eq!(
        door_pid(),
        None,
        "a squatter's port was recorded as a doorway skein had stamped"
    );
    // And skein ran no `sbx` at all — the mapping is a person's act now, and one made to a
    // squatter outlives the mistake, since skein withdraws nothing.
    let seq = fs::read_to_string(root.join("sbx.log")).unwrap();
    assert!(
        !seq.contains("--publish"),
        "a permanent host mapping was made to whatever holds :{port}:\n{seq}"
    );

    drop(squatter);
}

// ---- bootstrap.sh across the tmux socket move (SKEIN-1020) ---------------------------------------
//
// SKEIN-529 moved the cockpit's tmux socket from `.skein/server.tmux` into `.skein/private/`, and
// `start-door.sh` — which bootstrap.sh writes and runs — then asked only the new path whether the
// cockpit was running. On a fleet serving since before the move it was told "no" about a cockpit
// that was running, started a second supervisor beside it, and the first one kept the port and
// kept serving the build it was started with. The install printed "built <sha>" and "listening on
// :7878", both true: nothing it said could tell an upgraded fleet from one serving the old build.
//
// So these run the real bootstrap.sh, fed on stdin the way `sbx exec -i … bash < bootstrap.sh`
// feeds it, against a real tmux and the real doorway. Only three things are stood in for: `git`
// and `cargo`, because a clone and a release build are the network and minutes; and the server
// behind the door, which is a python stand-in that adopts descriptor 3 as `doorway.rs` does and
// answers `/api/health` with the build the fake cargo baked into it — so a process started before
// an install goes on answering with the OLD build, exactly as the real one did.
//
// **Everything it touches is inside the scratch root.** The environment is cleared and rebuilt,
// the fleet root, the volume and `$HOME` are under it, and the port is a free one — never 7878 —
// because bootstrap.sh's defaults are the live fleet (`tests-can-reach-the-live-fleet`). `sudo` is
// a stub that fails, so nothing here can escalate either.

/// The cockpit's tmux socket as every fleet had it before SKEIN-529 moved it under `private/`.
/// Nothing makes a session there any more: it is a fact about fleets installed before the move.
/// Spelled here independently of `fleet::pre_move_server_tmux_sock` and `start-door.sh`, the two
/// places that still ASK it (SKEIN-1020, SKEIN-1025), so a fixture built here is one neither of
/// them could have agreed with by construction.
fn legacy_sock(fleet_root: &Path) -> String {
    fleet_root
        .join(".skein/server.tmux")
        .to_string_lossy()
        .into_owned()
}

/// The token the stand-in server demands, so the install's question is asked the way the real
/// cockpit requires it — with the fleet's token — rather than of a server that answers anyone.
const TOKEN: &str = "fixture-token";

/// A stand-in skein-server: adopts the doorway's listener at descriptor 3 — or, run bare with no
/// `LISTEN_FDS`, binds `$STALE_PORT` itself, as a process no doorway started would — and answers
/// `/api/health` with `build`, refusing a request without the token as `apiauth::authorised` does.
/// `@BUILD@` is replaced by the fake cargo at "build" time, which is what makes an old process and
/// a new file answer differently.
const SERVER: &str = r#"#!/usr/bin/env python3
import http.server, json, os, socket
BUILD = "@BUILD@"
TOKEN = open(os.path.join(os.environ["SKEIN_HOME"], "api-token")).read().strip()
class Health(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/api/health" or self.headers.get("Authorization") != "Bearer " + TOKEN:
            self.send_response(401)
            self.end_headers()
            return
        body = json.dumps({"build": BUILD}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
if os.environ.get("LISTEN_FDS") == "1":
    server = http.server.HTTPServer(("", 0), Health, bind_and_activate=False)
    server.socket.close()
    server.socket = socket.socket(fileno=3)
else:
    server = http.server.HTTPServer(("", int(os.environ["STALE_PORT"])), Health)
server.serve_forever()
"#;

/// A fleet for bootstrap.sh to install into, built entirely under one scratch root.
struct Install {
    root: PathBuf,
    fleet_root: PathBuf,
    home: PathBuf,
    port: u16,
}

impl Install {
    fn new(root: &Path) -> Install {
        use std::os::unix::fs::PermissionsExt;
        let fleet_root = root.join("boxes");
        let home = root.join("skein");
        let bin = root.join("bin");
        for dir in [&home, &bin, &root.join("container-home")] {
            fs::create_dir_all(dir).unwrap();
        }
        // The one line this whole fixture exists to keep true, checked rather than trusted.
        assert!(
            root.starts_with("/var/tmp/") && !fleet_root.starts_with("/boxes"),
            "the upgrade fixture is not under its scratch root: {}",
            root.display()
        );
        fs::write(home.join("api-token"), format!("{TOKEN}\n")).unwrap();
        fs::write(root.join("server.py.in"), SERVER).unwrap();
        fs::write(root.join("meminfo"), "MemTotal: 4194304 kB\n").unwrap();

        let src = fleet_root.join(".skein/src");
        let stub = |name: &str, body: String| {
            let at = bin.join(name);
            fs::write(&at, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
        };
        // `git`: any call leaves a checkout carrying the REAL doorway, and the two questions the
        // script asks of it are answered with the revision this run is "at".
        stub(
            "git",
            format!(
                "mkdir -p '{src}/.git' '{src}/src'\n\
                 cat '{doorway}' > '{src}/src/server-doorway.py'\n\
                 case \"$*\" in *rev-parse*|*describe*) cat '{rev}' ;; esac\nexit 0",
                src = src.display(),
                doorway = concat!(env!("CARGO_MANIFEST_DIR"), "/src/server-doorway.py"),
                rev = root.join("rev").display(),
            ),
        );
        // `cargo`: "builds" the stand-in server with this run's revision baked in.
        stub(
            "cargo",
            format!(
                "case \"$*\" in *build*) ;; *) exit 0 ;; esac\n\
                 mkdir -p '{src}/target/release'\n\
                 sed \"s/@BUILD@/$(cat '{rev}')/\" '{tmpl}' > '{src}/target/release/skein-server'\n\
                 printf '#!/bin/sh\\n' > '{src}/target/release/skein'\n\
                 chmod 755 '{src}/target/release/skein-server' '{src}/target/release/skein'",
                src = src.display(),
                rev = root.join("rev").display(),
                tmpl = root.join("server.py.in").display(),
            ),
        );
        for present in ["cc", "curl", "jq"] {
            stub(present, "exit 0".into());
        }
        stub("nproc", "echo 2".into());
        stub(
            "sudo",
            "echo 'the upgrade fixture ran sudo' >&2; exit 1".into(),
        );
        Install {
            root: root.to_path_buf(),
            fleet_root,
            home,
            port: free_port(),
        }
    }

    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn new_sock(&self) -> String {
        server_tmux_sock_in(self.fleet_root.to_string_lossy().as_ref())
    }

    fn old_sock(&self) -> String {
        legacy_sock(&self.fleet_root)
    }

    fn doorway(&self) -> PathBuf {
        self.fleet_root.join(".skein/server-doorway.py")
    }

    /// Run bootstrap.sh at revision `rev`, on stdin, in an environment built from nothing.
    fn bootstrap(&self, rev: &str, answer_wait: u32) -> (bool, String) {
        fs::write(self.root.join("rev"), rev).unwrap();
        let script = fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/bootstrap.sh"))
            .expect("bootstrap.sh");
        let out = Command::new("bash")
            .stdin(script)
            .env_clear()
            .env("PATH", self.path())
            .env("HOME", self.root.join("container-home"))
            .env("SKEIN_FLEET_ROOT", &self.fleet_root)
            .env("SKEIN_HOME", &self.home)
            .env("SKEIN_SERVER_PORT", self.port.to_string())
            .env("SKEIN_MEMINFO", self.root.join("meminfo"))
            .env("SKEIN_FLEET_MEMORY", "4g")
            .env("SKEIN_FLEET_CPUS", "2")
            .env("SKEIN_SOURCE_URL", "file:///nowhere")
            .env("SKEIN_BOOTSTRAP_ANSWER_WAIT", answer_wait.to_string())
            .output()
            .expect("bootstrap.sh ran");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn supervising(&self, sock: &str) -> bool {
        Command::new("tmux")
            .args(["-S", sock, "has-session", "-t", "skein-server"])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Which sockets have a cockpit supervisor on them, by name — so a failure says which.
    fn supervisors(&self) -> Vec<&'static str> {
        let mut on = vec![];
        if self.supervising(&self.new_sock()) {
            on.push("private/server.tmux");
        }
        if self.supervising(&self.old_sock()) {
            on.push("server.tmux (pre-move)");
        }
        on
    }

    /// Start a supervisor on the PRE-move socket, as `start-door.sh` did before SKEIN-529 — the
    /// same loop, the same doorway, the same stamp; only the socket differs, which is the whole
    /// of what a fleet serving since then has.
    fn start_pre_move_supervisor(&self) {
        self.start_supervisor(&self.old_sock());
    }

    fn start_supervisor(&self, sock: &str) {
        let skein = self.fleet_root.join(".skein");
        let supervise = format!(
            "while [ -f '{d}' ]; do began=$(date +%s); \
             SKEIN_HOME='{h}' python3 '{d}' '{p}' '{s}' '{st}'; \
             [ $(($(date +%s) - began)) -lt 5 ] && sleep 2; done",
            d = self.doorway().display(),
            h = self.home.display(),
            p = self.port,
            s = skein.join("skein-server").display(),
            st = skein.join("server.door").display(),
        );
        let ok = Command::new("tmux")
            .args([
                "-S",
                sock,
                "new-session",
                "-d",
                "-s",
                "skein-server",
                &supervise,
            ])
            .env_clear()
            .env("PATH", self.path())
            .env("HOME", self.root.join("container-home"))
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "could not start a supervisor on {sock}");
    }

    /// End the tmux server on `sock` and wait until nothing holds the port.
    fn retire(&self, sock: &str) {
        let _ = Command::new("tmux")
            .args(["-S", sock, "kill-server"])
            .status();
        for _ in 0..100 {
            if !connects(self.port) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the port was still held after the supervisor on {sock} was ended");
    }

    /// The doorway's pid, off its stamp.
    fn door_pid(&self) -> Option<u32> {
        let stamp = fs::read_to_string(self.fleet_root.join(".skein/server.door")).ok()?;
        stamp.split_whitespace().next()?.parse().ok()
    }

    /// What `/api/health` on the cockpit's port says its build is — asked with the token, as the
    /// install asks it.
    fn answering(&self) -> Option<String> {
        use std::io::{Read, Write};
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
        let mut s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        write!(
            s,
            "GET /api/health HTTP/1.0\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"
        )
        .ok()?;
        let mut reply = String::new();
        s.read_to_string(&mut reply).ok()?;
        let body = reply.split("\r\n\r\n").nth(1)?;
        let json: serde_json::Value = serde_json::from_str(body).ok()?;
        json["build"].as_str().map(str::to_string)
    }

    fn wait_answering(&self, build: &str) -> bool {
        for _ in 0..150 {
            if self.answering().as_deref() == Some(build) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Doorway processes of THIS fleet: `python3 <its doorway> …`, found in `/proc`.
    fn doorways(&self) -> Vec<u32> {
        let doorway = self.doorway().to_string_lossy().into_owned();
        let mut found = vec![];
        for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let argv: Vec<String> = raw
                .split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            if argv.first().is_some_and(|a| {
                Path::new(a)
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("python"))
            }) && argv.get(1) == Some(&doorway)
            {
                found.push(pid);
            }
        }
        found
    }

    /// The doorways, once any that are on their way out have gone.
    fn settled_doorways(&self) -> Vec<u32> {
        let mut found = self.doorways();
        for _ in 0..30 {
            if found.len() <= 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            found = self.doorways();
        }
        found
    }
}

/// The skip every test below shares: a real doorway needs tmux and python3.
fn cannot_hold_a_door() -> bool {
    if !have("tmux") || !have("python3") {
        skip("this machine lacks tmux/python3, so it cannot hold the door");
        return true;
    }
    false
}

/// **A first install** — nothing running anywhere — ends with one supervisor, on the new socket,
/// serving the build it installed. The path that already worked, held here so the fix for the
/// upgrade cannot quietly break it.
///
/// What would make it fail: `start-door.sh` choosing the pre-move socket for a new session, or
/// the closing check refusing a server that is in fact the new build.
#[test]
fn bootstrap_on_a_fresh_fleet_starts_one_supervisor_serving_what_it_built() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);

    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "a first install did not succeed:\n{said}");
    assert_eq!(
        fleet.supervisors(),
        vec!["private/server.tmux"],
        "a first install did not leave exactly one supervisor, on the new socket:\n{said}"
    );
    assert_eq!(
        fleet.answering().as_deref(),
        Some("aaaa111"),
        "the cockpit is not serving the build the install just made:\n{said}"
    );
    assert!(
        said.contains("answers as aaaa111"),
        "the install did not say which build answered, which is the line that tells an upgraded \
         fleet from one still serving the old build:\n{said}"
    );
}

/// **A normal upgrade** — supervisor already on the new socket — reloads the running doorway in
/// place: same pid, same listening socket, and the new build answering. The other path that
/// already worked.
///
/// What would make it fail: a `new-session` beside the running one (two supervisors), or a
/// restart instead of the reload (a new doorway pid, a re-bound socket).
#[test]
fn bootstrap_over_a_supervisor_on_the_new_socket_reloads_it_in_place() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");
    let pid = fleet
        .door_pid()
        .expect("the first install stamped a doorway");
    let socket = door_socket(pid);

    let (ok, said) = fleet.bootstrap("bbbb222", 20);
    assert_eq!(
        fleet.supervisors(),
        vec!["private/server.tmux"],
        "an upgrade did not leave exactly one supervisor:\n{said}"
    );
    assert_eq!(
        fleet.door_pid(),
        Some(pid),
        "the upgrade replaced the doorway instead of reloading it:\n{said}"
    );
    assert_eq!(
        door_socket(pid),
        socket,
        "the cockpit's listening socket was closed and re-opened by the upgrade:\n{said}"
    );
    assert_eq!(
        fleet.answering().as_deref(),
        Some("bbbb222"),
        "the upgrade left the old build answering:\n{said}"
    );
    assert!(ok, "the upgrade did not succeed:\n{said}");
}

/// **SKEIN-1020 itself**: a fleet serving since before SKEIN-529, its supervisor on
/// `.skein/server.tmux`. The install must end with exactly one supervisor, serving the new build —
/// and with no gap on the port (the same doorway, the same socket), and with the pre-move socket
/// gone, since it is one every box can connect to.
///
/// What would make it fail, and did: `start-door.sh` asking only the new path. It then starts a
/// second supervisor, which is the first assertion; and the old doorway is never reloaded, so the
/// old build answers, which is the one after.
#[test]
fn bootstrap_over_a_supervisor_on_the_pre_move_socket_ends_with_one_serving_the_new_build() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");

    // Put the running cockpit where a pre-move fleet has it.
    fleet.retire(&fleet.new_sock());
    fleet.start_pre_move_supervisor();
    assert!(
        fleet.wait_answering("aaaa111"),
        "the pre-move supervisor never served — the fixture is wrong, not the install"
    );
    assert_eq!(
        fleet.supervisors(),
        vec!["server.tmux (pre-move)"],
        "the fixture is not a pre-move fleet"
    );
    let pid = fleet.door_pid().expect("the pre-move doorway stamped");
    let socket = door_socket(pid);

    let (ok, said) = fleet.bootstrap("bbbb222", 20);
    assert_eq!(
        fleet.supervisors(),
        vec!["private/server.tmux"],
        "upgrading a fleet whose supervisor is on the pre-move socket did not end with exactly \
         one supervisor, on the new socket:\n{said}"
    );
    assert_eq!(
        fleet.answering().as_deref(),
        Some("bbbb222"),
        "the pre-move supervisor kept serving the old build after the upgrade:\n{said}"
    );
    assert_eq!(
        fleet.door_pid(),
        Some(pid),
        "the pre-move doorway was replaced rather than reloaded, so the port was let go:\n{said}"
    );
    assert_eq!(
        door_socket(pid),
        socket,
        "the cockpit's listening socket was closed and re-opened by the migration:\n{said}"
    );
    assert_eq!(
        fleet.settled_doorways().len(),
        1,
        "more than one doorway is running for this fleet after the upgrade:\n{said}"
    );
    assert!(
        !Path::new(&fleet.old_sock()).exists(),
        "the pre-move socket is still there, and every box can connect to it (SKEIN-529):\n{said}"
    );
    assert!(ok, "the upgrade did not succeed:\n{said}");
}

/// **The state the bug leaves behind**: the pre-move supervisor holding the port and serving the
/// old build, and a second one on the new socket whose doorway loops on "cannot bind". An install
/// over that keeps the one holding the port — so nothing is let go — ends the other, and serves the
/// new build.
///
/// What would make it fail: keeping both (two supervisors), or ending the one that holds the port
/// (a new doorway pid).
#[test]
fn bootstrap_over_two_supervisors_keeps_the_one_holding_the_port_and_ends_the_other() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");
    fleet.retire(&fleet.new_sock());
    fleet.start_pre_move_supervisor();
    assert!(
        fleet.wait_answering("aaaa111"),
        "the pre-move supervisor never served"
    );
    let pid = fleet.door_pid().expect("the pre-move doorway stamped");
    // What the unfixed install did next: a second supervisor on the new socket.
    fleet.start_supervisor(&fleet.new_sock());
    assert_eq!(
        fleet.supervisors().len(),
        2,
        "the fixture does not have two supervisors"
    );

    let (ok, said) = fleet.bootstrap("bbbb222", 20);
    assert_eq!(
        fleet.supervisors(),
        vec!["private/server.tmux"],
        "two supervisors did not end as one:\n{said}"
    );
    assert_eq!(
        fleet.door_pid(),
        Some(pid),
        "the supervisor holding the port was the one ended, so the port was let go:\n{said}"
    );
    assert_eq!(
        fleet.answering().as_deref(),
        Some("bbbb222"),
        "the old build is still answering:\n{said}"
    );
    assert_eq!(
        fleet.settled_doorways().len(),
        1,
        "the ended supervisor's doorway is still running:\n{said}"
    );
    assert!(ok, "the install did not succeed:\n{said}");
}

/// **skein can stop — and does not double — a cockpit whose supervisor is still on the pre-move
/// socket** (SKEIN-1025).
///
/// A fleet serving since before SKEIN-529 keeps its supervisor at `.skein/server.tmux` until
/// `bootstrap.sh` next runs. The Rust side used to ask only `private/server.tmux`, so on that fleet
/// `stop_server` killed a session that was not there, `pkill`ed the doorway, and the surviving loop
/// started it again two seconds later; and `start_server` would have added a second supervisor.
///
/// **What makes it fail:** `stop_server` without its kill-session on the pre-move socket — the
/// supervisor is still listed afterwards, and the port answers again once the loop restarts the
/// doorway. `start_server` without its has-session there fails the first assertion instead: two
/// supervisors.
#[test]
fn skein_stops_a_cockpit_whose_supervisor_is_still_on_the_pre_move_socket() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");
    fleet.retire(&fleet.new_sock());
    fleet.start_pre_move_supervisor();
    assert!(
        fleet.wait_answering("aaaa111"),
        "the pre-move supervisor never served — the fixture is wrong, not skein"
    );
    assert_eq!(
        fleet.supervisors(),
        vec!["server.tmux (pre-move)"],
        "the fixture is not a pre-move fleet"
    );

    // skein pointed at the same fleet the install was, and allowed to run what it runs there.
    let mut pins = env_pins();
    pins.set("SKEIN_HOME", &fleet.home);
    pins.set("SKEIN_FLEET_ROOT", &fleet.fleet_root);
    pins.set("SKEIN_SERVER_PORT", fleet.port.to_string());
    let _crossings = skein::place::seam::real_crossings();
    let sandbox = skein::place::fleet_sandbox();

    start_server(&sandbox).expect("start_server over a running pre-move cockpit");
    assert_eq!(
        fleet.supervisors(),
        vec!["server.tmux (pre-move)"],
        "start_server started a second supervisor beside the pre-move one, and the two contend for \
         one port (SKEIN-1020's state)"
    );

    stop_server(&sandbox);
    assert_eq!(
        fleet.supervisors(),
        Vec::<&str>::new(),
        "stop_server left a supervisor running, which restarts the doorway it just killed"
    );
    // Longer than the supervisor's two-second restart, so a loop that survived has had its turn.
    let mut answered = false;
    for _ in 0..30 {
        answered |= connects(fleet.port);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !answered,
        "the cockpit's port answered again after stop_server — the stop did not stop"
    );
}

/// Field 22 of `/proc/<pid>/stat`, or `None` for a process that is gone or a zombie — the same
/// "is it still that process" question bootstrap.sh asks before it signals anything.
fn started_at(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(") ")? + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.first() == Some(&"Z") {
        return None;
    }
    fields.get(19)?.parse().ok()
}

/// Kills, on the way out of a test however it leaves, exactly the processes it recorded — each
/// only if it is still the process that was recorded (`BoxlikeNamespace`'s rule in
/// `src/testutil.rs`). Never by pattern: another lane's fixture can have the same shape.
struct Recorded(Vec<(u32, u64)>);

impl Drop for Recorded {
    fn drop(&mut self) {
        for &(pid, started) in &self.0 {
            if started_at(pid) == Some(started) {
                // SAFETY: `kill` has no memory effects; the pid was recorded by this test and
                // checked on the line above to still be the process it recorded.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
    }
}

/// **A stale cockpit that is this install's own is FIXED, not reported.** A doorway from this same
/// fleet — started before the upgrade, supervised by nothing any more, still holding the port and
/// answering with the old build — is exactly what an upgrade exists to replace, and bootstrap.sh is
/// the one thing that can prove it is its own (its command line names this install's doorway). So
/// it stops it, lets the new supervisor's doorway take the port, and the new build answers.
///
/// What would make it fail: reporting instead of fixing — the install then exits 1 over a holder
/// it could have stopped, which is the first assertion.
#[test]
fn bootstrap_stops_its_own_stale_doorway_and_the_new_build_answers() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");

    // The stale cockpit: this fleet's own doorway, orphaned — started through `sh … &` so that it
    // is reparented and reaped as a leftover in a real sandbox is — with the old build behind it.
    fleet.retire(&fleet.new_sock());
    let skein = fleet.fleet_root.join(".skein");
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "SKEIN_HOME='{h}' python3 '{d}' '{p}' '{s}' '{st}' </dev/null >'{log}' 2>&1 & echo $!",
            h = fleet.home.display(),
            d = fleet.doorway().display(),
            p = fleet.port,
            s = skein.join("skein-server").display(),
            st = skein.join("server.door").display(),
            log = root.join("stale-door.log").display(),
        ))
        .env_clear()
        .env("PATH", fleet.path())
        .output()
        .expect("the stale doorway started");
    let door: u32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("the stale doorway's pid");
    let mut recorded = Recorded(vec![]);
    if let Some(started) = started_at(door) {
        recorded.0.push((door, started));
    }
    assert!(
        fleet.wait_answering("aaaa111"),
        "the stale doorway never served — the fixture is wrong, not the install"
    );
    let server: Vec<u32> = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| {
            fs::read_to_string(format!("/proc/{pid}/status"))
                .unwrap_or_default()
                .lines()
                .any(|l| l.split_whitespace().collect::<Vec<_>>() == ["PPid:", &door.to_string()])
        })
        .collect();
    for &pid in &server {
        if let Some(started) = started_at(pid) {
            recorded.0.push((pid, started));
        }
    }
    assert_eq!(
        recorded.0.len(),
        2,
        "the fixture does not have a stale doorway with its server behind it: {:?}",
        recorded.0
    );

    let (ok, said) = fleet.bootstrap("bbbb222", 5);
    assert!(
        ok,
        "the install reported its own stale doorway instead of stopping it — the fix was one it \
         could prove was its own to make:\n{said}"
    );
    assert_eq!(
        fleet.answering().as_deref(),
        Some("bbbb222"),
        "the new build is not answering after the stale doorway was stopped:\n{said}"
    );
    for &(pid, started) in &recorded.0 {
        assert_ne!(
            started_at(pid),
            Some(started),
            "pid {pid} of the stale cockpit is still running:\n{said}"
        );
    }
    assert!(
        said.contains("stopping pid") && said.contains(&door.to_string()),
        "the install did not say what it stopped and why:\n{said}"
    );
    assert!(
        said.contains("answers as bbbb222"),
        "the install did not end by naming the build that answers:\n{said}"
    );
    assert_eq!(
        fleet.supervisors(),
        vec!["private/server.tmux"],
        "the fix did not leave exactly one supervisor:\n{said}"
    );
}

/// **A holder that is NOT provably this install's is left alone** — named, with the one command
/// that frees the port, and a sentence saying why the install did not run it itself. The holder is
/// a stale server from a DIFFERENT fleet root, which is the shape of the stranded fixture doorway
/// that took :7878 on the live fleet (SKEIN-1019): stopping it on a guess is the worse failure.
///
/// What would make it fail: judging a stranger to be ours — the install then stops it, which is
/// the first assertion; or trusting the file on disk, i.e. removing the closing check — the
/// install then reports success, which is the second.
#[test]
fn bootstrap_leaves_a_holder_that_is_not_its_own_and_names_the_command_that_frees_the_port() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);

    // Another fleet's server, from an older build, holding the port: its own fleet root, its own
    // volume, its own path — nothing about it names this install.
    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(elsewhere.join("skein")).unwrap();
    fs::write(elsewhere.join("skein/api-token"), format!("{TOKEN}\n")).unwrap();
    let stale = elsewhere.join("stale-server");
    fs::write(&stale, SERVER.replace("@BUILD@", "0ld0000")).unwrap();
    let child = Command::new("python3")
        .arg(&stale)
        .env_clear()
        .env("PATH", fleet.path())
        .env("SKEIN_FLEET_ROOT", elsewhere.join("boxes"))
        .env("SKEIN_HOME", elsewhere.join("skein"))
        .env("STALE_PORT", fleet.port.to_string())
        .spawn()
        .expect("the stale server ran");
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let stranger = Reap(child);
    let pid = stranger.0.id();
    assert!(
        fleet.wait_answering("0ld0000"),
        "the stale server never answered — the fixture is wrong, not the install"
    );
    let started = started_at(pid).expect("the stale server is running");

    let (ok, said) = fleet.bootstrap("aaaa111", 3);
    assert_eq!(
        started_at(pid),
        Some(started),
        "the install stopped pid {pid}, a process from another fleet root that it could not prove \
         was its own:\n{said}"
    );
    assert!(
        !ok,
        "the install reported success while another fleet's older build answered on the \
         cockpit's port:\n{said}"
    );
    assert!(
        said.contains(&format!("    kill {pid}\n")),
        "the install did not give the one command that frees the port:\n{said}"
    );
    assert!(
        said.contains("did not stop") && said.contains("prove"),
        "the install did not say why it left the holder running:\n{said}"
    );
    assert!(
        said.contains("0ld0000") && said.contains(&format!("pid {pid}")),
        "the install did not say what answers and who holds the port:\n{said}"
    );
    assert!(
        !said.contains("answers as"),
        "the install claimed a build answered that did not:\n{said}"
    );
}

/// A doorway too old to reload: it ignores SIGUSR1, binds the port itself, stamps itself, and goes
/// on answering with the build it was started at. `@BUILD@` is that build.
fn deaf_doorway(build: &str) -> String {
    let deaf = SERVER.replace("@BUILD@", build).replace(
        "import http.server, json, os, socket\n",
        "import http.server, json, os, signal, socket, sys\n\
         signal.signal(signal.SIGUSR1, signal.SIG_IGN)\n\
         os.environ[\"STALE_PORT\"] = sys.argv[1]\n\
         open(sys.argv[3], \"w\").write(\"%d %s\\n\" % (os.getpid(), sys.argv[1]))\n",
    );
    assert!(
        deaf.contains("SIG_IGN"),
        "the stand-in server's imports changed, so the deaf doorway is not deaf"
    );
    deaf
}

/// **The reload did not take**: the supervisor on the new socket is the CURRENT one, and its
/// doorway ignored the SIGUSR1 and goes on serving the old build. This is the likeliest way a
/// real upgrade ends up here, and it is the one where fixing it ends the only supervisor there is
/// — so the install has to put one back, with `start-door.sh`, or it leaves the port unsupervised.
///
/// What would make it fail: dropping the step that puts the door back (the `has-session ||
/// start-door.sh` line). The stale doorway and its loop are still stopped, nothing replaces them,
/// nothing answers, and the install exits 1 — which is the first assertion.
#[test]
fn bootstrap_replaces_a_supervisor_whose_doorway_ignored_the_reload() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");

    // The current session, with a doorway in it that will not reload.
    fleet.retire(&fleet.new_sock());
    fs::write(fleet.doorway(), deaf_doorway("aaaa111")).unwrap();
    fleet.start_supervisor(&fleet.new_sock());
    assert!(
        fleet.wait_answering("aaaa111"),
        "the deaf doorway never served — the fixture is wrong, not the install"
    );
    let loop_pid: u32 = String::from_utf8_lossy(
        &Command::new("tmux")
            .args([
                "-S",
                &fleet.new_sock(),
                "list-panes",
                "-t",
                "skein-server",
                "-F",
                "#{pane_pid}",
            ])
            .output()
            .expect("tmux list-panes")
            .stdout,
    )
    .trim()
    .parse()
    .expect("the supervisor loop's pid");
    let door = fleet.door_pid().expect("the deaf doorway stamped");
    let mut recorded = Recorded(vec![]);
    for pid in [loop_pid, door] {
        if let Some(started) = started_at(pid) {
            recorded.0.push((pid, started));
        }
    }
    assert_eq!(
        recorded.0.len(),
        2,
        "the fixture does not have a live supervisor loop with a deaf doorway in it"
    );

    let (ok, said) = fleet.bootstrap("bbbb222", 5);
    assert!(
        ok,
        "the install stopped a doorway that ignored its reload, and the supervisor it stopped with \
         it was the only one — so nothing was put back behind the port:\n{said}"
    );
    assert_eq!(
        fleet.answering().as_deref(),
        Some("bbbb222"),
        "the new build is not answering after the deaf doorway was replaced:\n{said}"
    );
    assert_eq!(
        fleet.supervisors(),
        vec!["private/server.tmux"],
        "the install did not end with exactly one supervisor, on the new socket:\n{said}"
    );
    for &(pid, started) in &recorded.0 {
        assert_ne!(
            started_at(pid),
            Some(started),
            "pid {pid} of the stale supervisor is still running:\n{said}"
        );
    }
    assert!(
        said.contains("stopping pid") && said.contains(&door.to_string()),
        "the install did not say what it stopped and why:\n{said}"
    );
}

// ---- the Update button's own run: SKEIN-1031 and SKEIN-1032 ------------------------------------
//
// Everything above runs `bootstrap.sh` the way a person does, on stdin. The Update button does not:
// `update::start` hands `update::run_script` to a detached tmux pane, and those bytes are all of
// bootstrap wrapped in a subshell whose output goes to the log the pane tails, with a marker after.
// So these run THOSE bytes — the same call `update::launch` makes — rather than a copy of their
// shape, in the fixture fleet above. The one thing left out is the tmux detach, which is a
// launcher and not part of what the run does; in its place is a terminal of the run's own, because
// a prompt on a terminal nobody is watching is the failure SKEIN-1032 is about, and a test with no
// terminal cannot see one — git without a terminal fails at once, prompt setting or none.

/// Runs `argv` on a pseudo-terminal of its own, as a tmux pane would, and kills its whole process
/// group at the deadline. Prints `exit <status>` or `TIMEOUT`, then everything written to the
/// terminal — which is where a prompt goes, whatever the run's own output is redirected to.
const PTY_RUN: &str = r#"import os, pty, select, signal, sys, time
limit = float(sys.argv[1])
pid, fd = pty.fork()
if pid == 0:
    os.execvp(sys.argv[2], sys.argv[2:])
said, status, end = b"", None, time.time() + limit
while True:
    ready, _, _ = select.select([fd], [], [], 0.2)
    if ready:
        try:
            said += os.read(fd, 4096)
        except OSError:
            pass
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
        break
    if time.time() > end:
        os.killpg(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
        break
print("exit %d" % os.waitstatus_to_exitcode(status) if status is not None else "TIMEOUT")
sys.stdout.write(said.decode(errors="replace"))
"#;

/// What one run of the Update button's script came to.
struct UpdateRun {
    /// It ended by itself before the deadline.
    finished: bool,
    /// Everything that reached its terminal — a prompt, if anything asked one.
    tty: String,
    /// The log the Update pane tails.
    log: String,
    /// The marker the pane stops at, when there is one.
    marker: Option<String>,
}

impl Install {
    /// Run the Update button's script — `skein::update::run_script`, exactly the bytes
    /// `update::launch` hands to tmux — at revision `rev`, on a terminal of its own, stopped at
    /// `limit`, with `env` added to the fixture's own. `under` is a command to run it beneath,
    /// standing in for whatever the pane's tmux server is.
    fn update_under(
        &self,
        rev: &str,
        limit: Duration,
        source_url: &str,
        env: &[(&str, String)],
        under: &[String],
    ) -> UpdateRun {
        fs::write(self.root.join("rev"), rev).unwrap();
        let log = self.home.join("update.log");
        let done = self.home.join("update.done");
        let _ = fs::remove_file(&done);
        fs::write(&log, "").unwrap();
        // The script reads these as it is assembled — `fleet::bootstrap_env` — so they are pinned
        // for exactly that call, and put back before anything runs.
        let script = {
            let mut pins = env_pins();
            pins.set("SKEIN_FLEET_ROOT", &self.fleet_root);
            pins.set("SKEIN_HOME", &self.home);
            pins.set("SKEIN_SERVER_PORT", self.port.to_string());
            pins.set("SKEIN_SOURCE_URL", source_url);
            pins.set("SKEIN_SOURCE_REF", "");
            skein::update::run_script(&log.to_string_lossy(), &done.to_string_lossy())
        };
        let at = self.root.join("update-run.sh");
        fs::write(&at, script).unwrap();
        let runner = self.root.join("pty-run.py");
        fs::write(&runner, PTY_RUN).unwrap();

        let mut argv: Vec<String> = under.to_vec();
        argv.extend([
            "python3".to_string(),
            runner.to_string_lossy().into_owned(),
            limit.as_secs().to_string(),
            "sh".to_string(),
            at.to_string_lossy().into_owned(),
        ]);
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("PATH", self.path())
            .env("HOME", self.root.join("container-home"))
            .env("SKEIN_MEMINFO", self.root.join("meminfo"))
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .output()
            .expect("the update's run started");
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        let (head, tty) = said.split_once('\n').unwrap_or((&said, ""));
        UpdateRun {
            finished: head.starts_with("exit "),
            tty: tty.to_string(),
            log: fs::read_to_string(&log).unwrap_or_default(),
            marker: fs::read_to_string(&done).ok(),
        }
    }

    fn update(&self, rev: &str, limit: Duration, env: &[(&str, String)]) -> UpdateRun {
        self.update_under(rev, limit, "file:///nowhere", env, &[])
    }

    /// Swap the stub `git` for this machine's own, with a checkout whose `origin` is `origin` and
    /// skein's REAL credential helper configured the way a box's gitconfig configures it.
    ///
    /// The helper answers only for `https://github.com`, and this remote is `http://127.0.0.1`, so
    /// it is reached through a wrapper that re-addresses git's question to github.com and hands it
    /// to `src/git-credential-skein.sh` unchanged. The helper's lookup — which file, in what order —
    /// is therefore the real one, and nothing is ever sent to github.com: git's request goes to
    /// the address in `origin`, whatever the helper was told.
    fn with_real_git(&self, origin: &str) {
        self.use_real_git();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .env_clear()
                .env("PATH", self.path())
                .env("HOME", self.root.join("container-home"))
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "git {args:?} failed in the fixture");
        };
        let src = self.fleet_root.join(".skein/src");
        fs::create_dir_all(&src).unwrap();
        git(&["init", "-q", &src.to_string_lossy()]);
        git(&[
            "-C",
            &src.to_string_lossy(),
            "remote",
            "add",
            "origin",
            origin,
        ]);
    }

    /// [`Install::with_real_git`] without the checkout: the stub `git` swapped for this machine's
    /// own and skein's real credential helper configured, so a run has to CLONE.
    fn use_real_git(&self) {
        use std::os::unix::fs::PermissionsExt;
        fs::remove_file(self.root.join("bin/git")).expect("the stub git was there to remove");
        let helper_dir = self.root.join("helper");
        fs::create_dir_all(&helper_dir).unwrap();
        let wrapper = helper_dir.join("git-credential-skein");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n\
                 sed -e 's/^protocol=http$/protocol=https/' \
                 -e 's/^host=127\\.0\\.0\\.1:[0-9]*$/host=github.com/' \
                 | sh '{}' \"$@\"\n",
                concat!(env!("CARGO_MANIFEST_DIR"), "/src/git-credential-skein.sh")
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            self.root.join("container-home/.gitconfig"),
            format!(
                "[credential]\n\thelper = {}\n\tuseHttpPath = true\n",
                wrapper.display()
            ),
        )
        .unwrap();
        fs::create_dir_all(self.root.join("tokens/read")).unwrap();
    }
}

/// A git remote that refuses everyone: every request is answered `401` with a Basic challenge, and
/// the `Authorization` header each one carried (or an empty string) is recorded.
fn refusing_remote() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Write};
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream);
            let mut auth = String::new();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                if line == "\r\n" {
                    break;
                }
                if let Some(v) = line.strip_prefix("Authorization: ") {
                    auth = v.trim().to_string();
                }
                line.clear();
            }
            seen.lock().unwrap().push(auth);
            let _ = reader.get_mut().write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"skein-test\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (port, heard)
}

/// The fixture's size, stated: a fresh fleet records none until its first run passes the gate.
fn stated_size() -> Vec<(&'static str, String)> {
    vec![
        ("SKEIN_FLEET_MEMORY", "4g".to_string()),
        ("SKEIN_FLEET_CPUS", "2".to_string()),
    ]
}

/// The one line the log carries for a remote that wanted a login — the owner's sentence, verbatim
/// (SKEIN-1171), for the address that was being fetched.
fn refused_line(url: &str) -> String {
    format!(
        "skein: GitHub refused to let this sandbox fetch {url} without a login — the repository is \
         private, or the address is wrong"
    )
}

/// **The Update button's fetch, for a remote that wants a login, ends in seconds with the owner's
/// sentence in the pane's log — it does not sit at a password prompt nobody can see**
/// (SKEIN-1032, SKEIN-1171).
///
/// This is the path that stranded the owner on 2026-09-22 and again on 2026-09-26: GitHub refused
/// the fetch, and git printed `Username for 'https://github.com':` on the pane's terminal and
/// waited.
///
/// **What would make it fail:** removing `export GIT_TERMINAL_PROMPT=0` from bootstrap.sh. git then
/// prompts on the run's terminal, the runner kills it at the deadline, and the first assertion
/// fails by name with the prompt quoted — bounded, so the suite does not hang with it. Dropping the
/// sentence from remote_git fails the log assertion; dropping its `exit 1` the marker assertion.
#[test]
fn the_updates_fetch_for_a_remote_that_wants_a_login_fails_fast_instead_of_prompting() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (port, heard) = refusing_remote();
    let origin = format!("http://127.0.0.1:{port}/skein-test-owner/thing.git");
    fleet.with_real_git(&origin);
    let tokens = root.join("tokens");
    let mut env = stated_size();
    env.push(("SKEIN_GIT_TOKENS", tokens.to_string_lossy().into_owned()));

    let limit = Duration::from_secs(30);
    let ran = fleet.update("bbbb222", limit, &env);
    assert!(
        ran.finished,
        "the Update button's run was still going after {limit:?} — git is waiting at a prompt on a \
         terminal nobody can see, which is the 37-minute hang of SKEIN-1032. The terminal says:\n{}\n\
         and the log:\n{}",
        ran.tty, ran.log
    );
    assert!(
        !heard.lock().unwrap().is_empty(),
        "the refusing remote was never asked — the fixture is wrong, not the update:\n{}",
        ran.log
    );
    assert!(
        !ran.tty.contains("Username for"),
        "the run asked for a username on its terminal:\n{}",
        ran.tty
    );
    assert_eq!(
        ran.marker.as_deref(),
        Some("1"),
        "the run did not end as a failed update, so the pane would not show it finished:\n{}",
        ran.log
    );
    assert!(
        ran.log.lines().any(|l| l == refused_line(&origin)),
        "the pane's log does not carry the owner's sentence for {origin}:\n{}",
        ran.log
    );
}

/// **The fetch sends no credential, even when skein's helper has one to give** (SKEIN-1171).
///
/// A token sits in `read/<owner>`, the file a host places for a box, and skein's real credential
/// helper is configured to hand it out. The remote records the `Authorization` header of every
/// request, so this asserts what went on the wire rather than what the log says.
///
/// **What would make it fail:** dropping `-c credential.helper=` from remote_git. The helper then
/// answers, the remote hears `Basic …` for the stored token and the first assertion fails with it;
/// git ends in "Authentication failed", so the sentence assertion fails too.
#[test]
fn the_updates_fetch_sends_no_stored_token_even_when_a_helper_has_one() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (port, heard) = refusing_remote();
    let origin = format!("http://127.0.0.1:{port}/skein-test-owner/thing.git");
    fleet.with_real_git(&origin);
    let tokens = root.join("tokens");
    fs::write(
        tokens.join("read/skein-test-owner"),
        "skein-test-stored-token\n",
    )
    .unwrap();
    let mut env = stated_size();
    env.push(("SKEIN_GIT_TOKENS", tokens.to_string_lossy().into_owned()));

    let limit = Duration::from_secs(30);
    let ran = fleet.update("bbbb222", limit, &env);
    assert!(
        ran.finished,
        "the Update button's run was still going after {limit:?}; its terminal says:\n{}\nand the \
         log:\n{}",
        ran.tty, ran.log
    );
    let said = heard.lock().unwrap().clone();
    assert!(
        !said.is_empty() && said.iter().all(|a| a.is_empty()),
        "the fetch presented a credential to the remote (or never asked it): {said:?}\n{}",
        ran.log
    );
    assert!(
        ran.log.lines().any(|l| l == refused_line(&origin)),
        "the pane's log does not carry the owner's sentence for {origin}:\n{}",
        ran.log
    );
    assert!(
        !ran.log.contains("skein-test-stored-token"),
        "the log printed the token itself:\n{}",
        ran.log
    );
}

/// **A first install's clone, refused a login, names the address it was given** (SKEIN-1171).
///
/// With no checkout yet, bootstrap clones `$SKEIN_SOURCE_URL` rather than fetching `origin`, and
/// the sentence names that address.
///
/// **What would make it fail:** calling the clone as a bare `git clone` rather than through
/// remote_git — the run then carries on past the failed clone, and the marker assertion fails
/// before the sentence one could; passing remote_git anything but `$url` fails the sentence.
#[test]
fn the_updates_clone_of_a_remote_that_wants_a_login_names_the_address_it_was_given() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (port, heard) = refusing_remote();
    fleet.use_real_git();
    let url = format!("http://127.0.0.1:{port}/skein-test-owner/fork.git");
    let env = stated_size();

    let limit = Duration::from_secs(30);
    let ran = fleet.update_under("bbbb222", limit, &url, &env, &[]);
    assert!(
        ran.finished && !heard.lock().unwrap().is_empty(),
        "the clone did not end by itself within {limit:?}, or never asked the remote; its \
         terminal says:\n{}\nand the log:\n{}",
        ran.tty,
        ran.log
    );
    assert!(
        ran.log.contains(&format!("cloning {url}")),
        "the run fetched instead of cloning — the fixture left a checkout behind:\n{}",
        ran.log
    );
    assert_eq!(ran.marker.as_deref(), Some("1"), "{}", ran.log);
    assert!(
        ran.log.lines().any(|l| l == refused_line(&url)),
        "the pane's log does not carry the owner's sentence for {url}:\n{}",
        ran.log
    );
}

/// **A failure that is not a login keeps git's own words, and gets no sentence it did not earn**
/// (SKEIN-1171).
///
/// A remote that is not there at all — nothing listening on the port — fails the fetch in git's
/// words ("Failed to connect"), and the log must not say GitHub refused a login.
///
/// **What would make it fail:** printing the sentence for every failure of remote_git (or matching
/// on anything git says after "fatal:"); the second assertion then fails.
#[test]
fn the_updates_fetch_that_fails_for_another_reason_keeps_gits_own_error() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let origin = format!(
        "http://127.0.0.1:{}/skein-test-owner/thing.git",
        free_port()
    );
    fleet.with_real_git(&origin);
    let env = stated_size();

    let limit = Duration::from_secs(30);
    let ran = fleet.update("bbbb222", limit, &env);
    assert!(
        ran.finished && ran.marker.as_deref() == Some("1"),
        "a fetch from nothing did not end as a failed update within {limit:?}:\n{}",
        ran.log
    );
    assert!(
        ran.log.contains("Failed to connect") && !ran.log.contains("without a login"),
        "the log does not keep git's own error, or blames a login for it:\n{}",
        ran.log
    );
}

/// A git remote that accepts every connection and never says a word: no status line, no headers,
/// no close. Each connection is held open for the life of the test process, because a remote that
/// hung up would be a different failure — git reports a closed connection at once. Returns the
/// port and how many connections it has accepted.
fn silent_remote() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = accepted.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            held.push(stream);
        }
    });
    (port, accepted)
}

/// **A fetch against a remote that accepts and never answers ends within a bound, and the log says
/// why** (SKEIN-1037).
///
/// Before this, git had no low-speed limit, so the Update button's fetch against such a remote sat
/// at "fetching" for as long as the connection stayed open — the pane said "updating…" and the
/// button stayed disabled until somebody found the tmux session. `SKEIN_NET_STALL_SECS=3` shortens
/// bootstrap's bound so the suite does not wait out the real minute; it is a skein variable that
/// only bootstrap.sh reads, so git is bounded here only if bootstrap passes the bound on.
///
/// **What would make each assertion fail:** deleting `export GIT_HTTP_LOW_SPEED_TIME` (or `_LIMIT`)
/// from bootstrap.sh leaves git waiting on the silent socket, the runner kills it at the deadline,
/// and the first assertion fails with `finished` false. Deleting the `"Operation too slow"` arm of
/// bootstrap.sh's remote_git leaves git's line alone in the log with no word of what it means or
/// what to do, and the log assertion fails.
#[test]
fn the_updates_fetch_from_a_remote_that_never_answers_ends_with_the_reason_in_the_log() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (port, accepted) = silent_remote();
    fleet.with_real_git(&format!(
        "http://127.0.0.1:{port}/skein-test-owner/thing.git"
    ));
    let mut env = stated_size();
    env.push(("SKEIN_NET_STALL_SECS", "3".to_string()));

    let limit = Duration::from_secs(30);
    let ran = fleet.update("bbbb222", limit, &env);
    assert!(
        ran.finished,
        "the Update button's fetch was still waiting on a remote that never answers after \
         {limit:?} — the pane would say \"updating…\" for as long as the connection stayed open. \
         The log:\n{}",
        ran.log
    );
    assert!(
        accepted.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the silent remote was never connected to, so nothing here waited on it — the fixture is \
         wrong, not the update:\n{}",
        ran.log
    );
    assert_eq!(
        ran.marker.as_deref(),
        Some("1"),
        "a fetch that gave up did not end the run as a failed update:\n{}",
        ran.log
    );
    assert!(
        ran.log.contains("Operation too slow")
            && ran
                .log
                .contains("stopped answering while the update was fetching skein-test-owner/thing")
            && ran.log.contains("press Update skein again"),
        "the pane's log does not carry git's reason, what it means in the owner's terms, and \
         what to do next:\n{}",
        ran.log
    );
}

/// **The toolchain rustup downloads during an install ends within a bound when its server stops
/// answering, and the log says why** (SKEIN-1090).
///
/// SKEIN-1037 bounded the curl that fetches the installer; the toolchain that installer then fetches
/// is rustup's own downloader, which bootstrap did not bound, so rustup's default of 180 seconds a
/// read — per attempt — was the only thing between a stalled mirror and an Update pane that says
/// "updating…". This runs the REAL rustup, as `rustup-init`, from bootstrap's own install block: the
/// stub `curl` answers sh.rustup.rs with a script that execs it with bootstrap's arguments, and
/// `RUSTUP_DIST_SERVER` is a local listener that accepts and never answers, so nothing leaves this
/// machine. `RUSTUP_USE_CURL=1` rides in the ambient environment because rustup's curl backend reads
/// the timeout as a connect timeout only, and that listener has already accepted.
///
/// **What would make each assertion fail:** deleting `export RUSTUP_DOWNLOAD_TIMEOUT` from
/// bootstrap.sh leaves rustup waiting out its own 180 seconds, the runner kills it at the deadline,
/// and the first assertion fails with `finished` false; so does deleting `unset RUSTUP_USE_CURL`,
/// which leaves the curl backend waiting with no read bound at all. Putting the install line back
/// to a bare pipeline under `set -e` ends the run with rustup's line as the last word, and the log
/// assertion fails.
#[test]
fn the_toolchain_download_from_a_server_that_never_answers_ends_with_the_reason_in_the_log() {
    use std::os::unix::fs::PermissionsExt;
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    if !have("rustup") {
        skip("this machine has no rustup, so there is no real toolchain download to bound");
        return;
    }
    let rustup = Command::new("sh")
        .args(["-c", "command -v rustup"])
        .output()
        .expect("sh ran");
    let rustup = PathBuf::from(String::from_utf8_lossy(&rustup.stdout).trim());
    let root = scratch();
    let fleet = Install::new(&root);
    let bin = root.join("bin");
    // `rustup-init` is rustup itself, told by its own name which of the two it is.
    std::os::unix::fs::symlink(&rustup, bin.join("rustup-init")).unwrap();
    let stub = |name: &str, body: &str| {
        let at = bin.join(name);
        fs::write(&at, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
    };
    // No cargo that runs, so bootstrap goes to install one; and the installer it downloads is a
    // one-line script that hands bootstrap's own arguments to the real rustup.
    stub("cargo", "exit 1");
    stub(
        "curl",
        "case \"$*\" in *sh.rustup.rs*) echo 'exec rustup-init \"$@\"' ;; esac\nexit 0",
    );
    let (port, accepted) = silent_remote();
    let mut env = stated_size();
    env.push(("SKEIN_NET_STALL_SECS", "3".to_string()));
    env.push(("RUSTUP_DIST_SERVER", format!("http://127.0.0.1:{port}")));
    env.push((
        "RUSTUP_UPDATE_ROOT",
        format!("http://127.0.0.1:{port}/rustup"),
    ));
    env.push(("RUSTUP_USE_CURL", "1".to_string()));

    let limit = Duration::from_secs(60);
    let ran = fleet.update("bbbb222", limit, &env);
    assert!(
        ran.finished,
        "the install's toolchain download was still waiting on a server that never answers after \
         {limit:?} — the pane would say \"updating…\" for as long as rustup kept waiting. The \
         log:\n{}",
        ran.log
    );
    assert!(
        accepted.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the silent server was never connected to, so nothing here waited on it — the fixture is \
         wrong, not the install:\n{}",
        ran.log
    );
    assert_eq!(
        ran.marker.as_deref(),
        Some("1"),
        "a toolchain download that gave up did not end the run as a failed update:\n{}",
        ran.log
    );
    assert!(
        ran.log.contains("operation timed out")
            && ran
                .log
                .contains("rustup could not download the Rust toolchain")
            && ran.log.contains("press Update skein again"),
        "the pane's log does not carry rustup's reason, what it means in the owner's terms, and \
         what to do next:\n{}",
        ran.log
    );
}

/// **The Update button ends the way bootstrap does: with the new build answering, even over a
/// doorway that ignored the reload** (SKEIN-1031).
///
/// The same fixture as `bootstrap_replaces_a_supervisor_whose_doorway_ignored_the_reload` — the
/// current supervisor, with a doorway in it that will not reload — driven through the button's own
/// script instead of a hand run. The button used to send a bare `kill -USR1` after the marker and
/// trust it, so here it left the old build serving, said nothing, and marked the update a success.
///
/// **What would make it fail:** skipping the closing check on the button's path — for instance
/// `[ "$from" = update ] && exit 0` just before it in bootstrap.sh, or the update running bootstrap
/// under `SKEIN_BOOTSTRAP_STOP_AFTER=build` again. The deaf doorway then goes on answering
/// `aaaa111`, which the answering assertion names.
#[test]
fn the_update_button_ends_with_the_new_build_answering_over_a_doorway_that_ignored_the_reload() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");

    fleet.retire(&fleet.new_sock());
    fs::write(fleet.doorway(), deaf_doorway("aaaa111")).unwrap();
    fleet.start_supervisor(&fleet.new_sock());
    assert!(
        fleet.wait_answering("aaaa111"),
        "the deaf doorway never served — the fixture is wrong, not the update"
    );
    let loop_pid: u32 = String::from_utf8_lossy(
        &Command::new("tmux")
            .args([
                "-S",
                &fleet.new_sock(),
                "list-panes",
                "-t",
                "skein-server",
                "-F",
                "#{pane_pid}",
            ])
            .output()
            .expect("tmux list-panes")
            .stdout,
    )
    .trim()
    .parse()
    .expect("the supervisor loop's pid");
    let door = fleet.door_pid().expect("the deaf doorway stamped");
    let mut recorded = Recorded(vec![]);
    for pid in [loop_pid, door] {
        if let Some(started) = started_at(pid) {
            recorded.0.push((pid, started));
        }
    }
    assert_eq!(
        recorded.0.len(),
        2,
        "the fixture does not have a live supervisor loop with a deaf doorway in it"
    );

    let limit = Duration::from_secs(90);
    let ran = fleet.update(
        "bbbb222",
        limit,
        &[("SKEIN_BOOTSTRAP_ANSWER_WAIT", "5".to_string())],
    );
    assert!(
        ran.finished,
        "the Update button's run did not end within {limit:?}:\n{}",
        ran.log
    );
    assert_eq!(
        fleet.answering().as_deref(),
        Some("bbbb222"),
        "after the Update button's run the cockpit still answers as the old build — the button \
         trusted a reload the doorway ignored, and checked nothing:\n{}",
        ran.log
    );
    assert_eq!(
        ran.marker.as_deref(),
        Some("0"),
        "the new build answers but the run did not record a success, so the pane would say the \
         update failed:\n{}",
        ran.log
    );
    assert!(
        ran.log.contains("stopping pid") && ran.log.contains(&door.to_string()),
        "the pane's log does not say what the update stopped and why:\n{}",
        ran.log
    );
    assert!(
        ran.log.contains("answers as bbbb222"),
        "the pane's log does not end by naming the build that answers:\n{}",
        ran.log
    );
    assert!(
        !ran.log.contains("sbx ports"),
        "the button's log carries the first-install epilogue, which is for a person at a shell:\n{}",
        ran.log
    );
    assert_eq!(
        fleet.supervisors(),
        vec!["private/server.tmux"],
        "the update did not end with exactly one supervisor, on the new socket:\n{}",
        ran.log
    );
    for &(pid, started) in &recorded.0 {
        assert_ne!(
            started_at(pid),
            Some(started),
            "pid {pid} of the stale supervisor is still running:\n{}",
            ran.log
        );
    }
}

/// Holds the cockpit's port as an old build, with this fleet's volume in its environment — both
/// halves of what bootstrap calls "provably ours" — and runs the update BENEATH itself, as the
/// tmux server hosting the Update pane does when the cockpit's own server started it and it
/// inherited the listening socket. Writes `survived <status>` when the run below it ends.
const HOLDING_PARENT: &str = r#"import http.server, json, os, subprocess, sys, threading
class Old(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"build": "0ld0000"}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
held = http.server.HTTPServer(("", int(os.environ["STALE_PORT"])), Old)
threading.Thread(target=held.serve_forever, daemon=True).start()
status = subprocess.call(sys.argv[2:])
open(sys.argv[1], "w").write("survived %d\n" % status)
"#;

/// **The closing check never stops the process the update is running under**, however much it
/// looks like this install's own stale cockpit.
///
/// Now that the Update button runs bootstrap's fix, the fix can meet its own ancestry: skein-server
/// hands its children the listening socket (its descriptor 3 is not close-on-exec), so a tmux
/// server it started holds :port with this install's `SKEIN_HOME` in its environment — and the
/// Update pane runs inside that tmux server. Stopping it would end the update with no status
/// recorded, the stranded pane of SKEIN-1032 by another road.
///
/// **What would make it fail:** bootstrap.sh's stoppable without its is_ancestor half. The fix then TERMs the
/// parent, which never writes `survived`, and the first assertion names that.
#[test]
fn the_closing_check_never_stops_the_process_the_update_is_running_under() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);
    let (ok, said) = fleet.bootstrap("aaaa111", 20);
    assert!(ok, "the first install did not succeed:\n{said}");
    fleet.retire(&fleet.new_sock());

    let parent = root.join("holding-parent.py");
    fs::write(&parent, HOLDING_PARENT).unwrap();
    let survived = root.join("survived");
    let ran = fleet.update_under(
        "bbbb222",
        Duration::from_secs(60),
        "file:///nowhere",
        &[
            ("SKEIN_BOOTSTRAP_ANSWER_WAIT", "3".to_string()),
            ("STALE_PORT", fleet.port.to_string()),
            ("SKEIN_HOME", fleet.home.to_string_lossy().into_owned()),
        ],
        &[
            "python3".to_string(),
            parent.to_string_lossy().into_owned(),
            survived.to_string_lossy().into_owned(),
        ],
    );
    assert!(
        fs::read_to_string(&survived).is_ok_and(|s| s.starts_with("survived")),
        "the update stopped the process it was running under — the Update pane's own tmux server, \
         in a real fleet — so the run ended without recording how:\n{}",
        ran.log
    );
    assert!(
        ran.finished,
        "the Update button's run did not end:\n{}",
        ran.log
    );
    assert_eq!(
        ran.marker.as_deref(),
        Some("1"),
        "the old build still answers, and the run did not say the update failed:\n{}",
        ran.log
    );
    assert!(
        ran.log
            .contains("is the process this update is running under"),
        "the log does not say why the holder of the port was left running:\n{}",
        ran.log
    );
}
