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

use skein::fleet::{
    ensure_fleet_server, fleet_serve_mounts, install_server, server_binary, server_doorway_path,
    server_path, server_tmux_sock, start_server, stop_server,
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

/// Not under `/tmp` for the same reason as `fleet_launch`'s scratch, and per-pid so two cargo
/// invocations cannot collide.
fn scratch() -> PathBuf {
    let d = PathBuf::from("/var/tmp").join(format!("skein-move-it-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
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
        assert_eq!(mode & 0o777, 0o755, "an unexecutable server is not installed");
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
        seq.lines().nth(publish).unwrap().contains(&format!("{port}:{port}/tcp")),
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
    assert_eq!(v["in_fleet"], "1", "the one variable: the started server IS the in-fleet one");
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
        mounts.iter().skip(1).all(|m| !m.starts_with(&*home.to_string_lossy())),
        "everything under the volume is already visible through it; mounting a path twice is not \
         obviously harmless: {mounts:?}"
    );

    let _ = fs::remove_dir_all(&root);
}
