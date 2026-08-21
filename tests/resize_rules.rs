//! The rules a resize keeps, each of which was learned the hard way and none of which had a test.
//!
//! A resize is destroy + create, and the thing it destroys is every box's uncommitted work. The
//! carrying machinery has thorough tests; the two rules *around* it did not, and both are ordering
//! or refusal properties that fail silently — a resize that loses a login or a Docker volume looks
//! exactly like one that worked.
//!
//! Its own binary because these drive skein through process-wide environment and need a fake `sbx`,
//! a fake warden, and a configured fleet at once.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const FLEET: &str = "resize-fleet";

fn serialize() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn scratch(what: &str) -> PathBuf {
    let dir = PathBuf::from("/var/tmp").join(format!("skein-resize-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An `sbx` that logs every call in order, and can be told to fail the Docker question.
fn logging_sbx(dir: &Path, log: &Path, docker: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nprintf 'sbx %s\\n' \"$*\" >> {log}\n\
             case \"$1\" in\n\
               ls) printf '[{{\"name\":\"{FLEET}\"}}]' ;;\n\
               exec)\n\
                 # The whole command line, so a test can see WHICH question was asked and when.\n\
                 case \"$*\" in\n\
                   *docker*) {docker} ;;\n\
                   *) : ;;\n\
                 esac ;;\n\
             esac\nexit 0\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A warden that logs what it was asked, so the order of a destroy against everything else is
/// visible.
fn logging_warden(log: PathBuf) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut raw = [0u8; 8192];
                let read = stream.read(&mut raw).unwrap_or(0);
                let request = String::from_utf8_lossy(&raw[..read]).to_string();
                let verb = if request.contains("/v1/destroy") {
                    "warden destroy"
                } else if request.contains("/v1/create") {
                    "warden create"
                } else {
                    "warden other"
                };
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log)
                    .map(|mut f| writeln!(f, "{verb}"));
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
    port
}

fn stage(what: &str, docker: &str) -> (PathBuf, PathBuf, String) {
    let root = scratch(what);
    let log = root.join("calls.log");
    logging_sbx(&root.join("bin"), &log, docker);
    let real = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{real}", root.join("bin").display()));
    std::env::set_var("SKEIN_HOME", root.join("skein"));
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));
    std::env::remove_var("SKEIN_LS_CMD");
    std::env::set_var(
        "SKEIN_WARDEN",
        format!("127.0.0.1:{}", logging_warden(log.clone())),
    );
    let mut config = skein::config::load_config();
    config.fleet_sandbox = FLEET.into();
    skein::config::save_config(&config).expect("configure the fleet");
    (root, log, real)
}

/// **Refuse on "could not ask", not only on "there is something".**
///
/// `/var/lib/docker` is a disk of its own, destroyed with the sandbox and carried by nothing. A
/// wedged dockerd is the state a fleet is most often in when somebody reaches for a resize — so
/// reading silence as "nothing to lose" is the one reading that can destroy something nobody was
/// told about. There was no test for it, and the one resize test that exists passes `--drop-docker`
/// specifically to avoid this path.
#[test]
fn a_resize_that_cannot_ask_about_docker_refuses_rather_than_assuming() {
    let _g = serialize();
    // The Docker question fails. Not "answers empty" — fails, which is what a wedged daemon does.
    // The scratch name deliberately avoids the word the fake matches on: the first version called
    // it "docker", so every `sbx exec` whose script mentioned the scratch path — including the
    // free-space check that runs first — matched the glob and failed. The test then asserted the
    // wrong refusal and would have passed against a resize that never reached the Docker question.
    let (root, log, real) = stage("dk", "exit 1");

    let refused = skein::fleet::resize_fleet("8g", "4", "", false)
        .expect_err("a resize that cannot ask about Docker must refuse");
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        refused.contains("could not check what Docker is holding"),
        "the refusal must name what it could not establish: {refused}"
    );
    assert!(
        refused.contains("untouched"),
        "a refusal has to say the sandbox is still there: {refused}"
    );
    assert!(
        refused.contains("--drop-docker"),
        "and how to go ahead anyway, or it is a dead end: {refused}"
    );
    // The whole point: nothing was destroyed.
    assert!(
        !calls.contains("warden destroy"),
        "the sandbox was destroyed despite not knowing what Docker was holding:\n{calls}"
    );
}

/// **The login is captured before the destroy.**
///
/// It lives in the sandbox's `$HOME`, which the rebuild destroys, and `ensure_fleet` restores it
/// afterwards — but only from something captured first. Its own call runs *after* `sbx create`, when
/// the sandbox is empty and there is nothing left to save. Measured the hard way: a login made
/// between two resizes was gone after the second, and nothing guarded the order.
#[test]
fn the_login_is_read_out_of_the_sandbox_before_it_is_destroyed() {
    let _g = serialize();
    // Docker answers "nothing at risk", so the resize gets past the refusal and on to the work.
    let (root, log, real) = stage("login", ": ");

    // It will not finish on a scratch host — there is no sandbox to rebuild into — and that is the
    // case that matters: the capture has to have happened by the time the destroy does.
    let _ = skein::fleet::resize_fleet("8g", "4", "", true);
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");
    let _ = std::fs::remove_dir_all(&root);

    let destroy = calls
        .lines()
        .position(|l| l == "warden destroy")
        .unwrap_or_else(|| panic!("the resize never reached the destroy:\n{calls}"));
    // `sync_fleet_login` reads the login files out of the sandbox's HOME with `cat "$HOME"/…`.
    let read_login = calls
        .lines()
        .position(|l| l.contains("sbx exec") && l.contains(".credentials.json"))
        .unwrap_or_else(|| panic!("the login was never read out of the sandbox:\n{calls}"));
    assert!(
        read_login < destroy,
        "the login was read after the sandbox was destroyed, which is reading an empty sandbox:\n{calls}"
    );
}
