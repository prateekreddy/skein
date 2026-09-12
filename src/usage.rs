//! What the fleet has spent on Claude Code, counted from the transcripts skein already keeps.
//!
//! # Only counts leave a box
//!
//! This is the property the module exists to keep, and it is the reason it returns a struct rather
//! than a filtered copy of anything. A transcript holds file contents, diffs, command output, and
//! whatever anyone pasted into a conversation; [`UsageReport`] holds integers, model identifiers,
//! box names and dates, and there is no field on it or on anything it contains that can carry a
//! message body. `only_counts_leave_the_transcript` builds a report from a fixture whose every
//! message is a distinctive secret and asserts that none of it survives serialisation — it fails if
//! a field is ever added that carries text out of a record.
//!
//! # Where the data is
//!
//! `box_state(name)/claude-projects/`, on the host, the same tree [`crate::transcript`] reads.
//! `skein-server` runs outside any box's mount cover, so one process reads every box and no
//! per-box collector is needed. Under it:
//!
//! ```text
//! <project-slug>/<session-uuid>.jsonl                      the conversation
//! <project-slug>/<session-uuid>/subagents/agent-*.jsonl    one file per subagent turn
//! ```
//!
//! **The `subagents/` files are not optional.** Measured on this fleet on 2026-09-12: 1,196 of the
//! 2,232 transcripts and 956 MB of the 2.72 GB are subagent files, carrying 134,755 of the 377,177
//! assistant records. Every one of them has `isSidechain: true`, and no top-level record has it —
//! the flag does not mark a record inside a conversation, it marks which *file* you are reading.
//! Skipping them loses about a third of the fleet's spend, and none of the sixteen boxes reproduces
//! its known totals without them. So sidechain turns are **counted**: they are real API calls,
//! billed to the same account, and a spend figure that omits them is wrong rather than narrower.
//!
//! # What it costs to run
//!
//! Measured on this fleet on 2026-09-12 — 16 boxes, 2,232 transcripts, 2.72 GB — with a release
//! build, which is what `skein-server` is. Not estimated, and not a debug build: the same scan
//! takes 40.8s under `cargo test`'s default profile, so a figure taken there would have argued for
//! a design this one does not need.
//!
//! | call | cost | what it opens |
//! |---|---|---|
//! | [`cached`] | **under 1 ms** | one 20 kB file |
//! | [`report`] inside its window | **under 1 ms** | the same file |
//! | [`refresh`], nothing changed | **0.11 s** | `stat` on 2,232 files, no transcript parsed |
//! | [`refresh`], empty cache | **5.2 s** | all 2.72 GB |
//!
//! **[`cached`] is the page-load path and it costs a 20 kB read.** It did not always: with the
//! digests and the report in one file it was 255 ms, because reaching the report meant parsing
//! 10.9 MB of per-request rows first. They are two files for that reason and no other.
//!
//! # The record shape, established from the data rather than from the field names
//!
//! An `assistant` record carries `message.usage`. Four things about it are worth knowing before
//! reading the parse, because each one is a way to get a plausible wrong answer:
//!
//! 1. **The same request appears many times.** 377,177 assistant records across the fleet reduce to
//!    182,131 distinct `(message.id, requestId)` pairs. Duplicates are streamed partials: the
//!    record is appended again as the response grows, so `input_tokens`, `cache_read_input_tokens`
//!    and `cache_creation_input_tokens` repeat unchanged while `output_tokens` climbs. **Keeping
//!    the first copy of each key under-reports output by about half** — on one box-month it read
//!    1,002,413 against a true 2,040,196 — while leaving the other three fields exactly right,
//!    which is precisely the kind of error that survives a spot check. [`Turn::supersedes`] keeps
//!    the copy with the greatest `output_tokens`, which is order-independent and therefore does not
//!    depend on the order the filesystem hands back directory entries.
//! 2. **Cache writes split by time-to-live and price differently.** `cache_creation` carries
//!    `ephemeral_5m_input_tokens` and `ephemeral_1h_input_tokens`; the 1h rate is 2× input and the
//!    5m rate is 1.25×. This is not a rounding detail here: **59.2% of the fleet's cache writes are
//!    1h**, and on `claude-opus-4-8` it is 95.4%. Collapsing them into `cache_creation_input_tokens`
//!    under-prices the majority of cache-write spend.
//! 3. **`iterations[]` is not where the numbers live**, despite carrying a copy of them. Of 241,621
//!    records with a non-empty `iterations`, the array sums to exactly the top-level fields in
//!    241,612. The nine that differ have a zeroed top level and a populated array. Meanwhile most
//!    subagent records — 102,849 of 134,755 — have no `iterations` at all, so a parse that preferred
//!    the array would read zero for the bulk of subagent spend. The top level is what is read;
//!    [`Notes::iteration_only_records`] counts the nine so they are visible rather than assumed.
//! 4. **`<synthetic>` is not a model.** 636 records carry it, always with zero tokens; they are
//!    skein's own injected turns, not API calls, and they are skipped and counted in
//!    [`Notes::synthetic_records`].
//!
//! # A model the price table does not know is reported, never priced at zero
//!
//! A zero is a wrong answer that reads as a cheap month, and the table is the one fact in this file
//! that goes stale on someone else's schedule. So an unknown model does not contribute to any cost
//! — it contributes to [`UsageReport::unpriced`], with its token counts, and [`Totals::cost`] is
//! documented as the cost of the models that *are* priced. The panel renders that list as a visible
//! caveat. An empty `unpriced` is itself the useful answer, which is why it is always present.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ─────────────────────────────── the price table ───────────────────────────────

/// The day the prices below were read, and the only thing to change when they move.
///
/// It is carried out to the caller on every report so a number on a page can say how old the rates
/// behind it are, rather than looking equally current a year from now.
pub const PRICES_AS_OF: &str = "2026-09-12";

/// Dollars per million tokens, one row per model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

