//! The spend reader, checked against a tally this repository did not produce.
//!
//! Everything in `src/usage.rs`'s own tests is a fixture this module's author wrote, which proves
//! the parse does what its author thought and nothing about whether that is what the transcripts
//! mean. This file is the other half: **the same numbers, from `ccusage`, an independent reader of
//! the same format.** The cells live in `tests/data/usage-oracle.json` — box names, model ids and
//! integers, no path and no message content — so the comparison is reproducible by anyone holding
//! the transcripts, and costs no network.
//!
//! **No box in this file is named.** The fixture is keyed by `usage::box_key`, an FNV-1a digest of
//! the box name, because a real box name in the tree fails `tools/residue-check.py` (SKEIN-629).
//! The test hashes the directory names it finds at run time, so a discrepancy still prints the box
//! the reader got wrong — the name comes from the filesystem, never from the repository.
//!
//! **The comparison reads a directory named by `$SKEIN_USAGE_TRANSCRIPT_ROOT` and never touches
//! `$SKEIN_HOME`.** That is not tidiness. `box_state_root()` is `skein_home()/boxes`, so a test
//! that reached the owner's transcripts the way the product does would have to pin `$SKEIN_HOME` at
//! the fleet the cells were taken from — the live one — and `usage::refresh` writes its cache into
//! `$SKEIN_HOME`. The suite that did that would deposit a 20 MB cache beside sixteen boxes' real
//! state on every `cargo test --all`, which is the shape of SKEIN-626 and SKEIN-685 both. So the
//! comparison only ever calls the two read-only seams, `usage::cells_for` and `usage::fingerprint`,
//! against a path it was handed.
//!
//! # The freshness rule has to be driven through the cache instead (SKEIN-847)
//!
//! The three tests at the end of this file call `usage::refresh` and `usage::report`, which write.
//! They pin `$SKEIN_HOME` at a scratch directory and build the entire fleet inside it — one box,
//! whose name they invent, holding transcripts they wrote — so there is no tension with the
//! paragraph above: what that prohibits is pinning `$SKEIN_HOME` at the owner's fleet, not pinning
//! it at all. `$SKEIN_FLEET_ROOT` is pinned as well, at an empty directory beside it. Nothing in
//! `usage` reads it today — `box_roots` goes through `box_state_root`, which is derived from
//! `$SKEIN_HOME` — and that is precisely why it is pinned rather than left alone: an unpinned
//! coupled variable is invisible only until something inside the library moves one read onto it,
//! and this one defaults to `/boxes`, the live fleet.
//!
//! **They write the stored reading's clock rather than waiting on it.** The rule's interesting
//! cases are equalities against `SystemTime::now()`, and an equality you arrive at by waiting is a
//! test whose answer depends on whether the process happened to cross a second. So these move the
//! timestamp inside `usage.json` — both copies of it, together — and choose ages where one more
//! second cannot change the answer; `src/usage.rs`'s
//! `the_freshness_window_excludes_its_own_boundary` holds the equalities themselves, with no clock
//! in them at all.

mod common;

use common::{env_lock, env_pins, Scratch};
use skein::usage;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ROOT_VAR: &str = "SKEIN_USAGE_TRANSCRIPT_ROOT";

#[derive(serde::Deserialize)]
struct Oracle {
    boxes: BTreeMap<String, OracleBox>,
    /// Boxes whose transcripts provably changed before a fingerprint could be recorded, each with
    /// the arithmetic showing the recorded cells are unreachable from the bytes that are there now.
    /// They still count towards the fleet size, so one cannot be moved here to quiet a failure
    /// without the move appearing in a diff beside its reason.
    excluded: BTreeMap<String, Excluded>,
}

#[derive(serde::Deserialize)]
struct Excluded {
    reason: String,
}

#[derive(serde::Deserialize)]
struct OracleBox {
    /// `(file count, total bytes, newest mtime ns)` when the cells were checked.
    fingerprint: (u64, u64, u64),
    cells: Vec<Cell>,
}

