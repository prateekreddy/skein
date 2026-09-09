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

use common::{env_lock, env_pins, EnvPins, Scratch};
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
                // The sighting answers from the same marker `sbx ls` reads, because it is the same
                // question asked of the other machine: in-fleet it is the ONLY way to ask it.
                let body = match request.starts_with("GET /v1/fleet") {
                    true => match marker.exists() {
                        true => format!("{{\"sandboxes\":[\"{FLEET}\"],\"capabilities\":[]}}"),
                        false => "{\"sandboxes\":[],\"capabilities\":[]}".to_string(),
                    },
                    false => r#"{"state":"ran","ok":true,"said":"done"}"#.to_string(),
                };
                let body = body.as_str();
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

/// **Which sandboxes exist, from the one source that can answer here.**
///
/// The warden's sighting. `sbx ls` was the other reader and it asks about a machine this process is
/// not standing on — skein runs inside the sandbox (SKEIN-576) — so `sbx::fleet_boxes` honestly
/// returns nothing here and the branch that read it went with the deployment that made it an
/// answer. One fact (`Remembered::SandboxListing`), and these tests are about the fact rather than
/// about a reader, which is why they ask through this rather than inline.
fn sandboxes_now() -> Vec<String> {
    skein::warden_client::sighting()
        .map(|s| s.sandboxes)
        .unwrap_or_default()
}

/// Set up a scratch host whose `sbx ls` answers from a marker the fake warden controls.
///
/// The pins come back **last** in the tuple so they drop **first**: bindings from one `let` are
/// dropped in reverse, so the variables stop naming the scratch root before the scratch root is
/// removed. `$PATH` and `$SKEIN_WARDEN` are pinned here as well as put back by hand in the tests —
/// each test restores them at the point it wants them restored, mid-body, and these pins are what
/// covers the path where an assertion before that line unwinds past it.
fn stage(what: &str) -> (Scratch, PathBuf, String, Arc<AtomicUsize>, EnvPins) {
    let root = Scratch::boxes(&format!("skein-gate-{what}"));
    let marker = root.join("sandbox-exists");
    conditional_sbx(&root.join("bin"), &marker);
    let real = std::env::var("PATH").unwrap_or_default();
    let mut pins = env_pins();
    pins.set("PATH", format!("{}:{real}", root.join("bin").display()))
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .unset("SKEIN_LS_CMD");
    let (port, asked) = fake_warden(marker.clone());
    pins.set("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
    let mut config = load_config();
    config.fleet_sandbox = FLEET.into();
    save_config(&config).expect("configure the fleet");
    // The gate is process-global and these tests run in one process, so the previous test's answer
    // is exactly the kind of remembered value they are about. Each starts from nothing remembered —
    // which is the one thing a test may do that production must not: the world it is about to warm
    // the gate with is the world it just built.
    skein::sbx::forget_fleet_boxes();
    // The sighting too, and for exactly the same reason: it is process-global, each test points
    // `$SKEIN_WARDEN` at a fresh fake, and a gate holding the previous test's answer would serve
    // one test's warden to the next. It is the second reader of the same fact (SKEIN-576) and it
    // has to be warmed the same way.
    skein::warden_client::forget_sighting();
    (root, marker, real, asked, pins)
}

/// Creating the sandbox settles the answer that says it does not exist — **whichever answer that
/// is in this deployment**.
///
/// The gate is warmed with the truth — there is no sandbox — and the act makes that answer wrong.
/// Without the settle the next reader is handed "no sandbox" by a gate that has no reason to doubt
/// itself, which is how a fleet gets created twice or reported missing while it runs.
///
/// **The property did not change; its source did** (SKEIN-576). Host-side the answer is `sbx ls`.
/// In-fleet `sbx ls` cannot answer at all — it is a question about the *machine*, and this process
/// is not standing on it — so the answer comes from the warden's sighting, which is where
/// `fleet::create_fleet_operation` reads it. Both are remembered, both go stale the instant a
/// create succeeds, and both are asserted here, because a test that checked only the one this
/// machine happens to use would go green on a deployment where the thing it protects is broken.
///
/// It found a real one: nothing settled the sighting. A create through the warden left it saying
/// "no sandboxes" for its whole freshness window, so the pane that had just made a fleet reported
/// it absent — which is the same failure as the listing's, at the answer's new source.
#[test]
fn creating_the_sandbox_settles_the_listing_that_said_it_was_absent() {
    let _env = env_lock();
    let (_root, marker, real, asked, _pins) = stage("create");

    assert!(
        fleet_boxes().unwrap_or_default().is_empty(),
        "the gate must start warm and right: there is no sandbox yet"
    );
    // The other answer, warmed the same way. Both gates now hold "absent", which is the truth
    // until the line below makes it false — and a remembered truth is exactly what this is about.
    assert!(
        skein::warden_client::sighting()
            .map(|s| s.sandboxes.is_empty())
            .unwrap_or(false),
        "the sighting must start warm and right: the warden can see no sandbox yet"
    );

    // `request_fleet_create` rather than `ensure_fleet` (SKEIN-576): creating a fleet stopped
    // being a side effect of starting a box and became the explicit act a person initiates, so the
    // settle it owes is owed by the act. The property is unchanged — what is under test is that
    // the listing does not go on saying "absent" after something made the sandbox.
    let _ = skein::fleet::request_fleet_create(FLEET, &[]);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the warden was not asked");
    assert!(marker.exists(), "the fake warden did not create anything");

    // One read, no `forget_fleet_boxes` in between.
    let listing = fleet_boxes();
    let seen: Vec<String> = listing
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|b| b.name)
        .collect();
    // The other answer, read the same way: once, with nothing forgotten in between. The fake
    // warden lists the sandbox now, so a sighting that still says otherwise is a remembered one.
    let sighted = skein::warden_client::sighting()
        .map(|s| s.sandboxes)
        .unwrap_or_default();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");
    // **`sbx ls` may not answer at all**, and that is the assertion rather than a precondition for
    // one. It asks about the machine this process is not standing on, so `None` is honest — "cannot
    // ask" rather than "absent" — and there is no remembered lie for a settle to correct. This used
    // to be one arm of two, the other asserting that a host's listing DID see the new sandbox; that
    // arm went with the host (SKEIN-576). What is left can still fail, and fails in the direction
    // that matters: a listing that answers here has started guessing, and a caller will act on it.
    assert!(
        listing.is_none(),
        "`sbx ls` produced an answer ({listing:?}), which it cannot do honestly — it is a question \
         about the host, and an answer here is a guess a caller will act on"
    );
    let _ = &seen;
    assert!(
        sighted.contains(&FLEET.to_string()),
        "the sandbox was created and the warden's sighting still says it is not there: \
         {sighted:?} — in-fleet that sighting IS the answer to whether the fleet exists, so a \
         person who just made one is told to make another"
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
    let (_root, marker, real, _asked, _pins) = stage("failing");
    std::fs::write(&marker, "made").unwrap();

    let seen = sandboxes_now();
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
    let after = sandboxes_now();
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
    let (_root, marker, real, _asked, _pins) = stage("panicking");
    std::fs::write(&marker, "made").unwrap();
    assert!(!sandboxes_now().is_empty());

    let fell_over = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        skein::fleet::disturbing(&[skein::fleet::Remembered::SandboxListing], || {
            std::fs::remove_file(&marker).unwrap();
            panic!("the doer fell over mid-act");
        })
    }));
    assert!(fell_over.is_err());

    let after = sandboxes_now();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");
    assert!(
        after.is_empty(),
        "a panic left the listing remembering a sandbox that is gone: {after:?}"
    );
}
