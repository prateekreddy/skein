//! **The rustup-init binary a first install downloads ends within a bound when its mirror stops
//! answering, and the log says why** (SKEIN-1120).
//!
//! bootstrap.sh bounds its own curl of sh.rustup.rs (SKEIN-1037) and rustup's toolchain download
//! (SKEIN-1090, its download timeout). Between the two, the installer script it runs fetches
//! the rustup-init BINARY with a curl of its own, and that curl sets no bound: rustup-init.sh
//! 1.29.0's `downloader()` passes `--retry 3 -C - --proto '=https' --tlsv1.2 --ciphers …
//! --silent --show-error --fail --location URL --output FILE`, and nothing else. What it does not
//! pass is `-q`, so curl reads a config file first — and bootstrap now hands it one, by pointing
//! curl's home-directory variable at a directory of its own.
//!
//! **A stand-in installer, not the real one, and why that is not the test asserting on itself.**
//! The real script is fetched from the network, which this suite never touches. The stand-in makes
//! the same curl call the real `downloader()` makes, minus `--ciphers` (whose argument depends on
//! the TLS library curl was built against), and it is REAL curl on the other end of it. The claim
//! it stands on — that the real script honours that file — was measured once by hand against
//! rustup-init.sh 1.29.0 and the same silent listener: with the file, four `curl: (28) Connection
//! timed out after 2003 milliseconds` and an exit in 15 s at a 2-second bound; without it, still
//! "downloading installer" when it was killed at 25 s.
//!
//! **A silent listener over https is the TLS handshake that never comes**: curl's connect phase,
//! which `--connect-timeout` bounds and nothing else does. Without the file, curl's own connect
//! default is 300 seconds an attempt, four attempts — so the run outlasts this test's limit and the
//! first assertion fails by name.

#[path = "common/mod.rs"]
mod common;

use common::{have, skip, Scratch};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Accepts every connection and never says a word, holding each open for the life of the test
/// process — a server that hung up would be a different failure, which curl reports at once.
fn silent_listener() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
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

fn stub(bin: &Path, name: &str, body: &str) {
    let at = bin.join(name);
    fs::write(&at, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The installer script as the stub `curl` hands it to bootstrap for sh.rustup.rs: the real
/// `downloader()`'s curl line, and the real script's two words around a failure.
const INSTALLER: &str = r#"set -u
echo 'info: downloading installer' >&2
if ! curl --retry 3 -C - --proto '=https' --tlsv1.2 --silent --show-error --fail --location \
    "$RUSTUP_UPDATE_ROOT/dist/x86_64-unknown-linux-gnu/rustup-init" \
    --output "$HOME/rustup-init"; then
  echo 'error: command failed: downloader' >&2
  exit 1
fi
"#;

/// **What would make it fail:** deleting the variable assignment that prefixes the installer line in
/// bootstrap.sh (or the `.curlrc` it names). curl then reads no bound, waits out its own 300-second
/// connect default on the silent handshake, the run is killed at `limit`, and the first assertion
/// fails with the log so far. Dropping the `if !` around the install instead ends the run with the
/// stand-in's line as the last word, and the log assertion fails.
#[test]
fn the_installer_download_from_a_mirror_that_never_answers_ends_with_the_reason_in_the_log() {
    if !have("curl") {
        skip("this machine has no curl, so there is no installer download to bound");
        return;
    }
    let curl = Command::new("sh")
        .args(["-c", "command -v curl"])
        .output()
        .expect("sh ran");
    let curl = String::from_utf8_lossy(&curl.stdout).trim().to_string();

    let root = Scratch::temp("skein-installer-stall-it");
    let bin = root.join("bin");
    let home = root.join("home");
    let skein_home = root.join("skein");
    let fleet_root = root.join("boxes");
    for dir in [&bin, &home, &skein_home, &fleet_root] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(root.join("meminfo"), "MemTotal: 4194304 kB\n").unwrap();
    fs::write(root.join("installer.sh"), INSTALLER).unwrap();

    // Present, so nothing asks apt; no cargo that runs, so bootstrap goes to install one.
    for present in ["cc", "git", "tmux", "python3", "jq"] {
        stub(&bin, present, "exit 0");
    }
    stub(&bin, "cargo", "exit 1");
    stub(&bin, "nproc", "echo 2");
    stub(
        &bin,
        "sudo",
        "echo 'the installer-stall fixture ran sudo' >&2; exit 1",
    );
    // sh.rustup.rs is answered from disk; every other download is this machine's real curl.
    stub(
        &bin,
        "curl",
        &format!(
            "case \"$*\" in *sh.rustup.rs*) cat '{}'; exit 0 ;; esac\nexec '{curl}' \"$@\"",
            root.join("installer.sh").display()
        ),
    );

    let (port, accepted) = silent_listener();
    let script =
        fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/bootstrap.sh")).expect("bootstrap.sh");
    let mut child = Command::new("bash")
        .stdin(script)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", &home)
        .env("SKEIN_FLEET_ROOT", &fleet_root)
        .env("SKEIN_HOME", &skein_home)
        .env("SKEIN_MEMINFO", root.join("meminfo"))
        .env("SKEIN_FLEET_MEMORY", "4g")
        .env("SKEIN_FLEET_CPUS", "2")
        .env("SKEIN_NET_STALL_SECS", "2")
        .env(
            "RUSTUP_UPDATE_ROOT",
            format!("https://127.0.0.1:{port}/rustup"),
        )
        // Its own group, so a run that outlives the limit is ended whole — the curl under it
        // included — rather than leaving a download waiting on the listener.
        .process_group(0)
        .spawn()
        .expect("bootstrap started");
    let stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut said = String::new();
        let _ = std::io::Read::read_to_string(&mut { stderr }, &mut said);
        said
    });

    let limit = Duration::from_secs(60);
    let until = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > until {
            // SAFETY: `kill` has no memory effects, and the group is led by a child this test
            // spawned with `process_group(0)` and has not reaped.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let log = log.join().unwrap_or_default();

    assert!(
        status.is_some(),
        "the installer's download was still waiting on a mirror that never answers after \
         {limit:?} — a first install would sit at \"installing a Rust toolchain\" for as long as \
         the connection stayed open. The log:\n{log}"
    );
    assert!(
        accepted.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the silent mirror was never connected to, so nothing here waited on it — the fixture is \
         wrong, not the install:\n{log}"
    );
    assert!(
        !status.unwrap().success(),
        "an installer download that gave up did not end the install as a failure:\n{log}"
    );
    // curl's reason by its EXIT CODE, not its words. 28 is the operation-timeout code in every curl
    // there is, and `--show-error` prints it as `curl: (28) …`; the words after it are curl's own
    // and depend on its version and on the phase the bound caught. The first spelling here matched
    // "Connection timed out", which is what curl 8.18 says, and it was red on the ubuntu-24.04
    // runner, whose curl says `curl: (28) SSL connection timeout` for the same stall (SKEIN-1216).
    // A mirror that hung up instead, or refused, is `(35)`, `(56)` or `(7)`, and fails this.
    assert!(
        log.contains("curl: (28)")
            && log.contains("rustup could not download the Rust toolchain")
            && log.contains("run bootstrap.sh again"),
        "the log does not carry curl's reason, what it means in the owner's terms, and what to do \
         next:\n{log}"
    );
}