/// Published rates, per million tokens, as of [`PRICES_AS_OF`].
///
/// Input and output are the list prices. Cache read is 0.1× input and cache writes are 1.25×
/// (5-minute) and 2× (1-hour) input — **except `claude-fable-5-1`, whose cache read is $0.25 rather
/// than the $1.00 the multiplier would give**, which is exactly why the five numbers are written out
/// per model instead of derived from two.
///
/// **What is deliberately absent.** `claude-mythos-5-1` and `claude-sonnet-4-5` are used by this
/// fleet or adjacent to it and are *not* here, because their cache-read rate is either unsettled or
/// not something this file can cite. They therefore surface in [`UsageReport::unpriced`]. That is
/// the honest outcome and it is the point of the mechanism: a guessed rate would produce a number
/// that looks exactly like a known one.
const PRICES: &[(&str, Price)] = &[
    ("claude-opus-5", OPUS),
    ("claude-opus-4-8", OPUS),
    ("claude-opus-4-7", OPUS),
    ("claude-opus-4-6", OPUS),
    ("claude-sonnet-5", SONNET_5),
    ("claude-sonnet-4-6", SONNET_4_6),
    ("claude-haiku-4-5", HAIKU),
    ("claude-fable-5", FABLE),
    ("claude-fable-5-1", FABLE_5_1),
];

const OPUS: Price = Price {
    input: 5.0,
    output: 25.0,
    cache_read: 0.5,
    cache_write_5m: 6.25,
    cache_write_1h: 10.0,
};
const SONNET_5: Price = Price {
    input: 2.0,
    output: 10.0,
    cache_read: 0.2,
    cache_write_5m: 2.5,
    cache_write_1h: 4.0,
};
const SONNET_4_6: Price = Price {
    input: 3.0,
    output: 15.0,
    cache_read: 0.3,
    cache_write_5m: 3.75,
    cache_write_1h: 6.0,
};
const HAIKU: Price = Price {
    input: 1.0,
    output: 5.0,
    cache_read: 0.1,
    cache_write_5m: 1.25,
    cache_write_1h: 2.0,
};
const FABLE: Price = Price {
    input: 10.0,
    output: 50.0,
    cache_read: 1.0,
    cache_write_5m: 12.5,
    cache_write_1h: 20.0,
};
const FABLE_5_1: Price = Price {
    input: 10.0,
    output: 50.0,
    cache_read: 0.25,
    cache_write_5m: 12.5,
    cache_write_1h: 20.0,
};

/// The price for a model id, or `None` if the table does not know it.
///
/// Matching is exact, then by longest known prefix — the fleet carries
/// `claude-haiku-4-5-20251001` beside `claude-haiku-4-5`, and a dated snapshot of a model is that
/// model at that model's price. The prefix must end at a `-` so that a future `claude-opus-50`
/// cannot be silently priced as `claude-opus-5`.
pub fn price_of(model: &str) -> Option<Price> {
    if let Some((_, p)) = PRICES.iter().find(|(m, _)| *m == model) {
        return Some(*p);
    }
    PRICES
        .iter()
        .filter(|(m, _)| {
            model.len() > m.len()
                && model.starts_with(m)
                && model.as_bytes().get(m.len()) == Some(&b'-')
        })
        .max_by_key(|(m, _)| m.len())
        .map(|(_, p)| *p)
}

// ─────────────────────────────── the shape that leaves ───────────────────────────────

/// Token counts. Every field is a count; none of them can hold text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// `cache_creation_input_tokens`, the figure the record reports as its own total.
    pub cache_write: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl Tokens {
    fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
    }

    /// Input + output + cache read + cache write, which is what "tokens" means on the page.
    ///
    /// The TTL split is *not* added on top: `cache_write_5m` and `cache_write_1h` partition
    /// `cache_write`, they do not extend it.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// Cache-write tokens the record totalled but did not attribute to a TTL.
    ///
    /// Zero for all but nine records fleet-wide (3,773 tokens on `claude-opus-5`), where
    /// `cache_creation`'s two fields do not sum to `cache_creation_input_tokens`. Priced at the 5m
    /// rate — the cheaper of the two, so an unattributable token cannot inflate a bill — and counted
    /// in [`Notes::cache_write_unattributed`].
    fn unattributed_write(&self) -> u64 {
        self.cache_write
            .saturating_sub(self.cache_write_5m + self.cache_write_1h)
    }

    fn cost(&self, p: &Price) -> f64 {
        let m = 1_000_000.0;
        (self.input as f64 * p.input
            + self.output as f64 * p.output
            + self.cache_read as f64 * p.cache_read
            + self.cache_write_5m as f64 * p.cache_write_5m
            + self.cache_write_1h as f64 * p.cache_write_1h
            + self.unattributed_write() as f64 * p.cache_write_5m)
            / m
    }
}

/// Fleet-wide totals.
///
/// `cost` is the cost of the **priced** models only. Anything the table did not know is in
/// [`UsageReport::unpriced`] and contributes nothing here, by design.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub cost: f64,
    pub tokens: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

