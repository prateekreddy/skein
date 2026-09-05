//! A doer that was not built is not in the binary — asked of a running warden, not read off source.
//!
//! §8.3's claim is that absence is absence: "a runtime check falls to a bug in the check; absent
//! code falls to nothing." Nothing in the default build can demonstrate that, because the default
//! build has both doers. So this test **builds a second warden without `destroy`**, starts it, and
//! asks it — which is the only evidence that would distinguish a compiled-out doer from a disabled
//! one.
//!
//! It builds into its own target directory. A nested `cargo` sharing the outer one blocks on the
//! build lock the test harness is already holding, which is a deadlock rather than a slow test.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Ask the kernel for a port rather than naming one. See the comment at the spawn below.
const ASK_THE_OS: &str = "0";

/// The port a warden bound, read back off the line it prints before it serves anything —
/// `skein-warden: listening on 127.0.0.1:<port>` (`warden/src/main.rs`). The bind happens before
/// the print, so a warden that got this far is holding the port it names.
///
/// The same reader as `tests/warden_roundtrip.rs`, which is in the other crate: this is a warden
/// integration test and cannot reach across.
fn port_it_bound(said: &str) -> Option<u16> {
    said.split_once("listening on 127.0.0.1:")?
        .1
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One request, one reply, connection closed — the only shape this warden speaks.
fn ask(port: u16, method: &str, path: &str, body: &str, secret: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the warden");
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    // The secret goes on every request, because the warden checks before it routes — including on
    // the endpoint this test exists to find missing. A 401 and a 404 would be indistinguishable to
    // an assertion looking for "no such endpoint".
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nx-skein-warden: {secret}\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write");
    let mut said = String::new();
    stream.read_to_string(&mut said).expect("read");
    said
}

#[test]
fn a_warden_built_without_destroy_does_not_have_a_destroy_endpoint() {
    if !have("cargo") {
        eprintln!("skipping: no cargo on PATH, so a second warden cannot be built");
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target =
        std::env::temp_dir().join(format!("skein-warden-nodestroy-{}", std::process::id()));

    let built = Command::new("cargo")
        .args([
            "build",
            "--quiet",
            "--manifest-path",
            root.join("Cargo.toml").to_str().unwrap(),
            "--no-default-features",
            "--features",
            "create",
            "--target-dir",
            target.to_str().unwrap(),
        ])
        .status()
        .expect("run cargo");
    assert!(built.success(), "a create-only warden must still build");

    let home = target.join("state");
    // **Killed however this test ends.** It used to be killed on the last line, so any assertion
    // that fired before then left a warden alive on the fixed port — for ever, since nothing else
    // knows about it. The NEXT run then connected to the corpse, found the port open, and failed on
    // a missing secret in a home that warden had never heard of. One failure poisoned every
    // subsequent run on the machine, and the second failure said nothing about the first.
    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    // **The kernel picks the port** (SKEIN-436). A written-down number is a collision between two
    // checkouts running `cargo test` at the same time — which is this machine's normal state, not a
    // corner — and the loser dies in `bind` before it can answer anything, so the failure arrives as
    // "the warden never came up" with nothing pointing at the cause. `tests/warden_roundtrip.rs`
    // was fixed this way and this file was not.
    let mut spawned = Command::new(target.join("debug/skein-warden"))
        .env("SKEIN_WARDEN_PORT", ASK_THE_OS)
        .env("SKEIN_WARDEN_HOME", &home)
        .env("SKEIN_WARDEN_LS_CMD", "printf '[]'")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the create-only warden");
    // Drained on a thread, and to EOF: a child whose stderr nobody reads blocks once the pipe
    // fills, and this one is killed rather than waited on, so the thread ends when the pipe closes.
    let talking = spawned.stderr.take().expect("the warden's stderr");
    let child = Reaped(spawned);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut talking = talking;
        let (mut said, mut buf, mut told) = (String::new(), [0u8; 512], false);
        while let Ok(n) = talking.read(&mut buf) {
            if n == 0 {
                break;
            }
            said.push_str(&String::from_utf8_lossy(&buf[..n]));
            if !told {
                if let Some(port) = port_it_bound(&said) {
                    told = tx.send(port).is_ok();
                }
            }
        }
    });
    let port = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the warden never said which port it bound");

    // Waiting on the SECRET FILE rather than on the port, because an open port is not evidence that
    // *this* warden opened it — that is exactly what the leak above turned into a false start. The
    // file is in the home this process was given, so nothing else can have written it.
    let began = Instant::now();
    let secret = loop {
        if let Ok(minted) = std::fs::read_to_string(home.join("secret")) {
            if !minted.trim().is_empty() {
                break minted.trim().to_string();
            }
        }
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the warden never came up, or never minted its secret in {}",
            home.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    // And it is listening, which the file alone does not promise.
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the warden minted its secret and never listened"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let listing = ask(port, "GET", "/v1/fleet", "", &secret);
    let destroyed = ask(
        port,
        "POST",
        "/v1/destroy",
        r#"{"operation":"op-x","sandbox":"skein-fleet"}"#,
        &secret,
    );
    // A create the warden will accept as a create. `argv_create` checks the argv BEFORE it asks
    // the approver (`warden/src/doer.rs:74`), so a request with no `args` is refused for being
    // malformed — a 409 that looks exactly like the one this test wants and means the opposite.
    // That is what it had been getting since `c87bd25` made the requester send the whole argv.
    let created = ask(
        port,
        "POST",
        "/v1/create",
        r#"{"operation":"op-y","sandbox":"skein-fleet",
            "args":["create","--name","skein-fleet","shell","/h/.skein"]}"#,
        &secret,
    );
    drop(child); // explicit, though `Reaped` would do it at the end of the scope either way
    let _ = std::fs::remove_dir_all(&target);

    // It says what it has, and what it has is what was linked.
    assert!(
        listing.contains(r#""capabilities":["create"]"#),
        "a create-only warden advertised something else: {listing}"
    );

    // **404, not 403.** "This warden cannot" and "this warden will not" are different facts and a
    // client that is told the wrong one retries the wrong thing.
    assert!(
        destroyed.starts_with("HTTP/1.1 404"),
        "destroy answered on a warden built without it: {destroyed}"
    );
    assert!(
        destroyed.contains("built without `destroy`") && destroyed.contains("not in the binary"),
        "the refusal must say the doer is absent rather than declined: {destroyed}"
    );

    // And the one it does have is present — refusing for the other reason, which is that nobody can
    // approve it yet. A 404 here would mean the feature split had removed both.
    // Present, and refusing for the *other* reason: this warden was started by a test with no
    // controlling terminal, so there is nobody to approve anything. A 404 here would mean the
    // feature split had removed both doers rather than one.
    assert!(
        created.starts_with("HTTP/1.1 409") && created.contains("no approval surface"),
        "create should be present and unapprovable, not absent: {created}"
    );
}

/// Every doer this warden has — the acts §8.3 makes removable at compile time.
///
/// One list, so that adding a fourth doer is one edit and the assertions below cannot be satisfied
/// by a manifest that declares it and never ships it.
const DOERS: [&str; 3] = ["create", "destroy", "unpublish"];

/// The two that only report have no feature at all, so there is nothing to build them without.
///
/// §8.3 states this as the deliberate exception to §12.10, and the check is on the manifest because
/// that is where a feature would have to appear to exist.
#[test]
fn the_reporting_endpoints_have_no_feature_that_could_remove_them() {
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read the manifest");
    let features = manifest
        .split_once("[features]")
        .expect("the manifest must declare its features")
        .1;
    for reporting in ["audit", "fleet", "observe", "sightings"] {
        assert!(
            !features.contains(&format!("\n{reporting} =")),
            "`{reporting}` became a feature — a capability that only REPORTS may not be built out, \
             or the design loses the ability to see and account for itself (§8.3)"
        );
    }
    for doer in DOERS {
        assert!(
            features.contains(&format!("\n{doer} =")),
            "`{doer}` must stay a feature: what compile-time removal is for is a host that should \
             never destroy a fleet"
        );
    }

    // **The default set, name by name, both directions** — every doer is in it, and nothing that
    // is not a doer is. This used to match the literal `default = ["create", "destroy"]`, which
    // went stale the moment `unpublish` was added (0fa20b8) and took `cargo test --all` red with
    // it; a literal also cannot notice the failure this check is actually for, which is a doer
    // added to the manifest and left OUT of the default set, shipping to nobody.
    let default_set = features
        .lines()
        .find(|line| line.trim_start().starts_with("default ="))
        .expect("the manifest must declare a default feature set");
    let mut ships: Vec<&str> = default_set
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .expect("the default feature set is a list")
        .0
        .split(',')
        .map(|name| name.trim().trim_matches('"'))
        .filter(|name| !name.is_empty())
        .collect();
    ships.sort_unstable();
    let mut doers = DOERS.to_vec();
    doers.sort_unstable();
    assert_eq!(
        ships, doers,
        "the default feature set and the doers have come apart ({default_set}) — every doer ships \
         by default, because resize is destroy + create (§7.3), and nothing that is not a doer is \
         removable at all (§8.3)"
    );
}
