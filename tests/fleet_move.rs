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
//!      `sbx ports <fleet>` unchanged, because sbx has no unpublish.

use skein::fleet::{
    ensure_fleet, ensure_fleet_door, ensure_fleet_server, fleet_serve_mounts, install_server,
    reload_server, server_binary, server_door_stamp_path, server_doorway_path, server_path,
    server_tmux_sock, start_server, stop_server, stop_serving,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const FLEET: &str = "test-fleet";

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

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

/// The same stand-in, with the guest command RECORDED AND NOT RUN.
///
/// For the tests whose claim is an order of operations rather than an effect: `ensure_fleet` runs
/// apt through the substrate and writes `/etc/docker` through `install_docker_config`, and a fake
/// that executed those would do both to the machine running the suite.
fn write_recording_sbx(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    fs::write(
        &p,
        r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SBX_LOG"
# stdin is drained rather than ignored: an install writes megabytes down this pipe and a reader
# that exits first turns the write into EPIPE, which is a failure the caller reports as its own.
case "$1" in exec) cat >/dev/null 2>&1 ;; esac
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Not under `/tmp` for the same reason as `fleet_launch`'s scratch, and per-pid so two cargo
/// invocations cannot collide.
fn scratch() -> Scratch {
    let d = PathBuf::from("/var/tmp").join(format!("skein-move-it-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    Scratch(d)
}

/// The scratch root, and **a teardown that outlives a panic.**
///
/// Every test in this file ends by removing its root — and a failing assertion skips that, because
/// a panic unwinds straight past it. What is left behind is not an idle directory. The supervisor
/// these tests start is `while [ -f <root>/boxes/.skein/server-doorway.py ]; do … done`, so its
/// exit condition is a file inside the very directory the teardown was going to remove: it keeps
/// restarting itself, and the server with it, for as long as that file survives. Four such
/// processes were found by the leaked-process gate on 2026-08-31, after an intermittent failure in
/// this file.
///
/// That gate is the one that reports a NUMBER rather than pass or fail, so a leak nobody clears
/// makes every later run's count wrong — the leak does not merely persist, it hides the next one.
///
/// `Drop` runs while unwinding, so this happens whether the test passed or failed, and **the order
/// is load-bearing**: the doorway script first, because removing it is the loop's own exit
/// condition; then the tmux server; then a beat for the supervisor to notice; then the directory.
/// Killing tmux while the script is still on disk is how a supervisor started by a `sbx exec`
/// somewhere else comes back.
///
/// Both paths are derived from the root rather than from `$SKEIN_FLEET_ROOT`, because by the time
/// this runs the environment is whatever the test last set — and a teardown that reads a variable
/// the failure may have left wrong is a teardown that cleans up somebody else's fleet.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let skein = self.0.join("boxes").join(".skein");
        let _ = fs::remove_file(skein.join("server-doorway.py"));
        let _ = Command::new("tmux")
            .args([
                "-S",
                &skein.join("server.tmux").to_string_lossy(),
                "kill-server",
            ])
            .status();
        std::thread::sleep(Duration::from_millis(250));
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Every test here drives skein through process-wide environment, so they take turns.
fn serialize() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
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

/// The move, in the order the design requires, with the payload treated as the binary it is.
#[test]
fn the_server_moves_into_the_fleet_behind_a_door_that_was_open_first() {
    let _env = env_lock();
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    write_fake_sbx(&root.join("bin"));
    let log = root.join("sbx.log");
    fs::write(&log, "").unwrap();
    std::env::set_var("SBX_LOG", &log);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let home = root.join("skein");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("SKEIN_HOME", &home);
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));
    let port = free_port();
    std::env::set_var("SKEIN_SERVER_PORT", port.to_string());

    // ---- the payload is a binary, and it arrives byte for byte ----
    //
    // 1 MiB, which is more than ten pipe buffers: the install must ride the threaded stdin writer
    // (`place.rs` — a pipe holds ~64KB and `write_all` past it blocks), and the bytes include NUL
    // and every high value, which is what `-t` would have corrupted.
    let mut payload = vec![0x7f, b'E', b'L', b'F'];
    payload.extend((0..1_048_576u32).map(|i| (i % 251) as u8));
    let carried = root.join("skein-server-build");
    fs::write(&carried, &payload).unwrap();
    std::env::set_var("SKEIN_SERVER_BINARY", &carried);
    install_server(FLEET).expect("install skein-server over stdin");
    assert_eq!(
        fs::read(server_path()).expect("the server landed in the fleet root"),
        payload,
        "the binary must arrive byte for byte — a corrupted install runs nothing"
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(server_path()).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "an unexecutable server is not installed"
        );
    }
    let doorway = fs::read_to_string(server_doorway_path()).expect("the doorway is installed too");
    assert!(
        doorway.contains("LISTEN_FDS") && doorway.contains("SKEIN_IN_FLEET"),
        "the doorway installed is the embedded one, whole"
    );

    // ---- the whole sequence, once, and the order read off the record ----
    //
    // The payload is ELF-shaped garbage, so the exec inside the doorway fails and retries — which
    // is itself the property under test: the DOOR is open and answering while the server behind it
    // is not, because the doorway bound before it forked.
    let got = ensure_fleet_server(FLEET).expect("the move");
    assert_eq!(got, port, "the sandbox's own number is preferred host-side");
    let seq = fs::read_to_string(&log).unwrap();
    let install = seq
        .lines()
        .position(|l| l.contains("cat >") && l.contains("skein-server"))
        .expect("the install was recorded");
    let start = seq
        .lines()
        .position(|l| l.contains("new-session"))
        .expect("the start was recorded");
    let publish = seq
        .lines()
        .position(|l| l.contains("--publish"))
        .expect("the publish was recorded");
    assert!(
        install < start && start < publish,
        "the sequence must be install, then start, then publish — a port published before \
         something holds it is a permanent mapping to nothing (sbx has no unpublish): {seq}"
    );
    assert!(
        seq.lines()
            .nth(publish)
            .unwrap()
            .contains(&format!("{port}:{port}/tcp")),
        "the publish maps the cockpit's own number: {seq}"
    );

    // ---- the handover, checked from the inheriting side ----
    //
    // The garbage server is replaced by an observer that adopts descriptor 3 exactly as
    // `doorway.rs` would, writes what it was handed, and then never accepts — so the connect
    // below succeeding is proof the DOORWAY holds the socket, not the server.
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
             'in_fleet': os.environ.get('SKEIN_IN_FLEET'),\n\
             'inherited_only': os.environ.get('SKEIN_LISTEN_INHERITED_ONLY'),\n\
             'skein_home': os.environ.get('SKEIN_HOME'),\n\
             'bound': s.getsockname()[1],\n\
         }, open(out, 'w'))\n\
         time.sleep(120)\n",
    )
    .unwrap();
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
        v["in_fleet"], "1",
        "the one variable: the started server IS the in-fleet one"
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

    stop_server(FLEET);
    let _ = Command::new("tmux")
        .args(["-S", &server_tmux_sock(), "kill-server"])
        .status();
    for var in ["SKEIN_SERVER_BINARY", "SKEIN_SERVER_PORT", "SBX_LOG"] {
        std::env::remove_var(var);
    }
    let _ = fs::remove_dir_all(&root);
}

