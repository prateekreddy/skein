//! The whole shared-sandbox launch, end to end, against a fake `sbx`.
//!
//! Everything the fleet path needs from the host is `sbx create` and `sbx exec`. But a sandbox is a
//! Linux machine with `bwrap`, `tmux` and `git` — and so is the machine running this test — so
//! `sbx exec <fleet> …` can simply mean "run it here" and the rest is genuinely exercised: a real
//! clone from a real remote, a real bwrap namespace, a real tmux server, real `nsenter` re-entry.
//!
//! What this deliberately does NOT cover is sbx's own behaviour — whether the flags are spelled
//! right, and where a workspace mount lands. Both were verified by hand against a real sandbox
//! instead (see `fleet::create_argv` and `fleet::fleet_workspace`), because no fake can answer them.
//!
//! Skipped rather than failed where the substrate is absent: this suite is about skein's logic, and
//! a machine without `bwrap` cannot host a box at all.

use skein::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const FLEET: &str = "test-fleet";
const BOX: &str = "web-main";

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn sh(script: &str) -> String {
    let out = Command::new("bash")
        .arg("-lc")
        .arg(script)
        .output()
        .expect("bash");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A stand-in for `sbx` that runs the guest command locally.
///
/// `exec` drops its flags and the sandbox name and execs the rest, so an `nsenter` hop reaches the
/// same namespace it would in a real sandbox. `create` only has to succeed — the sandbox in this
/// test is the machine itself.
fn write_fake_sbx(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    fs::write(
        &p,
        r#"#!/usr/bin/env bash
verb="$1"; shift
case "$verb" in
  create) exit 0 ;;
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

/// A bare repo with one commit on `main`, standing in for the remote a box clones from.
fn write_remote(root: &Path) -> String {
    let remote = root.join("remote.git");
    let seed = root.join("seed");
    let git = "git -c user.email=t@example.com -c user.name=test -c init.defaultBranch=main";
    sh(&format!(
        "set -e; git init --bare -q -b main {r}; {git} init -q {s}; \
         cd {s}; echo hello > README.md; {git} add -A; {git} commit -qm seed; \
         {git} remote add origin {r}; {git} push -q origin main",
        r = remote.display(),
        s = seed.display(),
        git = git
    ));
    remote.to_string_lossy().into_owned()
}

/// Deliberately **not** under `/tmp` or `$HOME`: a box binds its own directories over both, so a
/// box root beneath either is unreadable from outside — and `box-session.sh` refuses it outright.
/// The first run of this test put the scratch in `/tmp` and was correctly turned away.
fn scratch() -> PathBuf {
    let d = PathBuf::from("/var/tmp").join(format!("skein-fleet-it-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// One box, from nothing to running to gone.
///
/// A single test rather than several: each step consumes the previous one's real side effects (the
/// anchor pid only exists once the session starts, and the placement only means anything while that
/// pid lives), so splitting them would mean either re-running the launch per assertion or sharing
/// mutable state between tests through the environment — which is exactly what makes suites flaky.
#[test]
fn a_box_lives_and_dies_inside_the_fleet_sandbox() {
    if !have("bwrap") || !have("tmux") || !have("git") {
        eprintln!("skipping: this machine lacks bwrap/tmux/git, so it cannot host a box");
        return;
    }
    let root = scratch();
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);

    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    std::env::set_var("SKEIN_HOME", root.join("skein"));
    // /boxes needs root to create; the seam exists so this path is testable at all.
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));

    // ---- the launcher reaches a sandbox that has never seen this repo ----
    install_launcher(FLEET).expect("install box-session.sh");
    let launcher = box_session_path();
    assert!(
        Path::new(&launcher).exists(),
        "the launcher is embedded and installed over stdin, not served from a repo's store"
    );

    // ---- a checkout, from the remote at the base branch ----
    let place = own_sandbox(FLEET);
    place
        .exec(
            &clone_script(BOX, &remote, "main", "feat/auth"),
            Duration::from_secs(120),
        )
        .expect("clone");
    let tree = format!("{}/tree", box_root(BOX));
    assert_eq!(
        sh(&format!("git -C {tree} rev-parse --abbrev-ref HEAD")),
        "feat/auth",
        "the box starts on its own branch, cut from the remote base"
    );
    // A second box of the same name must not inherit this tree — it may hold uncommitted work.
    assert!(
        place
            .exec(
                &clone_script(BOX, &remote, "main", "feat/auth"),
                Duration::from_secs(60)
            )
            .is_err(),
        "an existing checkout is refused, not reused"
    );

    // ---- the session, and the anchor that outlives its launcher ----
    place
        .exec(
            &session_script(
                BOX,
                "skein-agent",
                "echo agent-started > /tmp/agent.log; exec sleep 400",
            ),
            Duration::from_secs(60),
        )
        .expect("start the box");
    let anchor = read_anchor(FLEET, BOX).expect("anchor pid");
    assert!(
        Path::new(&format!("/proc/{anchor}")).exists(),
        "the launcher has exited by now; the anchor must be the tmux server, which has not"
    );

    record_place(
        BOX,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            home: format!("{}/home", box_root(BOX)),
            tree: tree.clone(),
            sock: box_sock(BOX),
        },
    )
    .unwrap();

    // ---- and now every ordinary skein call lands inside that box ----
    let boxed = place_of(BOX).expect("placed");
    assert_eq!(
        boxed.sandbox, FLEET,
        "the box is not its own sandbox any more"
    );
    assert_eq!(
        boxed.exec("pwd", Duration::from_secs(30)).unwrap().trim(),
        tree,
        "scripts start at the repo root, which nsenter does not inherit"
    );
    assert_eq!(
        boxed
            .exec("echo $HOME", Duration::from_secs(30))
            .unwrap()
            .trim(),
        format!("{}/home", box_root(BOX)),
        "and read the box's own HOME — ~/.claude.json must never be the shared one"
    );
    assert_eq!(
        boxed
            .exec("cat /tmp/agent.log", Duration::from_secs(30))
            .unwrap()
            .trim(),
        "agent-started",
        "the agent really ran inside the namespace"
    );
    // The isolation, from the other side: the box's /tmp is invisible to everyone else.
    assert!(
        !Path::new("/tmp/agent.log").exists(),
        "the box's /tmp leaked into the sandbox's"
    );
    // Bytes, not text: this is the path the Files tab serves images and PDFs down.
    boxed
        .write("cat > blob", &[0u8, 159, 146, 150], Duration::from_secs(30))
        .unwrap();
    assert_eq!(
        boxed.bytes("cat blob", Duration::from_secs(30)).unwrap(),
        vec![0u8, 159, 146, 150],
        "a lossy UTF-8 hop here corrupts every binary the box serves"
    );

    // ---- liveness, without entering anything ----
    let sock = box_sock(BOX);
    assert!(
        sh(&format!(
            "tmux -S {sock} has-session -t skein-agent && echo yes"
        )) == "yes",
        "the socket sits outside the private mounts so the fleet can be listed from outside"
    );

    // ---- teardown drops the last process in the namespace, which frees it ----
    sh(&format!("tmux -S {sock} kill-server"));
    for _ in 0..40 {
        if !Path::new(&format!("/proc/{anchor}")).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !Path::new(&format!("/proc/{anchor}")).exists(),
        "killing the server must end the box"
    );
    forget_place(BOX);
    assert!(
        matches!(place_of(BOX).map(|p| p.sandbox), Some(name) if name == BOX),
        "a forgotten box falls back to the original model rather than a dead namespace"
    );

    let _ = fs::remove_dir_all(&root);
}