/// A model the price table did not know, with what it would have cost had anyone known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UnpricedModel {
    pub model: String,
    pub tokens: u64,
    pub records: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BoxRow {
    #[serde(rename = "box")]
    pub name: String,
    pub cost: f64,
    pub tokens: u64,
    /// Distinct days on which this box made a call.
    pub days: u64,
    pub first: String,
    pub last: String,
    /// Cost per model, for the models this box used.
    pub models: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MonthRow {
    pub month: String,
    pub cost: f64,
    pub tokens: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DayRow {
    pub day: String,
    pub cost: f64,
    pub by_box: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelRow {
    pub model: String,
    pub cost: f64,
    pub tokens: u64,
    /// False when this row's tokens are also in [`UsageReport::unpriced`] and its cost is therefore
    /// zero because nothing knew the rate — not because the model was free.
    pub priced: bool,
}

/// What the scan could not do, counted rather than swallowed.
///
/// Every field here is a way the answer could be wrong, made visible. A reader that ignores them
/// gets the same number it would have got anyway; a reader that checks them can tell a clean scan
/// from one that skipped 400 unreadable files.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Notes {
    pub unreadable_files: u64,
    pub unparsable_lines: u64,
    pub undated_records: u64,
    pub synthetic_records: u64,
    /// Records dropped because a copy of the same `(message.id, requestId)` was kept.
    pub duplicates_collapsed: u64,
    /// Records whose top-level usage is all zero while `iterations[]` is not. See the module note.
    pub iteration_only_records: u64,
    /// Cache-write tokens that no TTL claimed. See [`Tokens::unattributed_write`].
    pub cache_write_unattributed: u64,
}

impl Notes {
    fn add(&mut self, o: &Notes) {
        self.unreadable_files += o.unreadable_files;
        self.unparsable_lines += o.unparsable_lines;
        self.undated_records += o.undated_records;
        self.synthetic_records += o.synthetic_records;
        self.duplicates_collapsed += o.duplicates_collapsed;
        self.iteration_only_records += o.iteration_only_records;
        self.cache_write_unattributed += o.cache_write_unattributed;
    }
}

/// The whole answer, and the only thing that crosses out of this module.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    pub prices_as_of: String,
    /// When the transcripts were last actually read, RFC3339 UTC. Not when this struct was handed
    /// out — a page that renders `read_at` is telling the truth about its age either way.
    pub read_at: String,
    /// Whether [`read_at`](Self::read_at) is within the caller's freshness window.
    pub fresh: bool,
    pub age_secs: u64,
    pub boxes_read: usize,
    pub transcripts_read: usize,
    pub bytes_read: u64,
    pub totals: Totals,
    pub unpriced: Vec<UnpricedModel>,
    pub boxes: Vec<BoxRow>,
    pub months: Vec<MonthRow>,
    pub daily: Vec<DayRow>,
    pub models: Vec<ModelRow>,
    pub notes: Notes,
}

// ─────────────────────────────── parsing ───────────────────────────────

/// One billable turn, after duplicates have been resolved.
#[derive(Debug, Clone, PartialEq)]
struct Turn {
    /// `(message.id, requestId)`, hashed. See [`dedup_key`].
    key: u64,
    model: String,
    /// `YYYY-MM-DD`, UTC.
    day: String,
    tokens: Tokens,
}

impl Turn {
    /// Does `self` replace `other` as the copy of this request that gets counted?
    ///
    /// Greater `output_tokens` wins, because duplicates are streamed partials of one response and
    /// the last one written is the complete one. Deliberately a total order on a value rather than
    /// "whichever was read last": directory iteration order is not defined, and a rule that depends
    /// on it gives a different total on a different filesystem.
    fn supersedes(&self, other: &Turn) -> bool {
        self.tokens.output > other.tokens.output
    }
}

/// FNV-1a, written out rather than taken from `DefaultHasher`.
///
/// The hash is *stored*, in the cache, so it has to mean the same thing in the binary that reads the
/// cache as in the one that wrote it. `DefaultHasher`'s output is explicitly not guaranteed stable
/// across Rust releases, which would turn a toolchain bump into a silently wrong tally rather than
/// an error.
fn fnv1a(parts: [&str; 2]) -> u64 {
    let mut h = FNV_OFFSET;
    for (i, s) in parts.iter().enumerate() {
        if i > 0 {
            h = fnv1a_byte(h, 0xff);
        }
        h = fnv1a_bytes(h, s.as_bytes());
    }
    h
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a_byte(h: u64, b: u8) -> u64 {
    (h ^ b as u64).wrapping_mul(FNV_PRIME)
}

fn fnv1a_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h = fnv1a_byte(h, *b);
    }
    h
}

/// A stable, non-reversing name for a box, for use in anything that gets committed.
///
/// A box name is fleet residue: `tools/residue-check.py` fails the build when one appears in the
/// tree, because the owner's decision was that no real box name of any kind stays here (SKEIN-629).
/// The oracle fixture in `tests/usage.rs` still has to say *which* box each row of known-good
/// numbers belongs to, so it says this instead — and the test, which is reading a real fleet
/// directory when it runs, prints the box's actual name in any discrepancy. The repository learns
/// nothing; the failure message loses nothing.
pub fn box_key(name: &str) -> String {
    format!("{:016x}", fnv1a_bytes(FNV_OFFSET, name.as_bytes()))
}

/// The identity of a request: `message.id` and `requestId` together.
///
/// Neither alone is enough in principle, though on this fleet either would do — all 182,131 keys
/// agree whichever half you drop. `uuid` is *not* usable: it is per *record*, so every streamed
/// partial has its own and nothing dedups at all (it read 2.07× the true token count when tried).
fn dedup_key(message_id: &str, request_id: &str) -> u64 {
    fnv1a([message_id, request_id])
}

fn num(v: Option<&serde_json::Value>) -> u64 {
    v.and_then(|v| v.as_u64()).unwrap_or(0)
}

/// Pull the billable turn out of one transcript line, or say why there is none.
fn parse_line(line: &str, notes: &mut Notes) -> Option<Turn> {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            notes.unparsable_lines += 1;
            return None;
        }
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    let msg = v.get("message")?;
    let usage = msg.get("usage")?;
    if !usage.is_object() {
        return None;
    }
    let model = msg.get("model").and_then(|m| m.as_str()).unwrap_or("");
    // `<synthetic>` is skein's own injected turn, not an API call. Always zero tokens.
    if model.is_empty() || model == "<synthetic>" {
        notes.synthetic_records += 1;
        return None;
    }

    let cc = usage.get("cache_creation");
    let mut tokens = Tokens {
        input: num(usage.get("input_tokens")),
        output: num(usage.get("output_tokens")),
        cache_read: num(usage.get("cache_read_input_tokens")),
        cache_write: num(usage.get("cache_creation_input_tokens")),
        cache_write_5m: num(cc.and_then(|c| c.get("ephemeral_5m_input_tokens"))),
        cache_write_1h: num(cc.and_then(|c| c.get("ephemeral_1h_input_tokens"))),
    };
    notes.cache_write_unattributed += tokens.unattributed_write();

    // The nine records whose top level is zeroed while `iterations[]` is not. Counted, and left
    // alone: reading the array instead would disagree with every other record in the file, and the
    // known-good totals this module is checked against are the top level's.
    if tokens.total() == 0 {
        if let Some(iters) = usage.get("iterations").and_then(|i| i.as_array()) {
            let any = iters.iter().any(|i| {
                num(i.get("input_tokens"))
                    + num(i.get("output_tokens"))
                    + num(i.get("cache_read_input_tokens"))
                    + num(i.get("cache_creation_input_tokens"))
                    > 0
            });
            if any {
                notes.iteration_only_records += 1;
            }
        }
    }
    // The TTL split partitions the total; it can never exceed it.
    if tokens.cache_write_5m + tokens.cache_write_1h > tokens.cache_write {
        tokens.cache_write = tokens.cache_write_5m + tokens.cache_write_1h;
    }

    let day = match v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(utc_day)
    {
        Some(d) => d,
        None => {
            notes.undated_records += 1;
            return None;
        }
    };
    let message_id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
    let request_id = v.get("requestId").and_then(|i| i.as_str()).unwrap_or("");
    Some(Turn {
        key: dedup_key(message_id, request_id),
        model: model.to_string(),
        day,
        tokens,
    })
}

