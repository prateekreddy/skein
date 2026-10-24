//! The conversation as the box's own **record** has it, not as the screen had it.
//!
//! tmux repaints only the visible pane, so a reboot, a server restart or a reattach loses the
//! scrollback — while the runtime's JSONL record on the box's disk survives all three. This reads
//! that, tail-first, so a huge session opens instantly and pages backwards on demand.

use crate::place::located;
use crate::place::shared_record;
use crate::repos::agent_for_box;
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
    /// The byte offset of this message's line in its record: what a search result names, and what
    /// the transcript route is handed back to open the record at this message.
    pub at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptView {
    pub path: String,
    /// The record's session id — its file name without `.jsonl`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// true when this is the box's newest record, the one its agent is writing
    pub current: bool,
    /// size of the record on disk
    pub size: u64,
    /// how much of the record we read
    pub scanned: u64,
    /// where in the record the read began
    pub from: u64,
    /// true when `scanned` reached the beginning — there is nothing older to ask for
    pub complete: bool,
    /// true when the read reached the end — false only for a window opened at an older message
    pub latest: bool,
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

/// One line of a runtime's JSONL record as a readable message, with its text whole — unclipped,
/// because search reads it — or `None` for a line that is not the conversation.
fn parse_line(v: &serde_json::Value, at: u64) -> Option<TranscriptMsg> {
    let role = match v.get("type").and_then(|t| t.as_str()) {
        Some(r @ ("user" | "assistant")) => r,
        // every other line type is bookkeeping (mode, last-prompt, file-history-*, attachment…)
        _ => return None,
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
        return None;
    }
    Some(TranscriptMsg {
        role: role.to_string(),
        ts: v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        text: text.trim().to_string(),
        tools,
        sidechain: v
            .get("isSidechain")
            .and_then(|s| s.as_bool())
            .unwrap_or(false),
        at,
    })
}

/// Parse a runtime's JSONL record into readable messages. Pure, so the shapes are unit-tested
/// against lines taken from a real transcript rather than from an assumption about them.
#[cfg(test)]
pub(crate) fn parse_transcript_jsonl(body: &str) -> Vec<TranscriptMsg> {
    parse_records(body.as_bytes(), 0, None)
}

/// [`parse_transcript_jsonl`] over raw bytes that begin `base` bytes into the record, so each
/// message carries its line's offset in the file. Bytes rather than text because the offsets are
/// the file's: a lossy conversion of the whole read would move every one after an invalid byte.
///
/// Keeps [`TRANSCRIPT_MAX_MSGS`]: the newest, or — when `target` names a message in the read —
/// the ones around it, so a window opened at an older message still holds that message.
fn parse_records(body: &[u8], base: u64, target: Option<u64>) -> Vec<TranscriptMsg> {
    let mut out: Vec<TranscriptMsg> = Vec::new();
    let mut at = base;
    for line in body.split_inclusive(|b| *b == b'\n') {
        let here = at;
        at += line.len() as u64;
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue; // a half-written last line, or a shape we don't read
        };
        if let Some(mut msg) = parse_line(&v, here) {
            msg.text = clip(&msg.text, TRANSCRIPT_MAX_TEXT);
            out.push(msg);
        }
    }
    if out.len() > TRANSCRIPT_MAX_MSGS {
        let keep_from = match target.and_then(|t| out.iter().position(|m| m.at == t)) {
            Some(i) => i
                .saturating_sub(TRANSCRIPT_MAX_MSGS / 2)
                .min(out.len() - TRANSCRIPT_MAX_MSGS),
            None => out.len() - TRANSCRIPT_MAX_MSGS,
        };
        out.drain(..keep_from);
        out.truncate(TRANSCRIPT_MAX_MSGS);
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
/// the cockpit only ever wants the end of one — or, from a search result, the stretch around one
/// message: `session` names the record and `at` the message's offset in it, and the window is
/// `bytes` wide around that message rather than at the end.
fn read_host_transcript(
    name: &str,
    bytes: u64,
    session: Option<&str>,
    at: Option<u64>,
) -> Result<TranscriptView, String> {
    use std::io::{Read, Seek, SeekFrom};
    let records = walk_jsonl(&box_records(name));
    // Newest by mtime, across every project slug: a box has one tree, but a rebuilt box can carry an
    // older slug beside the current one, and "most recently written" is what the agent is using.
    let newest = records
        .iter()
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path.clone());
    let chosen = match session {
        None => newest.clone(),
        Some(id) => records
            .iter()
            .filter(|(path, _)| session_of(path) == id)
            .max_by_key(|(_, mtime)| *mtime)
            .map(|(path, _)| path.clone()),
    };
    let Some(path) = chosen else {
        return Ok(empty_view(if session.is_some() {
            "that session is no longer in this box's record"
        } else {
            "no conversation record found for this box yet — the agent writes one as it runs"
        }));
    };
    // Through `boxfile`: the box writes this directory, and a FIFO or a link to a device here
    // parked or exhausted the request (SKEIN-1257). Read no further than the window either, so a
    // file still being appended to cannot grow the read.
    let (mut file, meta) =
        crate::boxfile::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let size = meta.len();
    let (from, mut to) = match at {
        None => (size.saturating_sub(bytes), size),
        Some(at) if at >= size => return Err("that message is past the end of its record".into()),
        Some(at) => {
            let to = size.min(at.saturating_sub(bytes / 2).saturating_add(bytes));
            (to.saturating_sub(bytes), to)
        }
    };
    file.seek(SeekFrom::Start(from))
        .map_err(|e| format!("seek {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    (&mut file)
        .take(to - from)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    // A window opened at a message must hold that message's whole line, which can be longer than
    // half the window: read on to its newline, but never more than the largest window again.
    if let Some(at) = at {
        let mut more = 0u64;
        let mut chunk = Vec::new();
        while to < size
            && more < TRANSCRIPT_MAX_BYTES
            && buf
                .get((at - from) as usize..)
                .is_some_and(|rest| !rest.contains(&b'\n'))
        {
            chunk.clear();
            let want = (size - to).min(64 * 1024);
            (&mut file)
                .take(want)
                .read_to_end(&mut chunk)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            if chunk.is_empty() {
                break;
            }
            buf.extend_from_slice(&chunk);
            to += chunk.len() as u64;
            more += chunk.len() as u64;
        }
    }
    let to = from + buf.len() as u64;
    let complete = from == 0;
    let latest = to >= size;
    // A window almost always starts mid-line; that fragment is not a record and must not be parsed
    // as one (it would render as a half message with no role). One that stops short of the end
    // stops mid-line too.
    let mut start = 0;
    if !complete {
        start = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |i| i + 1);
    }
    let mut stop = buf.len();
    if !latest {
        stop = buf
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1)
            .max(start);
    }
    Ok(TranscriptView {
        path: path.to_string_lossy().into_owned(),
        session: session_of(&path),
        current: newest.as_ref() == Some(&path),
        size,
        scanned: (buf.len() as u64).min(size),
        from,
        complete,
        latest,
        messages: parse_records(&buf[start..stop], from + start as u64, at),
        note: String::new(),
    })
}

/// Where a fleet box's records are on the host: its bound-in `~/.claude/projects`.
fn box_records(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(crate::fleet::box_state(name)).join("claude-projects")
}

/// A record's session id: its file name without `.jsonl`.
fn session_of(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A session id as a request may name one: a file stem, which a path separator or a dot is not.
fn valid_session(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn empty_view(note: &str) -> TranscriptView {
    TranscriptView {
        path: String::new(),
        session: String::new(),
        current: true,
        size: 0,
        scanned: 0,
        from: 0,
        complete: true,
        latest: true,
        messages: vec![],
        note: note.to_string(),
    }
}

/// Every `*.jsonl` under `root`, with its mtime. Shallow recursion by hand rather than a crate:
/// the layout is `<slug>/<session>.jsonl`, two levels, and skein has no walkdir dependency.
fn walk_jsonl(root: &std::path::Path) -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    // `DirEntry::metadata` does not follow a link, and `Path::is_dir` did: a slug a box linked to
    // its own parent was walked again at every level, down to the kernel's limit on links in one
    // path, and each further such link multiplies that walk (SKEIN-1257). Real directories and
    // regular files only.
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            found.extend(walk_jsonl(&path));
        } else if meta.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            if let Ok(mtime) = meta.modified() {
                found.push((path, mtime));
            }
        }
    }
    found
}

