//! Where a box's work actually happens, and the one way to reach it.
//!
//! A box is an identity: a name, a branch, a repo, a conversation. *Where it runs* is a separate
//! thing — today one sbx sandbox per box, with the sandbox named after the box. Those two were
//! fused, so "the box" and "the sandbox" were the same string in six different helpers, and every
//! feature that touched a box hardcoded that assumption.
//!
//! [`Place`] separates them. `place_of(box)` is a lookup, not an identity, and every call into a
//! box goes through [`Place::exec`] / [`Place::write`] / [`Place::bytes`]. That is the whole point:
//! changing what backs a box — several boxes sharing one sandbox, each with its own HOME, tree and
//! cgroup — becomes a change to `place_of` rather than a sweep through every feature.
//!
//! There are two shapes, and a box says which one it is rather than skein guessing:
//!
//! - [`Where::OwnSandbox`] — one sbx sandbox per box, named after it. skein's original model.
//!   Each box is a microVM, so its `/tmp`, its `$HOME` and its memory are private for free — and
//!   its memory is *reserved*, which is the reason for the second shape.
//! - [`Where::Shared`] — many boxes inside one sandbox, each in its own bwrap namespace. Memory
//!   becomes a pool the boxes share instead of N reservations that sum, and `/tmp` and `$HOME`
//!   have to be made private deliberately, because a shared VM does not hand them over.

use crate::config::*;
use crate::util::*;
use crate::{skein_home, valid_name};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How a box's sandbox is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Where {
    /// The sandbox is this box's alone. Nothing to enter; the sandbox IS the box.
    OwnSandbox,
    /// The sandbox hosts several boxes. This one lives in a bwrap namespace whose init process is
    /// `ns_pid`, with its own `/tmp` and `$HOME` bound in there.
    ///
    /// Reaching in means joining that namespace. Both the user and mount namespaces have to be
    /// joined together — joining the mount namespace alone is refused — and credentials must be
    /// preserved, or `setgroups` fails for an unprivileged caller. Verified inside a real box;
    /// getting either detail wrong looks like a permissions bug rather than a missing flag.
    Shared {
        ns_pid: u32,
        /// The box's private HOME. Explicit rather than inherited: `nsenter` carries the caller's
        /// environment in, so without this a box would read skein's `$HOME`, not its own — and
        /// `~/.claude.json` and `~/.config/sync/env` are exactly the files that must not be shared.
        home: String,
        /// The box's checkout. Every script skein sends assumes it starts at the repo root.
        tree: String,
    },
}

/// Where one box runs.
///
/// `sandbox` is the sbx name to exec into; `name` is the box. Under [`Where::OwnSandbox`] they are
/// equal, and this type exists precisely so that they need not stay equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub sandbox: String,
    pub at: Where,
}

/// What skein records about a box living in a shared sandbox, written when its session starts.
///
/// A file rather than a lookup, because the namespace's init pid is knowable only to whoever
/// launched it, and skein must be able to reach a box after a restart of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceRecord {
    pub sandbox: String,
    pub ns_pid: u32,
    pub home: String,
    pub tree: String,
}

fn place_record_path(name: &str) -> PathBuf {
    skein_home().join("places").join(format!("{name}.json"))
}

/// Record where a box was started, so later calls can reach it.
pub fn record_place(name: &str, record: &PlaceRecord) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let dir = skein_home().join("places");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    write_atomic(&place_record_path(name), &dir, &bytes)
}

/// Forget a box's placement — its namespace died with it.
pub fn forget_place(name: &str) {
    if valid_name(name) {
        let _ = fs::remove_file(place_record_path(name));
    }
}

fn read_place_record(name: &str) -> Option<PlaceRecord> {
    serde_json::from_str(&fs::read_to_string(place_record_path(name)).ok()?).ok()
}

/// Resolve a box to where it runs.
///
/// `None` for a name that isn't one — every path into a box is gated here, so no caller has to
/// remember to validate before building an argv.
///
/// A box that skein started in a shared sandbox says so in its own record; everything else is the
/// original one-sandbox-per-box mapping. That ordering is deliberate: turning the fleet sandbox on
/// must not retroactively claim boxes that are still running as their own VM, or skein would exec
/// into a namespace that was never created.
pub fn place_of(name: &str) -> Option<Place> {
    if !valid_name(name) {
        return None;
    }
    if let Some(rec) = read_place_record(name) {
        // The pid is only meaningful while that process lives; a dead namespace means the box is
        // gone, not that it should be reached some other way.
        if PathBuf::from(format!("/proc/{}", rec.ns_pid)).exists() {
            return Some(Place {
                name: name.to_string(),
                sandbox: rec.sandbox,
                at: Where::Shared {
                    ns_pid: rec.ns_pid,
                    home: rec.home,
                    tree: rec.tree,
                },
            });
        }
    }
    Some(Place {
        name: name.to_string(),
        sandbox: name.to_string(),
        at: Where::OwnSandbox,
    })
}

