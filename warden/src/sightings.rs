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
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("it did not answer within {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
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