/// Read the tail of a box's conversation record. `bytes` is how much of the end to read — the
/// cockpit doubles it to page backwards, so a 14MB transcript never crosses the wire whole.
pub fn read_transcript(name: &str, bytes: u64) -> Result<TranscriptView, String> {
    read_transcript_at(name, bytes, None, None)
}

/// [`read_transcript`], or — with `session` and `at`, as a search result names them — the window
/// of that session's record around the message at that offset, however old it is.
pub fn read_transcript_at(
    name: &str,
    bytes: u64,
    session: Option<&str>,
    at: Option<u64>,
) -> Result<TranscriptView, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    if session.is_some_and(|id| !valid_session(id)) {
        return Err("invalid session".into());
    }
    let bytes = bytes.clamp(TRANSCRIPT_MIN_BYTES, TRANSCRIPT_MAX_BYTES);
    let empty = empty_view;
    let runtime = agent_for_box(name);
    // Claude keeps `~/.claude/projects/<slug>/<session>.jsonl`. Codex's rollout files are a
    // different shape and are NOT read here rather than guessed at — see docs/inventory.md §9.1 for
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
        return read_host_transcript(name, bytes, session, at);
    }
    let script = format!(
        "p=\"$(find \"$HOME/.claude/projects\" -type f -name '*.jsonl' -printf '%T@ %p\\n' 2>/dev/null | sort -nr | head -n1 | cut -d' ' -f2-)\"; \
         [ -n \"$p\" ] && [ -r \"$p\" ] || {{ echo '{TRANSCRIPT_NONE}'; exit 0; }}; \
         printf '{TRANSCRIPT_HEADER}%s %s\\n' \"$(wc -c <\"$p\")\" \"$p\"; \
         tail -c {bytes} \"$p\""
    );
    let raw = located(name)?.exec(&script, Duration::from_secs(60))?;
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
    let from = size.saturating_sub(scanned);
    let (skipped, body) = if complete {
        (0, body)
    } else {
        body.split_once('\n')
            .map(|(cut, rest)| (cut.len() as u64 + 1, rest))
            .unwrap_or((0, ""))
    };
    // Inside the box there is only ever the newest record's tail: search, and so a window opened
    // at an older message, reads a fleet box's record off the host.
    Ok(TranscriptView {
        path: path.to_string(),
        session: session_of(std::path::Path::new(path)),
        current: true,
        size,
        scanned,
        from,
        complete,
        latest: true,
        messages: parse_records(body.as_bytes(), from + skipped, None),
        note: String::new(),
    })
}

