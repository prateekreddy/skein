//! An act settles every remembered answer it makes wrong — the other three gates.
//!
//! `tests/fleet_launch.rs` covers the liveness gate for the three box acts. This covers the two
//! *sandbox* acts and the gate that was invalidated from nowhere in production at all: `sbx ls`.
//!
//! **Through `tests/` for the same reason as the others.** `cfg!(test)` is false for the library
//! these tests link, so the gates are live here and disabled in unit tests — a missing settle is
//! invisible inside the crate.
//!
//! **Read exactly once after the act.** A `Gate` serves its last good answer while refreshing behind
//! the caller, so a second read can be answered by the refresh the first one started — which hides
//! precisely the staleness under test. Every assertion here is one read.

mod common;

use common::{env_lock, Scratch};
use skein::config::{load_config, save_config};
use skein::sbx::fleet_boxes;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const FLEET: &str = "gate-fleet";

/// An `sbx` whose `ls` answer depends on whether a marker exists — so the act can change it.
fn conditional_sbx(dir: &Path, marker: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  ls) if [ -e {m} ]; then \
             printf '[{{\"name\":\"{FLEET}\"}}]'; else printf '[]'; fi ;;\n\
             *) : ;;\nesac\nexit 0\n",
            m = marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A warden that creates by touching the marker, and counts what it was asked.
fn fake_warden(marker: PathBuf) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let marker = marker.clone();
            let counted = Arc::clone(&counted);
            std::thread::spawn(move || {
                let mut raw = [0u8; 8192];
                let read = stream.read(&mut raw).unwrap_or(0);
                let request = String::from_utf8_lossy(&raw[..read]).to_string();
                if request.contains("/v1/create") {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let _ = std::fs::write(&marker, "made");
                } else if request.contains("/v1/destroy") {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let _ = std::fs::remove_file(&marker);
                }
                let body = r#"{"state":"ran","ok":true,"said":"done"}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            });
        }
    });
    (port, asked)
}

/// Set up a scratch host whose `sbx ls` answers from a marker the fake warden controls.
fn stage(what: &str) -> (Scratch, PathBuf, String, Arc<AtomicUsize>) {
    let root = Scratch::boxes(&format!("skein-gate-{what}"));
    let marker = root.join("sandbox-exists");
    conditional_sbx(&root.join("bin"), &marker);
    let real = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{real}", root.join("bin").display()));
    std::env::set_var("SKEIN_HOME", root.join("skein"));
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));
    std::env::remove_var("SKEIN_LS_CMD");
    let (port, asked) = fake_warden(marker.clone());
    std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
    let mut config = load_config();
    config.fleet_sandbox = FLEET.into();
    save_config(&config).expect("configure the fleet");
    // The gate is process-global and these tests run in one process, so the previous test's answer
    // is exactly the kind of remembered value they are about. Each starts from nothing remembered —
    // which is the one thing a test may do that production must not: the world it is about to warm
    // the gate with is the world it just built.
    skein::sbx::forget_fleet_boxes();
    (root, marker, real, asked)
}

/// Creating the sandbox settles the listing that says it does not exist.
///
/// The gate is warmed with the truth — there is no sandbox — and the act makes that answer wrong.
/// Without the settle the next reader is handed "no sandbox" by a gate that has no reason to doubt
/// itself, which is how a fleet gets created twice or reported missing while it runs.
#[test]
fn creating_the_sandbox_settles_the_listing_that_said_it_was_absent() {
    let _env = env_lock();
    let (_root, marker, real, asked) = stage("create");

    assert!(
        fleet_boxes().unwrap_or_default().is_empty(),
        "the gate must start warm and right: there is no sandbox yet"
    );

    // `ensure_fleet` goes on to install the substrate and the launcher, which this scratch host
    // cannot do — the create is what is under test and it is the first thing it does.
    let _ = skein::fleet::ensure_fleet(FLEET, &[]);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the warden was not asked");
    assert!(marker.exists(), "the fake warden did not create anything");

    // One read, no `forget_fleet_boxes` in between.
    let seen: Vec<String> = fleet_boxes()
        .unwrap_or_default()
        .into_iter()
        .map(|b| b.name)
        .collect();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");
    assert!(
        seen.contains(&FLEET.to_string()),
        "the sandbox was created and the listing still says it is not there: {seen:?}"
    );
}

/// An act that **fails** settles what it disturbed, which is the path a call at the end would miss.
///
/// This is the wrapper itself, against a real gate and a real change in the world — not a
/// particular act, because the acts are the thing that keeps changing. `resize_fleet` is wrapped and
/// so is the create, but a resize re-reads the listing on its way through `ensure_fleet`, so that
/// one gate is self-correcting there whether or not anything settles it. What the wrapper actually
/// buys is the other three gates and every path out that is not the happy one — and a test that
/// passes either way is not a test.
#[test]
fn an_act_that_fails_still_settles_what_it_disturbed() {
    let _env = env_lock();
    let (_root, marker, real, _asked) = stage("failing");
    std::fs::write(&marker, "made").unwrap();

    let seen: Vec<String> = fleet_boxes()
        .unwrap_or_default()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(
        seen.contains(&FLEET.to_string()),
        "the gate must start warm and right: the sandbox is there"
    );

    // The world changes and the act then fails — a destroy that got as far as the wire and lost the
    // reply, a create that made the sandbox and could not install the substrate. The remembered
    // listing is wrong from this moment and nothing else is going to notice.
    let failed: Result<(), String> =
        skein::fleet::disturbing(&[skein::fleet::Remembered::SandboxListing], || {
            std::fs::remove_file(&marker).unwrap();
            Err("it fell over after doing the thing".into())
        });
    assert!(failed.is_err());

    // One read, and no forget in between.
    let after: Vec<String> = fleet_boxes()
        .unwrap_or_default()
        .into_iter()
        .map(|b| b.name)
        .collect();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");
    assert!(
        after.is_empty(),
        "an act that failed left the listing remembering a sandbox that is gone: {after:?}"
    );
}

/// A panic settles it too. A `?` is the common path and a panic is the one a pair of calls loses.
#[test]
fn a_panic_inside_an_act_still_settles_what_it_disturbed() {
    let _env = env_lock();
    let (_root, marker, real, _asked) = stage("panicking");
    std::fs::write(&marker, "made").unwrap();
    assert!(!fleet_boxes().unwrap_or_default().is_empty());

    let fell_over = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        skein::fleet::disturbing(&[skein::fleet::Remembered::SandboxListing], || {
            std::fs::remove_file(&marker).unwrap();
            panic!("the doer fell over mid-act");
        })
    }));
    assert!(fell_over.is_err());

    let after = fleet_boxes().unwrap_or_default();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");
    assert!(
        after.is_empty(),
        "a panic left the listing remembering a sandbox that is gone: {:?}",
        after.into_iter().map(|b| b.name).collect::<Vec<_>>()
    );
}