#[derive(serde::Deserialize)]
struct Cell {
    month: String,
    model: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

fn oracle() -> Oracle {
    let raw = include_str!("data/usage-oracle.json");
    serde_json::from_str(raw).expect("the oracle fixture must parse")
}

/// Every box's `(month, model)` token cell must equal what ccusage read from the same bytes.
///
/// **What makes this able to fail**, rather than being a comparison of the reader with itself: the
/// rules in `src/usage.rs` were *found* by this comparison disagreeing, and each one leaves its own
/// signature. Keeping the first copy of a duplicated request instead of the largest leaves input,
/// cache-read and cache-write exactly right and roughly halves `output` alone — that shape, on 26
/// cells, is what it printed when it was deliberately reintroduced. Bucketing the day in a non-UTC
/// zone moves mass between adjacent months while the fleet total stays put.
///
/// Making the walk non-recursive — the change that stops `subagents/` being read — does **not**
/// print a delta, and it is worth knowing why before trusting this test to catch it. A transcript
/// lives at `claude-projects/<slug>/<session>.jsonl`, so a walk that does not recurse finds *no*
/// files at all, every fingerprint reads `(0, 0, 0)`, every box is treated as moved, and the
/// comparison loop runs zero times. It is the `compared > 0` assertion at the end that fails, not
/// any of the per-cell ones. That assertion is not belt-and-braces; it is the only thing standing
/// between a broken walk and a green run.
///
/// A box whose transcripts have moved since the cells were taken is **named and skipped, never
/// tolerated**: transcripts are live files, one was seen being rewritten in place mid-session, and
/// a comparison against bytes that no longer exist would be a failure with nothing behind it. The
/// fingerprint is what tells the two apart, and the count assertion at the end stops a box
/// disappearing quietly into that exemption.
#[test]
fn the_reader_reproduces_an_independent_tally_of_the_same_transcripts() {
    let Some(root) = std::env::var_os(ROOT_VAR).map(PathBuf::from) else {
        common::skip(&format!(
            "{ROOT_VAR} is unset, so the transcripts the oracle was taken from are not here. \
             Set it to the directory holding <box>/claude-projects."
        ));
        return;
    };
    let oracle = oracle();
    let mut compared = 0usize;
    let mut moved: Vec<String> = Vec::new();
    let mut absent: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    // The fixture holds digests; the filesystem holds names. Hashing what is on disk is what joins
    // them, and it is why the messages below can say which box while the committed file cannot.
    let mut present: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                present.insert(usage::box_key(name), name.to_string());
            }
        }
    }

    for (key, want) in &oracle.boxes {
        let Some(name) = present.get(key) else {
            absent.push(key.clone());
            continue;
        };
        let projects = root.join(name).join("claude-projects");
        if !projects.is_dir() {
            absent.push(name.clone());
            continue;
        }
        let now = usage::fingerprint(&projects);
        if now != want.fingerprint {
            moved.push(format!("{name}: was {:?}, now {:?}", want.fingerprint, now));
            continue;
        }
        compared += 1;
        let (got, _notes) = usage::cells_for(&projects);
        let mut seen = BTreeMap::new();
        for cell in &want.cells {
            seen.insert((cell.month.clone(), cell.model.clone()), cell);
            let key = (cell.month.clone(), cell.model.clone());
            let Some(mine) = got.get(&key) else {
                problems.push(format!(
                    "{name} {} {}: the oracle has {} input / {} output / {} cache-read / \
                     {} cache-write and the reader found no records at all",
                    cell.month,
                    cell.model,
                    cell.input,
                    cell.output,
                    cell.cache_read,
                    cell.cache_write
                ));
                continue;
            };
            let want4 = (cell.input, cell.output, cell.cache_read, cell.cache_write);
            let got4 = (mine.input, mine.output, mine.cache_read, mine.cache_write);
            if want4 != got4 {
                problems.push(format!(
                    "{name} {} {}: oracle (in {}, out {}, cache-read {}, cache-write {}) \
                     vs reader (in {}, out {}, cache-read {}, cache-write {}) \
                     — delta (in {}, out {}, cache-read {}, cache-write {})",
                    cell.month,
                    cell.model,
                    want4.0,
                    want4.1,
                    want4.2,
                    want4.3,
                    got4.0,
                    got4.1,
                    got4.2,
                    got4.3,
                    got4.0 as i128 - want4.0 as i128,
                    got4.1 as i128 - want4.1 as i128,
                    got4.2 as i128 - want4.2 as i128,
                    got4.3 as i128 - want4.3 as i128,
                ));
            }
        }
        // A cell the reader invented is as wrong as one it lost, and only this direction catches
        // a model that the parse started counting twice under two names.
        for (key, mine) in &got {
            if !seen.contains_key(key) {
                problems.push(format!(
                    "{name} {} {}: the reader found {} tokens in a cell the oracle does not have",
                    key.0,
                    key.1,
                    mine.total()
                ));
            }
        }
    }

    if !moved.is_empty() {
        eprintln!(
            "usage oracle: {} box(es) not compared, transcripts changed since the cells were \
             taken:\n  {}",
            moved.len(),
            moved.join("\n  ")
        );
    }
    if !absent.is_empty() {
        eprintln!(
            "usage oracle: {} box(es) not present under {ROOT_VAR}: {}",
            absent.len(),
            absent.join(", ")
        );
    }
    assert!(
        problems.is_empty(),
        "the reader disagrees with ccusage on {} cell(s):\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
    assert!(
        compared > 0,
        "every one of the {} boxes in the fixture was skipped, so this test asserted nothing. \
         {} had moved, {} were absent.",
        oracle.boxes.len(),
        moved.len(),
        absent.len()
    );
}