// ---------- search: the whole record, not the loaded tail ----------

/// Most messages one search returns. The cockpit draws each as a row, and past a couple of hundred
/// a list is not being read — the search wants narrowing.
pub const SEARCH_MAX_HITS: usize = 200;

/// Most bytes one search reads across every record it opens. A box's records together can be
/// gigabytes; the newest session is always read whole, and older ones until this is spent.
pub const SEARCH_MAX_SCAN: u64 = 512 * 1024 * 1024;

/// Longest line a search parses. A conversation line is kilobytes; a line past this is a tool's
/// output, which search does not read, and holding it whole would be the box choosing our memory.
const SEARCH_MAX_LINE: usize = 4 * 1024 * 1024;

/// Longest query, in characters.
pub const SEARCH_MAX_QUERY: usize = 200;

/// How much of a message stands either side of the match in a result.
const SNIPPET_BEFORE: usize = 60;
const SNIPPET_AFTER: usize = 120;

/// Whose words a search reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    User,
    Assistant,
    Both,
}

impl Who {
    pub fn parse(text: &str) -> Option<Who> {
        match text {
            "user" => Some(Who::User),
            "assistant" => Some(Who::Assistant),
            "" | "both" => Some(Who::Both),
            _ => None,
        }
    }
    fn takes(self, role: &str) -> bool {
        match self {
            Who::User => role == "user",
            Who::Assistant => role == "assistant",
            Who::Both => true,
        }
    }
}