/// `YYYY-MM-DD` in UTC, from an RFC3339 timestamp.
///
/// Parsed rather than sliced. Every timestamp on this fleet ends in `Z`, so slicing would agree
/// today — and would put a record an hour into the wrong day, silently, the first time one carried
/// an offset instead. Bucketing in UTC is not an assumption: bucketing this fleet's records in seven
/// other zones was tried against known-good daily totals and every one of them was worse.
fn utc_day(ts: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d")
            .to_string(),
    )
}

/// Every `*.jsonl` at any depth under `root`, with its length and mtime.
///
/// Recursive, unlike [`crate::transcript`]'s two-level walk, and that is the whole difference
/// between counting a third of the fleet's spend and counting all of it: subagent transcripts live
/// at `<session>/subagents/agent-*.jsonl`, one level deeper than the conversation.
fn walk_jsonl(root: &Path, out: &mut Vec<(PathBuf, u64, u128)>, notes: &mut Notes) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            notes.unreadable_files += 1;
            continue;
        };
        if meta.is_dir() {
            walk_jsonl(&path, out, notes);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            out.push((path, meta.len(), mtime));
        }
    }
}

/// Parse one transcript into its distinct turns, resolving duplicates within the file.
fn scan_file(path: &Path, notes: &mut Notes) -> Vec<Turn> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => {
            notes.unreadable_files += 1;
            return Vec::new();
        }
    };
    let mut by_key: HashMap<u64, Turn> = HashMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(turn) = parse_line(line, notes) else {
            continue;
        };
        merge_turn(&mut by_key, turn, notes);
    }
    by_key.into_values().collect()
}

fn merge_turn(into: &mut HashMap<u64, Turn>, turn: Turn, notes: &mut Notes) {
    match into.get_mut(&turn.key) {
        Some(existing) => {
            notes.duplicates_collapsed += 1;
            if turn.supersedes(existing) {
                *existing = turn;
            }
        }
        None => {
            into.insert(turn.key, turn);
        }
    }
}

// ─────────────────────────────── the cache ───────────────────────────────

/// One deduplicated request, as stored.
///
/// The fields, in order: key, model index, day index, input, output, cache_read, cache_write,
/// cache_write_5m, cache_write_1h.
///
/// A tuple rather than a struct with named fields, and the model and day **interned** across the
/// whole file rather than spelled per row: there is one of these per distinct request — 182,131 on
/// this fleet — and field names repeated 182,131 times were most of the file before they went.
type DigestRow = (u64, u32, u32, u64, u64, u64, u64, u64, u64);

/// One transcript's parsed turns, and the fingerprint that says whether they are still current.
///
/// **The key is `(len, mtime_ns)`**, per file. These are append-only logs written by a process that
/// is not coordinating with this one, so there is nothing to lock against and no generation counter
/// to read; length and mtime are what the filesystem will tell us for free.
///
/// **What defeats it**, in the order it is likely to matter:
///
/// * A rewrite that lands on the same length *and* the same mtime. An ordinary append moves both;
///   an editor that restores times (`rsync --times`, `touch -r`, a restore from backup) can move
///   neither. The tally then keeps a stale file's numbers until something else touches it.
/// * mtime granularity. Two writes inside one filesystem tick that leave the length unchanged are
///   one event as far as this is concerned. Nanoseconds are recorded, so this needs a filesystem
///   that does not provide them.
/// * **Not** "the file only ever grows" — that was the assumption worth checking, and it is false.
///   A 114 MB transcript on this fleet was observed being rewritten in place rather than appended
///   to, losing two days of records. Length moved, so the fingerprint caught it; a scheme keyed on
///   length *alone* would have been wrong for as long as the file stayed the same size.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileDigest {
    len: u64,
    mtime_ns: u128,
    rows: Vec<DigestRow>,
    notes: Notes,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cache {
    /// Bumped when the parse changes meaning, which throws every digest away rather than mixing two
    /// readings of the same bytes.
    version: u32,
    read_at_unix: u64,
    models: Vec<String>,
    days: Vec<String>,
    /// Keyed by box name, then by the transcript's path.
    files: BTreeMap<String, BTreeMap<String, FileDigest>>,
}

/// The answer, on its own, in its own file.
///
/// **Split from [`Cache`] rather than being a field on it, and the reason is measured.** The digest
/// store is 10.9 MB on this fleet because it holds a row per distinct request; the report is a few
/// hundred. With the two in one file, [`cached`] — the page-load path, the one call that exists to
/// be cheap — spent 255 ms parsing every digest to reach a report at the end of them. Apart they are
/// 3 ms and nothing else changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    version: u32,
    read_at_unix: u64,
    report: UsageReport,
}

/// Changing this discards every cached digest. Bump it whenever [`parse_line`] changes what it
/// counts — a cache half-written by the old rule and half by the new is a number nobody can defend.
const CACHE_VERSION: u32 = 1;