/// The fixture has to keep covering the fleet it was taken from.
///
/// Without this, the way to make the comparison above pass is to delete a box from the fixture, and
/// nothing would say so. Sixteen is what `ccusage` was run against on 2026-09-12.
#[test]
fn the_oracle_fixture_still_covers_the_whole_fleet() {
    let oracle = oracle();
    assert_eq!(
        oracle.boxes.len() + oracle.excluded.len(),
        16,
        "the oracle fixture covered 16 boxes when it was taken"
    );
    assert_eq!(
        oracle.boxes.len(),
        13,
        "13 of those 16 reproduced ccusage exactly. Changing this number is a claim about the \
         reader \u{2014} that it now agrees with more boxes, or fewer \u{2014} and belongs in a \
         commit message rather than in a fixture edit nobody reads."
    );
    let cells: usize = oracle.boxes.values().map(|b| b.cells.len()).sum();
    assert_eq!(
        cells, 36,
        "...across 36 (month, model) cells, which is what those 13 boxes hold"
    );
    for (name, why) in &oracle.excluded {
        assert!(
            why.reason.len() > 200,
            "{name} is excluded from the comparison with a reason too short to be evidence"
        );
    }
    assert!(
        oracle.boxes.values().all(|b| b.fingerprint.0 > 0),
        "a box with no transcripts cannot hold anyone to anything"
    );
}

/// The fixture must stay a table of numbers.
///
/// The module's stated property is that only counts leave a transcript, and committing a fixture is
/// the one place that property could be broken by hand rather than by code — a convenient excerpt
/// of a real conversation pasted in to reproduce a bug would do it, and would look helpful in the
/// diff. So the fixture is checked for the things a transcript carries and a tally does not.
#[test]
fn the_oracle_fixture_carries_no_transcript_content() {
    let raw = include_str!("data/usage-oracle.json");
    for marker in [
        "\"content\"",
        "\"text\"",
        "\"message\"",
        "jsonl",
        "/Users/",
        "/home/",
        "/boxes/",
    ] {
        assert!(
            !raw.contains(marker),
            "the oracle fixture contains {marker}, which belongs to a transcript and not to a \
             table of counts"
        );
    }
}

// ─────────────────────────────── the freshness rule ───────────────────────────────

/// The box this fixture invents. **Not a real box name, and it must never become one** — a real
/// name in the tree is SKEIN-629 and `tools/residue-check.py` is what catches it. The oracle
/// comparison above needs the real names and so carries FNV-1a digests instead; these tests need
/// only *a* box, so they make one up and the question does not arise.
const FIXTURE_BOX: &str = "spend-fixture-box";

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs()
}