/// A re-serve lands on a fleet whose previous server is still running — which is the ordinary
/// case, since `ensure_fleet_server` installs *before* it stops so that a failed install leaves a
/// working cockpit up.
///
/// This is a regression test for a defect the first version of this file did not catch: the install
/// wrote `cat > <path>` straight onto the server path, and writing to an ELF a process is currently
/// executing fails with `ETXTBSY` — so the second `skein fleet-serve` against a live fleet refused
/// to install at all. The first test here missed it because nothing was executing the path it wrote
/// to, which is exactly the condition a live fleet does not satisfy.
#[test]
fn a_server_is_replaced_while_the_old_one_is_still_running() {
    let _env = env_lock();
    let _guard = serialize();
    if !have("python3") {
        eprintln!("skipping: no python3 to stand in for a running server");
        return;
    }
    let root = scratch();
    write_fake_sbx(&root.join("bin"));
    std::env::set_var("SBX_LOG", root.join("sbx.log"));
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    std::env::set_var("SKEIN_HOME", root.join("skein"));
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));

    // A real ELF at the server's path, actually executing. A `#!` script would not reproduce this:
    // the kernel opens the interpreter as the executable and lets go of the script itself, so only
    // a genuine binary holds the write lock a live server holds.
    let installed = PathBuf::from(server_path());
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::copy("/usr/bin/python3", &installed).expect("an ELF to stand in for the old server");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut running = Command::new(&installed)
        .args(["-c", "import time; time.sleep(120)"])
        .spawn()
        .expect("the old server is running");

    let replacement = root.join("skein-server-build");
    let mut payload = vec![0x7f, b'E', b'L', b'F'];
    payload.extend((0..4096u32).map(|i| (i % 251) as u8));
    fs::write(&replacement, &payload).unwrap();
    std::env::set_var("SKEIN_SERVER_BINARY", &replacement);

    install_server(FLEET).expect(
        "installing over a running server must work — it is renamed into place, not written onto",
    );
    assert_eq!(
        fs::read(server_path()).unwrap(),
        payload,
        "the new binary is what is at the path now"
    );
    // And the old one is undisturbed: a rename unlinks the directory entry, so the process that
    // was executing it keeps its inode and dies when it is told to, not when it is replaced.
    assert!(
        running.try_wait().unwrap().is_none(),
        "replacing the binary must not kill the server that is still serving through it"
    );

    let _ = running.kill();
    let _ = running.wait();
    for var in ["SKEIN_SERVER_BINARY", "SBX_LOG"] {
        std::env::remove_var(var);
    }
    let _ = fs::remove_dir_all(&root);
}