/// The name of the one sandbox that hosts every box, when the shared model is on. Empty ⇒ each box
/// gets its own sandbox, which is skein's original behaviour and stays the default.
pub fn fleet_sandbox() -> String {
    load_config().fleet_sandbox.trim().to_string()
}

impl Place {
    /// The argv that runs `script` in this place.
    ///
    /// Its own function so the wire format is testable without a sandbox — and because it is the
    /// contract the takeover guard asserts.
    pub fn exec_argv(&self, script: &str) -> Vec<String> {
        let mut argv = vec!["sbx".to_string(), "exec".into(), self.sandbox.clone()];
        argv.extend(self.enter());
        argv.push("bash".into());
        argv.push("-lc".into());
        argv.push(self.wrap(script));
        argv
    }

    /// The `nsenter` hop that puts a command inside this box's namespace — empty when the sandbox
    /// is the box, which is what keeps the original model byte-for-byte unchanged.
    fn enter(&self) -> Vec<String> {
        match &self.at {
            Where::OwnSandbox => vec![],
            Where::Shared { ns_pid, .. } => vec![
                "nsenter".into(),
                format!("--user=/proc/{ns_pid}/ns/user"),
                format!("--mount=/proc/{ns_pid}/ns/mnt"),
                "--preserve-credentials".into(),
                "--".into(),
            ],
        }
    }

    /// Put the script where it expects to be: at the repo root, with the box's own HOME.
    ///
    /// `nsenter` carries the *caller's* environment and working directory into the namespace, so
    /// neither is inherited from the box. A script that assumed it started at the tree root would
    /// otherwise run somewhere arbitrary, and one reading `~/.config/sync/env` would read skein's.
    fn wrap(&self, script: &str) -> String {
        match &self.at {
            Where::OwnSandbox => script.to_string(),
            Where::Shared { home, tree, .. } => format!(
                "export HOME={} && cd {} && {script}",
                sh_quote(home),
                sh_quote(tree)
            ),
        }
    }

    /// The argv for running a command here *without* a shell — `["cat", path]` and friends.
    ///
    /// For callers that stream stdout somewhere other than a buffer, so they keep their own
    /// plumbing while the sandbox name still resolves through here rather than being assumed.
    pub fn raw_argv(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec!["sbx".to_string(), "exec".into(), self.sandbox.clone()];
        argv.extend(self.enter());
        argv.extend(args.iter().map(|a| a.to_string()));
        argv
    }

    fn command(&self, script: &str) -> Command {
        let argv = self.exec_argv(script);
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        command
    }

