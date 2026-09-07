//! skein asks, a person approves, `sbx` runs — the whole of delivery step 3, end to end.
//!
//! Both callers exercised, which was the argument for building the warden before anything moved.
//! Nothing here is stubbed except `sbx` itself: the warden is the real binary, built and started;
//! the approval is a real one typed at a real terminal; and skein reaches it through
//! `warden_client`, the same path `ensure_fleet` and `resize_fleet` now take.
//!
//! **Under a pty, and that is the point rather than a detail.** The warden's approval surface is
//! `/dev/tty` — it opens it, or it decides nobody is there and refuses everything (§8.1). A test
//! that could approve without a terminal would be testing a warden with a hole in it. So this one
//! allocates a pty, starts the warden inside it, and types.

mod common;

use common::{have, skip, Scratch};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// An `sbx` that records the argv and the environment it was given, and succeeds.
fn recording_sbx(dir: &Path, log: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nprintf 'argv %s\\n' \"$*\" >> {log}\n\
             printf 'disk %s\\n' \"${{DOCKER_SANDBOXES_ROOT_SIZE:-unset}}\" >> {log}\n\
             echo made\nexit 0\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// What the warden is asked for, and the whole of SKEIN-436 in one constant: a port of the OS's
/// choosing, at the warden's own bind. Used at both ends of the check below, so the second warden
/// cannot drift into asking for something the first did not.
const ASK_THE_OS: &str = "0";

/// The port a warden bound, read back off the line it prints before it serves anything —
/// `skein-warden: listening on 127.0.0.1:<port>` (warden/src/main.rs). The bind happens before the
/// print, so a warden that got this far is holding the port it names.
fn port_it_bound(said: &str) -> Option<u16> {
    said.split_once("listening on 127.0.0.1:")?
        .1
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

/// Start a second warden asking for a port exactly the way the first one did, and give back the
/// port it was given — `None` if it never got one.
///
/// This is the other checkout. Two `cargo test` runs on this box at the same time is its normal
/// state, not a corner, and it is what a written-down port breaks: the second warden asks for a
/// number the first is already holding, `bind` refuses, and it exits before printing anything.
///
/// No pty, deliberately. The warden says where it is listening before it goes looking for a
/// terminal, so a pipe carries everything this needs and costs neither a pty nor an approval.
fn a_second_warden_asking_the_same_way(bin: &Path, home: &Path) -> Option<u16> {
    let mut child = Command::new(bin)
        .env("SKEIN_WARDEN_PORT", ASK_THE_OS)
        .env("SKEIN_WARDEN_HOME", home)
        .env("SKEIN_WARDEN_LS_CMD", "printf '[]'")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start a second warden");
    let mut talking = child.stderr.take().expect("the second warden's stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    // On its own thread for the reason `wait_for` below gives: a read from a live and quiet child
    // never returns, so a deadline checked between reads is never reached. A warden that could not
    // bind is the easy case — it exits, its stderr closes, and the read ends of its own accord.
    std::thread::spawn(move || {
        let (mut said, mut buf) = (String::new(), [0u8; 1024]);
        while let Ok(n) = talking.read(&mut buf) {
            if n == 0 {
                break;
            }
            said.push_str(&String::from_utf8_lossy(&buf[..n]));
            if let Some(port) = port_it_bound(&said) {
                let _ = tx.send(Some(port));
                return;
            }
        }
        let _ = tx.send(None);
    });
    let got = rx.recv_timeout(Duration::from_secs(30)).unwrap_or(None);
    let _ = child.kill();
    let _ = child.wait();
    got
}

/// Read from the pty until `want` appears, or give up.
///
/// **The reading happens on its own thread, and the deadline is on the channel.** The obvious
/// version — loop on `reader.read()` while checking a deadline — checks the clock only *between*
/// reads, and a read on a pty whose child is alive and quiet never returns. So the deadline was
/// unreachable in exactly the case it exists for: the warden refusing instead of prompting. It cost
/// twenty minutes of a hung suite to notice, which is the argument for the thread rather than a
/// preference for it.
fn wait_for(reader: &mut Box<dyn Read + Send>, seen: &mut String, want: &str) -> bool {
    use std::sync::mpsc;
    let deadline = Instant::now() + Duration::from_secs(30);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    // The reader is borrowed for the life of this call, and the thread it is handed to outlives it
    // only in the failing case — where the process is about to end anyway.
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
                    // What is left unread stays on the pty for the next call, which is why the
                    // reader is handed back rather than dropped.
                    *reader = Box::new(Draining { rx });
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
}

/// The rest of the pty, once a reading thread owns it: what the thread has already taken, delivered
/// in the order it arrived. Without this, a second `wait_for` would read from a pty another thread
/// is also reading, and the two would split the output between them.
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

#[test]
fn skein_asks_the_warden_a_person_approves_and_sbx_runs_once() {
    if !have("cargo") {
        return skip("no cargo on PATH, so the warden cannot be built");
    }
    let root = Scratch::boxes("skein-warden-rt");
    let target = root.join("target");
    let log = root.join("sbx.log");
    recording_sbx(&root.join("bin"), &log);

    let built = Command::new("cargo")
        .args([
            "build",
            "--quiet",
            "--manifest-path",
            "warden/Cargo.toml",
            "--target-dir",
            target.to_str().unwrap(),
        ])
        .status()
        .expect("run cargo");
    assert!(built.success(), "the warden must build");

    // A pty, so `/dev/tty` exists inside the warden and its approval surface is real.
    let pty = NativePtySystem::default()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    // **Asked of the OS at the warden's own bind, and never chosen here** (SKEIN-436).
    //
    // This was `39_611`, one number every checkout on the machine used, so a second `cargo test` in
    // another worktree was a second warden reaching for a port already bound and one of the two
    // lost. It was then `TcpListener::bind("127.0.0.1:0")`, read back and dropped — which left a
    // window between the drop and the warden's own bind, narrow but real.
    //
    // `SKEIN_WARDEN_PORT=0` has no window, because there is no allocation before the bind: the only
    // process that ever holds this port is the warden, and the number comes back OUT of it. That is
    // the warden's own intent, not a trick found here — "Port 0 gives an ephemeral one, which is
    // how the tests get an address without racing for a fixed number" (`bind`, warden/src/serve.rs).
    //
    // It reaches the same place as the socket handover `tests/ui/lift.mjs` does for the UI suites
    // (`openDoor`, SKEIN-443) and costs less: the handover exists because a *node* suite cannot ask
    // `skein-server` to choose, so it opens the socket itself and passes the descriptor. Doing that
    // here would mean teaching `LISTEN_FDS` to a crate that deliberately depends on nothing
    // (warden/Cargo.toml) — a production change, to reach a property the warden already offers.
    let warden_bin = target.join("debug/skein-warden");
    let mut cmd = CommandBuilder::new(warden_bin.to_str().unwrap());
    cmd.env("SKEIN_WARDEN_PORT", ASK_THE_OS);
    cmd.env("SKEIN_WARDEN_HOME", root.join("state").to_str().unwrap());
    cmd.env("SKEIN_WARDEN_LS_CMD", "printf '[]'");
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let mut child = pty.slave.spawn_command(cmd).expect("start the warden");
    let mut screen = pty.master.try_clone_reader().expect("read the pty");
    let mut keyboard = pty.master.take_writer().expect("write to the pty");

    let mut seen = String::new();
    // **The message says which failure this was** (SKEIN-436). It used to blame the terminal for
    // everything, and when the real cause was a port already bound the pty was perfectly fine — a
    // warden that never started prints nothing, so the sentence arrived with an empty transcript
    // under it and sent the reader to look at the pty.
    assert!(
        wait_for(&mut screen, &mut seen, "approvals are asked at"),
        "the warden never said where it asks for approvals. It printed {} byte(s), so {}:\n{seen}",
        seen.len(),
        match seen.is_empty() {
            true => "it produced nothing at all — it most likely never started",
            false => "it started and then did not get there; what it did print is below",
        }
    );
    let port = port_it_bound(&seen).unwrap_or_else(|| {
        panic!("the warden never said which port it bound, so there is nothing to ask:\n{seen}")
    });

    // **NAMED: two checkouts can run `cargo test` at once, which is what SKEIN-436 is.** Asked of
    // the real mechanism rather than of the source — a second warden, started while this one is up
    // and holding its port, asking for a port exactly the way this one did. With a number written
    // down here the second cannot bind, exits, and reports nothing; with the OS choosing, it is
    // given a different port and says so. The `!=` is the control: two answers that were the same
    // number would mean the port is not really being handed out twice.
    let second = a_second_warden_asking_the_same_way(&warden_bin, &root.join("second"));
    assert!(
        matches!(second, Some(p) if p != port),
        "a second warden, asking for a port the same way this one did while this one is listening \
         on {port}, was given {second:?} — so two checkouts cannot test at the same time, which on \
         this box is the normal state"
    );

    // ---- skein's side: the same call `ensure_fleet` makes ----
    //
    // The secret is read from the warden's home, so skein has to be looking at the same one — which
    // is the join this test now covers. `warden_client` takes the path from the environment rather
    // than from `config`, deliberately: a client that had to ask `config` anything is a client the
    // thing it is talking about could shape. This test is alone in its file, so setting it here
    // races nothing.
    std::env::set_var("SKEIN_WARDEN_HOME", root.join("state"));
    // `$SKEIN_HOME` for the same reason and under the same "alone in its file": `create_argv` below
    // resolves it, and `config::skein_home` refuses an unpinned test rather than answering with the
    // real `~/.skein` (SKEIN-626). Inside this scratch, so what the argv is built from is this
    // test's, not the machine's.
    std::env::set_var("SKEIN_HOME", root.join("skein-home"));
    let warden = skein::warden_client::Warden::at("127.0.0.1", port);
    // The REAL argv, from the function that builds it, rather than a hand-written stand-in. A
    // fixture holding part of an argv is what let the warden prepend a second verb and a second
    // name to every create while three assertions in this file stayed green (SKEIN-456).
    let argv = skein::fleet::create_argv("skein-fleet", &["/h/.skein".to_string()]);
    let env: Vec<(String, String)> = vec![("DOCKER_SANDBOXES_ROOT_SIZE".into(), "200g".into())];

    let asking = {
        let (argv, env) = (argv.clone(), env.clone());
        std::thread::spawn(move || {
            skein::warden_client::Warden::at("127.0.0.1", port).create("skein-fleet", &argv, &env)
        })
    };

    // ---- the person's side ----
    assert!(
        wait_for(&mut screen, &mut seen, "Type the operation id"),
        "no approval was put to the terminal:\n{seen}"
    );
    // What is on the screen is what will run — including the environment, which is most of what
    // this command does. An approval that showed the argv and not the disk size would be showing a
    // 20 GB create as a 200 GB one.
    assert!(
        seen.contains("DOCKER_SANDBOXES_ROOT_SIZE=200g"),
        "the environment was not in what the person was shown:\n{seen}"
    );
    // The whole line, not a phrase inside it. `contains("sbx create skein-fleet")` was true of
    // `sbx create skein-fleet create --name skein-fleet …`, which is what the warden really ran and
    // what sbx rejects — its usage is `sbx create [flags] AGENT PATH [PATH...]`, so the second word
    // is read as the agent.
    assert!(
        seen.contains(&format!("sbx {}", argv.join(" "))),
        "what the person was shown is not the argv skein sent:\n  sent: sbx {}\n{seen}",
        argv.join(" ")
    );

    let operation =
        skein::warden_client::operation_id_with_env("create", "skein-fleet", &argv, &env);
    assert!(
        seen.contains(&operation),
        "the id on the screen is not the id skein asked under:\n{seen}"
    );
    writeln!(keyboard, "{operation}").unwrap();
    keyboard.flush().unwrap();

    let answered = asking.join().unwrap().expect("the warden answered");
    assert_eq!(
        answered.happened(),
        Some(true),
        "an approved create did not happen: {}",
        answered.detail()
    );

    // ---- and the retry, which is the whole of §8.2 ----
    // Same call, same derived id, no second prompt and no second `sbx`.
    let again = warden
        .create("skein-fleet", &argv, &env)
        .expect("the warden answered the retry");
    assert!(
        matches!(again, skein::warden_client::Answered::Replayed(_)),
        "a retry was treated as new work: {again:?}"
    );

    // And the other half: a caller that cannot read the secret is refused, on the endpoint that
    // only reports. After 4c the narrow bind stops being the boundary, and this is what replaces it
    // — so the test is over the same socket the approved create just went through.
    std::env::set_var("SKEIN_WARDEN_HOME", root.join("nowhere"));
    let stranger = skein::warden_client::Warden::at("127.0.0.1", port).look();
    std::env::remove_var("SKEIN_WARDEN_HOME");
    let refusal = stranger.err().unwrap_or_default();
    assert!(
        refusal.contains("does not know who is asking") || refusal.contains("unreadable"),
        "a caller holding no secret was answered anyway: {refusal}"
    );

    let _ = child.kill();
    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    let creates = ran.lines().filter(|l| l.starts_with("argv create")).count();
    assert_eq!(
        creates, 1,
        "sbx ran {creates} times for one operation:\n{ran}"
    );
    // And it ran the argv skein sent, whole. A count of lines *starting* `argv create` cannot tell
    // that apart from a doubled one, because a doubled argv starts with `create` too.
    let sent = format!("argv {}", argv.join(" "));
    assert!(
        ran.lines().any(|l| l == sent),
        "sbx was run with an argv that is not the one skein sent:\n  sent: {sent}\n  ran:\n{ran}"
    );
    assert!(
        ran.contains("disk 200g"),
        "the environment did not reach the command:\n{ran}"
    );
}