/// A scratch `$SKEIN_HOME` with one box's empty project directory inside it.
///
/// Returns the scratch and that directory. The scratch is returned rather than dropped here because
/// it has to outlive the `EnvPins` that names it — every call site binds it first for that reason,
/// so `$SKEIN_HOME` stops pointing at the directory before the directory goes.
fn fixture_home(prefix: &str) -> (Scratch, PathBuf) {
    let home = Scratch::temp(prefix);
    let projects = home
        .path()
        .join("boxes")
        .join(FIXTURE_BOX)
        .join("claude-projects")
        .join("a-project");
    std::fs::create_dir_all(&projects).expect("a fixture project directory");
    (home, projects)
}

/// One assistant record, which is one billable turn. The body is deliberately distinctive: if it
/// ever appears in a report, `only_counts_leave_the_transcript` is what should have caught it.
fn write_transcript(projects: &Path, session: &str, output: u64) {
    let line = serde_json::json!({
        "type": "assistant",
        "timestamp": "2026-09-01T12:00:00.000Z",
        "requestId": format!("req-{session}"),
        "isSidechain": false,
        "message": {
            "id": format!("msg-{session}"),
            "model": "claude-opus-5",
            "content": [{"type": "text", "text": "BODY-SHOULD-NEVER-LEAVE"}],
            "usage": {
                "input_tokens": 10,
                "output_tokens": output,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0
            }
        }
    })
    .to_string();
    std::fs::write(projects.join(format!("{session}.jsonl")), line + "\n")
        .expect("a fixture transcript");
}

/// Move the stored reading's clock, **both copies of it together**.
///
/// `usage.json` carries the read time twice: `read_at_unix`, which `usage::cached` ages against, and
/// `report.read_at`, the RFC3339 string a page renders in its freshness sentence. Writing one and
/// leaving the other is a file no refresh could have produced, and it would leave these tests
/// asserting against the exact defect this repository keeps buying — a status display reporting on
/// something other than what it names.
fn backdate_stored_reading(home: &Path, unix: u64) {
    let path = home.join("usage.json");
    let raw = std::fs::read_to_string(&path).expect("a refresh must have written usage.json");
    let mut stored: serde_json::Value = serde_json::from_str(&raw).expect("usage.json must parse");
    stored["read_at_unix"] = serde_json::json!(unix);
    stored["report"]["read_at"] =
        serde_json::json!(chrono::DateTime::from_timestamp(unix as i64, 0)
            .expect("a representable instant")
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string());
    std::fs::write(&path, stored.to_string()).expect("the stored reading must be writable");
    let back = usage::cached().expect("the stored reading must still load after being backdated");
    assert!(
        back.read_at.starts_with("20"),
        "the backdated file no longer carries a renderable read_at: {}",
        back.read_at
    );
}

/// A reading inside the window is served, and serving it opens no transcript.
///
/// **What makes this able to fail:** a `report` that dropped its cache branch, or a rule inverted
/// so that youth means stale. Either shows up as a second transcript that was written *after* the
/// only reading and is nonetheless counted. That direction matters as much as the SKEIN-847 one:
/// the cost table in `src/usage.rs` says an unnecessary rescan is 5.2 s on a path that exists to
/// cost under a millisecond, so a fix for the boundary that rescans everything would look correct
/// from the cockpit and be the worse bug.
#[test]
fn a_reading_inside_the_window_is_served_from_the_cache() {
    let _env = env_lock();
    let (home, projects) = fixture_home("skein-usage-window-it");
    let mut pins = env_pins();
    pins.set("SKEIN_HOME", home.path())
        .set("SKEIN_FLEET_ROOT", home.path().join("no-fleet-here"));

    write_transcript(&projects, "one", 100);
    let first = usage::refresh().expect("the first count");
    assert_eq!(first.transcripts_read, 1, "one transcript, counted once");

    write_transcript(&projects, "two", 200);
    backdate_stored_reading(home.path(), now_unix() - 1800);
    let served = usage::report(Duration::from_secs(3600)).expect("a report");

    assert_eq!(
        served.transcripts_read, 1,
        "a reading half an hour old was re-read under an hour-long window, so the second \
         transcript was counted when nothing should have been opened at all"
    );
    assert!(
        served.fresh,
        "a reading inside the window is reported fresh"
    );
    assert_eq!(
        served.totals.tokens, 110,
        "the served tally is the stored one, unchanged"
    );
}

