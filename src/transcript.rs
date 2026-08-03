//! The conversation as the box's own **record** has it, not as the screen had it.
//!
//! tmux repaints only the visible pane, so a reboot, a server restart or a reattach loses the
//! scrollback — while the runtime's JSONL record on the box's disk survives all three. This reads
//! that, tail-first, so a huge session opens instantly and pages backwards on demand.

use crate::util::*;
use crate::{agent_for_box, sbx_guest_output, valid_name};
use serde::Serialize;
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
