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

/// One of skein's own call sites, with what it spent (SKEIN-1074).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SiteRow {
    /// `S1` … `S11`, [`crate::ai::Site::code`]. The cockpit keys its words on this.
    pub site: String,
    pub cost: f64,
    pub tokens: u64,
    /// Distinct model calls: a labelled session, or one ledger window of a conversation.
    pub calls: u64,
}

/// What skein's pull-request readings cost, and how many there were to divide it by (SKEIN-1076).
///
/// `cost` and `tokens` are every S1–S5 call in this reading — the narrow fallback and the readings
/// that did not finish included — so dividing by `finished` charges an unfinished reading to the
/// ones that finished, rather than dropping it. The span is the headline total's: everything this
/// reading counted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Readings {
    pub finished: u64,
    pub unfinished: u64,
    pub cost: f64,
    pub tokens: u64,
    /// `cost / finished`, or `None` when no reading finished and there is nothing to divide by.
    pub per_finished: Option<f64>,
    /// `tokens / finished`, rounded, beside it.
    pub tokens_per_finished: Option<u64>,
}

impl Readings {
    /// Divide by the readings that **finished**, never by the attempts: an unfinished reading's
    /// spend is charged to the ones that did, because a cheaper call that has to be made twice is
    /// not cheaper (token-spend.md, the owner's answer 1).
    fn divide(&mut self) {
        let (per, tokens) = match self.finished {
            0 => (None, None),
            n => (
                Some(self.cost / n as f64),
                Some((self.tokens as f64 / n as f64).round() as u64),
            ),
        };
        self.per_finished = per;
        self.tokens_per_finished = tokens;
    }
}

/// skein's own model calls, apart from the boxes' work (SKEIN-1074, SKEIN-1076).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OwnSpend {
    pub cost: f64,
    pub tokens: u64,
    pub calls: u64,
    /// One row per call site that made a call, most expensive first.
    pub sites: Vec<SiteRow>,
    pub readings: Readings,
    // The per-tracker-item figure belongs here, fleet-wide, once SKEIN-1139 gives skein a record of
    // when a box held and finished an item. Nothing stands in for it until then.
    /// A box holds a session skein derived that the ledger does not know: a call made before skein
    /// labelled its own, still counted under that box.
    pub unlabelled_in_boxes: bool,
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
    /// skein's own calls. Their cost is in [`totals`](Self::totals), the months, the models and
    /// the days, and in no box's row.
    #[serde(default)]
    pub own: OwnSpend,
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
    /// The record's timestamp in epoch milliseconds, which is what places a turn of a pull
    /// request's conversation inside one call's ledger window.
    ms: i64,
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

    let stamp = v.get("timestamp").and_then(|t| t.as_str());
    let (day, ms) = match stamp.and_then(utc_day).zip(stamp.and_then(epoch_ms)) {
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
        ms,
        tokens,
    })
}

/// Epoch milliseconds, from an RFC3339 timestamp.
fn epoch_ms(ts: &str) -> Option<i64> {
    Some(
        chrono::DateTime::parse_from_rfc3339(ts)
            .ok()?
            .timestamp_millis(),
    )
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
    /// Each row's timestamp in epoch ms, kept only for a session skein derived — the one kind
    /// whose records are attributed by when they happened (SKEIN-1074). Empty for every other
    /// file, which is nearly all of them.
    #[serde(default)]
    ts: Vec<i64>,
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
    /// The fleet login home's transcripts, by path: where skein's own calls in this process land.
    #[serde(default)]
    own_files: BTreeMap<String, FileDigest>,
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
const CACHE_VERSION: u32 = 2;

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

/// Where skein's own calls in this process leave their transcripts: the fleet login home's
/// `.claude/projects` (SKEIN-1074).
///
/// **Not under any box, which is why these were invisible.** A call spawned here runs with `HOME`
/// at the fleet's login (`ai::tried`, via `fleet::login_home`), so the CLI files it under that
/// home, and [`box_roots`] only ever walked `boxes/*/claude-projects`. Read for skein's own
/// sessions only: anything else in this home is not a box's work and was never counted.
fn own_root() -> PathBuf {
    crate::fleet::fleet_home_dir()
        .join(".claude")
        .join("projects")
}

/// The session a transcript belongs to: the file's own name, or the directory a subagent file sits
/// under — `<slug>/<session>.jsonl` and `<slug>/<session>/subagents/agent-*.jsonl`.
fn session_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let second = rel.components().nth(1)?.as_os_str().to_str()?;
    Some(second.trim_end_matches(".jsonl").to_string())
}

