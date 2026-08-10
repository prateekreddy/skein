//! What the turn-state probe decides, run as the shell script a box actually runs.
//!
//! `box-status.sh` is where every box's state on the board comes from, and until now it had no test
//! at all — the Rust side (`signals.rs`) is thoroughly covered, but it can only reason about what
//! the probe wrote. That gap shipped a real bug: `PostCompact` wrote `working` unconditionally,
//! which is right for an *auto* compaction (it fires mid-turn, the turn resumes) and wrong for a
//! manual `/compact`, which you type at the prompt while the box waits on you. A box you had just
//! compacted read "working" until an idle Notification a minute later happened to correct it.
//!
//! The script needs nothing but a directory and `bash`, so this drives the real file — not a copy —
//! through a throwaway store and reads back the JSON the board would have read.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BOX: &str = "web-main";

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A throwaway store with the probe pointed at it.
///
/// Deliberately **not** under `/tmp` or `$HOME` — a box binds its own directories over both — and
/// not under a git checkout either, since the script resolves its store from `git rev-parse
/// --show-toplevel` and would otherwise climb out into the real one.
struct Probe {
    root: PathBuf,
}

impl Probe {
    fn new(what: &str) -> Probe {
        let root = PathBuf::from("/var/tmp")
            .join(format!("skein-turnstate-it-{what}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claude")).unwrap();
        Probe { root }
    }

    fn script() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/probe/box-status.sh")
    }

    fn fire(&self, mode: &str) {
        self.fire_with(mode, "");
    }

    /// Fire one hook, with its JSON payload on stdin exactly as the runtime delivers it.
    fn fire_with(&self, mode: &str, payload: &str) {
        let mut child = Command::new("bash")
            .arg(Self::script())
            .arg(mode)
            .env("CLAUDE_PROJECT_DIR", &self.root)
            .env("SKEIN_BOX", BOX)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bash");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "`{mode}` exited {:?}", out.status);
        // A UserPromptSubmit hook's stdout is injected into the prompt, so every mode must be
        // silent — including the ones that only read.
        assert!(
            out.stdout.is_empty(),
            "`{mode}` printed to stdout: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    /// (status, detail) as the board reads them. Detail is empty when the state carries none.
    fn state(&self) -> (String, String) {
        let p = self
            .root
            .join(".claude")
            .join("status")
            .join(format!("{BOX}.json"));
        let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let v: serde_json::Value = serde_json::from_str(&text).expect(&text);
        let get = |k: &str| {
            v.get(k)
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string()
        };
        (get("status"), get("detail"))
    }

    fn status(&self) -> String {
        self.state().0
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_manual_compaction_leaves_the_box_where_it_was_waiting() {
    let p = Probe::new("manual");
    // You typed /compact at the prompt: the turn had already ended.
    p.fire("waiting");
    p.fire("compacting");
    assert_eq!(p.status(), "compacting", "busy for the duration, not stuck");
    p.fire("compacted");
    assert_eq!(
        p.status(),
        "waiting",
        "a compaction interrupts a box; it does not give it work"
    );
}

#[test]
fn an_automatic_compaction_still_returns_the_box_to_work() {
    let p = Probe::new("auto");
    // The other trigger: the context filled mid-turn and the turn resumes afterwards.
    p.fire("working");
    p.fire("compacting");
    p.fire("compacted");
    assert_eq!(p.status(), "working");
}

#[test]
fn the_session_start_that_a_compaction_fires_does_not_claim_the_box_wants_you() {
    let p = Probe::new("startedmid");
    p.fire("working");
    p.fire("compacting");
    // SessionStart fires with source=compact, between the two compaction hooks.
    p.fire("started");
    assert_eq!(p.status(), "compacting");
    p.fire("compacted");
    assert_eq!(p.status(), "working");
}

#[test]
fn a_state_that_carries_a_reason_keeps_it_across_a_compaction() {
    if !have("jq") {
        eprintln!("skipping: no jq, so the probe cannot read a detail back");
        return;
    }
    let p = Probe::new("detail");
    p.fire_with("error", r#"{"error_type":"rate_limit"}"#);
    assert_eq!(p.state(), ("error".into(), "API error: rate limit".into()));
    p.fire("compacting");
    p.fire("compacted");
    // Restoring the state but dropping the reason would be the same bug in a quieter form: the row
    // would say "error" with nothing saying why.
    assert_eq!(p.state(), ("error".into(), "API error: rate limit".into()));
}

#[test]
fn a_compaction_with_no_note_behind_it_falls_back_to_working() {
    let p = Probe::new("nonote");
    // An older skein wired PreCompact to a script that left no note, or the hook never fired.
    // Neither may leave the box with no state at all.
    p.fire("compacted");
    assert_eq!(p.status(), "working");
}

#[test]
fn the_note_is_consumed_so_it_cannot_be_restored_onto_a_later_compaction() {
    let p = Probe::new("consumed");
    p.fire("waiting");
    p.fire("compacting");
    p.fire("compacted");
    assert_eq!(p.status(), "waiting");
    // A compaction that dies before PostCompact would otherwise leave its note lying around for
    // whichever compaction came next to pick up.
    p.fire("working");
    p.fire("compacted");
    assert_eq!(p.status(), "working");
}

#[test]
fn a_box_waiting_on_its_own_sub_agents_is_working_not_asking_you() {
    // The counter this rests on is the probe's oldest piece of real logic and was equally untested.
    let p = Probe::new("subagents");
    p.fire("working");
    p.fire("agent-start");
    p.fire("agent-start");
    p.fire("notify-waiting");
    assert_eq!(p.status(), "working", "it is waiting on THEM, not on you");
    p.fire("agent-stop");
    p.fire("agent-stop");
    p.fire("notify-waiting");
    assert_eq!(p.status(), "waiting");
    // And a new turn resets it, so a lost SubagentStop cannot wedge the box as busy forever.
    p.fire("agent-start");
    p.fire("working");
    p.fire("notify-blocked");
    assert_eq!(p.status(), "blocked");
}

#[test]
fn a_restarted_session_does_not_go_on_reading_ended() {
    let p = Probe::new("restart");
    p.fire_with("ended", r#"{"reason":"exit"}"#);
    assert_eq!(p.state(), ("ended".into(), "session ended: exit".into()));
    p.fire("started");
    assert_eq!(p.state(), ("waiting".into(), String::new()));
}
