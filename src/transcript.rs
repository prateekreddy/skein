//! The conversation as the box's own **record** has it, not as the screen had it.
//!
//! tmux repaints only the visible pane, so a reboot, a server restart or a reattach loses the
//! scrollback — while the runtime's JSONL record on the box's disk survives all three. This reads
//! that, tail-first, so a huge session opens instantly and pages backwards on demand.

use crate::place::shared_record;
use crate::repos::agent_for_box;
use crate::sandbox::sbx_guest_output;
use crate::util::valid_name;
use crate::util::*;
use serde::Serialize;
use std::fs;
use std::time::Duration;

/// One message as the transcript recorded it — human-readable parts only. Tool payloads are
/// summarised, never inlined: a single tool_result can be megabytes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TranscriptMsg {
    pub role: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ts: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// compact `name(detail)` summaries of the tools this turn called
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// a subagent's chatter, not the main thread
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sidechain: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptView {
    pub path: String,
    /// size of the record on disk
    pub size: u64,
    /// how much of the tail we read
    pub scanned: u64,
    /// true when `scanned` reached the beginning — there is nothing older to ask for
    pub complete: bool,
    pub messages: Vec<TranscriptMsg>,
    /// why there is nothing to show, when there isn't
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// Newest-first per message cap and per-message text cap: the cockpit renders a conversation, not a
/// core dump, and the payload crosses a websocket to a browser.
pub(crate) const TRANSCRIPT_MAX_MSGS: usize = 400;

pub(crate) const TRANSCRIPT_MAX_TEXT: usize = 4000;

pub const TRANSCRIPT_MIN_BYTES: u64 = 32 * 1024;

pub const TRANSCRIPT_MAX_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) const TRANSCRIPT_HEADER: &str = "SKEIN_TX ";

pub(crate) const TRANSCRIPT_NONE: &str = "SKEIN_TX_NONE";

/// Summarise a tool call as `name(detail)` — the detail being whichever well-known input field says
/// what it acted on. Never the whole input: a file write's input is the entire file.
pub(crate) fn tool_summary(block: &serde_json::Value) -> String {
    let name = block
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("tool")
        .to_string();
    let input = block.get("input");
    let detail = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
    ]
    .iter()
    .find_map(|k| input?.get(*k)?.as_str())
    .map(|d| clip(d.split('\n').next().unwrap_or(d), 60));
    match detail {
        Some(d) if !d.is_empty() => format!("{name}({d})"),
        _ => name,
    }
}

/// Parse a runtime's JSONL record into readable messages. Pure, so the shapes are unit-tested
/// against lines taken from a real transcript rather than from an assumption about them.
pub(crate) fn parse_transcript_jsonl(body: &str) -> Vec<TranscriptMsg> {
    let mut out: Vec<TranscriptMsg> = Vec::new();
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // a half-written last line, or a shape we don't read
        };
        let role = match v.get("type").and_then(|t| t.as_str()) {
            Some(r @ ("user" | "assistant")) => r,
            // every other line type is bookkeeping (mode, last-prompt, file-history-*, attachment…)
            _ => continue,
        };
        let content = v.get("message").and_then(|m| m.get("content"));
        let mut text = String::new();
        let mut tools = Vec::new();
        match content {
            Some(serde_json::Value::String(s)) => text.push_str(s),
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                        Some("tool_use") => tools.push(tool_summary(block)),
                        // tool_result and thinking are deliberately dropped: the first is the bulk
                        // of the file and unreadable, the second isn't the conversation.
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if text.trim().is_empty() && tools.is_empty() {
            continue;
        }
        out.push(TranscriptMsg {
            role: role.to_string(),
            ts: v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            text: clip(text.trim(), TRANSCRIPT_MAX_TEXT),
            tools,
            sidechain: v
                .get("isSidechain")
                .and_then(|s| s.as_bool())
                .unwrap_or(false),
        });
    }
    if out.len() > TRANSCRIPT_MAX_MSGS {
        out.drain(..out.len() - TRANSCRIPT_MAX_MSGS);
    }
    out
}

