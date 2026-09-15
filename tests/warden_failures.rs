//! What a real warden answers when something underneath it fails.
//!
//! The same harness as `tests/warden_roundtrip.rs` — the real binary, a real pty for the approval,
//! and skein reaching it through `warden_client` — because what is asserted here is only worth
//! anything against the real mechanism: an `open` the filesystem refuses, and a process that is
//! actually gone. The pty helpers are repeated from that file rather than moved into `common`, so
//! neither test changes because the other's file did.
//!
//! **One lock for every test here.** Each pins `$SKEIN_WARDEN_HOME`, which `warden_client` reads the
//! secret from, and cargo runs one binary's tests as threads of one process.

mod common;

use common::{env_lock, env_pins, have, skip, Scratch};
use portable_pty::{Child, CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};
use skein::warden_client::{
    operation_id_with_env, perform_through, Act, Answered, Performed, Warden,
};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// An `sbx` that records every argv it is given, and succeeds.
///
/// **And an `rm` that does not return while `gate` exists**, which is how a warden is caught
/// *inside* its command rather than before it: the argv line is written first, so the test can see
/// the command was reached, and then the script waits. Removing the gate lets it finish, so a test
/// never has to signal a process it did not spawn.
fn fake_sbx(dir: &Path, log: &Path, gate: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nprintf 'argv %s\\n' \"$*\" >> '{log}'\n\
             if [ \"$1\" = rm ]; then while [ -e '{gate}' ]; do sleep 0.1; done; fi\n\
             echo made\nexit 0\n",
            log = log.display(),
            gate = gate.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The warden, built into cargo's own scratch beside this checkout's build output.
///
/// `CARGO_TARGET_TMPDIR` rather than a fresh directory per test, so the second test here and the
/// second run of either are incremental rather than a whole build of the warden's dependencies.
fn built_warden() -> PathBuf {
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("skein-warden-for-failures");
    let built = Command::new("cargo")
        .args([
            "build",
            "--quiet",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/warden/Cargo.toml"),
            "--target-dir",
            target.to_str().unwrap(),
        ])
        .status()
        .expect("run cargo");
    assert!(built.success(), "the warden must build");
    target.join("debug/skein-warden")
}

/// The port a warden bound, off the line it prints before it serves anything.
fn port_it_bound(said: &str) -> Option<u16> {
    said.split_once("listening on 127.0.0.1:")?
        .1
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

/// Read from the pty until `want` appears, or give up — on a thread, with the deadline on the
/// channel, for the reason `tests/warden_roundtrip.rs::wait_for` gives: a read from a live and quiet
/// child never returns, so a clock checked between reads is never reached.
fn wait_for(reader: &mut Box<dyn Read + Send>, seen: &mut String, want: &str) -> bool {
    use std::sync::mpsc;
    let deadline = Instant::now() + Duration::from_secs(30);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let taken = std::mem::replace(reader, Box::new(std::io::empty()));
    std::thread::spawn(move || {
        let mut taken = taken;
        let mut buf = [0u8; 1024];
        while let Ok(n) = taken.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        match rx.recv_timeout(left) {
            Ok(chunk) => {
                seen.push_str(&String::from_utf8_lossy(&chunk));
                if seen.contains(want) {
                    *reader = Box::new(Draining { rx });
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
}

/// What a reading thread has already taken off the pty, in the order it arrived.
struct Draining {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
}

impl Read for Draining {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.rx.recv_timeout(Duration::from_secs(30)) {
            Ok(chunk) => {
                let n = chunk.len().min(out.len());
                out[..n].copy_from_slice(&chunk[..n]);
                Ok(n)
            }
            Err(_) => Ok(0),
        }
    }
}

/// A warden running in a pty of its own, with the state directory `root/state`.
///
/// **Stopped on drop, including when an assertion unwinds.** A warden left running holds a port
/// and a pty for the rest of the suite; `stop` signals the one pid this test spawned and reaps it,
/// and remembers that it did, so a drop after an explicit stop cannot signal a reused pid.
struct Running {
    child: Box<dyn Child + Send + Sync>,
    screen: Box<dyn Read + Send>,
    keyboard: Box<dyn Write + Send>,
    seen: String,
    port: u16,
    stopped: bool,
    _pty: PtyPair,
}

impl Running {
    fn start(bin: &Path, root: &Path, extra: &[(&str, PathBuf)]) -> Running {
        let pty = NativePtySystem::default()
            .openpty(PtySize {
                rows: 40,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open a pty");
        let mut cmd = CommandBuilder::new(bin);
        // The OS's port, at the warden's own bind — SKEIN-436, as in `warden_roundtrip`.
        cmd.env("SKEIN_WARDEN_PORT", "0");
        cmd.env("SKEIN_WARDEN_HOME", root.join("state"));
        cmd.env("SKEIN_WARDEN_LS_CMD", "printf '[]'");
        cmd.env(
            "PATH",
            format!(
                "{}:{}",
                root.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        for (key, value) in extra {
            cmd.env(key, value);
        }
        let child = pty.slave.spawn_command(cmd).expect("start the warden");
        let screen = pty.master.try_clone_reader().expect("read the pty");
        let keyboard = pty.master.take_writer().expect("write to the pty");
        let mut running = Running {
            child,
            screen,
            keyboard,
            seen: String::new(),
            port: 0,
            stopped: false,
            _pty: pty,
        };
        assert!(
            running.wait_for("approvals are asked at"),
            "the warden never said where it asks for approvals:\n{}",
            running.seen
        );
        running.port = port_it_bound(&running.seen).unwrap_or_else(|| {
            panic!(
                "the warden never said which port it bound:\n{}",
                running.seen
            )
        });
        running
    }

    fn wait_for(&mut self, want: &str) -> bool {
        wait_for(&mut self.screen, &mut self.seen, want)
    }

    fn type_line(&mut self, line: &str) {
        writeln!(self.keyboard, "{line}").unwrap();
        self.keyboard.flush().unwrap();
    }

    /// The pid this test spawned, and nothing else: portable-pty's `kill` is `kill(pid, SIGHUP)`.
    fn stop(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.stopped = true;
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// **An approved create whose audit log cannot be written runs, and skein is told it went
/// unrecorded** (SKEIN-554).
///
/// The owner's decision, 2026-09-15: not a refusal. The command runs and the person is warned — so
/// the failure has to arrive in what skein receives, which is `Performed`, the value both of
/// `src/fleet.rs`'s callers of `perform` match on. This drives that value, through the real binary.
///
/// The log is a **directory where its file should be**, under `$SKEIN_WARDEN_AUDIT`, which moves
/// the log and the outcomes and leaves the outcomes writable. `open` refuses a directory with
/// `EISDIR` for every uid, so the test does not depend on who runs it.
#[test]
fn an_approved_create_whose_log_cannot_be_written_runs_and_skein_is_told_it_went_unrecorded() {
    if !have("cargo") {
        return skip("no cargo on PATH, so the warden cannot be built");
    }
    let _env = env_lock();
    let root = Scratch::boxes("skein-warden-unrecorded");
    let log = root.join("sbx.log");
    fake_sbx(&root.join("bin"), &log, &root.join("gate"));
    let bin = built_warden();

    let record = root.join("record");
    let wall = record.join("audit.jsonl");
    std::fs::create_dir_all(&wall).unwrap();

    // Bound after `root`, so the names stop pointing into it before it is removed.
    let mut pins = env_pins();
    pins.set("SKEIN_WARDEN_HOME", root.join("state"))
        .set("SKEIN_HOME", root.join("skein-home"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"));
    let mut warden = Running::start(&bin, &root, &[("SKEIN_WARDEN_AUDIT", record.clone())]);

    let argv = skein::fleet::create_argv("skein-fleet", &["/h/.skein".to_string()]);
    let env = vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), "200g".to_string())];
    let act = Act::Create {
        sandbox: "skein-fleet".into(),
        argv: argv.clone(),
        env: env.clone(),
    };
    let port = warden.port;
    let asking = std::thread::spawn(move || perform_through(&Warden::at("127.0.0.1", port), &act));
    assert!(
        warden.wait_for("Type the operation id"),
        "no approval was put to the terminal:\n{}",
        warden.seen
    );
    warden.type_line(&operation_id_with_env("create", "skein-fleet", &argv, &env));
    let performed = asking.join().unwrap();
    warden.stop();

    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    let creates = ran.lines().filter(|l| l.starts_with("argv create")).count();
    assert_eq!(
        creates, 1,
        "an approved create still runs when the log cannot be written (SKEIN-554), exactly once; \
         sbx ran {creates} times:\n{ran}"
    );
    match &performed {
        Performed::Warden(answered) => assert_eq!(answered.happened(), Some(true)),
        other => panic!("an approved create that ran was not answered as done: {other:?}"),
    }
    let missing = performed.unrecorded();
    assert!(
        !missing.is_empty()
            && missing
                .iter()
                .all(|why| why.contains(&wall.display().to_string())),
        "the create ran with nothing written to {}, and what skein received does not say so — the \
         log that exists because skein cannot audit itself (§5) went missing without a word: \
         {performed:?}",
        wall.display()
    );
}

/// Wait until `log` has a line starting `want`, so a test can tell "sbx was reached" from "sbx was
/// about to be reached" — the distinction the marker exists for.
fn wait_for_line(log: &Path, want: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .any(|l| l.starts_with(want))
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Run `f` on a thread and give up on it after `secs`.
///
/// **So that a warden which wrongly puts the same work to a person again fails a test rather than
/// hanging it.** `perform_through` does not return while an approval is on the screen, and a red
/// that never arrives is read as a test that is merely slow.
fn answered_within<T: Send + 'static>(
    secs: u64,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).ok()
}

/// **A warden killed while an approval is on the screen does not keep the work for ever**
/// (SKEIN-533).
///
/// The id is derived from the work (`operation_id_with_env`), not minted, so "ask again" is the
/// same id — and a claim nothing releases meant that piece of work was answered `409 undecided` for
/// the rest of the retention window, with no way out but deleting a file on the host. The kill is
/// real: the pid this test spawned, and nothing else, exactly as `Running::stop` does it.
///
/// The assertion that carries it is that the same work reaches a person **a second time**. Its
/// counterfactual is a store that never releases an abandoned claim: the second warden then answers
/// immediately, no approval is ever printed, and the `wait_for` below fails.
#[test]
fn a_warden_killed_while_an_approval_is_on_the_screen_can_be_asked_the_same_thing_again() {
    if !have("cargo") {
        return skip("no cargo on PATH, so the warden cannot be built");
    }
    let _env = env_lock();
    let root = Scratch::boxes("skein-warden-killed-asking");
    let log = root.join("sbx.log");
    fake_sbx(&root.join("bin"), &log, &root.join("gate"));
    let bin = built_warden();

    let mut pins = env_pins();
    pins.set("SKEIN_WARDEN_HOME", root.join("state"))
        .set("SKEIN_HOME", root.join("skein-home"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"));

    let argv = skein::fleet::create_argv("skein-fleet", &["/h/.skein".to_string()]);
    let env = vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), "200g".to_string())];
    let asked = || Act::Create {
        sandbox: "skein-fleet".into(),
        argv: argv.clone(),
        env: env.clone(),
    };

    // The first warden is killed with the approval still in front of a person.
    let mut first = Running::start(&bin, &root, &[]);
    let port = first.port;
    let act = asked();
    let abandoned =
        std::thread::spawn(move || perform_through(&Warden::at("127.0.0.1", port), &act));
    assert!(
        first.wait_for("Type the operation id"),
        "no approval was put to the terminal:\n{}",
        first.seen
    );
    first.stop();
    let _ = abandoned.join().unwrap();

    // Nothing ran, which is what makes this the releasable case rather than the other one.
    let before = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !before.lines().any(|l| l.starts_with("argv create")),
        "the warden was killed after it reached sbx, so this is not the case under test:\n{before}"
    );

    // A second warden, the same state directory, and the same work — so the same operation id.
    let mut second = Running::start(&bin, &root, &[]);
    let port = second.port;
    let act = asked();
    let asking = std::thread::spawn(move || perform_through(&Warden::at("127.0.0.1", port), &act));
    assert!(
        second.wait_for("Type the operation id"),
        "the same work was never put to a person again: the killed warden left its claim behind, \
         and because the id is derived from the work rather than minted there is no other id to \
         ask under (SKEIN-533):\n{}",
        second.seen
    );
    second.type_line(&operation_id_with_env("create", "skein-fleet", &argv, &env));
    let performed = asking.join().unwrap();
    second.stop();

    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    let creates = ran.lines().filter(|l| l.starts_with("argv create")).count();
    assert_eq!(
        creates, 1,
        "the retried create must run exactly once; sbx ran {creates} times:\n{ran}"
    );
    match &performed {
        Performed::Warden(answered) => assert_eq!(answered.happened(), Some(true)),
        other => panic!("the retried create was not answered as done: {other:?}"),
    }
}

/// **And a destroy killed after it reached `sbx` is never run again** (SKEIN-533).
///
/// The other half, and the half that makes the first one safe to have. `sbx rm` may have destroyed
/// the fleet before the warden died, so "we do not know" is the only honest answer and the only
/// safe one — releasing this claim would destroy a second fleet. The marker written before the
/// command is what tells this case from the one above.
///
/// The fake `sbx` waits inside `rm` while a gate file exists, so the warden is killed *during* the
/// command rather than around it. The orphaned script is let go by removing the gate, never by
/// signalling a process this test did not spawn.
///
/// Counterfactual: a store that released this claim too. The second warden would then put the same
/// destroy to a person, `answered_within` would time out waiting for an answer that is sitting at
/// an approval prompt, and a second `argv rm` line would appear in the log.
#[test]
fn a_destroy_killed_after_it_reached_sbx_is_never_run_a_second_time() {
    if !have("cargo") {
        return skip("no cargo on PATH, so the warden cannot be built");
    }
    let _env = env_lock();
    let root = Scratch::boxes("skein-warden-killed-running");
    let log = root.join("sbx.log");
    let gate = root.join("gate");
    // `fake_sbx` makes the directory; the gate has to exist before the warden can reach the script.
    std::fs::write(&gate, b"holds sbx rm open until this file goes").unwrap();
    fake_sbx(&root.join("bin"), &log, &gate);
    let bin = built_warden();

    let mut pins = env_pins();
    pins.set("SKEIN_WARDEN_HOME", root.join("state"))
        .set("SKEIN_HOME", root.join("skein-home"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"));

    let mut first = Running::start(&bin, &root, &[]);
    let port = first.port;
    let killed = std::thread::spawn(move || {
        perform_through(
            &Warden::at("127.0.0.1", port),
            &Act::Destroy {
                sandbox: "skein-fleet".into(),
            },
        )
    });
    assert!(
        first.wait_for("Type the operation id"),
        "no approval was put to the terminal:\n{}",
        first.seen
    );
    first.type_line(&operation_id_with_env("destroy", "skein-fleet", &[], &[]));
    // The script writes its argv line and only then waits, so this is "sbx has been reached".
    assert!(
        wait_for_line(&log, "argv rm"),
        "sbx was never reached, so the warden is not being killed mid-destroy"
    );
    first.stop();
    // Let the orphaned script finish on its own rather than signalling it.
    std::fs::remove_file(&gate).unwrap();
    let _ = killed.join().unwrap();

    let mut second = Running::start(&bin, &root, &[]);
    let port = second.port;
    let performed = answered_within(30, move || {
        perform_through(
            &Warden::at("127.0.0.1", port),
            &Act::Destroy {
                sandbox: "skein-fleet".into(),
            },
        )
    });
    let performed = match performed {
        Some(performed) => performed,
        None => panic!(
            "the same destroy was put to a person again — it may already have destroyed the \
             fleet, and approving it a second time destroys another one (SKEIN-533)"
        ),
    };
    second.stop();

    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    let destroys = ran.lines().filter(|l| l.starts_with("argv rm")).count();
    assert_eq!(
        destroys, 1,
        "a destroy that may already have happened was run a second time:\n{ran}"
    );
    match &performed {
        Performed::Uncertain(answered) => {
            // The variant rather than its wording: what the caller branches on is the type, and a
            // test that matched the sentence would go red the day the sentence is reworded.
            assert!(
                matches!(answered, Answered::Undecided(_)),
                "a destroy whose warden died inside sbx has to come back undecided — the one \
                 answer that neither re-runs it nor claims it did not happen: {answered:?}"
            );
            assert_eq!(answered.happened(), None);
            assert!(
                answered.detail().contains("may have happened"),
                "the answer has to tell a person it may have happened, or there is nothing for \
                 them to act on: {answered:?}"
            );
        }
        other => panic!("a destroy that may have happened was answered as settled: {other:?}"),
    }
}