/// A reading past the window is re-read, and the new transcript appears.
///
/// **What makes this able to fail:** a rule that serves whatever is stored — the shape a
/// `young_enough` stuck at `true` produces, and the one that would make the hourly ceiling
/// permanent. It is also the test that proves the two above are not both passing because nothing
/// ever rescans.
#[test]
fn a_reading_past_the_window_is_re_read() {
    let _env = env_lock();
    let (home, projects) = fixture_home("skein-usage-stale-it");
    let mut pins = env_pins();
    pins.set("SKEIN_HOME", home.path())
        .set("SKEIN_FLEET_ROOT", home.path().join("no-fleet-here"));

    write_transcript(&projects, "one", 100);
    usage::refresh().expect("the first count");
    write_transcript(&projects, "two", 200);
    let stale_at = now_unix() - 7200;
    backdate_stored_reading(home.path(), stale_at);

    let re_read = usage::report(Duration::from_secs(3600)).expect("a report");
    assert_eq!(
        re_read.transcripts_read, 2,
        "a reading two hours old was served under an hour-long window"
    );
    assert_eq!(
        re_read.totals.tokens, 320,
        "the re-read tally includes the transcript that arrived after the first count"
    );
    assert!(
        re_read.age_secs < 3600,
        "the re-read reading still reports the age of the reading it replaced ({}s), which is \
         the freshness sentence lying about which reading it is showing",
        re_read.age_secs
    );
}

/// No window admits a reading as old as itself — which is SKEIN-847, since a window of nothing
/// therefore admits nothing at all.
///
/// **What makes this able to fail:** restoring `<=` in `usage::report`. That is the defect: a
/// person presses Refresh, presses it again because they did not believe the first, and the second
/// press lands inside the same whole second, where `age_secs == 0` and `0 <= 0` served the reading
/// they were trying to replace.
///
/// **Why a loop over three ages rather than one call with `Duration::ZERO`.** The case is an
/// equality against a clock nobody controls: backdate to exactly `age`, and if the process crosses
/// a second before `report` reads it the age is `age + 1`, which is stale under *either* rule — so
/// a single call would pass on the broken code whenever it was unlucky, and there would be no sign.
/// Three ages make that three independent sub-millisecond escapes, and `at_the_boundary` makes even
/// that loud: a run in which the clock moved under every iteration fails saying it asserted
/// nothing, in the idiom of `compared > 0` above. The equalities themselves are held without any
/// clock at all by `the_freshness_window_excludes_its_own_boundary` in `src/usage.rs`.
#[test]
fn no_window_admits_a_reading_as_old_as_itself() {
    let _env = env_lock();
    let (home, projects) = fixture_home("skein-usage-zero-it");
    let mut pins = env_pins();
    pins.set("SKEIN_HOME", home.path())
        .set("SKEIN_FLEET_ROOT", home.path().join("no-fleet-here"));
    let second = projects.join("two.jsonl");
    let mut at_the_boundary = 0usize;

    for age in [0u64, 1, 2] {
        // Back to one transcript and one reading of it, whatever the previous pass left.
        let _ = std::fs::remove_file(&second);
        write_transcript(&projects, "one", 100);
        let only = usage::refresh().expect("the count this pass starts from");
        assert_eq!(
            only.transcripts_read, 1,
            "the pass for a {age}s window did not start from a single-transcript reading"
        );

        write_transcript(&projects, "two", 200);
        backdate_stored_reading(home.path(), now_unix() - age);
        let observed = usage::cached().expect("the stored reading").age_secs;
        if observed == age {
            at_the_boundary += 1;
        }

        let got = usage::report(Duration::from_secs(age)).expect("a report");
        assert_eq!(
            got.transcripts_read, 2,
            "a window of {age}s served back a reading {observed}s old instead of re-reading: \
             the press a person makes because they did not believe the first one"
        );
        assert_eq!(
            got.totals.tokens, 320,
            "the re-read tally must include the transcript written after the stored reading"
        );
    }

    assert!(
        at_the_boundary > 0,
        "the stored reading aged past the window under all three passes, so none of them put \
         report at the boundary and this test asserted nothing about it"
    );
}