/// One message a search found: where it is, who said it, and the stretch around the first match,
/// cut in three so the page can mark the match without parsing anything.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchHit {
    pub session: String,
    pub at: u64,
    pub role: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ts: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sidechain: bool,
    pub before: String,
    pub matched: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptSearch {
    /// Newest first: the newest session first, and the newest message first within each.
    pub hits: Vec<SearchHit>,
    /// true when there were more matches than [`SEARCH_MAX_HITS`]
    pub capped: bool,
    /// the newest record's session id, so a result can say "this session"
    pub current: String,
    /// how many records were read, and how many were left unread by [`SEARCH_MAX_SCAN`]
    pub sessions: usize,
    pub unread: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// Search a box's conversation for `query`: case-insensitive plain text, in `who`'s messages, in
/// its newest session or — `all` — in every session it has. Only what the Transcript tab shows as
/// text is read: tool calls, their output and thinking are not.
///
/// Reads the same tree [`read_transcript`] does, the same way: off the host, through
/// [`crate::boxfile`] — no link followed, no FIFO waited on, each file no further than the length
/// it had when it was opened.
pub fn search_transcript(
    name: &str,
    query: &str,
    who: Who,
    all: bool,
) -> Result<TranscriptSearch, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let query = query.trim();
    if query.is_empty() {
        return Err("nothing to search for".into());
    }
    if query.chars().count() > SEARCH_MAX_QUERY {
        return Err(format!("a search is at most {SEARCH_MAX_QUERY} characters"));
    }
    let empty = |note: String| TranscriptSearch {
        hits: vec![],
        capped: false,
        current: String::new(),
        sessions: 0,
        unread: 0,
        note,
    };
    let runtime = agent_for_box(name);
    if runtime != "claude" {
        return Ok(empty(format!(
            "search reads Claude's record only — {runtime}'s has a different shape"
        )));
    }
    if shared_record(name).is_none() {
        return Ok(empty(
            "search reads a fleet box's record, and this box keeps its record inside itself".into(),
        ));
    }
    let mut records = walk_jsonl(&box_records(name));
    records.sort_by(|a, b| b.1.cmp(&a.1));
    let current = records
        .first()
        .map(|(p, _)| session_of(p))
        .unwrap_or_default();
    if !all {
        records.truncate(1);
    }
    let needle = Needle::new(query);
    let mut out = TranscriptSearch {
        hits: vec![],
        capped: false,
        current,
        sessions: 0,
        unread: 0,
        note: String::new(),
    };
    let mut budget = SEARCH_MAX_SCAN;
    for (i, (path, _)) in records.iter().enumerate() {
        if out.hits.len() >= SEARCH_MAX_HITS {
            out.capped = true;
            break;
        }
        // The newest session is read whole whatever its size; an older one only while the budget
        // lasts, and never begun past it.
        if i > 0 && budget == 0 {
            out.unread = records.len() - i;
            break;
        }
        let keep = SEARCH_MAX_HITS - out.hits.len();
        let Ok((found, total, read)) = search_record(path, &needle, who, keep) else {
            continue; // a FIFO, a link or a device where a record should be: not a record
        };
        out.sessions += 1;
        budget = budget.saturating_sub(read);
        if total > found.len() {
            out.capped = true;
        }
        out.hits.extend(found.into_iter().rev());
    }
    if out.unread > 0 {
        out.note = format!(
            "{} older sessions were not searched — a search reads {}M at most",
            out.unread,
            SEARCH_MAX_SCAN >> 20
        );
    }
    Ok(out)
}

/// The query, ready to be looked for: lower-cased by character for the match itself, and — when it
/// is plain ASCII with nothing JSON escapes — as bytes, to skip a line without parsing it.
struct Needle {
    lower: String,
    chars: Vec<char>,
    raw: Option<Vec<u8>>,
}

impl Needle {
    fn new(query: &str) -> Needle {
        let lower: String = query.chars().flat_map(char::to_lowercase).collect();
        // A JSON writer escapes `"`, `\` and control characters, so a query holding one is not in
        // the line as typed; without one, an ASCII query is in the line's bytes exactly when it is
        // in the message, give or take case.
        let plain = query.is_ascii()
            && !query
                .chars()
                .any(|c| c == '"' || c == '\\' || c.is_control());
        Needle {
            chars: lower.chars().collect(),
            raw: plain.then(|| query.to_ascii_lowercase().into_bytes()),
            lower,
        }
    }