/// Read a fleet box's conversation straight off the host.
///
/// The box binds `~/.claude/projects` in from `box_state`, so the newest record under it is the same
/// file the agent is writing — live, with no `sbx exec` in the way, and readable whether the box is
/// running, stopped, or its whole sandbox is gone.
///
/// Seeks to the tail rather than reading the file: these transcripts reach tens of megabytes, and
/// the cockpit only ever wants the end of one.
fn read_host_transcript(name: &str, bytes: u64) -> Result<TranscriptView, String> {
    use std::io::{Read, Seek, SeekFrom};
    let empty = |note: &str| TranscriptView {
        path: String::new(),
        size: 0,
        scanned: 0,
        complete: true,
        messages: vec![],
        note: note.to_string(),
    };
    let root = std::path::PathBuf::from(crate::fleet::box_state(name)).join("claude-projects");
    // Newest by mtime, across every project slug: a box has one tree, but a rebuilt box can carry an
    // older slug beside the current one, and "most recently written" is what the agent is using.
    let newest = walk_jsonl(&root)
        .into_iter()
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path);
    let Some(path) = newest else {
        return Ok(empty(
            "no conversation record found for this box yet — the agent writes one as it runs",
        ));
    };
    let mut file = fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let from = size.saturating_sub(bytes);
    file.seek(SeekFrom::Start(from))
        .map_err(|e| format!("seek {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let body = String::from_utf8_lossy(&buf).into_owned();
    let complete = from == 0;
    // A tail almost always starts mid-line; that fragment is not a record and must not be parsed as
    // one (it would render as a half message with no role).
    let body = if complete {
        body.as_str()
    } else {
        body.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    };
    Ok(TranscriptView {
        path: path.to_string_lossy().into_owned(),
        size,
        scanned: (buf.len() as u64).min(size),
        complete,
        messages: parse_transcript_jsonl(body),
        note: String::new(),
    })
}

/// Every `*.jsonl` under `root`, with its mtime. Shallow recursion by hand rather than a crate:
/// the layout is `<slug>/<session>.jsonl`, two levels, and skein has no walkdir dependency.
fn walk_jsonl(root: &std::path::Path) -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_jsonl(&path));
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                found.push((path, mtime));
            }
        }
    }
    found
}

/// Read the tail of a box's conversation record. `bytes` is how much of the end to read — the
/// cockpit doubles it to page backwards, so a 14MB transcript never crosses the wire whole.
pub fn read_transcript(name: &str, bytes: u64) -> Result<TranscriptView, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let bytes = bytes.clamp(TRANSCRIPT_MIN_BYTES, TRANSCRIPT_MAX_BYTES);
    let empty = |note: &str| TranscriptView {
        path: String::new(),
        size: 0,
        scanned: 0,
        complete: true,
        messages: vec![],
        note: note.to_string(),
    };
    let runtime = agent_for_box(name);
    // Claude keeps `~/.claude/projects/<slug>/<session>.jsonl`. Codex's rollout files are a
    // different shape and are NOT read here rather than guessed at — see docs/turn-state.md for
    // why skein captures a runtime's format from a real box before claiming to understand it.
    if runtime != "claude" {
        return Ok(empty(&format!(
            "the transcript reader is wired for Claude only — {runtime}'s record has a different shape and hasn't been captured yet"
        )));
    }
    // A fleet box keeps its record on the HOST (box-session.sh binds it in), so read it from there.
    // Not an optimisation — a correctness fix: this path used to shell into the box, so a STOPPED
    // box reported "no conversation record" when it had one all along, and the cockpit could not
    // show you what a box had been doing right when you most wanted to know.
    if shared_record(name).is_some() {
        return read_host_transcript(name, bytes);
    }
    let script = format!(
        "p=\"$(find \"$HOME/.claude/projects\" -type f -name '*.jsonl' -printf '%T@ %p\\n' 2>/dev/null | sort -nr | head -n1 | cut -d' ' -f2-)\"; \
         [ -n \"$p\" ] && [ -r \"$p\" ] || {{ echo '{TRANSCRIPT_NONE}'; exit 0; }}; \
         printf '{TRANSCRIPT_HEADER}%s %s\\n' \"$(wc -c <\"$p\")\" \"$p\"; \
         tail -c {bytes} \"$p\""
    );
    let raw = sbx_guest_output(name, &script, Duration::from_secs(60))?;
    let (header, body) = raw.split_once('\n').unwrap_or((raw.trim_end(), ""));
    if header.trim() == TRANSCRIPT_NONE {
        return Ok(empty(
            "no conversation record found in this box yet — the agent writes one as it runs",
        ));
    }
    let Some(rest) = header.strip_prefix(TRANSCRIPT_HEADER) else {
        return Err("could not locate the box's conversation record".into());
    };
    let (size_text, path) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
    let size: u64 = size_text.parse().unwrap_or(0);
    let scanned = (body.len() as u64).min(size);
    let complete = scanned >= size;
    // A tail almost always starts mid-line; that fragment is not a record and must not be parsed
    // as one (it would render as a half message with no role).
    let body = if complete {
        body
    } else {
        body.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    };
    Ok(TranscriptView {
        path: path.to_string(),
        size,
        scanned,
        complete,
        messages: parse_transcript_jsonl(body),
        note: String::new(),
    })
}