/// The carrier refuses what the sandbox cannot run, naming the fix — this is the mac host's
/// cross-build story as a sentence rather than prose in a doc.
#[test]
fn a_server_the_sandbox_cannot_run_is_refused_with_the_cross_build_named() {
    let _env = env_lock();
    let _guard = serialize();
    let root = scratch();

    std::env::set_var("SKEIN_SERVER_BINARY", root.join("does-not-exist"));
    let why = server_binary().expect_err("a missing binary was carried");
    assert!(
        why.contains("cargo build --release --bin skein-server"),
        "the refusal names how to build one: {why}"
    );

    let mach_o = root.join("skein-server-mach-o");
    fs::write(&mach_o, b"\xcf\xfa\xed\xfe not for this kernel").unwrap();
    std::env::set_var("SKEIN_SERVER_BINARY", &mach_o);
    let why = server_binary().expect_err("a non-ELF binary was carried into a Linux sandbox");
    assert!(
        why.contains("ELF") && why.contains("unknown-linux-musl"),
        "the refusal names the cross-build target: {why}"
    );

    std::env::remove_var("SKEIN_SERVER_BINARY");
    let _ = fs::remove_dir_all(&root);
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
    let _guard = serialize();
    let root = scratch();
    let home = root.join("skein");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("SKEIN_HOME", &home);

    let mounts = fleet_serve_mounts().expect("the volume mount needs no grant now");
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

    let _ = fs::remove_dir_all(&root);
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
struct Staged(PathBuf);

impl Drop for Staged {
    fn drop(&mut self) {
        unstage(&self.0);
    }
}

/// Stage a fake fleet: a recording `sbx` on PATH, a scratch volume, and a free cockpit port.
fn stage(root: &Path) -> u16 {
    write_fake_sbx(&root.join("bin"));
    fs::write(root.join("sbx.log"), "").unwrap();
    std::env::set_var("SBX_LOG", root.join("sbx.log"));
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let home = root.join("skein");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("SKEIN_HOME", &home);
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));
    let port = free_port();
    std::env::set_var("SKEIN_SERVER_PORT", port.to_string());
    port
}

fn unstage(root: &Path) {
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
    let _ = fs::remove_dir_all(root);
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
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());

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
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());

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
    let _guard = serialize();
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());
    // Recording only: every `exec` is logged and nothing is run.
    write_recording_sbx(&root.join("bin"));
    // The fleet already exists, so nothing is created and the warden is never asked.
    std::env::set_var(
        "SKEIN_LS_CMD",
        format!("echo '[{{\"name\":\"{FLEET}\",\"status\":\"running\"}}]'"),
    );
    skein::sbx::forget_fleet_boxes();

    let _ = ensure_fleet(FLEET, &[]);

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
    assert!(
        !seq.lines().any(|l| l.starts_with("create")),
        "a fleet that already exists was created again:\n{seq}"
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
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());
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