/// Which of skein's call sites a turn was, if it was one of skein's own.
///
/// A labelled session says so in its id. A turn of a pull request's conversation is placed by the
/// ledger window its timestamp falls in, and the window's index keeps two calls in one session
/// apart when they are counted.
fn own_call(
    session: Option<&str>,
    ms: i64,
    ledger: &crate::ai::OwnLedger,
) -> Option<(crate::ai::Site, String)> {
    let session = session?;
    if let Some(site) = crate::ai::site_of_session(session) {
        return Some((site, session.to_string()));
    }
    let (i, site) = ledger.window_at(session, ms)?;
    Some((site, format!("{session}#{i}")))
}

/// One of skein's own turns, and which call it was part of.
#[derive(Debug, Clone)]
struct OwnTurn {
    turn: Turn,
    site: crate::ai::Site,
    call: String,
}

/// Where one transcript's turns go.
enum Root {
    Box(String),
    Own,
}

/// Re-read the fleet, reusing every transcript whose length and mtime have not moved.
///
/// This is the only function that opens a transcript. See the module's cost table before calling it
/// from anything a person is waiting on.
pub fn refresh() -> Result<UsageReport, String> {
    let previous = load_digests().unwrap_or_default();
    let ledger = crate::ai::own_ledger();
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
    // skein's own turns, wherever they ran, deduplicated the same way across all of them.
    let mut own: HashMap<u64, OwnTurn> = HashMap::new();
    let mut unlabelled_in_boxes = false;

    let mut roots: Vec<(Root, PathBuf)> = box_roots()
        .into_iter()
        .map(|(name, projects)| (Root::Box(name), projects))
        .collect();
    roots.push((Root::Own, own_root()));

    for (root, projects) in roots {
        let mut files = Vec::new();
        walk_jsonl(&projects, &mut files, &mut notes);
        let empty = BTreeMap::new();
        let old = match &root {
            Root::Box(name) => previous.files.get(name).unwrap_or(&empty),
            Root::Own => &previous.own_files,
        };
        let mut digests: BTreeMap<String, FileDigest> = BTreeMap::new();

        for (path, len, mtime_ns) in files {
            transcripts += 1;
            bytes += len;
            let session = session_of(&projects, &path);
            let derived = session
                .as_deref()
                .is_some_and(crate::ai::is_derived_session);
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
                        ts: match derived {
                            true => parsed.iter().map(|t| t.ms).collect(),
                            false => Vec::new(),
                        },
                        notes: file_notes,
                    }
                }
            };
            notes.add(&digest.notes);
            for (i, row) in digest.rows.iter().enumerate() {
                let Some(model) = cache.models.get(row.1 as usize) else {
                    continue;
                };
                let Some(day) = cache.days.get(row.2 as usize) else {
                    continue;
                };
                let turn = Turn {
                    key: row.0,
                    model: model.clone(),
                    day: day.clone(),
                    ms: digest.ts.get(i).copied().unwrap_or(0),
                    tokens: Tokens {
                        input: row.3,
                        output: row.4,
                        cache_read: row.5,
                        cache_write: row.6,
                        cache_write_5m: row.7,
                        cache_write_1h: row.8,
                    },
                };
                match (own_call(session.as_deref(), turn.ms, &ledger), &root) {
                    (Some((site, call)), _) => merge_own(&mut own, OwnTurn { turn, site, call }),
                    (None, Root::Box(name)) => {
                        // One of skein's own, from before it labelled them: it stays the box's.
                        unlabelled_in_boxes |= derived;
                        merge_turn(
                            per_box.entry(name.clone()).or_default(),
                            turn,
                            &mut Notes::default(),
                        )
                    }
                    // Not labelled and not in a box: never counted before, and not a box's work.
                    (None, Root::Own) => {}
                }
            }
            digests.insert(key, digest);
        }
        match root {
            Root::Box(name) => {
                per_box.entry(name.clone()).or_default();
                cache.files.insert(name, digests);
            }
            Root::Own => cache.own_files = digests,
        }
    }

    let now = SystemTime::now();
    let read_at_unix = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut report = aggregate(
        &per_box,
        &own.into_values().collect::<Vec<_>>(),
        notes,
        transcripts,
        bytes,
        read_at_unix,
    );
    report.own.readings.finished = ledger.readings_finished;
    report.own.readings.unfinished = ledger.readings_unfinished;
    report.own.readings.divide();
    report.own.unlabelled_in_boxes = unlabelled_in_boxes;
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