// ---------- verification: does a box's work actually build and pass? ----------
// The board says who needs you. It can't say whose work stands up — a row reading "waiting, 238
// files changed, 'both bugs fixed'" tells you nothing about whether it compiles, so every box is
// guilty until you personally re-run it. A verify runs the repo's own check command INSIDE the box
// (`sbx_guest_output` — the captured-output primitive the handoff flow already leans on), records
// the outcome beside the other per-box signals, and the row reports it.
//
// **Nothing triggers this.** No tick, no turn-end hook, no schedule calls `run_verify` — it is a
// click, deliberately, because a check is a real `cargo test` burning cores on the dev's own Mac
// and six boxes verifying at once would be six of them. The guards below (single-flight, liveness,
// mid-turn) are exactly what an automatic trigger would have to satisfy, so turning one on later is
// a call site, not a redesign. The one place it would go: the transition into `waiting` in
// `load_views`, gated on a setting that does not exist yet.

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::testutil::*;
    #[allow(unused_imports)]
    use std::{env, fs};

    // The record used to be read by shelling into the box, so a box that was stopped — or whose
    // sandbox was gone entirely — reported "no conversation record" when it had one all along.
    // That is precisely when you most want to see what it had been doing. A fleet box binds its
    // record in from the host, so it is readable with nothing running at all.
    #[test]
    fn a_stopped_boxs_conversation_is_still_readable() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        // A placement whose anchor pid is long dead: the box is stopped, and place_of would refuse
        // to enter it. shared_record still identifies it as a fleet box, which is the distinction.
        crate::place::record_place(
            "web-main",
            &crate::place::PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 0,
                home: String::new(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
            },
        )
        .unwrap();
        let slug = std::path::PathBuf::from(crate::fleet::box_state("web-main"))
            .join("claude-projects/-boxes-web-main-tree");
        fs::create_dir_all(&slug).unwrap();
        fs::write(
            slug.join("s.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":\"ship it\"}}\n\
             {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n",
        )
        .unwrap();

        let view = read_transcript("web-main", TRANSCRIPT_MIN_BYTES).expect("read");
        assert_eq!(
            view.messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["ship it", "done"],
            "a stopped box's conversation must still render: {}",
            view.note
        );
        assert!(
            view.complete,
            "a short record was reported as a partial tail, so the cockpit would page for more"
        );
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn the_transcript_reader_keeps_the_conversation_and_drops_the_bookkeeping() {
        // Line shapes taken from a real 14MB Claude Code record, whose 3800 lines are: user,
        // assistant, system, mode, permission-mode, last-prompt, file-history-snapshot,
        // file-history-delta, attachment, queue-operation, bridge-session. Only two are the
        // conversation; 913 of the "user" lines are tool_result payloads that would drown it.
        let body = concat!(
            r#"{"type":"mode","mode":"default"}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-07-31T08:10:00Z","message":{"role":"user","content":"restart the server"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-07-31T08:10:05Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"long private reasoning"},{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo build --bins","description":"build"}}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-07-31T08:10:09Z","message":{"role":"user","content":[{"type":"tool_result","content":"<12MB of build output>"}]}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-07-31T08:11:00Z","message":{"role":"assistant","content":[{"type":"text","text":"subagent says hi"}]}}"#,
            "\n",
            r#"{"type":"file-history-snapshot","messageId":"x"}"#,
            "\n",
        );
        let msgs = parse_transcript_jsonl(body);
        assert_eq!(
            msgs.len(),
            3,
            "two conversation turns + one subagent line: {msgs:#?}"
        );
        assert_eq!(msgs[0].role, "user");
        assert_eq!(
            msgs[0].text, "restart the server",
            "a string content body is the human's message"
        );
        assert_eq!(
            msgs[1].text, "On it.",
            "thinking is not the conversation and is dropped"
        );
        assert_eq!(
            msgs[1].tools,
            vec!["Bash(cargo build --bins)"],
            "tool calls are summarised, not inlined"
        );
        assert!(
            msgs[2].sidechain,
            "subagent chatter is marked so the UI can dim it"
        );
        // a tool_result-only turn carries no human-readable content and must not become a message
        assert!(
            !msgs.iter().any(|m| m.text.contains("12MB")),
            "tool results stay out of the view"
        );
    }

    #[test]
    fn a_half_line_from_the_tail_is_never_parsed_as_a_message() {
        // Reading the last N bytes of a JSONL file almost always lands mid-line. That fragment is
        // not a record; parsing it would render a message with no role and half a sentence.
        let fragment =
            r#"pe":"assistant","message":{"content":[{"type":"text","text":"...half a line"}]}}"#;
        assert!(parse_transcript_jsonl(fragment).is_empty());
        let after = format!(
            "{fragment}\n{}",
            r#"{"type":"user","message":{"content":"whole line"}}"#
        );
        // the reader drops everything up to the first newline when it tailed; the parser also
        // refuses the fragment on its own, so both halves of the defence hold
        assert_eq!(parse_transcript_jsonl(&after).len(), 1);
    }

    #[test]
    fn long_messages_and_tool_details_are_clipped_not_dumped() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("abcdefghij", 4), "abcd…");
        let huge = "x".repeat(TRANSCRIPT_MAX_TEXT + 500);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{huge}"}}]}}}}"#
        );
        let msgs = parse_transcript_jsonl(&line);
        assert_eq!(
            msgs[0].text.chars().count(),
            TRANSCRIPT_MAX_TEXT + 1,
            "clipped, with the ellipsis"
        );
        // a write tool's input is the whole file — the summary must take the path, not the content
        let write = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/src/lib.rs","content":"...entire file..."}}]}}"#;
        assert_eq!(
            parse_transcript_jsonl(write)[0].tools,
            vec!["Write(/src/lib.rs)"]
        );
    }
}