/// And `skein fleet-serve` against a live fleet takes that path: it reloads the running doorway
/// rather than stopping and starting one, which is what makes an upgrade windowless.
#[test]
fn a_re_serve_reloads_the_running_doorway_rather_than_restarting_it() {
    let _env = env_lock();
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());
    let carried = root.join("skein-server-build");
    let mut payload = vec![0x7f, b'E', b'L', b'F'];
    payload.extend((0..4096u32).map(|i| (i % 251) as u8));
    fs::write(&carried, &payload).unwrap();
    std::env::set_var("SKEIN_SERVER_BINARY", &carried);

    // The door first, as `ensure_fleet` opens it at create.
    ensure_fleet_door(FLEET).expect("the door opens");
    assert!(wait_for_door(port), "the door never opened");
    let pid = door_pid().expect("a doorway");
    let socket = door_socket(pid);

    let before = fs::read_to_string(root.join("sbx.log")).unwrap().len();
    assert_eq!(
        ensure_fleet_server(FLEET).expect("the serve"),
        port,
        "the sandbox's own number is preferred host-side"
    );
    let log = fs::read_to_string(root.join("sbx.log")).unwrap();
    let serve = &log[before..];
    assert!(
        serve.contains("-USR1"),
        "the serve did not reload the running doorway:\n{serve}"
    );
    assert!(
        !serve.contains("new-session"),
        "the serve started a second doorway, which can only mean the first one gave up its \
         socket:\n{serve}"
    );
    assert_eq!(
        door_pid(),
        Some(pid),
        "the doorway did not survive the serve"
    );
    assert_eq!(
        door_socket(pid),
        socket,
        "the cockpit's socket was closed and re-opened by the serve"
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
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());
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
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());

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
/// was written for. The session's socket lives at `<fleet>/.skein/server.tmux`, *inside* the
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
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
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
        let port = stage(&root);
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
/// wrong one: `sbx` has no unpublish verb, so the host mapping outlives whatever holds the port —
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
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());
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
         interval in which it was free, and sbx cannot unpublish the mapping that points at it \
         (architecture §9.4)"
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

/// Something already holds the cockpit's port inside the sandbox. Nothing is published: the host
/// mapping is permanent and cannot be withdrawn, so handing it to a squatter hands it the browser
/// and the fleet token with it.
///
/// The guard cannot be a TCP connect, which is what makes this worth a test — a squatter accepts
/// exactly as the doorway does. It is the doorway's own stamp that tells them apart.
#[test]
fn a_squatter_on_the_cockpits_port_is_never_published_to() {
    let _env = env_lock();
    let _guard = serialize();
    if !have("tmux") || !have("python3") {
        eprintln!("skipping: this machine lacks tmux/python3, so it cannot hold the door");
        return;
    }
    let root = scratch();
    let port = stage(&root);
    let _teardown = Staged(root.to_path_buf());
    let carried = root.join("skein-server-build");
    // ELF-shaped and nothing more: what is under test is whether the port gets published, which
    // is decided before anything behind the door has a chance to run.
    fs::write(&carried, [0x7f, b'E', b'L', b'F']).unwrap();
    std::env::set_var("SKEIN_SERVER_BINARY", &carried);

    // The squat: a box got there first and is answering on the cockpit's number.
    let squatter = std::net::TcpListener::bind(("0.0.0.0", port)).expect("the squatter binds");
    assert!(
        connects(port),
        "the squatter accepts, exactly as the doorway would"
    );

    let why = ensure_fleet_server(FLEET).expect_err("the cockpit was published to a squatter");
    assert!(
        why.contains("§9.4") && why.contains("not held by the doorway"),
        "the refusal must name the squat: {why}"
    );
    let seq = fs::read_to_string(root.join("sbx.log")).unwrap();
    assert!(
        !seq.contains("--publish"),
        "a permanent host mapping was made to whatever holds :{port} — sbx has no unpublish, so \
         that mapping outlives the mistake:\n{seq}"
    );

    drop(squatter);
}

/// Cargo builds ONE binary per file in `tests/`, and runs the tests in it as parallel threads of a
/// single process. `$SKEIN_HOME`, `$SKEIN_FLEET_ROOT`, `$SKEIN_SERVER_BINARY` and `$SKEIN_SERVER_PORT` are process-global, so
/// without this every test here writes into the middle of the others: one test's fake sandbox root
/// answers another's call, and the symptom is an assertion about which server is listening rather than
/// an error that names the cause.
///
/// The same lock, by the same argument, as `src/testutil.rs`'s `env_lock` — a separate one because
/// that one is `#[cfg(test)]` inside the library crate and no integration binary can reach it.
/// Poisoning is ignored for the reason given there: the guarded data is `()`, and cascading the
/// first panic into every other test buries the real failure.
///
/// `tools/env-lock-check.py` is what keeps this true as tests are added here.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