fn merge_own(into: &mut HashMap<u64, OwnTurn>, own: OwnTurn) {
    match into.get_mut(&own.turn.key) {
        Some(existing) if own.turn.supersedes(&existing.turn) => *existing = own,
        Some(_) => {}
        None => {
            into.insert(own.turn.key, own);
        }
    }
}

/// Turn the deduplicated turns into the shape that leaves.
///
/// skein's own turns are in every total, month, model and day, and in no box's row: the headline
/// is still the whole bill, and a box is charged only for what it did.
fn aggregate(
    per_box: &BTreeMap<String, HashMap<u64, Turn>>,
    own: &[OwnTurn],
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
    let mut day_cost: BTreeMap<String, f64> = BTreeMap::new();
    let mut boxes: Vec<BoxRow> = Vec::new();

    // Everything a turn contributes that does not depend on whose it was. Returns its cost.
    let mut count = |turn: &Turn, totals: &mut Totals| -> f64 {
        let cost = price_of(&turn.model)
            .map(|p| turn.tokens.cost(&p))
            .unwrap_or(0.0);
        let entry = by_model.entry(turn.model.clone()).or_default();
        entry.0.add(&turn.tokens);
        entry.1 += 1;
        let month = turn.day[..7].to_string();
        by_month.entry(month.clone()).or_default().add(&turn.tokens);
        *by_month_cost.entry(month).or_default() += cost;
        *day_cost.entry(turn.day.clone()).or_default() += cost;
        totals.cost += cost;
        totals.input += turn.tokens.input;
        totals.output += turn.tokens.output;
        totals.cache_read += turn.tokens.cache_read;
        totals.cache_write += turn.tokens.cache_write;
        totals.cache_write_5m += turn.tokens.cache_write_5m;
        totals.cache_write_1h += turn.tokens.cache_write_1h;
        totals.tokens += turn.tokens.total();
        cost
    };

    for (name, turns) in per_box {
        let mut row = BoxRow {
            name: name.clone(),
            ..Default::default()
        };
        let mut days: BTreeMap<String, ()> = BTreeMap::new();
        let mut box_tokens = Tokens::default();
        for turn in turns.values() {
            let cost = count(turn, &mut totals);
            box_tokens.add(&turn.tokens);
            row.cost += cost;
            *row.models.entry(turn.model.clone()).or_default() += cost;
            days.insert(turn.day.clone(), ());
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
        if !turns.is_empty() {
            boxes.push(row);
        }
    }
    boxes.sort_by(|a, b| b.cost.total_cmp(&a.cost));

    let mut mine = OwnSpend::default();
    let mut sites: BTreeMap<crate::ai::Site, (SiteRow, BTreeMap<&str, ()>)> = BTreeMap::new();
    let mut calls: BTreeMap<&str, ()> = BTreeMap::new();
    for o in own {
        let cost = count(&o.turn, &mut totals);
        let tokens = o.turn.tokens.total();
        mine.cost += cost;
        mine.tokens += tokens;
        calls.insert(&o.call, ());
        let (row, site_calls) = sites.entry(o.site).or_insert_with(|| {
            (
                SiteRow {
                    site: o.site.code(),
                    ..Default::default()
                },
                BTreeMap::new(),
            )
        });
        row.cost += cost;
        row.tokens += tokens;
        site_calls.insert(&o.call, ());
        if o.site.is_reading() {
            mine.readings.cost += cost;
            mine.readings.tokens += tokens;
        }
    }
    mine.calls = calls.len() as u64;
    mine.sites = sites
        .into_values()
        .map(|(mut row, site_calls)| {
            row.calls = site_calls.len() as u64;
            row
        })
        .collect();
    mine.sites.sort_by(|a, b| b.cost.total_cmp(&a.cost));

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
    // The day's cost is every turn that day, skein's own included, so it is summed on its own
    // rather than from `by_box` — which names only boxes, and would drop skein's share.
    let daily = day_cost
        .into_iter()
        .map(|(day, cost)| DayRow {
            by_box: by_day.remove(&day).unwrap_or_default(),
            cost,
            day,
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
        own: mine,
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

/// The freshness rule itself: a stored reading is served only while it is **strictly younger** than
/// the window the caller named.
///
/// Its own function, in the idiom of `prq::refresh`'s `unexpired_within`, because it is the
/// smallest statement of the rule and the only piece of it a test can hold without a clock. Every
/// interesting case is an equality, and an equality against `SystemTime::now()` cannot be asserted
/// twice the same way; `the_freshness_window_excludes_its_own_boundary` asserts them all against
/// this.
///
/// **Strict, and that is the whole of SKEIN-847.** Three things follow from it, and the first is
/// the one the fleet noticed:
///
/// * `Duration::ZERO` rescans, always — not by a special case, but because nothing is younger than
///   no time at all. `prq::refresh::queue` has spelled `force` that way since SKEIN-430 and got the
///   comparison right; this module wrote the same idiom with `<=` and so served a reading taken in
///   the same whole second (`age_secs == 0`) back to the one caller that had asked for a guaranteed
///   re-read.
/// * A reading exactly `max_age` old is re-read, which is what makes the hourly ceiling the "older
///   than an hour" rule it claims to be.
/// * A window is therefore the ages it *admits*, and a zero window admits nothing — the reading of
///   `max_age` under which no argument to [`report`] is a trap.
fn young_enough(age: Duration, max_age: Duration) -> bool {
    age < max_age
}

/// The tally, re-read unless a stored one is younger than `max_age`. See [`young_enough`].
///
/// This is the "at most once an hour" rule, and the caller names the hour rather than this module
/// assuming it; nothing else should name a small one, because [`refresh`]'s cost table is the reason
/// this function exists.
///
/// **Asking for a fresh reading is [`refresh`], not `report(Duration::ZERO)`.** Zero does rescan —
/// `no_window_admits_a_reading_as_old_as_itself` in `tests/usage.rs` holds it to that, and until
/// SKEIN-847 it did not — but it says "serve me nothing older than nothing" where the caller means
/// "read the fleet", and the two coincide only as long as the boundary stays strict. `/api/usage`
/// calls [`refresh`] on its asked-for path for that reason.
pub fn report(max_age: Duration) -> Result<UsageReport, String> {
    if let Some(cached) = cached() {
        if young_enough(Duration::from_secs(cached.age_secs), max_age) {
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
                ms: 0,
                tokens: Tokens {
                    input: 1_000_000,
                    ..Default::default()
                },
            },
        );
        per_box.insert("boxy".to_string(), turns);
        let r = aggregate(&per_box, &[], Notes::default(), 1, 0, 0);
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
        let report = aggregate(&per_box, &[], notes, 1, body.len() as u64, 0);
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

    /// The whole of SKEIN-847, stated where it can be asserted without a clock.
    ///
    /// Every case below is an equality, which is why it is here and not against
    /// `SystemTime::now()`: a test that backdates a stored reading to exactly `max_age` and then
    /// calls [`report`] is asserting on whether the process crossed a second between the two, so it
    /// would pass on the broken code most of the time and on the fixed code always — green either
    /// way, which is no test at all. `tests/usage.rs` drives [`report`] end to end at ages where
    /// one extra second cannot change the answer, and this holds the boundary itself.
    ///
    /// The last two lines are the guard against the wrong fix. Special-casing a zero *age* — "a
    /// reading from this second is suspect, rescan" — also makes `report(HOUR)` rescan every time a
    /// page loads in the same second as a refresh, which is [`refresh`]'s 5.2 s on the path that
    /// exists to cost under 1 ms. The window is what is empty, not the reading.
    #[test]
    fn the_freshness_window_excludes_its_own_boundary() {
        const HOUR: Duration = Duration::from_secs(3600);
        assert!(
            !young_enough(Duration::ZERO, Duration::ZERO),
            "a zero window admits nothing, so the reading taken in this same second is not \
             young enough for a caller who named no window at all — SKEIN-847 is `<=` here"
        );
        assert!(
            !young_enough(HOUR, HOUR),
            "a reading exactly an hour old is re-read, which is what makes the hourly ceiling \
             the `older than an hour` rule it claims to be"
        );
        assert!(
            !young_enough(HOUR + Duration::from_secs(1), HOUR),
            "an hour and a second old is stale by any reading of the rule"
        );
        assert!(
            young_enough(HOUR - Duration::from_secs(1), HOUR),
            "a reading 59:59 old is still inside the hour, and a rule that rescans here has \
             stopped being a ceiling at all"
        );
        assert!(
            young_enough(Duration::ZERO, Duration::from_secs(1)),
            "the age zero is not itself suspect: under a window that admits anything, a reading \
             taken this second is the freshest there is"
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

    /// A fixture fleet: `$SKEIN_HOME`, `$SKEIN_FLEET_ROOT` and an ambient `$HOME`, all pinned
    /// inside one temp directory, with a login the fleet can use — so a model call spawned here
    /// runs with `HOME` at the fleet login home, as it does in production.
    fn own_fixture(env: &mut crate::testutil::EnvPins, dir: &Path) -> PathBuf {
        let home = dir.join("home");
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", dir.join("fleet"));
        env.set("HOME", dir.join("bare"));
        let login = home.join("fleet-home/.claude");
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(
            login.join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#,
        )
        .unwrap();
        home
    }

    /// A box's own work: one transcript whose session id the CLI drew, so it is the box's.
    fn box_work(home: &Path, name: &str) {
        let projects = home
            .join("boxes")
            .join(name)
            .join("claude-projects/a-project");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join("0b7f7d0e-3c1a-4d2e-9f00-123456789abc.jsonl"),
            line(
                "box-msg",
                "box-req",
                "claude-opus-5",
                "2026-09-01T10:00:00Z",
                (7, 7, 7, 0, 0),
            ),
        )
        .unwrap();
    }

    /// **skein's own call is counted under its call site and under no box** (SKEIN-1074).
    ///
    /// Spawned, not asserted about: a stub `claude` reads the `--session-id` it was handed and
    /// writes a transcript under that id into `$HOME/.claude/projects`, which is what the real CLI
    /// does, and the usage reader then has to find it. The stub stamps its record from
    /// `$EPOCHREALTIME` rather than `date +%N`: this box's `date` prints nanoseconds without their
    /// leading zeros, so `.049` came out as `.49413299` and landed outside the call's window. A one-shot (S1) and a turn of a pull
    /// request's conversation (S3) both go through it, because they are labelled two different
    /// ways — the one by its id, the other by its window in the ledger.
    ///
    /// Fails if the fleet login home is not scanned (S1 and S3 both vanish), if the one-shot's id
    /// stops carrying its site, if a conversation record from before the call is attributed to it,
    /// or if skein's calls are charged to a box.
    #[cfg(unix)]
    #[test]
    fn skeins_own_call_is_counted_under_its_call_site_and_under_no_box() {
        use crate::ai::Site;
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir: &Path = dir.as_ref();
        let mut env = crate::testutil::env_pins();
        let home = own_fixture(&mut env, dir);
        box_work(&home, "thing");

        let bin = dir.join("claude-that-files-a-transcript");
        std::fs::write(
            &bin,
            r#"#!/usr/bin/env bash
cat > /dev/null
while [ $# -gt 0 ]; do case "$1" in --session-id|--resume) id=$2; shift;; esac; shift; done
mkdir -p "$HOME/.claude/projects/-stub"
s=${EPOCHREALTIME%.*}; f=${EPOCHREALTIME#*.}
now=$(date -u -d "@$s" +%Y-%m-%dT%H:%M:%S).${f:0:3}Z
printf '{"type":"assistant","timestamp":"%s","requestId":"req-%s","message":{"id":"msg-%s","model":"claude-haiku-4-5","usage":{"input_tokens":1000,"output_tokens":100}}}\n' "$now" "$id" "$id" >> "$HOME/.claude/projects/-stub/$id.jsonl"
echo "$id"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        env.set("SKEIN_CLAUDE_BIN", &bin);

        // A conversation already on disk from before skein labelled its calls: one record, long
        // ago, which no ledger window covers.
        let conversation = crate::ai::conversation_for("acme", 41);
        let filed = home.join("fleet-home/.claude/projects/-stub");
        std::fs::create_dir_all(&filed).unwrap();
        std::fs::write(
            filed.join(format!("{conversation}.jsonl")),
            format!(
                "{}\n",
                line(
                    "old-msg",
                    "old-req",
                    "claude-sonnet-5",
                    "2026-01-01T10:00:00Z",
                    (5_000, 0, 0, 0, 0)
                )
            ),
        )
        .unwrap();

        crate::ai::forget_refusal();
        let said = crate::ai::as_site(Site::Summary, || {
            crate::ai::claude_oneshot_telling(
                "x",
                Some("claude-haiku-4-5"),
                Duration::from_secs(30),
            )
        })
        .expect("the stub answers");
        assert_eq!(
            crate::ai::site_of_session(&said),
            Some(Site::Summary),
            "the one-shot's session id does not carry its call site: {said}"
        );
        let turned = crate::ai::as_site(Site::Review, || {
            crate::ai::claude_in_turn(
                "x",
                Some("claude-sonnet-5"),
                Duration::from_secs(30),
                crate::ai::Turn::Opening {
                    id: &conversation,
                    at: dir,
                },
                None,
                crate::ai::Machine::Wherever,
            )
        })
        .expect("the stub answers");
        assert_eq!(turned.said, conversation);

        let r = refresh().expect("the fixture fleet reads");
        let site = |code: &str| r.own.sites.iter().find(|s| s.site == code).cloned();
        let summary = site("S1").expect("the one-shot is not under S1");
        assert_eq!((summary.tokens, summary.calls), (1_100, 1));
        let review = site("S3").expect("the conversation turn is not under S3");
        assert_eq!(
            (review.tokens, review.calls),
            (1_100, 1),
            "the conversation's record from before the call was attributed to it"
        );
        assert_eq!(r.own.tokens, 2_200);
        assert_eq!(r.own.calls, 2);
        assert_eq!(
            r.boxes
                .iter()
                .map(|b| (b.name.as_str(), b.tokens))
                .collect::<Vec<_>>(),
            vec![("thing", 21)],
            "skein's own calls were charged to a box, or the box lost its own work"
        );
        assert_eq!(
            r.totals.tokens,
            21 + 2_200,
            "the headline total is no longer the whole bill"
        );
        assert!(!r.own.unlabelled_in_boxes);
    }

    /// **A conversation turn that ran in a box leaves the box by its ledger window, and nothing
    /// else of the box goes with it** (SKEIN-1074).
    ///
    /// Fails if windows are ignored (the S4 turn stays the box's), if a record outside every window
    /// is taken from the box, or if the box's older skein session is not said to be counted there.
    #[test]
    fn a_turn_in_a_box_leaves_the_box_by_its_window_and_nothing_else_does() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir: &Path = dir.as_ref();
        let mut env = crate::testutil::env_pins();
        let home = own_fixture(&mut env, dir);
        box_work(&home, "thing");
        let conversation = crate::ai::conversation_for("acme", 7);
        let projects = home.join("boxes/thing/claude-projects/-work");
        std::fs::create_dir_all(&projects).unwrap();
        let during = "2026-09-02T10:00:05.000Z";
        let before = "2026-09-02T09:00:00.000Z";
        std::fs::write(
            projects.join(format!("{conversation}.jsonl")),
            [
                line("in", "in", "claude-sonnet-5", during, (300, 0, 0, 0, 0)),
                line("out", "out", "claude-sonnet-5", before, (40, 0, 0, 0, 0)),
            ]
            .join("\n"),
        )
        .unwrap();
        let from = epoch_ms("2026-09-02T10:00:00Z").unwrap();
        std::fs::write(
            crate::ai::own_ledger_path(),
            format!(
                "{}\n",
                serde_json::json!({"site": "S4", "session": conversation, "from": from, "to": from + 10_000})
            ),
        )
        .unwrap();

        let r = refresh().expect("the fixture fleet reads");
        assert_eq!(
            r.own
                .sites
                .iter()
                .map(|s| (s.site.as_str(), s.tokens))
                .collect::<Vec<_>>(),
            vec![("S4", 300)]
        );
        assert_eq!(
            r.boxes.iter().map(|b| b.tokens).collect::<Vec<_>>(),
            vec![21 + 40],
            "the box kept the sweep's turn, or lost the record no window covers"
        );
        assert!(
            r.own.unlabelled_in_boxes,
            "a box holds a skein session from before it was labelled, and the report does not say so"
        );
    }
}