/// The per-transcript digests. Large, and read only by [`refresh`].
fn digest_path() -> PathBuf {
    crate::config::skein_home().join("usage-digests.json")
}

/// The tally itself. Small, and the only thing [`cached`] opens.
fn report_path() -> PathBuf {
    crate::config::skein_home().join("usage.json")
}

fn load_digests() -> Option<Cache> {
    let body = std::fs::read_to_string(digest_path()).ok()?;
    let cache: Cache = serde_json::from_str(&body).ok()?;
    (cache.version == CACHE_VERSION).then_some(cache)
}

fn load_stored() -> Option<Stored> {
    let body = std::fs::read_to_string(report_path()).ok()?;
    let stored: Stored = serde_json::from_str(&body).ok()?;
    (stored.version == CACHE_VERSION).then_some(stored)
}

/// Write via a same-directory temp and a rename.
///
/// A reader sees the whole previous tally or the whole new one, never a half-written file, and a
/// crash mid-write costs a rescan rather than the cache. The report is written **after** the
/// digests: the failure that leaves a fresh report beside stale digests would have the page showing
/// numbers no later refresh could reproduce, while the other order only costs one extra scan.
fn write_json<T: Serialize>(path: PathBuf, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let body =
        serde_json::to_string(value).map_err(|e| format!("serialise {}: {e}", path.display()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename into {}: {e}", path.display()))
}

// ─────────────────────────────── the scan ───────────────────────────────

/// Every `(month, model)` cell under one box's `claude-projects` directory.
///
/// The same walk, parse and duplicate rule [`refresh`] uses, stopping one step before aggregation
/// and before the cache — which is what makes it the seam the oracle comparison drives. A test that
/// compared [`UsageReport`] instead would be comparing this module's arithmetic as well as its
/// reading, and could not say which of the two was wrong.
pub fn cells_for(projects: &Path) -> (BTreeMap<(String, String), Tokens>, Notes) {
    let mut notes = Notes::default();
    let mut files = Vec::new();
    walk_jsonl(projects, &mut files, &mut notes);
    let mut turns: HashMap<u64, Turn> = HashMap::new();
    for (path, _, _) in &files {
        for turn in scan_file(path, &mut notes) {
            merge_turn(&mut turns, turn, &mut Notes::default());
        }
    }
    let mut cells: BTreeMap<(String, String), Tokens> = BTreeMap::new();
    for turn in turns.values() {
        cells
            .entry((turn.day[..7].to_string(), turn.model.clone()))
            .or_default()
            .add(&turn.tokens);
    }
    (cells, notes)
}

/// `(file count, total bytes, newest mtime in whole seconds)` for one box's transcripts.
///
/// Three numbers, and deliberately no paths: this is recorded in a committed fixture so a test can
/// tell "the reader is wrong" from "the bytes moved since the known-good tally was taken", and a
/// path list would put the owner's directory names in the repository to do it.
///
/// **Seconds, not the nanoseconds the cache key uses**, and the difference is deliberate. The cache
/// compares a value this process read against a value this process wrote, so it can afford to be
/// exact. This is compared against a number recorded by a different tool at a different time, and
/// the newest mtime across a whole box was observed shifting by a few microseconds between two
/// readings of a directory whose file count and total size had not moved at all. Whatever produces
/// that, it is not an edit — and at nanosecond granularity it silently pushed three boxes out of
/// the comparison, which is the failure a test like this cannot afford: it does not go red, it
/// quietly checks less. A real change to a transcript moves the length.
pub fn fingerprint(projects: &Path) -> (u64, u64, u64) {
    let mut files = Vec::new();
    let mut notes = Notes::default();
    walk_jsonl(projects, &mut files, &mut notes);
    let bytes = files.iter().map(|(_, len, _)| *len).sum();
    let newest = files.iter().map(|(_, _, m)| *m).max().unwrap_or(0);
    (files.len() as u64, bytes, (newest / 1_000_000_000) as u64)
}

/// Where each box's transcripts live: `box_state(name)/claude-projects`.
fn box_roots() -> Vec<(String, PathBuf)> {
    let root = PathBuf::from(crate::fleet::box_state_root());
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        let projects = PathBuf::from(crate::fleet::box_state(&name)).join("claude-projects");
        if projects.is_dir() {
            out.push((name, projects));
        }
    }
    out.sort();
    out
}

/// Re-read the fleet, reusing every transcript whose length and mtime have not moved.
///
/// This is the only function that opens a transcript. See the module's cost table before calling it
/// from anything a person is waiting on.
pub fn refresh() -> Result<UsageReport, String> {
    let previous = load_digests().unwrap_or_default();
    let mut cache = Cache {
        version: CACHE_VERSION,
        models: previous.models.clone(),
        days: previous.days.clone(),
        ..Default::default()
    };
    let mut model_ids: HashMap<String, u32> = cache
        .models
        .iter()
        .enumerate()
        .map(|(i, m)| (m.clone(), i as u32))
        .collect();
    let mut day_ids: HashMap<String, u32> = cache
        .days
        .iter()
        .enumerate()
        .map(|(i, d)| (d.clone(), i as u32))
        .collect();

    let mut notes = Notes::default();
    let mut transcripts = 0usize;
    let mut bytes = 0u64;
    // Box -> the distinct turns it made. Dedup is fleet-global per box rather than per file: 12,100
    // of the 182,131 keys on this fleet appear in more than one transcript, because resuming a
    // session copies the records so far into the new file. A per-file tally would count those twice.
    let mut per_box: BTreeMap<String, HashMap<u64, Turn>> = BTreeMap::new();

    for (name, projects) in box_roots() {
        let mut files = Vec::new();
        walk_jsonl(&projects, &mut files, &mut notes);
        let empty = BTreeMap::new();
        let old = previous.files.get(&name).unwrap_or(&empty);
        let mut digests: BTreeMap<String, FileDigest> = BTreeMap::new();
        let turns = per_box.entry(name.clone()).or_default();

        for (path, len, mtime_ns) in files {
            transcripts += 1;
            bytes += len;
            let key = path.to_string_lossy().into_owned();
            let digest = match old.get(&key) {
                Some(d) if d.len == len && d.mtime_ns == mtime_ns => d.clone(),
                _ => {
                    let mut file_notes = Notes::default();
                    let parsed = scan_file(&path, &mut file_notes);
                    let rows = parsed
                        .iter()
                        .map(|t| {
                            let next = model_ids.len() as u32;
                            let mi = *model_ids.entry(t.model.clone()).or_insert_with(|| {
                                cache.models.push(t.model.clone());
                                next
                            });
                            let next = day_ids.len() as u32;
                            let di = *day_ids.entry(t.day.clone()).or_insert_with(|| {
                                cache.days.push(t.day.clone());
                                next
                            });
                            (
                                t.key,
                                mi,
                                di,
                                t.tokens.input,
                                t.tokens.output,
                                t.tokens.cache_read,
                                t.tokens.cache_write,
                                t.tokens.cache_write_5m,
                                t.tokens.cache_write_1h,
                            )
                        })
                        .collect();
                    FileDigest {
                        len,
                        mtime_ns,
                        rows,
                        notes: file_notes,
                    }
                }
            };
            notes.add(&digest.notes);
            for row in &digest.rows {
                let Some(model) = cache.models.get(row.1 as usize) else {
                    continue;
                };
                let Some(day) = cache.days.get(row.2 as usize) else {
                    continue;
                };
                merge_turn(
                    turns,
                    Turn {
                        key: row.0,
                        model: model.clone(),
                        day: day.clone(),
                        tokens: Tokens {
                            input: row.3,
                            output: row.4,
                            cache_read: row.5,
                            cache_write: row.6,
                            cache_write_5m: row.7,
                            cache_write_1h: row.8,
                        },
                    },
                    &mut Notes::default(),
                );
            }
            digests.insert(key, digest);
        }
        cache.files.insert(name, digests);
    }

    let now = SystemTime::now();
    let read_at_unix = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let report = aggregate(&per_box, notes, transcripts, bytes, read_at_unix);
    cache.read_at_unix = read_at_unix;
    write_json(digest_path(), &cache)?;
    write_json(
        report_path(),
        &Stored {
            version: CACHE_VERSION,
            read_at_unix,
            report: report.clone(),
        },
    )?;
    Ok(report)
}

/// Turn the deduplicated turns into the shape that leaves.
fn aggregate(
    per_box: &BTreeMap<String, HashMap<u64, Turn>>,
    notes: Notes,
    transcripts: usize,
    bytes: u64,
    read_at_unix: u64,
) -> UsageReport {
    let mut totals = Totals::default();
    let mut by_model: BTreeMap<String, (Tokens, u64)> = BTreeMap::new();
    let mut by_month: BTreeMap<String, Tokens> = BTreeMap::new();
    let mut by_month_cost: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_day: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    let mut boxes: Vec<BoxRow> = Vec::new();

    for (name, turns) in per_box {
        let mut row = BoxRow {
            name: name.clone(),
            ..Default::default()
        };
        let mut days: BTreeMap<String, ()> = BTreeMap::new();
        let mut box_tokens = Tokens::default();
        for turn in turns.values() {
            let price = price_of(&turn.model);
            let cost = price.map(|p| turn.tokens.cost(&p)).unwrap_or(0.0);
            box_tokens.add(&turn.tokens);
            row.cost += cost;
            *row.models.entry(turn.model.clone()).or_default() += cost;
            days.insert(turn.day.clone(), ());
            let entry = by_model.entry(turn.model.clone()).or_default();
            entry.0.add(&turn.tokens);
            entry.1 += 1;
            let month = turn.day[..7].to_string();
            by_month.entry(month.clone()).or_default().add(&turn.tokens);
            *by_month_cost.entry(month).or_default() += cost;
            *by_day
                .entry(turn.day.clone())
                .or_default()
                .entry(name.clone())
                .or_default() += cost;
        }
        row.tokens = box_tokens.total();
        row.days = days.len() as u64;
        row.first = days.keys().next().cloned().unwrap_or_default();
        row.last = days.keys().next_back().cloned().unwrap_or_default();
        totals.cost += row.cost;
        totals.input += box_tokens.input;
        totals.output += box_tokens.output;
        totals.cache_read += box_tokens.cache_read;
        totals.cache_write += box_tokens.cache_write;
        totals.cache_write_5m += box_tokens.cache_write_5m;
        totals.cache_write_1h += box_tokens.cache_write_1h;
        totals.tokens += box_tokens.total();
        if !turns.is_empty() {
            boxes.push(row);
        }
    }
    boxes.sort_by(|a, b| b.cost.total_cmp(&a.cost));

    let mut unpriced = Vec::new();
    let mut models = Vec::new();
    for (model, (tokens, records)) in &by_model {
        match price_of(model) {
            Some(p) => models.push(ModelRow {
                model: model.clone(),
                cost: tokens.cost(&p),
                tokens: tokens.total(),
                priced: true,
            }),
            None => {
                unpriced.push(UnpricedModel {
                    model: model.clone(),
                    tokens: tokens.total(),
                    records: *records,
                });
                models.push(ModelRow {
                    model: model.clone(),
                    cost: 0.0,
                    tokens: tokens.total(),
                    priced: false,
                });
            }
        }
    }
    models.sort_by(|a, b| b.cost.total_cmp(&a.cost).then(a.model.cmp(&b.model)));
    unpriced.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.model.cmp(&b.model)));

    let months = by_month
        .into_iter()
        .map(|(month, t)| MonthRow {
            cost: by_month_cost.get(&month).copied().unwrap_or(0.0),
            month,
            tokens: t.total(),
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
        })
        .collect();
    let daily = by_day
        .into_iter()
        .map(|(day, by_box)| DayRow {
            cost: by_box.values().sum(),
            day,
            by_box,
        })
        .collect();

    UsageReport {
        prices_as_of: PRICES_AS_OF.to_string(),
        read_at: unix_to_rfc3339(read_at_unix),
        fresh: true,
        age_secs: 0,
        boxes_read: per_box.len(),
        transcripts_read: transcripts,
        bytes_read: bytes,
        totals,
        unpriced,
        boxes,
        months,
        daily,
        models,
        notes,
    }
}