    /// Run `script` and return its stdout as text. A non-zero exit is an error carrying the box's
    /// own stderr, because the box's words are always more use than "exited 1".
    pub fn exec(&self, script: &str, timeout: Duration) -> Result<String, String> {
        let out = self.bytes(script, timeout)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Run `script` and return its stdout as **raw bytes**.
    ///
    /// Separate from [`Place::exec`] because a lossy UTF-8 hop corrupts every image and PDF the
    /// Files tab serves — the bug is silent and the file merely looks broken.
    pub fn bytes(&self, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        let mut command = self.command(script);
        let out = bounded_output(&mut command, "sbx exec", timeout)?;
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if detail.is_empty() {
                format!("sbx exec exited {}", out.status)
            } else {
                detail
            });
        }
        Ok(out.stdout)
    }

    /// The argv that runs `script` here with stdin attached. `-i` is not decoration: without it
    /// `sbx exec` does not wire a pipe to the guest, and the body is silently discarded.
    pub fn write_argv(&self, script: &str) -> Vec<String> {
        let mut argv = vec![
            "sbx".to_string(),
            "exec".into(),
            "-i".into(),
            self.sandbox.clone(),
        ];
        argv.extend(self.enter());
        argv.push("bash".into());
        argv.push("-lc".into());
        argv.push(self.wrap(script));
        argv
    }

    /// Run `script` with `body` on its **stdin**.
    ///
    /// The only way skein sends a box anything sensitive: `sbx exec`'s argv is visible in `ps` on
    /// the host, so a token passed as an argument is a token in every process listing and every
    /// shell history. Streamed rather than buffered, so a large attachment costs the host nothing.
    pub fn write(&self, script: &str, body: &[u8], timeout: Duration) -> Result<(), String> {
        let argv = self.write_argv(script);
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("sbx exec: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("sbx exec: no stdin")?
            .write_all(body)
            .map_err(|e| format!("sbx exec: writing stdin: {e}"))?;
        // Deadlined rather than a bare wait: a box that never exits would otherwise hang the
        // caller — and one of this function's callers is holding a freshly minted credential.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => return Err(format!("sbx exec exited {status}")),
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    return Err("sbx exec timed out".into());
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    // The argv IS the contract. Every feature that touches a box produces this shape, so pinning
    // both spellings here is what makes the shared-sandbox switch reviewable in one place rather
    // than as a diff across a dozen files.
    #[test]
    fn its_own_sandbox_is_reached_exactly_as_it_always_was() {
        let p = Place {
            name: "web-main".into(),
            sandbox: "web-main".into(),
            at: Where::OwnSandbox,
        };
        assert_eq!(
            p.exec_argv("echo hi"),
            ["sbx", "exec", "web-main", "bash", "-lc", "echo hi"],
            "byte-for-byte the original argv — no nsenter hop, no wrapper"
        );
        // `-i` is load-bearing: without it sbx wires no pipe and the body vanishes silently.
        assert_eq!(
            p.write_argv("cat > f"),
            ["sbx", "exec", "-i", "web-main", "bash", "-lc", "cat > f"]
        );
        assert_eq!(
            p.raw_argv(&["cat", "/tmp/x"]),
            ["sbx", "exec", "web-main", "cat", "/tmp/x"],
            "no shell for a streamed copy — the path is an argv element, not a word to split"
        );
    }

    // The three details that are easy to get wrong and all look like permissions bugs: the user
    // and mount namespaces must be joined TOGETHER (mount alone is refused), credentials must be
    // preserved (or setgroups fails unprivileged), and HOME/cwd must be set explicitly because
    // nsenter carries the caller's, not the box's.
    #[test]
    fn a_shared_sandbox_is_entered_by_namespace_with_the_boxs_own_home() {
        let p = Place {
            name: "web-main".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: 4242,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
            },
        };
        assert_eq!(
            p.exec_argv("git status"),
            [
                "sbx",
                "exec",
                "skein-fleet",
                "nsenter",
                "--user=/proc/4242/ns/user",
                "--mount=/proc/4242/ns/mnt",
                "--preserve-credentials",
                "--",
                "bash",
                "-lc",
                "export HOME='/boxes/web-main/home' && cd '/boxes/web-main/tree' && git status",
            ]
        );
        // The stdin path keeps `-i` in front of the sandbox and the hop after it.
        let w = p.write_argv("cat > f");
        assert_eq!(&w[..4], ["sbx", "exec", "-i", "skein-fleet"]);
        assert!(w.contains(&"--preserve-credentials".to_string()));
        // And a streamed copy enters the namespace too, or it would `cat` the wrong /tmp entirely.
        assert_eq!(
            p.raw_argv(&["cat", "/tmp/artifact"]),
            [
                "sbx",
                "exec",
                "skein-fleet",
                "nsenter",
                "--user=/proc/4242/ns/user",
                "--mount=/proc/4242/ns/mnt",
                "--preserve-credentials",
                "--",
                "cat",
                "/tmp/artifact",
            ]
        );
    }

    // Gating resolution means no caller can build an argv from a name that was never checked.
    // What `valid_name` guarantees is that a name is not a *path* — it may contain spaces and
    // shell metacharacters, which are inert here because the name is an argv element, never
    // interpolated into a shell string. The paths that DO build shell strings quote it.
    #[test]
    fn a_name_that_could_be_a_path_has_no_place() {
        for bad in ["", "../etc", "a/b", "a\\b", "x\0y", &"n".repeat(129)] {
            assert!(place_of(bad).is_none(), "resolved {bad:?}");
        }
        let spaced = place_of("a b").expect("a space is not a traversal");
        assert_eq!(
            spaced.exec_argv("true")[2],
            "a b",
            "it stays one argv element, so nothing can split it"
        );
    }

    // A recorded placement is only good while its namespace is alive. A stale record pointing at a
    // recycled pid would send a box's commands into whatever process now holds that number.
    #[test]
    fn a_placement_outlives_nothing_and_a_dead_namespace_is_not_followed() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        std::env::set_var("SKEIN_HOME", &dir);

        assert_eq!(
            place_of("web-main").map(|p| p.at),
            Some(Where::OwnSandbox),
            "no record ⇒ the original model"
        );

        // A live pid: this test process itself, which is certainly running.
        record_place(
            "web-main",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: std::process::id(),
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
            },
        )
        .unwrap();
        let p = place_of("web-main").unwrap();
        assert_eq!(p.sandbox, "skein-fleet");
        assert!(matches!(p.at, Where::Shared { .. }));

        // A pid that cannot be running: pid 0 is never a process.
        record_place(
            "web-main",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 0,
                home: "/h".into(),
                tree: "/t".into(),
            },
        )
        .unwrap();
        assert_eq!(
            place_of("web-main").map(|p| p.at),
            Some(Where::OwnSandbox),
            "a dead namespace must not be entered — fall back rather than exec into a stranger"
        );

        forget_place("web-main");
        assert_eq!(place_of("web-main").map(|p| p.at), Some(Where::OwnSandbox));
        std::env::remove_var("SKEIN_HOME");
    }
}
