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
//! **It reads a directory named by `$SKEIN_USAGE_TRANSCRIPT_ROOT` and never touches `$SKEIN_HOME`.**
//! That is not tidiness. `box_state_root()` is `skein_home()/boxes`, so a test that reached the
//! transcripts the way the product does would have to pin `$SKEIN_HOME` at the fleet the cells were
//! taken from — the owner's live one — and `usage::refresh` writes its cache into `$SKEIN_HOME`.
//! The suite that did that would deposit a 20 MB cache beside sixteen boxes' real state on every
//! `cargo test --all`, which is the shape of SKEIN-626 and SKEIN-685 both. So this test only ever
//! calls the two read-only seams, `usage::cells_for` and `usage::fingerprint`, against a path it
//! was handed.

mod common;

use skein::usage;
use std::collections::BTreeMap;
use std::path::PathBuf;

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