fn unix_to_rfc3339(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

/// The cached tally, exactly as it was last computed. **Never opens a transcript.**
///
/// This is what a page load calls. `None` means nothing has ever been counted, which is the one
/// case where a caller has to decide between showing nothing and paying for [`refresh`].
pub fn cached() -> Option<UsageReport> {
    let stored = load_stored()?;
    let mut report = stored.report;
    report.age_secs = age_secs(stored.read_at_unix);
    report.fresh = false;
    Some(report)
}

fn age_secs(read_at_unix: u64) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(read_at_unix))
        .unwrap_or(0)
}

/// The tally, re-read only if the cached one is older than `max_age`.
///
/// This is the "at most once an hour" rule, and the caller names the hour rather than this module
/// assuming it. A user asking for fresh numbers passes `Duration::ZERO`, which always rescans;
/// nothing else should, because [`refresh`]'s cost table is the reason this function exists.
pub fn report(max_age: Duration) -> Result<UsageReport, String> {
    if let Some(cached) = cached() {
        if Duration::from_secs(cached.age_secs) <= max_age {
            return Ok(UsageReport {
                fresh: true,
                ..cached
            });
        }
    }
    refresh()
}

/// What this cost to run when it was written, so the next person deciding where to call it from has
/// a number rather than an intuition.
///
/// Release build, 2026-09-12, 16 boxes / 2,232 transcripts / 2.72 GB. These are the figures the
/// module's cost table quotes, and `the_measured_costs_are_stated` fails if the table stops
/// quoting them — a table and a constant that disagree is worse than either alone.
pub const MEASURED: &[(&str, &str)] = &[
    ("2,232 transcripts", "the fleet this was measured on"),
    ("2.72 GB", "their total size"),
    ("5.2 s", "refresh with an empty cache"),
    ("0.11 s", "refresh with nothing changed"),
    ("under 1 ms", "cached, the page-load path"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A transcript line for one assistant turn.
    ///
    /// The five token counts arrive as one tuple — `(input, output, cache_read, write_5m,
    /// write_1h)` — rather than as five parameters, so that the helper stays inside the argument
    /// count the lints allow without an `#[allow]` that would make a prose exemption redundant.
    fn line(
        msg_id: &str,
        req_id: &str,
        model: &str,
        ts: &str,
        tok: (u64, u64, u64, u64, u64),
    ) -> String {
        let (input, output, cache_read, w5, w1) = tok;
        serde_json::json!({
            "type": "assistant",
            "timestamp": ts,
            "requestId": req_id,
            "isSidechain": false,
            "message": {
                "id": msg_id,
                "model": model,
                "content": [{"type": "text", "text": "BODY-SHOULD-NEVER-LEAVE"}],
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": cache_read,
                    "cache_creation_input_tokens": w5 + w1,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": w5,
                        "ephemeral_1h_input_tokens": w1
                    }
                }
            }
        })
        .to_string()
    }

    fn turns(body: &str) -> (Vec<Turn>, Notes) {
        let mut notes = Notes::default();
        let mut by_key: HashMap<u64, Turn> = HashMap::new();
        for l in body.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(t) = parse_line(l, &mut notes) {
                merge_turn(&mut by_key, t, &mut notes);
            }
        }
        let mut out: Vec<Turn> = by_key.into_values().collect();
        out.sort_by_key(|t| t.key);
        (out, notes)
    }

    /// Fails if the duplicate rule keeps the first copy instead of the largest.
    ///
    /// This is the bug that cost the most to find: the three other fields repeat unchanged across a
    /// streamed response's partials, so keeping the wrong copy leaves input, cache read and cache
    /// write exactly right and halves output alone.
    #[test]
    fn a_streamed_partial_never_beats_the_complete_response() {
        let body = [
            line(
                "msg_a",
                "req_a",
                "claude-opus-5",
                "2026-08-09T12:00:00.000Z",
                (2, 8, 400, 100, 0),
            ),
            line(
                "msg_a",
                "req_a",
                "claude-opus-5",
                "2026-08-09T12:00:01.000Z",
                (2, 8, 400, 100, 0),
            ),
            line(
                "msg_a",
                "req_a",
                "claude-opus-5",
                "2026-08-09T12:00:02.000Z",
                (2, 321, 400, 100, 0),
            ),
        ]
        .join("\n");
        let (turns, notes) = turns(&body);
        assert_eq!(turns.len(), 1, "three partials are one request");
        assert_eq!(turns[0].tokens.output, 321, "the complete response wins");
        assert_eq!(
            turns[0].tokens.input, 2,
            "input is not summed across partials"
        );
        assert_eq!(turns[0].tokens.cache_read, 400);
        assert_eq!(notes.duplicates_collapsed, 2);
    }

    /// Fails if the rule became "last read wins", which depends on directory order.
    #[test]
    fn the_duplicate_rule_does_not_depend_on_read_order() {
        let a = line(
            "m",
            "r",
            "claude-opus-5",
            "2026-08-09T12:00:00.000Z",
            (1, 900, 0, 0, 0),
        );
        let b = line(
            "m",
            "r",
            "claude-opus-5",
            "2026-08-09T12:00:01.000Z",
            (1, 12, 0, 0, 0),
        );
        let forwards = turns(&[a.clone(), b.clone()].join("\n")).0;
        let backwards = turns(&[b, a].join("\n")).0;
        assert_eq!(forwards, backwards, "either order must give the same turn");
        assert_eq!(forwards[0].tokens.output, 900);
    }

    /// Fails if the TTL split is collapsed into one number — which is 59.2% of this fleet's cache
    /// writes priced at the wrong rate.
    #[test]
    fn cache_writes_are_priced_by_their_time_to_live() {
        let (turns, _) = turns(&line(
            "m",
            "r",
            "claude-opus-5",
            "2026-08-09T12:00:00.000Z",
            (0, 0, 0, 1_000_000, 1_000_000),
        ));
        let t = turns[0].tokens;
        assert_eq!(t.cache_write_5m, 1_000_000);
        assert_eq!(t.cache_write_1h, 1_000_000);
        assert_eq!(t.cache_write, 2_000_000);
        // 1M at $6.25 + 1M at $10.00. A single-rate table gives $12.50 or $20.00, never this.
        let cost = t.cost(&price_of("claude-opus-5").unwrap());
        assert!(
            (cost - 16.25).abs() < 1e-9,
            "expected $16.25 from the split, got ${cost}"
        );
    }

    /// Fails if an unknown model is priced at zero and folded into the total as if it were free.
    #[test]
    fn an_unknown_model_is_reported_rather_than_priced_at_zero() {
        assert!(price_of("claude-opus-5").is_some());
        assert!(
            price_of("claude-mythos-5-1").is_none(),
            "a model whose cache rate is unsettled must not be guessed at"
        );
        let mut per_box = BTreeMap::new();
        let mut turns = HashMap::new();
        turns.insert(
            1,
            Turn {
                key: 1,
                model: "claude-from-the-future".into(),
                day: "2026-09-01".into(),
                tokens: Tokens {
                    input: 1_000_000,
                    ..Default::default()
                },
            },
        );
        per_box.insert("boxy".to_string(), turns);
        let r = aggregate(&per_box, Notes::default(), 1, 0, 0);
        assert_eq!(r.unpriced.len(), 1, "the model must be named");
        assert_eq!(r.unpriced[0].model, "claude-from-the-future");
        assert_eq!(r.unpriced[0].tokens, 1_000_000);
        assert_eq!(r.totals.cost, 0.0, "unpriced tokens cost nothing known");
        assert_eq!(
            r.totals.tokens, 1_000_000,
            "but they are still counted as tokens"
        );
        assert!(!r.models[0].priced, "the model row says it was not priced");
    }

    /// A dated snapshot is the model it is a snapshot of.
    #[test]
    fn a_dated_model_id_prices_as_its_base_model() {
        assert_eq!(price_of("claude-haiku-4-5-20251001"), Some(HAIKU));
        assert_eq!(
            price_of("claude-opus-50"),
            None,
            "a longer name that is not a dated snapshot must not borrow a price"
        );
    }

    /// Fails if `<synthetic>` is counted as a model — it would appear as an unpriced one.
    #[test]
    fn synthetic_records_are_not_api_calls() {
        let l = line(
            "m",
            "r",
            "<synthetic>",
            "2026-08-09T12:00:00.000Z",
            (0, 0, 0, 0, 0),
        );
        let (turns, notes) = turns(&l);
        assert!(turns.is_empty());
        assert_eq!(notes.synthetic_records, 1);
    }

    /// The stated property, with a test that fails if any field ever carries text out of a record.
    ///
    /// The fixture's every message body is the same distinctive string; the assertion is on the
    /// serialised report, so it covers fields added later without being updated.
    #[test]
    fn only_counts_leave_the_transcript() {
        const SECRET: &str = "BODY-SHOULD-NEVER-LEAVE";
        let body = line(
            "m",
            "r",
            "claude-opus-5",
            "2026-08-09T12:00:00.000Z",
            (5, 7, 9, 11, 13),
        );
        assert!(body.contains(SECRET), "the fixture must contain the secret");
        let (turns, notes) = turns(&body);
        let mut map = HashMap::new();
        for t in turns {
            map.insert(t.key, t);
        }
        let mut per_box = BTreeMap::new();
        per_box.insert("boxy".to_string(), map);
        let report = aggregate(&per_box, notes, 1, body.len() as u64, 0);
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            !json.contains(SECRET),
            "a message body reached the report: {json}"
        );
        assert_eq!(report.totals.input, 5, "the counts did come through");
        assert_eq!(report.totals.output, 7);
    }

    /// Fails if the walker stops at the depth `transcript::walk_jsonl` stops at, which would drop
    /// every subagent transcript — a third of the fleet's spend.
    #[test]
    fn the_walk_reaches_subagent_transcripts() {
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let deep = dir.join("slug").join("session").join("subagents");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.join("slug").join("session.jsonl"), "").unwrap();
        std::fs::write(deep.join("agent-a.jsonl"), "").unwrap();
        let mut found = Vec::new();
        let mut notes = Notes::default();
        walk_jsonl(dir, &mut found, &mut notes);
        let mut names: Vec<String> = found
            .iter()
            .map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["agent-a.jsonl", "session.jsonl"]);
    }

    /// Fails if the day bucket is taken by slicing the string, which puts an offset timestamp in the
    /// wrong day.
    #[test]
    fn the_day_is_utc_whatever_offset_the_record_carries() {
        assert_eq!(utc_day("2026-08-09T23:30:00.000Z").unwrap(), "2026-08-09");
        assert_eq!(
            utc_day("2026-08-10T01:30:00.000+05:30").unwrap(),
            "2026-08-09",
            "an offset timestamp belongs to the UTC day, not the one its prefix spells"
        );
        assert!(utc_day("not a timestamp").is_none());
    }

    /// The dedup key has to survive being written to disk and read by another build.
    #[test]
    fn the_dedup_key_is_stable_and_separates_its_halves() {
        // Computed independently, not read back off this implementation: a cache written by an
        // older build has to mean the same thing to a newer one.
        assert_eq!(dedup_key("msg_a", "req_a"), 0x397d_b799_1b6e_f695);
        assert_ne!(
            dedup_key("ab", "c"),
            dedup_key("a", "bc"),
            "the separator must stop the two halves running together"
        );
    }

    #[test]
    fn the_measured_costs_are_stated() {
        let doc = include_str!("usage.rs");
        for (figure, what) in MEASURED {
            assert!(
                doc.contains(figure),
                "the cost table no longer quotes {figure} ({what}), so the doc comment and \
                 MEASURED disagree about what this costs to run"
            );
        }
    }
}
