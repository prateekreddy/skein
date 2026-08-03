//! The one sandbox that hosts many boxes.
//!
//! An sbx sandbox is a microVM, and its memory is a *reservation*: skein's original model gives each
//! box one, so five boxes reserve five times the peak even though four of them are idle in an editor.
//! On a 36 GB machine that is what makes the host start refusing work. The boxes' needs are spiky and
//! rarely simultaneous — a build wants 5 GB for two minutes, editing wants almost nothing — so the
//! fix is to make memory a pool they share rather than N ceilings that sum.
//!
//! So: one sandbox, many boxes, each in its own bwrap namespace with its own `/tmp`, `$HOME` and
//! checkout (see `box-session.sh`). Inside a VM, cgroup limits are *ceilings* rather than
//! reservations — a box capped at 8 GB that uses 200 MB costs 200 MB — which is the whole reason
//! this shape wins.
//!
//! Everything here is inert until [`crate::place::fleet_sandbox`] names a sandbox. Boxes already
//! running as their own VM keep running that way: [`crate::place::place_of`] follows a box's own
//! record, so turning this on never retroactively reinterprets one.

use crate::config::*;
use crate::place::own_sandbox;
use crate::util::*;
use crate::{fleet_boxes, valid_name};
use std::time::Duration;

/// The launcher, embedded so it can be installed into a sandbox that has never seen this repo.
/// The fleet sandbox hosts boxes from *many* repos, so it cannot be served out of any one repo's
/// store — and shipping it through a store would put runtime tooling in shared data besides.
const BOX_SESSION_SH: &str = include_str!("box-session.sh");

/// Where box roots live inside the fleet sandbox.
///
/// Deliberately not under `$HOME` or `/tmp`: `box-session.sh` binds the box's own directories over
/// both, so a root beneath either would be visible only from inside the box that owns it — and the
/// tmux socket and anchor pidfile that skein reads from outside live in this root.
pub const FLEET_ROOT: &str = "/boxes";

/// Where the launcher is installed inside the fleet sandbox.
pub const BOX_SESSION_PATH: &str = "/boxes/.skein/box-session.sh";

/// One box's root inside the fleet sandbox. Callers must have validated `name`; every path skein
/// derives for a box hangs off this, so a name containing `..` would escape the layout entirely.
pub fn box_root(name: &str) -> String {
    format!("{FLEET_ROOT}/{name}")
}

/// The box's tmux socket — the same path inside the namespace and out, which is what lets skein
/// list, attach to and kill a box without entering it first.
pub fn box_sock(name: &str) -> String {
    format!("{}/session.sock", box_root(name))
}

/// The file `box-session.sh` writes the box's anchor pid to. Read from outside, so it must sit in
/// the box root rather than in the box's private `/tmp`.
pub fn box_pidfile(name: &str) -> String {
    format!("{}/anchor.pid", box_root(name))
}

/// The `sbx create` argv for the fleet sandbox.
///
/// `shell` rather than an agent: nothing runs in the sandbox itself — every agent runs inside a box's
/// namespace, started by `box-session.sh`. The workspace is `~/.skein/repos`, the *parent* of every
/// repo's store, so adding a repo later needs no recreate; a box's checkout is not mounted at all,
/// because boxes clone from the remote onto VM-local disk (measured ~5× faster to write and ~14×
/// faster to read than a virtiofs mount, which matters for a build).
///
/// Memory and CPU are left to sbx's own defaults unless configured. Passing nothing is already the
/// win — one reservation shared by every box instead of one each — and picking a number here would
/// be guessing at a machine skein cannot see.
pub fn create_argv(sandbox: &str, workspace: &str) -> Vec<String> {
    let config = load_config();
    let mut argv = vec!["create".to_string(), "--name".into(), sandbox.to_string()];
    let memory = config.fleet_memory.trim();
    if !memory.is_empty() {
        argv.push("-m".into());
        argv.push(memory.to_string());
    }
    let cpus = config.fleet_cpus.trim();
    if !cpus.is_empty() {
        argv.push("--cpus".into());
        argv.push(cpus.to_string());
    }
    argv.push("shell".into());
    argv.push(workspace.to_string());
    argv
}

/// Is the fleet sandbox already there?
///
/// `None` when sbx could not be asked at all — which is not the same as "absent", and must not be,
/// or a wedged daemon would have skein try to create a sandbox that already exists.
pub fn fleet_exists(sandbox: &str) -> Option<bool> {
    Some(fleet_boxes()?.iter().any(|b| b.name == sandbox))
}

