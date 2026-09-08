//! The rules a resize keeps, each of which was learned the hard way and none of which had a test.
//!
//! A resize is destroy + create, and the thing it destroys is every box's uncommitted work. The
//! carrying machinery has thorough tests; the two rules *around* it did not, and both are ordering
//! or refusal properties that fail silently — a resize that loses a login or a Docker volume looks
//! exactly like one that worked.
//!
//! **The create half is a person's act at the host now** (SKEIN-679): the destroy ends the process
//! performing it, so `skein resize` refuses and prints the lines rather than running them, and the
//! phases after the destroy are gone. The rules below are the rules of the half that is left — and
//! the third test here is the guard that keeps the CLI on the refusing side of it.
//!
//! Its own binary because these drive skein through process-wide environment and need a stood-in
//! executor, a fake warden, and a configured fleet at once.
//!
//! **What a fleet-scope command runs is stood in for through `place::seam`, not through `$PATH`**
//! (SKEIN-592). A fake `sbx` on `$PATH` only intercepts when there is an `sbx` hop to intercept:
//! in-fleet the script runs on this machine, the fake is bypassed, and this file read the box's
//! **live Docker volumes** — it named three belonging to other people's work. Only a read that
//! time; the arm beside it destroys a sandbox. The `$PATH` route cannot be reopened either, because
//! fleet-scope scripts run under a fixed PATH on purpose (ISO-1). The seam is a compile-time
//! substitution nothing outside this process can select, and it is absent from a release build.

mod common;

use common::{env_lock, Scratch};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLEET: &str = "resize-fleet";

/// **The stand-in for every fleet-scope command**, in both deployments.
///
/// It writes the transcript line the fake `sbx` used to write — `sbx exec <script>` — so the
/// ordering assertions below read the same either way, and then answers the Docker question the way
/// this test needs. Nothing real is run: that is the point, and it is what makes running this file
/// in-fleet safe.
///
/// Every argv is logged, including ones this fixture did not expect. A command that slipped past
/// the stand-in would otherwise run for real and be invisible, which is exactly how this file came
/// to be reading live volumes.
fn stand_in_for_fleet_commands(log: PathBuf, docker: String) -> skein::place::seam::Installed {
    skein::place::seam::install(Box::new(move |argv: &[String]| {
        let script = argv.last().cloned().unwrap_or_default();
        let answer = match script.contains("docker") {
            true => docker.clone(),
            false => ":".to_string(),
        };
        Some(vec![
            "sh".to_string(),
            "-c".into(),
            format!(
                "printf 'sbx exec %s\\n' {script} >> {log}\n{answer}\n",
                script = shell_quote(&script),
                log = log.display(),
            ),
        ])
    }))
}

/// Single-quoted for `sh`, with embedded quotes closed and reopened — the scripts carry them.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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

fn stage(what: &str, docker: &str) -> (Scratch, PathBuf, String, skein::place::seam::Installed) {
    let root = Scratch::boxes(&format!("skein-resize-{what}"));
    let log = root.join("calls.log");
    // `sbx ls` still comes from a fake on `$PATH`: it is a question about the machine rather than a
    // fleet-scope command, so it does not go through `Place` and the seam never sees it.
    logging_sbx(&root.join("bin"), &log, docker);
    let stood_in = stand_in_for_fleet_commands(log.clone(), docker.to_string());
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
    (root, log, real, stood_in)
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
    let _env = env_lock();
    // The Docker question fails. Not "answers empty" — fails, which is what a wedged daemon does.
    // The scratch name deliberately avoids the word the fake matches on: the first version called
    // it "docker", so every `sbx exec` whose script mentioned the scratch path — including the
    // free-space check that runs first — matched the glob and failed. The test then asserted the
    // wrong refusal and would have passed against a resize that never reached the Docker question.
    let (_root, log, real, _stood_in) = stage("dk", "exit 1");

    let refused = skein::fleet::resize_fleet("8g", "4", "", false)
        .expect_err("a resize that cannot ask about Docker must refuse");
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");

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
/// It lives in the sandbox's `$HOME`, which the destroy takes, and `ensure_fleet` restores the kept
/// copy at the first box start in whatever sandbox comes next — but only from something captured
/// first, and its own call runs *after* `sbx create`, when the sandbox is empty and there is
/// nothing left to save. Measured the hard way: a login made between two resizes was gone after the
/// second, and nothing guarded the order. Still the rule now that nothing here rebuilds
/// (SKEIN-679): the capture is what makes the next sandbox a fleet somebody is already logged into.
#[test]
fn the_login_is_read_out_of_the_sandbox_before_it_is_destroyed() {
    let _env = env_lock();
    // Docker answers "nothing at risk", so the resize gets past the refusal and on to the work.
    let (_root, log, real, _stood_in) = stage("login", ": ");

    // It will not finish on a scratch host — there is no sandbox to rebuild into — and that is the
    // case that matters: the capture has to have happened by the time the destroy does.
    let _ = skein::fleet::resize_fleet("8g", "4", "", true);
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    std::env::set_var("PATH", real);
    std::env::remove_var("SKEIN_WARDEN");

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
/// **`skein resize` refuses in-fleet, and it refuses before it can reach the destroy** (SKEIN-679).
///
/// `resize_fleet` copies every box out and then asks the warden to destroy the sandbox skein is
/// running inside. The CLI used to call it with no guard at all — only the cockpit's route asked —
/// so the one surface with no browser between a person and the irreversible half was the unguarded
/// one. The phases that were supposed to put the fleet back afterwards could not run, because the
/// destroy ends this process; they are gone, and the CLI now says so instead of trying.
///
/// **Read out of the source, deliberately.** `cmd_resize` is private to the `skein` binary, and the
/// alternative to reading it is running it: on a box with a fleet configured that is an `sbx rm -f`
/// against the machine the test is running on — the exact thing the guard exists to prevent, done
/// on the one day the guard is broken. Same technique, and the same reason, as
/// `neither_lifecycle_route_reaches_its_work_by_a_path_that_skips_the_check` in the server.
///
/// **What would make this fail**: giving `cmd_resize` a call to `resize_fleet` again, guarded or
/// not. Proved by putting the old body back, which fired the second assertion.
#[test]
fn the_cli_resize_refuses_instead_of_reaching_the_destroy() {
    let cli =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/skein.rs"))
            .expect("read the CLI");
    // `cmd_resize` is a top-level function, so its closing brace is the first one in column zero
    // after the signature. Anything indented is inside it. (Structural cuts in this repo match at
    // the symbol's OWN indent for exactly this reason — at column zero the two happen to agree.)
    let from = cli
        .find("\nfn cmd_resize(")
        .expect("the CLI has no cmd_resize any more — re-read this test");
    let body = &cli[from..][..cli[from..].find("\n}\n").expect("cmd_resize does not end")];

    assert!(
        body.contains("fleet_lifecycle_refusal("),
        "`skein resize` no longer asks where skein is running: in-fleet what it called next \
         destroyed the machine this process is on (docs/architecture.md §7.5):\n{body}"
    );
    assert!(
        !body.contains("resize_fleet("),
        "`skein resize` reaches the destroy again — and the phases that were supposed to put the \
         fleet back afterwards no longer exist:\n{body}"
    );
}
