//! The move (delivery §3 4c), end to end against a fake `sbx`: skein-server is installed into the
//! fleet sandbox over stdin, started behind a socket that was opened first, and published — with
//! the host path untouched, one unset variable away.
//!
//! Same shape as `tests/fleet_launch.rs` and for the same reason: no `sbx` exists here, but a
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
/// `tests/isolation_bwrap.rs::a_box_on_a_mounted_volume_cannot_read_the_fleets_credentials` is
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
/// Spelled here and in `start-door.sh` and nowhere in skein, because nothing makes a session there
/// any more: it is a fact about fleets installed before the move, not a place skein uses.
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

/// **The check that would have told the truth**: when what answers on the port is not the build
/// just installed, the install says so, fails, and names the pid holding the port — rather than
/// printing "built <sha>" and "listening" over a cockpit serving something else.
///
/// The holder here is a stale server from an older build that no supervisor knows about — the
/// shape of the stranded fixture doorway that took :7878 on the live fleet (SKEIN-1019).
///
/// What would make it fail: trusting the file on disk, i.e. removing the closing check — the
/// install then reports success, which is the first assertion.
#[test]
fn bootstrap_fails_naming_the_holder_when_the_answering_build_is_not_the_one_it_installed() {
    let _env = env_lock();
    if cannot_hold_a_door() {
        return;
    }
    let root = scratch();
    let fleet = Install::new(&root);

    // A stale server holding the port: the stand-in, pre-built at an old revision, run bare.
    let stale = root.join("stale-server");
    fs::write(&stale, SERVER.replace("@BUILD@", "0ld0000")).unwrap();
    let child = Command::new("python3")
        .arg(&stale)
        .env_clear()
        .env("PATH", fleet.path())
        .env("SKEIN_FLEET_ROOT", &fleet.fleet_root)
        .env("SKEIN_HOME", &fleet.home)
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
    let stale_server = Reap(child);
    assert!(
        fleet.wait_answering("0ld0000"),
        "the stale server never answered — the fixture is wrong, not the install"
    );

    let (ok, said) = fleet.bootstrap("aaaa111", 3);
    assert!(
        !ok,
        "the install reported success while an older build answered on the cockpit's port:\n{said}"
    );
    assert!(
        said.contains("0ld0000"),
        "the install did not say which build is answering:\n{said}"
    );
    assert!(
        said.contains(&format!("pid {}", stale_server.0.id())),
        "the install did not name the process holding the port:\n{said}"
    );
    assert!(
        !said.contains("answers as"),
        "the install claimed a build answered that did not:\n{said}"
    );
}