    /// Whether a raw line could hold the query at all.
    fn may_be_in(&self, line: &[u8]) -> bool {
        let Some(raw) = &self.raw else {
            return true;
        };
        line.windows(raw.len())
            .any(|w| w.iter().zip(raw).all(|(a, b)| a.to_ascii_lowercase() == *b))
    }

    /// The byte range of the first case-insensitive match in `text`.
    fn find(&self, text: &str) -> Option<(usize, usize)> {
        if !text.to_lowercase().contains(&self.lower) {
            return None;
        }
        let at: Vec<(usize, char)> = text.char_indices().collect();
        'start: for i in 0..at.len() {
            let mut k = 0;
            let mut j = i;
            while k < self.chars.len() {
                let Some((_, c)) = at.get(j) else {
                    continue 'start;
                };
                for lc in c.to_lowercase() {
                    if self.chars.get(k) != Some(&lc) {
                        continue 'start;
                    }
                    k += 1;
                }
                j += 1;
            }
            let end = at.get(j).map_or(text.len(), |(b, _)| *b);
            return Some((at[i].0, end));
        }
        None
    }
}

/// Search one record. Returns the last `keep` hits in record order, how many there were in all,
/// and how many bytes were read.
fn search_record(
    path: &std::path::Path,
    needle: &Needle,
    who: Who,
    keep: usize,
) -> std::io::Result<(std::collections::VecDeque<SearchHit>, usize, u64)> {
    use std::io::{BufRead, Read};
    let (file, meta) = crate::boxfile::open(path)?;
    let session = session_of(path);
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, file.take(meta.len()));
    let mut found = std::collections::VecDeque::new();
    let mut total = 0;
    let mut offset = 0u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let start = offset;
        let mut over = false;
        loop {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                break;
            }
            let (take, ends) = match buf.iter().position(|b| *b == b'\n') {
                Some(i) => (i, true),
                None => (buf.len(), false),
            };
            if !over {
                if line.len() + take <= SEARCH_MAX_LINE {
                    line.extend_from_slice(&buf[..take]);
                } else {
                    over = true;
                    line.clear();
                }
            }
            let used = take + usize::from(ends);
            reader.consume(used);
            offset += used as u64;
            if ends {
                break;
            }
        }
        if offset == start {
            break;
        }
        if over || !needle.may_be_in(&line) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(msg) = parse_line(&v, start) else {
            continue;
        };
        if !who.takes(&msg.role) {
            continue;
        }
        let Some((from, to)) = needle.find(&msg.text) else {
            continue;
        };
        total += 1;
        if keep == 0 {
            continue;
        }
        if found.len() == keep {
            found.pop_front();
        }
        found.push_back(SearchHit {
            session: session.clone(),
            at: start,
            role: msg.role,
            ts: msg.ts,
            sidechain: msg.sidechain,
            before: snippet_before(&msg.text[..from]),
            matched: one_line(&msg.text[from..to]),
            after: snippet_after(&msg.text[to..]),
        });
    }
    Ok((found, total, offset))
}

/// Whitespace runs — newlines included — as one space, so a snippet is one line. A run at either
/// end is kept as its space: it is what separates a snippet's halves from the match.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    if space && !out.is_empty() {
        out.push(' ');
    }
    out
}

fn snippet_before(text: &str) -> String {
    let flat = one_line(text);
    let n = flat.chars().count();
    if n <= SNIPPET_BEFORE {
        // the leading space a run collapsed to is kept: it separates the snippet from the match
        return flat.trim_start().to_string();
    }
    let tail: String = flat.chars().skip(n - SNIPPET_BEFORE).collect();
    format!("…{tail}")
}

fn snippet_after(text: &str) -> String {
    let flat = one_line(text);
    if flat.chars().count() <= SNIPPET_AFTER {
        return flat.trim_end().to_string();
    }
    let head: String = flat.chars().take(SNIPPET_AFTER).collect();
    format!("{head}…")
}

