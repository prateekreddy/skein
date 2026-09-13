//! Fleet observation: what sandboxes this machine has (§8.3).
//!
//! **Never compilable-out**, for the same reason as the audit sink: it only reports. §8.3 makes the
//! pair the deliberate exception to §12.10 — a capability that performs may be left unbuilt; one
//! that reports may not.
//!
//! **Why the warden has to be the one to answer.** `sbx ls` is host-only. In-fleet skein cannot run
//! the check that gates its own first run, so `fleet_exists` becomes a `Source: http` call to here.
//! It reads, it decides nothing, and §8.5's one-outstanding-request rule does not apply to it —
//! rate-limiting the check that lets skein start would turn a safety measure into the thing that
//! stops it starting.
//!
//! **On demand, not on a tick.** Decided rather than assumed: `sbx ls` is the right instrument for
//! "what fleets exist on this machine", which is a question a person asks when they run more than
//! one — not "which boxes exist", which is what skein's board still uses it for and which the
//! placement records already answer without a subprocess. So there is no cache and no gate here. A
//! person is waiting for this answer, and handing them a remembered one would be answering a
//! different question than the one they asked.

use serde::Serialize;
use std::process::Command;
use std::time::Duration;

/// What the machine has, as the warden can see it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Sighting {
    /// Sandbox names, as `sbx ls` gives them. Not interpreted — which of these is a skein fleet is
    /// skein's question, and answering it here would put a second opinion in the system.
    pub sandboxes: Vec<String>,
}

/// Ask `sbx` what exists.
///
/// The command is overridable for tests through `$SKEIN_WARDEN_LS_CMD`, in the same shape skein's
/// own `$SKEIN_LS_CMD` uses. A seam rather than a mock: the thing under test is that the warden
/// parses what `sbx` actually prints, and a hand-written stub would test the stub.
pub fn look() -> Result<Sighting, String> {
    let asked = std::env::var("SKEIN_WARDEN_LS_CMD")
        .ok()
        .filter(|s| !s.is_empty());
    let mut command = match &asked {
        Some(script) => {
            let mut sh = Command::new("sh");
            sh.arg("-c").arg(script);
            sh
        }
        None => {
            let mut sbx = Command::new("sbx");
            sbx.args(["ls", "--json"]);
            sbx
        }
    };
    let out = bounded(&mut command, Duration::from_secs(10))?;
    parse(&out)
}

/// Names out of `sbx ls --json`, whichever of its three shapes it used.
///
/// An **empty document is a fleet with nothing in it**, and an unparseable one is an error. Those
/// two must not collapse: "no fleet exists, so create one" and "I could not tell" lead to different
/// actions, and the second one dressed as the first creates a second fleet.
pub fn parse(raw: &str) -> Result<Sighting, String> {
    let value: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("`sbx ls --json` did not print JSON ({e}): {}", clip(raw)))?;
    let rows: Vec<serde_json::Value> = match value {
        serde_json::Value::Array(rows) => rows,
        serde_json::Value::Object(map) => {
            // `{"sandboxes": [...]}` and `{"<name>": {...}}` are both shapes it has used.
            match map.values().find(|v| v.is_array()) {
                Some(serde_json::Value::Array(rows)) => rows.clone(),
                _ => {
                    let mut named: Vec<String> = map.keys().cloned().collect();
                    named.sort();
                    return Ok(Sighting { sandboxes: named });
                }
            }
        }
        _ => return Err(format!("`sbx ls --json` printed {}", clip(raw))),
    };
    let mut sandboxes: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            row.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.to_string())
        })
        .collect();
    sandboxes.sort();
    Ok(Sighting { sandboxes })
}

fn clip(raw: &str) -> String {
    let trimmed = raw.trim();
    match trimmed.char_indices().nth(200) {
        Some((at, _)) => format!("{}…", &trimmed[..at]),
        None => trimmed.to_string(),
    }
}

