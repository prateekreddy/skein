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

    // Port 0 would be answered by the kernel, but the warden prints its address to stderr and
    // reading that back is a second thing to get wrong. A fixed high port, retried, is simpler and
    // this test is the only thing on it.
    let port: u16 = 39_517;
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
    let child = Reaped(
        Command::new(target.join("debug/skein-warden"))
            .env("SKEIN_WARDEN_PORT", port.to_string())
            .env("SKEIN_WARDEN_HOME", &home)
            .env("SKEIN_WARDEN_LS_CMD", "printf '[]'")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the create-only warden"),
    );

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
    let created = ask(
        port,
        "POST",
        "/v1/create",
        r#"{"operation":"op-y","sandbox":"skein-fleet"}"#,
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
    for doer in ["create", "destroy"] {
        assert!(
            features.contains(&format!("\n{doer} =")),
            "`{doer}` must stay a feature: what compile-time removal is for is a host that should \
             never destroy a fleet"
        );
    }
    assert!(
        features.contains(r#"default = ["create", "destroy"]"#),
        "both doers ship by default, because resize is destroy + create (§7.3)"
    );
}