// ---------- history: the verification that briefly lived here ----------
// This section once documented a `Verify` feature added the day before by the commit that
// extracted this file (`refactor(core): extract tracking, diff, files, transcript and verify`):
// a click-triggered `cargo test` run inside the box through `sbx_guest_output`, whose result the
// board would show beside a box's other per-box signals. There was no `run_verify` for a tick,
// hook or schedule to call — the trigger was the click itself.
//
// The feature is gone, not merely uncalled: the very next commit,
// `feat: remove Verify, and tell a box who it commits as`, deleted it entire — "the module, the
// chip, the dockbar button, the output modal, the per-repo check command and its global default,
// the routes and the docs" — and this file's own section header outlived that deletion. Nothing
// below implements or calls any of it; the `exec` at the top of this file is unrelated (it reads
// a box's conversation transcript, not a check's output).

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
                ..Default::default()
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

    // The cockpit reads the newest `*.jsonl` under a fleet box's `claude-projects/`, which the box
    // writes. Planted: a FIFO newer than the real record, and a slug linked to a directory outside
    // holding a newer record still. Fails if the tail read opens plainly (the FIFO is newer than
    // the real record, so the open waits for a writer and `within` fails), or if the walk follows
    // the link (the outside record is the newest and the path below is not the real one).
    #[test]
    fn a_fifo_or_a_linked_slug_in_a_boxs_record_is_not_followed() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
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
                ..Default::default()
            },
        )
        .unwrap();
        let projects =
            std::path::PathBuf::from(crate::fleet::box_state("web-main")).join("claude-projects");
        let slug = projects.join("-boxes-web-main-tree");
        fs::create_dir_all(&slug).unwrap();
        fs::write(
            slug.join("s.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":\"ship it\"}}\n",
        )
        .unwrap();
        let outside = home.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join("newest.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":\"not this box's\"}}\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, slug.join("linked")).unwrap();
        // The FIFO is the newest file: the record is stamped a minute into the past.
        let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(slug.join("s.jsonl"))
            .and_then(|f| f.set_times(fs::FileTimes::new().set_modified(earlier)))
            .unwrap();
        let fifo = slug.join("z.jsonl");
        crate::boxfile::plant_fifo(&fifo);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(outside.join("newest.jsonl"))
            .and_then(|f| f.set_times(fs::FileTimes::new().set_modified(later)))
            .unwrap();

        let view = crate::boxfile::within(&fifo, "the transcript tail", || {
            read_transcript("web-main", TRANSCRIPT_MIN_BYTES)
        })
        .expect("read");
        assert_eq!(view.path, slug.join("s.jsonl").to_string_lossy());
        assert_eq!(
            view.messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["ship it"]
        );
    }

    // ---------- search ----------

    /// A placed fleet box `web-main` whose `claude-projects/` is returned, under pinned homes.
    fn searchable_box(home: &std::path::Path) -> std::path::PathBuf {
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
                ..Default::default()
            },
        )
        .unwrap();
        let _ = home;
        let slug = std::path::PathBuf::from(crate::fleet::box_state("web-main"))
            .join("claude-projects/-boxes-web-main-tree");
        fs::create_dir_all(&slug).unwrap();
        slug
    }

    fn user(ts: &str, text: &str) -> String {
        serde_json::json!({"type": "user", "timestamp": ts, "message": {"role": "user", "content": text}})
            .to_string()
    }

    fn claude(ts: &str, text: &str) -> String {
        serde_json::json!({"type": "assistant", "timestamp": ts, "message": {"role": "assistant",
            "content": [{"type": "text", "text": text}]}})
        .to_string()
    }

    fn stamp(path: &std::path::Path, secs_ago: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_times(fs::FileTimes::new().set_modified(when)))
            .unwrap();
    }

    /// Two sessions: an older one, and the newest — which has the parser mentioned once by each
    /// side and again in a tool call, a tool's output and a thought, none of which is the
    /// conversation.
    fn two_sessions(slug: &std::path::Path) {
        let old = [
            user("2026-10-01T09:00:00Z", "is the parser done?"),
            claude("2026-10-01T09:00:05Z", "Not yet."),
        ];
        fs::write(slug.join("older.jsonl"), old.join("\n") + "\n").unwrap();
        stamp(&slug.join("older.jsonl"), 3600);
        let new = [
            user("2026-10-02T09:00:00Z", "Where does the PARSER live?"),
            serde_json::json!({"type": "assistant", "timestamp": "2026-10-02T09:00:03Z", "message": {"content": [
                {"type": "thinking", "thinking": "the parser is in src"},
                {"type": "tool_use", "name": "Grep", "input": {"pattern": "parser"}}]}})
            .to_string(),
            serde_json::json!({"type": "user", "timestamp": "2026-10-02T09:00:04Z", "message": {"content": [
                {"type": "tool_result", "content": "src/parser.rs: fn parser()"}]}})
            .to_string(),
            claude("2026-10-02T09:00:05Z", "The parser lives in `src/transcript.rs`."),
        ];
        fs::write(slug.join("newest.jsonl"), new.join("\n") + "\n").unwrap();
    }

    fn said(found: &TranscriptSearch) -> Vec<(String, String, String)> {
        found
            .hits
            .iter()
            .map(|h| (h.session.clone(), h.role.clone(), h.matched.clone()))
            .collect()
    }

    // Fails if a filter is ignored (the user and Claude lists come back with both sides), if the
    // tool call, its output or the thought is searched (a third hit, matching `parser` in them), or
    // if the match is case-sensitive (the user's `PARSER` is missed).
    #[test]
    fn each_filter_reads_only_whose_words_it_names_and_never_a_tool() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let slug = searchable_box(&home);
        two_sessions(&slug);
        let s = |who| said(&search_transcript("web-main", "parser", who, false).unwrap());
        let me = (
            "newest".to_string(),
            "user".to_string(),
            "PARSER".to_string(),
        );
        let it = (
            "newest".to_string(),
            "assistant".to_string(),
            "parser".to_string(),
        );
        assert_eq!(s(Who::User), [me.clone()]);
        assert_eq!(s(Who::Assistant), [it.clone()]);
        assert_eq!(s(Who::Both), [it, me], "both sides, the newest first");
    }

    // Fails if `all` is ignored (the older session's question is missing), or if the default
    // scope reads more than the newest session (it is there without `all`).
    #[test]
    fn the_default_scope_is_the_newest_session_and_all_reads_every_one() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let slug = searchable_box(&home);
        two_sessions(&slug);
        let this = search_transcript("web-main", "parser", Who::User, false).unwrap();
        assert_eq!(this.sessions, 1);
        assert_eq!(this.current, "newest");
        assert!(this.hits.iter().all(|h| h.session == "newest"));
        let every = search_transcript("web-main", "parser", Who::User, true).unwrap();
        assert_eq!(
            said(&every),
            [
                ("newest".into(), "user".into(), "PARSER".into()),
                ("older".into(), "user".into(), "parser".into()),
            ],
            "every session, the newest first"
        );
        assert_eq!(every.sessions, 2);
        assert_eq!(every.hits[1].before, "is the ");
        assert_eq!(every.hits[1].after, " done?");
    }

    // The whole session, not the window the tab has loaded: the message the search must find is
    // the first line, behind more than the largest tail the tab ever reads. Fails if search reads
    // through `read_transcript`'s tail. And opening the record at that hit's offset must return a
    // window holding that very message — fails if `at` is ignored (the window is the tail, which
    // does not hold it).
    #[test]
    fn a_match_older_than_any_tail_is_found_and_opened_where_it_is() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let slug = searchable_box(&home);
        let filler = serde_json::json!({"type": "mode", "pad": "y".repeat(1000)}).to_string();
        let mut lines = vec![user(
            "2026-10-02T08:00:00Z",
            "the very first ask: rename the widget",
        )];
        lines.extend(std::iter::repeat_n(filler, 40));
        lines.push(user("2026-10-02T09:00:00Z", "and the last one"));
        fs::write(slug.join("long.jsonl"), lines.join("\n") + "\n").unwrap();
        let tail = read_transcript("web-main", TRANSCRIPT_MIN_BYTES).unwrap();
        assert!(
            !tail.messages.iter().any(|m| m.text.contains("widget")),
            "the control: the tail does not reach the first message"
        );
        let found = search_transcript("web-main", "WIDGET", Who::Both, false).unwrap();
        assert_eq!(
            said(&found),
            [("long".into(), "user".into(), "widget".into())]
        );
        let hit = &found.hits[0];
        let opened = read_transcript_at(
            "web-main",
            TRANSCRIPT_MIN_BYTES,
            Some(&hit.session),
            Some(hit.at),
        )
        .unwrap();
        let there = opened.messages.iter().find(|m| m.at == hit.at);
        assert_eq!(
            there.map(|m| m.text.as_str()),
            Some("the very first ask: rename the widget")
        );
        assert!(
            !opened.latest,
            "a window at an old message is not the end of the record"
        );
        assert!(opened.current);
    }

    // Opening a result in another session: fails if `session` is ignored (the newest record is
    // read and the older message is not in it), and a session named with a path is refused.
    #[test]
    fn a_result_in_another_session_opens_that_session() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let slug = searchable_box(&home);
        two_sessions(&slug);
        let every = search_transcript("web-main", "done", Who::User, true).unwrap();
        let hit = &every.hits[0];
        assert_eq!(hit.session, "older");
        let opened = read_transcript_at(
            "web-main",
            TRANSCRIPT_MIN_BYTES,
            Some("older"),
            Some(hit.at),
        )
        .unwrap();
        assert!(!opened.current);
        assert!(opened.path.ends_with("older.jsonl"));
        assert!(opened
            .messages
            .iter()
            .any(|m| m.at == hit.at && m.text == "is the parser done?"));
        assert!(
            read_transcript_at("web-main", TRANSCRIPT_MIN_BYTES, Some("../older"), Some(0))
                .is_err()
        );
    }

    // Planted where the box can: a slug linked to a directory outside holding a matching record,
    // a record-named link to it, and a FIFO named like a record. Fails if the walk follows a link
    // (the outside line comes back). The FIFO is refused twice — the walk keeps regular files only,
    // and the open is `boxfile`'s — so it takes both undone to fail: then the open waits for a
    // writer and `within` fails.
    #[test]
    fn search_follows_no_link_and_waits_on_no_fifo_in_a_boxs_record() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let slug = searchable_box(&home);
        two_sessions(&slug);
        let outside = home.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join("x.jsonl"),
            user("2026-10-03T00:00:00Z", "the parser, from outside") + "\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, slug.join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.join("x.jsonl"), slug.join("link.jsonl")).unwrap();
        let fifo = slug.join("zz.jsonl");
        crate::boxfile::plant_fifo(&fifo);
        let found = crate::boxfile::within(&fifo, "the transcript search", || {
            search_transcript("web-main", "parser", Who::Both, true)
        })
        .unwrap();
        assert!(
            !found.hits.iter().any(|h| h.after.contains("outside")),
            "a record outside the box was searched: {:?}",
            said(&found)
        );
        assert_eq!(found.hits.len(), 3);
    }

    // Fails if the cap is not applied (250 come back), if `capped` is not said, or if the ones kept
    // are not the newest (the first hit would be message 0, not message 249).
    #[test]
    fn results_stop_at_the_cap_keep_the_newest_and_say_so() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let slug = searchable_box(&home);
        let lines: Vec<String> = (0..250)
            .map(|i| user("2026-10-02T09:00:00Z", &format!("deploy number {i}")))
            .collect();
        fs::write(slug.join("many.jsonl"), lines.join("\n") + "\n").unwrap();
        let found = search_transcript("web-main", "deploy", Who::Both, false).unwrap();
        assert_eq!(found.hits.len(), SEARCH_MAX_HITS);
        assert!(found.capped);
        assert_eq!(found.hits[0].after, " number 249");
        let few = search_transcript("web-main", "number 7", Who::Both, false).unwrap();
        assert!(!few.capped, "{} matches is not capped", few.hits.len());
    }

    // The byte prefilter is for an ASCII query only. Fails if a non-ASCII query goes through it, or
    // the match is not case-folded by character: `CAFÉ` is not `café` in bytes.
    #[test]
    fn a_query_outside_ascii_is_matched_by_character_case() {
        let n = Needle::new("café");
        assert!(n.raw.is_none());
        assert_eq!(n.find("au CAFÉ ce soir"), Some((3, 8)));
        let a = Needle::new("Parser");
        assert!(a.may_be_in(br#"{"text":"the PARSER"}"#));
        assert!(!a.may_be_in(br#"{"text":"the lexer"}"#));
        assert_eq!(
            Needle::new("x\"y").raw,
            None,
            "a quote is escaped in the line"
        );
    }
}
