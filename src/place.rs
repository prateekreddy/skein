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
//! Today `place_of` returns the identity mapping, so the argv is byte-for-byte what it always was.

use crate::util::*;
use crate::valid_name;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Where one box runs.
///
/// `sandbox` is the sbx name to exec into; `name` is the box. They are equal today and the type
/// exists precisely so that they need not stay equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub sandbox: String,
}

/// Resolve a box to where it runs.
///
/// `None` for a name that isn't one — every path into a box is gated here, so no caller has to
/// remember to validate before building an argv.
pub fn place_of(name: &str) -> Option<Place> {
    valid_name(name).then(|| Place {
        name: name.to_string(),
        // The identity mapping: one sandbox per box, named after it. This single line is what a
        // shared-sandbox model replaces.
        sandbox: name.to_string(),
    })
}

impl Place {
    /// The argv that runs `script` in this place.
    ///
    /// Its own function so the wire format is testable without a sandbox — and because it is the
    /// contract the takeover guard asserts.
    pub fn exec_argv(&self, script: &str) -> Vec<String> {
        vec![
            "sbx".into(),
            "exec".into(),
            self.sandbox.clone(),
            "bash".into(),
            "-lc".into(),
            script.into(),
        ]
    }

    /// The argv for running a command here *without* a shell — `["cat", path]` and friends.
    ///
    /// For callers that stream stdout somewhere other than a buffer, so they keep their own
    /// plumbing while the sandbox name still resolves through here rather than being assumed.
    pub fn raw_argv(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec!["sbx".to_string(), "exec".into(), self.sandbox.clone()];
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
        vec![
            "sbx".into(),
            "exec".into(),
            "-i".into(),
            self.sandbox.clone(),
            "bash".into(),
            "-lc".into(),
            script.into(),
        ]
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

    // The argv IS the contract. Every feature that touches a box produces this shape, and the
    // shared-sandbox model will change it in exactly one place — so pinning it here is what makes
    // that change reviewable rather than a diff across a dozen files.
    #[test]
    fn a_place_is_the_one_spelling_of_reaching_into_a_box() {
        let p = place_of("web-main").unwrap();
        assert_eq!(p.sandbox, "web-main", "one sandbox per box, for now");
        assert_eq!(
            p.exec_argv("echo hi"),
            ["sbx", "exec", "web-main", "bash", "-lc", "echo hi"]
        );
        assert_eq!(
            p.raw_argv(&["cat", "/tmp/x"]),
            ["sbx", "exec", "web-main", "cat", "/tmp/x"],
            "no shell for a streamed copy — the path is an argv element, not a word to split"
        );
        // `-i` is load-bearing: without it sbx wires no pipe and the body vanishes silently.
        assert_eq!(
            p.write_argv("cat > f"),
            ["sbx", "exec", "-i", "web-main", "bash", "-lc", "cat > f"]
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
}