/// Create the fleet sandbox if it is missing, then install the launcher into it.
///
/// Idempotent: safe to call before every launch, which is how a sandbox the user removed by hand
/// comes back rather than leaving every box unstartable.
pub fn ensure_fleet(sandbox: &str, workspace: &str) -> Result<(), String> {
    if !valid_name(sandbox) {
        return Err("invalid fleet sandbox name".into());
    }
    match fleet_exists(sandbox) {
        Some(true) => {}
        Some(false) => {
            let argv = create_argv(sandbox, workspace);
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            let (out, err, code) = run_capture("sbx", &args)?;
            if code != 0 {
                let detail = if err.trim().is_empty() { out } else { err };
                return Err(format!(
                    "creating fleet sandbox {sandbox}: {}",
                    detail.trim()
                ));
            }
        }
        None => {
            return Err("sbx did not answer; cannot tell whether the fleet sandbox exists".into())
        }
    }
    ensure_substrate(sandbox)?;
    install_launcher(sandbox)
}

/// Install the tools a box needs in order to exist at all.
///
/// Measured in a real sandbox: the `shell` image ships `bwrap` and `git` but **not `tmux`**, and a
/// box without tmux cannot start — `box-session.sh` refuses, because the session *is* the box.
///
/// skein's kit installs jq and tmux for ordinary boxes, but it cannot serve this one: its startup
/// hook returns early in non-clone mode ("already has an in-repo .claude"), and a fleet sandbox is
/// neither a clone nor a mounted repo. So it provisions its own substrate rather than bending a hook
/// written for a different shape. jq comes along because the store probes that run inside boxes need it.
///
/// bwrap is checked but never installed: without it there is no isolation to be had, and quietly
/// continuing would give every box the sandbox's own `/tmp` and `$HOME` — the exact collision this
/// design exists to prevent.
pub fn ensure_substrate(sandbox: &str) -> Result<(), String> {
    let script = "need=''; \
         command -v tmux >/dev/null 2>&1 || need=\"$need tmux\"; \
         command -v jq   >/dev/null 2>&1 || need=\"$need jq\"; \
         command -v bwrap >/dev/null 2>&1 || { echo 'skein: this sandbox image has no bwrap; boxes cannot be isolated in it' >&2; exit 1; }; \
         [ -n \"$need\" ] || exit 0; \
         timeout 180 sudo apt-get install -y -qq $need >/dev/null 2>&1 \
           || { timeout 120 sudo apt-get update -qq >/dev/null 2>&1 && timeout 180 sudo apt-get install -y -qq $need >/dev/null 2>&1; }; \
         missing=''; for t in $need; do command -v \"$t\" >/dev/null 2>&1 || missing=\"$missing $t\"; done; \
         [ -z \"$missing\" ] || { echo \"skein: the fleet sandbox is missing required tools:$missing\" >&2; exit 1; }";
    own_sandbox(sandbox)
        .exec(script, Duration::from_secs(400))
        .map(|_| ())
}

/// Write `box-session.sh` into the sandbox, over stdin rather than as an argument — the script is
/// large and `sbx exec`'s argv is visible in every process listing on the host.
pub fn install_launcher(sandbox: &str) -> Result<(), String> {
    let dir = BOX_SESSION_PATH
        .rsplit_once('/')
        .map(|(d, _)| d)
        .unwrap_or(FLEET_ROOT);
    let script = format!(
        "mkdir -p {} && cat > {} && chmod 755 {}",
        sh_quote(dir),
        sh_quote(BOX_SESSION_PATH),
        sh_quote(BOX_SESSION_PATH)
    );
    // The fleet sandbox itself, not a box inside it — no namespace to enter.
    own_sandbox(sandbox).write(&script, BOX_SESSION_SH.as_bytes(), Duration::from_secs(30))
}

/// The shell that prepares a box's checkout inside the fleet sandbox.
///
/// Clones from the **remote** at `base`, then creates or checks out the box's branch. Boxes used to
/// be cloned from the host's own clone, which meant a new box inherited whatever was stale or
/// half-committed there; from the remote it starts from the same base the diff is taken against.
///
/// Refuses rather than reuses when the tree is already populated: a box root left behind by a
/// previous box of the same name would otherwise silently give the new one someone else's work.
pub fn clone_script(name: &str, url: &str, base: &str, branch: &str) -> String {
    let root = box_root(name);
    let tree = format!("{root}/tree");
    format!(
        "set -e; \
         if [ -e {tree_q}/.git ]; then echo 'skein: {name} already has a checkout; destroy the box first' >&2; exit 1; fi; \
         mkdir -p {root_q}; \
         git clone --branch {base_q} {url_q} {tree_q}; \
         cd {tree_q}; \
         git checkout -B {branch_q}",
        tree_q = sh_quote(&tree),
        root_q = sh_quote(&root),
        base_q = sh_quote(base),
        url_q = sh_quote(url),
        branch_q = sh_quote(branch),
    )
}