/// Run a command with a deadline, so a wedged `sbx` daemon is an error rather than a hang.
///
/// A thread and a channel rather than a runtime: this crate has no async in it, and adding one for
/// a call made when a person presses a button would be the largest dependency in the warden.
fn bounded(command: &mut Command, timeout: Duration) -> Result<String, String> {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own process group, which is what makes the deadline below able to end the WORK and
        // not just the process this one happens to hold a handle on. Killing `child` alone killed
        // a shell and left what it started with `ppid` 1 (SKEIN-912). The group has to be made
        // here, at the spawn, because a group cannot be created after the fact — and it has to be
        // a NEW one: the child would otherwise inherit the warden's own group, and a negative kill
        // against that is the warden killing itself.
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not run it: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err(format!(
                        "it exited {}{}",
                        status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "on a signal".into()),
                        match String::from_utf8_lossy(&out.stderr).trim() {
                            "" => String::new(),
                            said => format!(" — {}", clip(said)),
                        }
                    ));
                }
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            None if std::time::Instant::now() >= deadline => {
                end_group(&mut child);
                return Err(format!("it did not answer within {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// End the whole process group `child` leads, and reap `child`.
///
/// **Only sound for a child spawned with `process_group(0)`.** Its pgid is then its own pid, and an
/// UNREAPED child's pid cannot be recycled — so the group named here cannot have become somebody
/// else's between the `try_wait` that said "still running" and this line. Against a child that
/// inherited the caller's group, the same call would signal the caller.
///
/// `SIGKILL` rather than `SIGTERM`: the deadline has already expired, so what is being killed is by
/// definition not answering, and asking it politely is a second wait with nothing behind it.
///
/// **What still escapes, and deliberately.** A process that moves ITSELF out of the group after
/// exec — `setsid`, or a daemon that double-forks — is not in the group any more and does not get
/// the signal. That is not a hole to be plugged: skein's own box observer is started exactly that
/// way (`src/runtime.rs:164`) *in order to* outlive the command that starts it. Catching those too
/// would need a cgroup or a pid namespace, which is a sandbox, not a timeout.
fn end_group(child: &mut std::process::Child) {
    let group = child.id() as libc::pid_t;
    // SAFETY: `kill` has no memory effects, and the argument is a pid this process owns and has
    // not reaped, so it names this child's group and can name nothing else. A failure means the
    // group is already empty, which is the outcome being asked for.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    // Reaped, or every timeout leaves a zombie behind on a path a sick fleet takes repeatedly.
    // This is NOT a wait for the command: `SIGKILL` cannot be caught, blocked or ignored, so the
    // child is already dead and this returns as fast as the kernel can hand back its status. The
    // deadline is the property this function exists to keep, and it still holds.
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape `sbx ls --json` has printed, and the one that must not be mistaken for an error.
    #[test]
    fn an_empty_fleet_is_not_an_unreadable_one() {
        assert_eq!(parse("[]").unwrap().sandboxes, Vec::<String>::new());
        assert_eq!(parse("{}").unwrap().sandboxes, Vec::<String>::new());
        assert_eq!(
            parse(r#"[{"name":"b"},{"name":"a"}]"#).unwrap().sandboxes,
            vec!["a", "b"]
        );
        assert_eq!(
            parse(r#"{"sandboxes":[{"name":"skein-fleet"}]}"#)
                .unwrap()
                .sandboxes,
            vec!["skein-fleet"]
        );
        assert_eq!(
            parse(r#"{"skein-fleet":{"status":"running"}}"#)
                .unwrap()
                .sandboxes,
            vec!["skein-fleet"]
        );

        // And the distinction that matters: "no fleet" and "I could not tell" lead to different
        // actions, and the second dressed as the first creates a second fleet.
        let why = parse("sbx: command not found").unwrap_err();
        assert!(why.contains("did not print JSON"), "{why}");
        assert!(parse("").is_err());
    }

    /// A wedged daemon is an error with a deadline on it, not a warden that never answers.
    #[test]
    fn a_daemon_that_does_not_answer_is_an_error_rather_than_a_hang() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 30");
        let began = std::time::Instant::now();
        let why = bounded(&mut command, Duration::from_millis(200)).unwrap_err();
        assert!(why.contains("did not answer"), "{why}");
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "it waited for the command rather than for its deadline"
        );
    }

    /// The deadline ends what the command **started**, and not only the command (SKEIN-912).
    ///
    /// The defect this was written against: `bounded` killed `child`, and `child` is a shell. A
    /// `sh -c` that backgrounds anything — and, with `/bin/sh` a symlink to dash, a `sh -c` that
    /// backgrounds *nothing*, because dash forks a single `-c` command rather than exec'ing it —
    /// leaves that work running with `ppid` 1 and nothing left that will ever reap it. In
    /// production that work is `sbx`, on the host, at the moment somebody is about to retry it.
    ///
    /// The script backgrounds explicitly rather than relying on dash forking, so this reproduces
    /// the defect under any `/bin/sh` rather than only under the one this box happens to have.
    #[test]
    fn a_deadline_ends_what_the_command_started_and_not_only_the_command() {
        let dir = std::env::temp_dir().join(format!("skein-warden-bounded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("grandchild.pid");
        let _ = std::fs::remove_file(&pidfile);
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());

        let began = std::time::Instant::now();
        let running = std::thread::spawn(move || {
            let mut command = Command::new("sh");
            command.arg("-c").arg(&script);
            bounded(&mut command, Duration::from_secs(3))
        });

        // PRESENT first: an absence that was never a presence proves nothing (SKEIN-833). Both
        // halves of the observation are themselves deadlined against the `bounded` call above, so
        // a box slow enough to see the grandchild only after the kill fails loudly here instead of
        // quietly passing the "it is gone" half against a process that was never there.
        let grandchild = loop {
            if let Some(pid) = pid_in(&pidfile) {
                break pid;
            }
            assert!(
                began.elapsed() < Duration::from_secs(2),
                "the command never wrote the pid of what it started, so nothing below was observed"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            a_live_sleep(grandchild),
            "pid {grandchild} was not a running `sleep` before the deadline, so this run proves \
             nothing about what the deadline ends"
        );
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "the grandchild was seen only after the deadline could already have fired"
        );

        let why = running
            .join()
            .expect("the bounded call panicked")
            .expect_err("a 30s sleep cannot finish inside 3s");
        assert!(why.contains("did not answer"), "{why}");

        // GONE. Polled rather than read once, because a kill is a signal and the reaping is init's
        // job — but a `sleep 30` cannot hide inside two seconds of polling.
        let since = std::time::Instant::now();
        while a_live_sleep(grandchild) {
            assert!(
                since.elapsed() < Duration::from_secs(2),
                "the shell was killed and the `sleep 30` it started (pid {grandchild}) outlived it"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&pidfile);
    }

    /// The pid a script wrote, once it has written all of it.
    ///
    /// `parse` rather than a bare read, because the file exists from the instant the redirection is
    /// set up and is empty until the shell writes into it — reading it once is a race that shows up
    /// as an occasional unparseable empty string.
    fn pid_in(path: &std::path::Path) -> Option<i32> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// Is that pid a live `sleep`, right now?
    ///
    /// `/proc` rather than `kill(pid, 0)`: a killed process is a zombie until something reaps it,
    /// and `kill(pid, 0)` answers yes to a zombie — which would report a process the deadline had
    /// already ended as a survivor, for as long as init took to get to it. The command name is
    /// checked from the same read, so a pid the kernel has since handed to something else reads as
    /// gone rather than as the `sleep` that is no longer there.
    fn a_live_sleep(pid: i32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // The comm field is parenthesised and may itself contain spaces or a `)`, so it is taken
        // from the FIRST `(` to the LAST `)` — the one place a naive split on whitespace is wrong.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            return false;
        };
        let state = stat[close + 1..].split_whitespace().next().unwrap_or("Z");
        &stat[open + 1..close] == "sleep" && state != "Z"
    }

    /// The command is a seam, so what is tested is the parse of what `sbx` really prints.
    #[test]
    fn the_listing_comes_from_the_command_it_is_told_to_run() {
        let _env = crate::env_lock();
        std::env::set_var(
            "SKEIN_WARDEN_LS_CMD",
            r#"printf '[{"name":"skein-fleet"}]'"#,
        );
        let seen = look().unwrap();
        std::env::remove_var("SKEIN_WARDEN_LS_CMD");
        assert_eq!(seen.sandboxes, vec!["skein-fleet"]);
    }
}