/// The shell that starts a box: its namespace, its tmux server, and the agent inside it.
pub fn session_script(name: &str, session: &str, agent_command: &str) -> String {
    format!(
        "{launcher} {name_q} {root_q} {pid_q} {session_q} bash -lc {cmd_q}",
        launcher = sh_quote(BOX_SESSION_PATH),
        name_q = sh_quote(name),
        root_q = sh_quote(&box_root(name)),
        pid_q = sh_quote(&box_pidfile(name)),
        session_q = sh_quote(session),
        cmd_q = sh_quote(agent_command),
    )
}

/// Read back the anchor pid `box-session.sh` recorded, so the host can write the box's placement.
///
/// The pid is knowable only inside the sandbox, and only after the session starts — which is why
/// placement is recorded after launch rather than predicted before it.
pub fn read_anchor(sandbox: &str, name: &str) -> Result<u32, String> {
    let script = format!("cat {}", sh_quote(&box_pidfile(name)));
    let out = own_sandbox(sandbox).exec(&script, Duration::from_secs(10))?;
    out.trim()
        .parse::<u32>()
        .map_err(|_| format!("box {name} did not report an anchor pid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    // The layout is load-bearing rather than cosmetic: box-session.sh binds the box's own /tmp and
    // $HOME over the sandbox's, so anything skein must read from OUTSIDE the box — the tmux socket
    // and the anchor pid — has to live somewhere neither bind covers.
    #[test]
    fn a_boxs_paths_avoid_everything_its_namespace_binds_over() {
        for path in [
            box_root("web-main"),
            box_sock("web-main"),
            box_pidfile("web-main"),
            BOX_SESSION_PATH.to_string(),
        ] {
            assert!(path.starts_with("/boxes/"), "{path} escaped the layout");
            assert!(
                !path.starts_with("/tmp/"),
                "{path} is under the private /tmp"
            );
            assert!(!path.contains("/home/"), "{path} is under a private HOME");
        }
        assert_eq!(box_sock("web-main"), "/boxes/web-main/session.sock");
        assert_eq!(box_pidfile("web-main"), "/boxes/web-main/anchor.pid");
    }

    // The sandbox is agentless and mounts the store parent, not any one repo — the two properties
    // that let it host boxes from every repo without being recreated when one is added.
    #[test]
    fn the_fleet_sandbox_is_agentless_and_mounts_the_store_parent() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SKEIN_HOME", tempdir());
        let argv = create_argv("skein-fleet", "/h/.skein/repos");
        assert_eq!(
            argv,
            [
                "create",
                "--name",
                "skein-fleet",
                "shell",
                "/h/.skein/repos"
            ],
            "no -m/--cpus unless configured: one shared reservation is already the win, and a \
             number picked here would be guessing at a machine skein cannot see"
        );
        assert!(
            !argv.contains(&"--clone".to_string()),
            "the sandbox is not a checkout; boxes clone from the remote onto VM-local disk"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // A box that already has a checkout is refused, not reused. Silently adopting the tree left by a
    // previous box of the same name would hand the new one someone else's uncommitted work.
    #[test]
    fn preparing_a_checkout_starts_from_the_remote_base_and_never_reuses_a_tree() {
        let script = clone_script("web-main", "git@github.com:o/r.git", "main", "feat/auth");
        // The quoting closes before `/.git`, which the shell concatenates back into one word.
        assert!(script.contains("if [ -e '/boxes/web-main/tree'/.git ]"));
        assert!(script.contains("exit 1"));
        assert!(
            script.contains("git clone --branch 'main' 'git@github.com:o/r.git'"),
            "from the remote at the base branch — the same base the diff is taken against"
        );
        assert!(
            script.contains("git checkout -B 'feat/auth'"),
            "a branch with a slash is one argument, not a path"
        );
    }

    // Every argument is quoted: a branch like `feat/auth` or a name with a space must reach the
    // launcher whole, and the launcher does its own refusing from there.
    #[test]
    fn starting_a_box_hands_the_launcher_quoted_arguments() {
        let script = session_script("web-main", "skein-agent", "claude --continue");
        assert!(script.starts_with("'/boxes/.skein/box-session.sh' 'web-main'"));
        assert!(script.contains("'/boxes/web-main' '/boxes/web-main/anchor.pid' 'skein-agent'"));
        assert!(
            script.ends_with("bash -lc 'claude --continue'"),
            "the agent command stays one argument: {script}"
        );
    }
}
