//! Which tests do not run on this platform, and why — said out loud rather than left to be noticed.
//!
//! Some of what skein tests is a **shell script it installs into a box**, and a box is Linux by
//! construction: it is a `bwrap` namespace inside a Docker sandbox. Those scripts are written for a
//! GNU userland on purpose — `sed -i` with no argument, `readlink -f`, `sort -z`,
//! `tar --ignore-failed-read`, `/proc/<pid>/stat` — and every one of those spellings is the correct
//! one where the script actually runs. On a Mac they fail, and the failure says nothing about the
//! thing under test.
//!
//! So those tests carry `#[cfg(target_os = "linux")]`. The danger in that is obvious and is the same
//! danger this repository has already been bitten by twice in one week: **a check that quietly does
//! not run reads exactly like a check that passed.** The browser suites went unrun for 160 commits
//! and a blank cockpit shipped; the warden's reduced builds were red for as long as nobody ran them.
//!
//! Two things stop the same thing happening here.
//!
//! **The gated set is declared**, and a test that gets gated without being written down fails the
//! build. So gating one is a line in a diff rather than a quiet subtraction.
//!
//! **On a platform where they are skipped, the skip has a test of its own** — one whose NAME is the
//! notice, because `cargo test` prints names and hides the stdout of anything that passed. Somebody
//! running the suite on a Mac reads that name in the list and knows what they have and have not
//! just proved. It is
//! `box_side_tests_do_not_run_on_this_platform_because_a_box_is_linux`, spelled out in full
//! because spelling it in full is the entire point: this note gave three words less of it until
//! SKEIN-610, and a notice whose reader searches the output for a string that is never printed is
//! not a notice.

mod common;

use common::{LIB, REQUIREMENTS};
use std::path::Path;

/// Every test that only runs on Linux, with the reason it cannot run anywhere else.
///
/// The reason is here rather than only at the test because this is the list somebody reads when they
/// are deciding whether a green run on their machine means what they want it to mean.
const GATED: &[(&str, &str)] = &[
    (
        "a_box_that_cannot_be_entered_says_which_proof_failed",
        "runs the crossing guard, whose subject is /proc/<pid>/stat and the kernel's boot id",
    ),
    (
        "a_stop_never_sweeps_the_namespace_it_is_running_in",
        "reads this process's start time from /proc/self/stat — a box's identity IS that triple",
    ),
    (
        "an_unstamped_anchor_is_refused_even_where_the_boot_id_cannot_be_read",
        "reads /proc/self/stat for a start time and /proc/<pid>/ns/mnt for a namespace, and plants \
         a `cat` on PATH so the kernel's boot id comes back empty — all three are Linux's",
    ),
    (
        "a_sudo_it_cannot_shim_is_left_alone_rather_than_breaking_the_box",
        "runs a block of box-session.sh, which uses `readlink -f` (GNU)",
    ),
    (
        "the_sweep_verifies_the_anchor_and_falls_back_only_when_it_cannot",
        "proves an anchor against /proc/<pid>",
    ),
    (
        "mailbox_turn_boundary_delivery_round_trip",
        "runs the mailbox scripts skein installs into a box",
    ),
    (
        "a_registry_key_named_after_the_sandbox_is_not_a_recipient",
        "runs mailbox.sh's own prune against a real registry, and ages a message with GNU `touch -d`",
    ),
    (
        "a_legacy_box_named_by_its_vm_is_still_a_recipient",
        "the same, in the legacy world the script's identity chain branches on",
    ),
    (
        "box_token_usage_sums_new_assistant_entries_and_is_idempotent",
        "drives box-token-usage.sh in the userland it is installed into",
    ),
    (
        "codex_status_line_setup_defaults_without_overriding_user_choice",
        "runs Codex's interactive_setup, which uses `sed -i` with no argument (GNU)",
    ),
    (
        "codex_statusline_uses_default_quota_when_named_pool_arrives_last",
        "reads the config that same `sed -i` writes",
    ),
    (
        "one_live_login_heals_every_box_whose_own_is_dead",
        "runs the reconciler's own script — GNU-shaped, and the fleet it repairs is Linux",
    ),
    (
        "healing_does_nothing_when_there_is_nothing_to_heal",
        "the same script, in the two cases where it must write nothing at all",
    ),
    (
        "a_copy_the_fleet_has_moved_past_is_replaced_even_though_it_claims_to_be_alive",
        "the same script again, on the fleet state that only rotation produces",
    ),
    (
        "a_tracker_install_that_hangs_does_not_hold_up_the_box",
        "runs the tail of skein-startup.sh, whose bound is `timeout` (GNU coreutils)",
    ),
    (
        "a_box_is_ready_only_for_the_start_it_is_on",
        "runs box_ready_script against a real tmux server on a unix socket the test creates — the \
         script is what a box is asked, and a box is Linux by construction",
    ),
    (
        "shared_home_import_is_dry_run_first_explicit_and_filtered",
        "the inventory and import shells use `sort -z` and `tar --ignore-failed-read` (GNU)",
    ),
    (
        "no_probe_files_a_signal_under_the_sandboxs_name",
        "runs all eleven scripts skein installs into a box, mailbox.sh and sandbox-bootstrap.sh \
         included — `flock` and `date -u -d` are GNU, and a box is Linux by construction",
    ),
];

/// Every `.rs` file under `dir`, at any depth, sorted.
///
/// The whole tree, not one level of it: a module that became a directory (`src/fleet/`,
/// `src/prq/`) keeps its tests in the files beneath it, and a flat read of `src/` drops every one
/// of them out of the gates in this file while they go on passing.
fn rust_files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for path in std::fs::read_dir(&dir)
            .expect("src is readable")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
        {
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Every `#[cfg(target_os = "linux")] #[test]` in the library, by name.
fn gated_in_source() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let files = rust_files_under(&src);
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.trim() != r#"#[cfg(target_os = "linux")]"# {
                continue;
            }
            // The attribute sits above `#[test]`, which sits above the function. Anything else with
            // this attribute is not a test and is not this file's business.
            if lines.get(i + 1).map(|l| l.trim()) != Some("#[test]") {
                continue;
            }
            if let Some(decl) = lines.get(i + 2) {
                if let Some(rest) = decl.trim().strip_prefix("fn ") {
                    if let Some(name) = rest.split('(').next() {
                        found.push(name.to_string());
                    }
                }
            }
        }
    }
    found.sort();
    found
}

/// The gated set in the source is the gated set written down here.
///
/// Both directions. An undeclared gate is a test somebody removed from half the world without saying
/// so; a declared one that no longer exists is a warning about nothing, which is how a list stops
/// being read.
#[test]
fn every_platform_gated_test_is_declared_with_its_reason() {
    let found = gated_in_source();
    let mut declared: Vec<String> = GATED.iter().map(|(name, _)| name.to_string()).collect();
    declared.sort();

    let undeclared: Vec<&String> = found.iter().filter(|n| !declared.contains(n)).collect();
    assert!(
        undeclared.is_empty(),
        "these tests are gated to Linux and are not in GATED, so on any other platform they \
         disappear with nothing to say they have: {undeclared:?}\n\
         Add each with the reason it cannot run elsewhere — the reason is the point, since it is \
         what tells somebody whether a green run on their machine means what they want."
    );
    let stale: Vec<&String> = declared.iter().filter(|n| !found.contains(n)).collect();
    assert!(
        stale.is_empty(),
        "GATED names tests that are no longer gated (or no longer exist): {stale:?}"
    );
    for (name, why) in GATED {
        assert!(!why.trim().is_empty(), "{name} is declared with no reason");
    }
}

/// On Linux everything runs, and this says so where the skip notice would otherwise be.
#[cfg(target_os = "linux")]
#[test]
fn box_side_tests_all_run_on_this_platform() {
    assert!(
        !GATED.is_empty(),
        "nothing is gated any more, so this pair of tests has nothing left to report — delete them \
         rather than leaving a notice about an empty set"
    );
}

/// The notice. Its **name** is the message, because `cargo test` hides the output of a passing test.
///
/// Not a failure: demanding Linux to run the suite would be a worse answer than saying what was
/// skipped. Not `#[ignore]` either — that prints `ignored` with no reason attached to it.
#[cfg(not(target_os = "linux"))]
#[test]
fn box_side_tests_do_not_run_on_this_platform_because_a_box_is_linux() {
    eprintln!(
        "\n{} test(s) did not run here. They exercise scripts skein installs INTO a box, and a box \
         is a bwrap namespace inside a Linux sandbox — the GNU spellings they use are correct where \
         they actually run:\n",
        GATED.len()
    );
    for (name, why) in GATED {
        eprintln!("  {name}\n      {why}");
    }
    eprintln!("\nRun `cargo test` inside a box for the whole suite.\n");
}

// ---------------------------------------------------------------------------------------------
// The other kind of gate: a machine without a tool this suite drives
// ---------------------------------------------------------------------------------------------

/// Every `tests/*.rs`, as (binary name, source).
///
/// **And every `tests/<name>/main.rs`, as the one binary cargo builds from it**, with the source of
/// every `.rs` file in that directory joined — main.rs first — because a guard in a sibling module
/// is a guard of that binary (SKEIN-1109/1110 split `fleet_launch` and `isolation_bwrap` that way).
/// A directory without a `main.rs` is not a binary: `tests/common/` is a module every binary
/// declares, and is read where it is declared rather than here.
fn integration_sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/ is readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs") || p.join("main.rs").is_file())
        .collect();
    files.sort();
    for file in files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        if file.is_dir() {
            let mut parts: Vec<_> = std::fs::read_dir(&file)
                .expect("a split test binary's directory is readable")
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "rs"))
                .collect();
            parts.sort_by_key(|p| (p.file_name().is_none_or(|n| n != "main.rs"), p.clone()));
            let joined = parts
                .iter()
                .map(|p| std::fs::read_to_string(p).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n");
            out.push((name, joined));
            continue;
        }
        out.push((name, std::fs::read_to_string(&file).unwrap_or_default()));
    }
    out
}

/// A test binary whose skip is not about a tool on this machine's PATH.
///
/// The same bargain `GATED` and `UNREFUSABLE` make, for a third kind of gate, and it needs a list of
/// its own because `common::REQUIREMENTS` cannot hold it. That list is a list of TOOLS: it is read
/// with `have(tool)`, which is `command -v`, and clause 3 below derives against the `have("…")` call
/// sites in each file. A binary that skips for want of an environment variable naming a corpus has
/// nothing to put in that list which would not be a lie about what `command -v` would find.
///
/// Checked in four directions by `every_environment_gated_binary_is_declared_and_still_gated`, so an
/// entry cannot outlive its guard, name a file that is gone, name a binary that is also in
/// `REQUIREMENTS`, or name a variable the file has stopped reading.
const ENV_GATED: &[(&str, &str, &str)] = &[(
    "usage",
    "SKEIN_USAGE_TRANSCRIPT_ROOT",
    "the oracle it reproduces was taken from a real transcript corpus, which is neither committable \
     nor installable — the variable points at the directory holding <box>/claude-projects. Nothing \
     is on PATH to look for, so `have()` cannot ask this question and REQUIREMENTS cannot answer it",
)];

/// The `tests/*.rs` binaries `tools/noskip-check.py` declares environmentally gated, read out of it.
///
/// `ENV_GATED` above would otherwise be a second hand-kept list of the same fact, and this repo has
/// already paid for one of those: the gate list lived in `ci.yml` and in an unchecked local runner
/// and the two drifted in both directions before anybody looked (`tools/gates.sh`'s own header). So
/// the membership of `ENV_GATED` is derived from the list that already exists rather than asserted
/// beside it — `ENVIRONMENTAL` in `tools/noskip-check.py`, whose entry for `tests/usage.rs` says the
/// same thing in the same words: `$SKEIN_USAGE_TRANSCRIPT_ROOT` is "not a tool and cannot be
/// installed on a runner". That table is keyed by GUARD and this file needs BINARIES, which is the
/// only reason there are two shapes of it at all.
///
/// **It refuses rather than returning nothing.** A table it cannot find, or finds empty, would make
/// the cross-check below vacuous in the exact way `every_binary_that_skips_declares_what_this_machine_needs`
/// was — so it panics naming what it could not read.
fn noskip_environmental_binaries() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/noskip-check.py");
    let text = std::fs::read_to_string(&path).expect("tools/noskip-check.py is readable");
    let start = text
        .find("\nENVIRONMENTAL = [")
        .expect("tools/noskip-check.py no longer holds an `ENVIRONMENTAL = [` table this can read");
    let rest = &text[start..];
    let end = rest[1..].find("\n]").expect(
        "the `ENVIRONMENTAL` table in tools/noskip-check.py does not end at a `]` on its own line",
    );
    let block = &rest[..end];

    let mut out: Vec<String> = Vec::new();
    for (i, _) in block.match_indices("\"tests/") {
        let s = &block[i + 1..];
        let quoted = &s[..s.find('"').expect("a closing quote")];
        // `tests/<name>.rs`, or any file of a `tests/<name>/` binary — see `integration_sources`.
        if let Some(stem) = quoted
            .strip_prefix("tests/")
            .and_then(|n| n.strip_suffix(".rs"))
            .map(|n| n.split('/').next().unwrap_or(n))
        {
            if !out.iter().any(|x| x == stem) {
                out.push(stem.to_string());
            }
        }
    }
    assert!(
        !out.is_empty(),
        "the `ENVIRONMENTAL` table in tools/noskip-check.py names no tests/*.rs binary, so either \
         its shape changed or this reader has stopped recognising it — and the cross-check below is \
         then green about two lists it cannot compare"
    );
    out.sort();
    out
}

/// The two lists of environmental gates name the same binaries.
///
/// `ENV_GATED` here and `ENVIRONMENTAL` in `tools/noskip-check.py` are the same claim seen from two
/// sides: one says "this binary skips on something that is not a tool", the other says "this guard
/// may refuse on a machine that has every tool its binary declares". A binary in the second and not
/// the first is one this file would demand a requirement for that does not exist; one in the first
/// and not the second is a skip the no-skip run would fail the build over.
///
/// **What makes it fail:** deleting the `tests/usage.rs` entry from `ENVIRONMENTAL`, or adding an
/// entry there for a `tests/*.rs` that is in neither `REQUIREMENTS` nor `ENV_GATED`. Proved by doing
/// the first.
#[test]
fn the_two_lists_of_environmental_gates_name_the_same_binaries() {
    let declared: Vec<&str> = REQUIREMENTS.iter().map(|(name, _)| *name).collect();
    let mut want: Vec<String> = noskip_environmental_binaries()
        .into_iter()
        .filter(|name| !declared.contains(&name.as_str()))
        .collect();
    want.sort();
    let mut have: Vec<String> = ENV_GATED
        .iter()
        .map(|(name, _, _)| name.to_string())
        .collect();
    have.sort();
    assert_eq!(
        have, want,
        "ENV_GATED in this file and the `ENVIRONMENTAL` table in tools/noskip-check.py disagree \
         about which binaries skip on something common::REQUIREMENTS cannot express. Whichever is \
         right, two lists saying different things is the state that made the gate list drift."
    );
}

/// Every line of a `tests/*.rs` that SKIPS — the statement, not a mention of one.
///
/// **What this replaces, and why a literal was the wrong shape (SKEIN-897).** The needle here was
/// the literal `return skip(`, and `tests/usage.rs:116` spells the same thing as `common::skip(…);`
/// followed by `return;` on the next line. So clause 1 below had never seen that binary: it skips,
/// it declares nothing, and the check that exists to make exactly that impossible agreed with a
/// subset of the truth for as long as the subset was all it could see. It was found by the
/// `noskip-check` gate (SKEIN-881) on that gate's first real run, not by this file.
///
/// Counted before it was changed, over every `skip(` in `tests/*.rs` outside this file — 72
/// occurrences, of which `return skip(` matched 65. Of the seven it missed, three are `.skip(n)` on
/// an iterator and are not skips at all, two are `tests/harness.rs` calling `common::skip` inside a
/// `catch_unwind` as the SUBJECT of a test rather than to skip one, and one is the real site. One
/// binary, `usage`, was invisible to the check as a result.
///
/// So the rule is about the shape of the CALL rather than about one spelling of it: the text before
/// `skip(` must be nothing, or a `::` path, after an optional leading `return`. That accepts every
/// spelling the tree uses — `return skip(`, `return common::skip(`, `crate::testutil::skip(`, and a
/// bare `common::skip(` statement — and it rejects the three near-misses that matter, each of which
/// is a real thing in this tree rather than a hypothetical:
///
///   * `.skip(1)` — an iterator, preceded by a `.`.
///   * `let quiet = std::panic::catch_unwind(|| common::skip("…"));` — a real call to `skip`, and
///     `tests/harness.rs` does not skip: it is the test OF the skip mechanism, and the call is an
///     argument rather than a statement. A needle that only asked "is `skip` called here" would
///     demand a requirement of it, and clause 2 would then demand that requirement be non-empty —
///     so the check would have forced somebody to write down a need that binary does not have.
///   * `// return skip(…)` and `assert!(src.contains("return skip("))` — a mention is not an
///     instance. Seven findings in this repository in one week came from counting those as calls.
///
/// `the_skip_scanner_reads_every_spelling_that_actually_skips` holds all of that to fixtures, so the
/// rule cannot quietly stop recognising one of them.
fn skip_sites(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        let stmt = code.strip_prefix("return ").unwrap_or(code);
        let Some(at) = stmt.find("skip(") else {
            continue;
        };
        let path = &stmt[..at];
        let is_path = path.is_empty()
            || (path.ends_with("::")
                && path
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':'));
        if is_path {
            out.push((i + 1, code.to_string()));
        }
    }
    out
}

/// The scanner reads every spelling that skips, and nothing that merely mentions one.
///
/// Both halves are fixtures taken from real lines in this tree rather than invented, because the
/// near-misses are what the rule is for and an invented one proves nothing about the code.
///
/// **What makes it fail:** narrowing `skip_sites` back to the literal `return skip(` drops three of
/// the accepted lines; widening it to "is `skip` called anywhere on this line" accepts the
/// `catch_unwind` line and the two mentions. Proved by doing both.
#[test]
fn the_skip_scanner_reads_every_spelling_that_actually_skips() {
    let skips = [
        r#"        return skip("jq is not installed");"#,
        r#"        return common::skip("jq is not installed");"#,
        r#"        return crate::testutil::skip("no bwrap here");"#,
        r#"        common::skip(&format!("{ROOT_VAR} is unset"));"#,
        r#"    skip("why");"#,
        r#"        return skip("#,
    ];
    for line in skips {
        assert_eq!(
            skip_sites(line).len(),
            1,
            "this line skips a test and the scanner does not see it, so a binary spelling its \
             guard this way declares nothing and nothing notices: {line}"
        );
    }

    let not_skips = [
        r#"        .skip(1)"#,
        r#"        .skip(from.saturating_sub(1))"#,
        r#"        let quiet = std::panic::catch_unwind(|| common::skip("a tool"));"#,
        r#"        let n = rest.skip_while(|x| *x).count();"#,
        r#"        // return skip("this one is a comment")"#,
        r#"/// return common::skip("jq is not installed");"#,
        r#"        assert!(src.contains("return skip("));"#,
    ];
    for line in not_skips {
        assert!(
            skip_sites(line).is_empty(),
            "the scanner read this as a test skipping, which it is not — a mention is not an \
             instance, and a binary flagged for one is asked to write down a need it does not \
             have: {line}"
        );
    }
}

/// The environment-gated list is still describing the tree, in every direction it can rot in.
///
/// **What makes it fail:** deleting the guard from `tests/usage.rs` and leaving the entry; renaming
/// `SKEIN_USAGE_TRANSCRIPT_ROOT` on one side only; adding `usage` to `common::REQUIREMENTS` as well,
/// which would leave two lists each believing they own it. Proved by doing all three.
#[test]
fn every_environment_gated_binary_is_declared_and_still_gated() {
    let sources = integration_sources();
    let declared: Vec<&str> = REQUIREMENTS.iter().map(|(name, _)| *name).collect();
    for (name, var, why) in ENV_GATED {
        assert!(
            !why.trim().is_empty(),
            "tests/{name}.rs is environment-gated with no reason"
        );
        assert!(
            !declared.contains(name),
            "tests/{name}.rs is in ENV_GATED and in common::REQUIREMENTS, so two lists claim it and \
             neither is the one to correct — it belongs in exactly one"
        );
        let Some((_, src)) = sources.iter().find(|(n, _)| n == name) else {
            panic!("ENV_GATED names tests/{name}.rs, which does not exist");
        };
        assert!(
            !skip_sites(src).is_empty(),
            "ENV_GATED says tests/{name}.rs skips for want of ${var}, and nothing in it skips any \
             more — either the guard was lost or the entry is stale"
        );
        assert!(
            src.contains(var),
            "ENV_GATED says tests/{name}.rs gates on ${var} and that name does not appear in the \
             file, so this entry describes a guard that has moved or gone"
        );
    }
}

/// If a comment or a literal begins at `b[i]`, the index just past it, and whether it is a comment.
///
/// A LIFETIME is not a token here — `'a` has no closing quote — so the caller steps over that
/// quote as one ordinary byte rather than hunting for a close that does not exist. Deliberately
/// the same rule as `tools/rustcut.py::skip_token`, which is what `tools/prose-check.py` reads
/// Rust through; the crate boundary is why there are two and not one, the same bargain
/// `common::bwrap_works` and `testutil::bwrap_works` already make.
fn token_end(b: &[u8], i: usize) -> Option<(usize, bool)> {
    if b[i..].starts_with(b"//") {
        let end = b[i..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(b.len(), |p| i + p);
        return Some((end, true));
    }
    if b[i..].starts_with(b"/*") {
        let (mut depth, mut j) = (1usize, i + 2);
        while j < b.len() && depth > 0 {
            if b[j..].starts_with(b"/*") {
                depth += 1;
                j += 2;
            } else if b[j..].starts_with(b"*/") {
                depth -= 1;
                j += 2;
            } else {
                j += 1;
            }
        }
        return Some((j, true));
    }
    let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
    // `r"…"`, `r#"…"#`, `br##"…"##` — matched before the ordinary string branch so that a `//`
    // inside one is not read as the start of a comment.
    if boundary && (b[i] == b'r' || (b[i] == b'b' && b.get(i + 1) == Some(&b'r'))) {
        let mut j = if b[i] == b'b' { i + 2 } else { i + 1 };
        let opened = j;
        while b.get(j) == Some(&b'#') {
            j += 1;
        }
        let hashes = j - opened;
        if b.get(j) == Some(&b'"') {
            j += 1;
            while j < b.len() {
                if b[j] == b'"'
                    && b[j + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|&&c| c == b'#')
                        .count()
                        == hashes
                {
                    return Some((j + 1 + hashes, false));
                }
                j += 1;
            }
            return Some((b.len(), false));
        }
    }
    if b[i] == b'"' || (boundary && b[i] == b'b' && b.get(i + 1) == Some(&b'"')) {
        let mut j = if b[i] == b'b' { i + 2 } else { i + 1 };
        while j < b.len() {
            if b[j] == b'\\' {
                j += 2;
                continue;
            }
            if b[j] == b'"' {
                return Some((j + 1, false));
            }
            j += 1;
        }
        return Some((b.len(), false));
    }
    // `'x'`, `'\n'`, `'\u{1f600}'`. The quote inside `'"'` is the hazard this branch is for: a
    // cutter that reads it as opening a string swallows the `//` that follows and keeps a whole
    // comment as code.
    if b[i] == b'\'' {
        let mut j = i + 1;
        if b.get(j) == Some(&b'\\') {
            j += 1;
            while j < b.len() && b[j] != b'\'' && b[j] != b'\n' {
                j += 1;
            }
        } else {
            j += 1;
            while j < b.len() && (b[j] & 0xC0) == 0x80 {
                j += 1;
            }
        }
        if b.get(j) == Some(&b'\'') {
            return Some((j + 1, false));
        }
    }
    None
}

/// `src` with every comment removed and every literal left intact.
///
/// **The bug this ends, which is the one this repository keeps paying for: a mention is not an
/// instance** (SKEIN-908). Clause 3 below decided whether a binary calls a capability probe by
/// asking whether its file's TEXT contains the probe's name — so a module doc that merely explained
/// what `bwrap_works()` is made the gate demand `bwrap` in a binary that needs no namespace at all.
/// It was hit for real by `tests/browser_suites.rs`, and the fix taken at the time was to reword the
/// comment, which is prose bent around a scanner: the sentence then says something slightly other
/// than what its author meant, and nothing records why.
///
/// `tools/prose-check.py` had already solved this for itself — it cuts comments with
/// `tools/rustcut.py` before deciding which symbols the tree still has, for exactly this reason.
/// This is that, in Rust.
///
/// Literals are KEPT, and that is not an oversight: the tool name in `have("jq")` is itself a string
/// literal, so a cutter that dropped literals would blind the clause it exists to sharpen. The
/// consequence is that a needle inside a string still counts, which is why this file excludes itself
/// from the scan — see the note in `every_binary_that_skips_declares_what_this_machine_needs`.
fn code_only(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        match token_end(b, i) {
            // Keep the newlines a comment spanned, so line numbers downstream still line up.
            Some((end, true)) => {
                out.extend(b[i..end].iter().filter(|&&c| c == b'\n'));
                i = end;
            }
            Some((end, false)) => {
                out.extend_from_slice(&b[i..end]);
                i = end;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out)
        .expect("only whole comments are dropped, and they end on char boundaries")
}

/// The scanner reads a comment as prose and a literal as code, in both directions.
///
/// Every fixture is a line this tree actually contains or a hazard it actually holds, and the
/// quotes in it are UNBALANCED where that is what discriminates — because the first draft of
/// `tools/rustcut.py` had a fixture whose brace hazards all cancelled and deleting its whole string
/// branch left the self-check green. **This test made that mistake once on the way in**: the
/// raw-string case was at first only `r#"// have("jq") …"#`, whose quotes pair up either way, so
/// deleting the raw-string branch left it green. The case that discriminates is `r#"say "hi"#`,
/// whose single interior quote shifts every pairing after it.
///
/// **What makes it fail:** deleting the string branch of `token_end` reads the `//` inside the
/// fourth line's string literal as the start of a comment and cuts the rest of that literal away;
/// deleting the raw-string branch reads the sixth line's `//` as
/// the start of a comment's worth of string and leaves its `have("jq")` standing as code; deleting
/// the char-literal branch keeps the eighth line's comment IN, because the quote inside `'"'` is
/// then read as opening a string. Proved by doing all three.
#[test]
fn the_code_scanner_reads_a_comment_as_prose_and_a_literal_as_code() {
    // (line, needle, is it code?)
    let cases: &[(&str, &str, bool)] = &[
        (
            "//! `chromium` means `common::chromium_ready()`",
            "chromium_ready()",
            false,
        ),
        (
            "/// the same way `bwrap` means `bwrap_works()`",
            "bwrap_works()",
            false,
        ),
        (
            "    let n = 1; /* have(\"du\") */ let m = 2;",
            "have(\"du\")",
            false,
        ),
        (
            "    let s = \"a//b\"; // have(\"git\")",
            "have(\"git\")",
            false,
        ),
        ("    let s = \"a//b\"; // have(\"git\")", "a//b", true),
        // A raw string whose ONE interior quote shifts the pairing of every quote after it, so
        // reading it as an ordinary string swallows the `//` below and keeps the comment as code.
        (
            "    let r = r#\"say \"hi\"#; // have(\"jq\")",
            "have(\"jq\")",
            false,
        ),
        (
            "    let r = r#\"// have(\"jq\") in a raw string\"#;",
            "have(\"jq\")",
            true,
        ),
        (
            "    let q = '\"'; // have(\"tmux\")",
            "have(\"tmux\")",
            false,
        ),
        ("    if !have(\"jq\") {", "have(\"jq\")", true),
        ("    if !bwrap_works() {", "bwrap_works()", true),
    ];
    for (line, needle, is_code) in cases {
        let cut = code_only(line);
        assert_eq!(
            cut.contains(needle),
            *is_code,
            "code_only read `{needle}` in `{line}` as {}, and it is {} — a scanner that cannot tell \
             a comment from code either demands a requirement nobody has, or misses a guard that is \
             really there. It produced: {cut:?}",
            if *is_code { "prose" } else { "code" },
            if *is_code { "code" } else { "prose" }
        );
    }
}

/// Every `common::Tool` as the SOURCE declares it — `(constant, tool name, its probe function)`.
///
/// Read from the text because the one thing the compiled constant cannot hand a test is the NAME of
/// the function in `probe`: a `fn() -> bool` is a pointer at runtime, and the clauses below need the
/// spelling to find a call to it in a test file. Everything else about the declaration is checked
/// against what the compiler saw, by `capability_probes`, so a reader that has drifted fails rather
/// than reporting a smaller suite than exists.
///
/// **A `Tool` it cannot read is a panic, not a skip.** Falling back to "no probe" is exactly the
/// silence SKEIN-915 is about.
fn tool_declarations(text: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("pub const ") {
        let rest = &text[at + "pub const ".len()..];
        let decl = &rest[..rest.find(';').unwrap_or(rest.len())];
        let Some((konst, after)) = decl.split_once(':') else {
            continue;
        };
        let Some((ty, value)) = after.split_once('=') else {
            continue;
        };
        if ty.trim() != "Tool" {
            continue;
        }
        let konst = konst.trim().to_string();
        let body = value
            .trim()
            .strip_prefix("Tool")
            .map(str::trim)
            .and_then(|v| v.strip_prefix('{'))
            .and_then(|v| v.strip_suffix('}'))
            .unwrap_or_else(|| {
                panic!(
                    "tests/common/mod.rs declares the tool `{konst}` in a shape this reader cannot \
                     read — it expects `Tool {{ name: \"…\", probe: … }}`, and read: {value:?}"
                )
            });
        let raw = field(body, "name", &konst);
        let name = raw
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or_else(|| {
                panic!(
                    "tests/common/mod.rs declares `{konst}` with a name that is not a plain string \
                     literal — read: {raw:?}"
                )
            });
        let probe = field(body, "probe", &konst);
        let probe = if probe.starts_with("None") {
            None
        } else if let Some(some) = probe.strip_prefix("Some(") {
            Some(
                some.split(')')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            )
        } else {
            panic!(
                "tests/common/mod.rs declares `{konst}` with a probe that is neither `None` nor \
                 `Some(<function>)` — read: {probe:?}. A probe this cannot read must not be taken \
                 for an absent one, which would put `{name}` back on `command -v` in silence"
            )
        };
        out.push((konst, name.to_string(), probe));
    }
    out
}

/// One `key: …` of a struct literal, from the colon to the end of the value.
fn field<'a>(body: &'a str, key: &str, konst: &str) -> &'a str {
    let needle = format!("{key}:");
    let at = body.find(&needle).unwrap_or_else(|| {
        panic!("tests/common/mod.rs declares the tool `{konst}` with no `{needle}` field")
    });
    let rest = body[at + needle.len()..].trim_start();
    &rest[..rest.find(',').unwrap_or(rest.len())]
}

/// The suite's capability probes, as `(function, the declared tool it answers for)`.
///
/// **Derived from the DECLARATION, which is what SKEIN-915 changed.** A capability used to be a name
/// in `REQUIREMENTS` with a nullary `pub fn <tool>_<verb>() -> bool` beside it, and this function
/// rediscovered the pair by matching those names. That rule reads what is present and can say
/// nothing about what is absent: delete `chromium_ready` and `chromium` is a name like `jq`, probed
/// with `command -v`, which answers 127 where Playwright's browser lives — the SKEIN-899 defect, back
/// with nothing to go red. `common::Tool` carries the probe as a `fn() -> bool` instead, so deleting
/// one does not compile, and the `<tool>_<verb>` convention is gone from both gates: this reads the
/// `probe:` field, and so does `tools/noskip-check.py::capabilities`.
///
/// **What is read from the text is checked against what the compiler saw.** The text is read for one
/// thing only — the probe's spelling, which a `fn` pointer does not carry — and every other part of
/// it (which tools exist, which of them have a probe) is asserted equal to `REQUIREMENTS` itself. A
/// reader that has stopped matching therefore fails HERE, naming the drift, instead of returning
/// nothing and leaving the clauses below green about a suite they cannot see.
fn capability_probes() -> Vec<(String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/common/mod.rs");
    let text = code_only(&std::fs::read_to_string(&path).expect("tests/common/mod.rs is readable"));
    let declarations = tool_declarations(&text);

    // What the COMPILER sees: every tool any binary requires, and whether it carries a probe. One
    // tool declared twice with two answers is a fault of its own — `have("bwrap")` in one binary and
    // `bwrap_works()` in another is the 27-day CI hole (SKEIN-549) in a single list.
    let mut compiled: Vec<(&str, bool)> = Vec::new();
    for (binary, tools) in REQUIREMENTS {
        for tool in *tools {
            match compiled.iter().find(|(name, _)| *name == tool.name) {
                Some((_, probed)) => assert_eq!(
                    *probed,
                    tool.probe.is_some(),
                    "common::REQUIREMENTS declares `{}` for tests/{binary}.rs with a probe and \
                     elsewhere without one, so one of the two asks a different question from the \
                     guards",
                    tool.name
                ),
                None => compiled.push((tool.name, tool.probe.is_some())),
            }
        }
    }
    compiled.sort();

    let mut read: Vec<(&str, bool)> = declarations
        .iter()
        .map(|(_, name, probe)| (name.as_str(), probe.is_some()))
        .collect();
    read.sort();
    assert_eq!(
        read, compiled,
        "the `common::Tool` declarations this read out of tests/common/mod.rs are not the ones the \
         compiler put in common::REQUIREMENTS. Either a declared tool is required by no binary, or \
         this reader has drifted from the shape of the declaration — in which case a capability it \
         cannot see goes back to being probed with `command -v`, silently, which is what SKEIN-915 \
         closed. Read: {read:?}, compiled: {compiled:?}"
    );

    let nullary: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("pub fn "))
        .filter_map(|rest| rest.split_once('('))
        .filter(|(_, rest)| rest.replace(' ', "").starts_with(")->bool"))
        .map(|(name, _)| name)
        .collect();
    let probes: Vec<(String, String)> = declarations
        .into_iter()
        .filter_map(|(konst, name, probe)| probe.map(|probe| (konst, name, probe)))
        .map(|(konst, name, probe)| {
            assert!(
                nullary.contains(&probe.as_str()),
                "common::{konst} says `{name}` is answered by `{probe}()`, and tests/common/mod.rs \
                 has no `pub fn {probe}() -> bool`. The compiler would have refused that, so what \
                 has drifted is this reader — and a probe it cannot find is a capability it would \
                 report to nobody"
            );
            (probe, name)
        })
        .collect();
    assert!(
        !probes.is_empty(),
        "common::REQUIREMENTS carries no capability probe at all, so either `bwrap` and `chromium` \
         were both reduced to PATH lookups — which is the SKEIN-899 defect twice over — or this \
         reader cannot see them"
    );
    probes
}

/// What a machine needs to run this suite is written down, and it still matches the code.
///
/// The failure this closes is the one `cargo test` is built to hide: a guard returns early, the test
/// passes, and cargo captures the notice because captured output is what a PASSING test gets. Forty
/// such guards existed across twelve files, eighteen of them silent, and the tools they wanted were
/// listed in no file at all — so `cargo test` on a Mac printed 143 green lines having proved a
/// fraction of them, with no way for the reader to tell which.
///
/// **Grepping a run's log is not the check**, for exactly that reason. Two things are, and this is
/// the first: the list exists, and it is derived against rather than trusted. The second is
/// `$SKEIN_TESTS_NO_SKIP`, which turns every `common::skip` into a panic — a green run under it is a
/// run in which nothing was skipped, and it needs nobody to read any output at all.
///
/// **What makes it fail:** planting `common::skip("…"); return;` in a binary that declares nothing.
/// Proved by doing it in `tests/board_cost.rs` — and by running the same planted code against the
/// version of this file before SKEIN-897, which was green about it, because its needle was the
/// literal `return skip(` and that is not how `tests/usage.rs` spells the same thing. See
/// `skip_sites` for what was counted and what the rule is now.
#[test]
fn every_binary_that_skips_declares_what_this_machine_needs() {
    // This file is the scanner, and every needle below is a literal in it — so scanning itself
    // reports itself. Excluded, and excluded by asking the compiler which file this is rather than
    // by writing the name down: the name written down is itself a mention, and `tools/prose-check.py`
    // reads every non-comment string in `tests/` when it decides which symbols the tree still has.
    // (Spelling the needles in halves instead is what the first three runs of this test did, and it
    // made them unreadable.) There are no guards here to miss: a platform gate in this file is a
    // `#[cfg]`, which the test above covers.
    let me = Path::new(file!())
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let sources: Vec<(String, String)> = integration_sources()
        .into_iter()
        .filter(|(name, _)| *name != me)
        .collect();
    let declared: Vec<&str> = REQUIREMENTS.iter().map(|(name, _)| *name).collect();
    let env_gated: Vec<&str> = ENV_GATED.iter().map(|(name, _, _)| *name).collect();
    let skipping: Vec<&String> = sources
        .iter()
        .filter(|(_, src)| !skip_sites(src).is_empty())
        .map(|(name, _)| name)
        .collect();

    // 0. The scanner still recognises how this suite spells a skip. Clause 1 rests entirely on it
    //    and is VACUOUS without it — a needle that matches nothing reports no undeclared binary,
    //    because it can see no skipping binary at all, and reads exactly like a clean tree. Clause
    //    2 would catch that afterwards, one declared binary at a time, in the language of a lost
    //    guard; this says it once, in the language of the actual fault. The floor is derived from
    //    the list rather than written down, so it cannot drift away from it.
    let want = REQUIREMENTS.iter().filter(|(name, _)| *name != LIB).count();
    assert!(
        skipping.len() >= want,
        "the scanner found {} binaries in tests/ that skip, and common::REQUIREMENTS alone declares \
         {want} — so it has stopped recognising how a skip is spelled, and clause 1 below is now \
         green about a suite it cannot see. It found: {skipping:?}",
        skipping.len()
    );

    // 1. A binary that can skip is a binary with a requirement, and it has to be written down —
    //    in REQUIREMENTS if what it wants is a tool, in ENV_GATED above if it is not.
    let undeclared: Vec<&&String> = skipping
        .iter()
        .filter(|name| !declared.contains(&name.as_str()) && !env_gated.contains(&name.as_str()))
        .collect();
    assert!(
        undeclared.is_empty(),
        "these binaries skip tests and are in neither common::REQUIREMENTS nor ENV_GATED in this \
         file, so what they need is written down nowhere: {undeclared:?}"
    );

    // 2. And the other direction, or the list rots into a description of a suite that has moved on.
    for (name, tools) in REQUIREMENTS {
        // The library is not a `tests/*.rs` and has no source here to find. Its entry is held to
        // the same two directions by `the_library_binary_declares_what_this_machine_needs` below.
        if *name == LIB {
            continue;
        }
        let Some((_, src)) = sources.iter().find(|(n, _)| n == name) else {
            panic!("common::REQUIREMENTS names tests/{name}.rs, which does not exist");
        };
        let named: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert!(
            !skip_sites(src).is_empty(),
            "common::REQUIREMENTS says tests/{name}.rs needs {named:?}, but nothing in it skips — \
             either the guard was lost or the entry is stale"
        );
        assert!(
            !tools.is_empty(),
            "tests/{name}.rs is declared needing nothing"
        );
    }

    // 3. Every tool a file actually gates on is in that file's list. One-directional on purpose:
    //    `have("x")` and a capability probe both NAME their tool, while `real_git()` and a bare
    //    `Command::new("python3")` do not, so those are declared and this cannot check them.
    //
    //    **Over the CODE, not over the text** (SKEIN-908). The capability half used to ask whether
    //    the file's raw bytes contained `bwrap_works()`, so a module doc that merely explained the
    //    probe made this demand `bwrap` of a binary that needs no namespace — which is how
    //    `tests/browser_suites.rs` came to have a sentence written around a scanner instead of
    //    around its subject. `code_only` is the cut, and the probes are derived rather than named
    //    here, so `chromium_ready()` is covered by the same clause that covers `bwrap_works()`
    //    without a second line that says so.
    let probes = capability_probes();
    for (name, raw) in &sources {
        let src = code_only(raw);
        let tools: Vec<&str> = REQUIREMENTS
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.iter().map(|t| t.name).collect())
            .unwrap_or_default();
        let gate = "have(\"";
        for (i, _) in src.match_indices(gate) {
            let rest = &src[i + gate.len()..];
            let tool = &rest[..rest.find('"').expect("a closing quote")];
            assert!(
                tools.contains(&tool),
                "tests/{name}.rs gates on `{tool}` and common::REQUIREMENTS does not list it, so a \
                 machine without it skips silently as far as anybody reading that list is concerned"
            );
        }
        for (probe, tool) in &probes {
            if src.contains(&format!("{probe}()")) {
                assert!(
                    tools.contains(&tool.as_str()),
                    "tests/{name}.rs calls `{probe}()`, which is this suite's probe for `{tool}`, \
                     and common::REQUIREMENTS does not list `{tool}` for it — so a machine without \
                     that capability skips silently as far as anybody reading that list is concerned"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The third kind of gate: the library's own tests, which for a long time nothing here could see
// ---------------------------------------------------------------------------------------------
//
// `$SKEIN_TESTS_NO_SKIP` and `REQUIREMENTS` both grew up around `tests/`, and `common` is
// `tests/common/mod.rs` — compiled into integration binaries, unreachable from the crate's own
// `#[cfg(test)]` tests. So the `cargo test --lib` binary, the largest test surface in the tree, was
// outside both: fifteen of its tests skipped through a bare `eprintln!` and an early `return`, which
// the switch cannot refuse and which cargo hides because a skipped test PASSES. A run that asked for
// no skips got fifteen and was told nothing (SKEIN-790).
//
// Fixing those fifteen without this section would have fixed fifteen instances of a class. What
// follows is the part that stops a sixteenth: the shape is refused in the source rather than
// counted, so adding one the old way fails a test instead of passing quietly.

/// A library skip the gates below cannot demand be refusable, and why.
///
/// The same bargain `GATED` above makes, for the same reason: an exception that is written down is a
/// line in a diff, and one that is not is a quiet subtraction. Checked in both directions by
/// `the_library_skip_exemptions_are_still_true`, so an entry that stops being needed fails this file
/// rather than sitting here describing a tree that has moved on.
const UNREFUSABLE: &[(&str, &str, &str)] = &[(
    "fleet/create.rs",
    "creating_a_fleet_is_asked_of_the_warden_and_never_run_here",
    "NOT a skip at all: the `return` is a stub server thread leaving its accept loop when the \
         listener is gone. It is here because the scanner reads `return` and cannot read intent",
)];

/// One `#[test]` in the library: where it is, and the text of its body.
struct LibTest {
    file: String,
    name: String,
    line: usize,
    body: Vec<String>,
}

/// Every `#[test]` in `src/`, with its body — spans found by INDENT, not by counting braces.
///
/// Brace counting is the obvious way and it does not work on this tree. `src/fleet/` embeds whole
/// shell scripts in string literals, and a `{` inside one is indistinguishable from a block to any
/// counter that does not also tokenise Rust's strings, raw strings, char literals and comments — a
/// counter that tried it here ran one span over 11,000 lines and reported every later test's
/// contents as belonging to `the_host_and_the_launcher_agree_on_what_a_login_is`.
///
/// rustfmt gives a cheaper invariant instead: a `fn` at indentation N closes with a `}` at
/// indentation N, and nothing nested inside it is ever at that indentation. That is an assumption,
/// so it is checked rather than trusted — see `the_library_test_scanner_reads_whole_bodies`.
fn lib_tests() -> Vec<LibTest> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files_under(&src);

    let mut found = Vec::new();
    for file in files {
        let name = file
            .strip_prefix(&src)
            .expect("a file found under src")
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            if lines[i].trim() != "#[test]" {
                i += 1;
                continue;
            }
            // `#[test]` may sit among other attributes; walk past them to the signature.
            let mut decl = i + 1;
            while decl < lines.len() && lines[decl].trim_start().starts_with("#[") {
                decl += 1;
            }
            let Some(sig) = lines
                .get(decl)
                .filter(|l| l.trim_start().starts_with("fn "))
            else {
                i += 1;
                continue;
            };
            let indent = sig.len() - sig.trim_start().len();
            let close = format!("{}}}", " ".repeat(indent));
            let mut end = decl + 1;
            while end < lines.len() && lines[end] != close {
                end += 1;
            }
            found.push(LibTest {
                file: name.clone(),
                name: sig.trim_start()[3..]
                    .split('(')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                line: decl + 1,
                body: lines[decl + 1..end.min(lines.len())]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            });
            i = end + 1;
        }
    }
    found
}

/// Is `skip` *called* anywhere in these lines — as opposed to `.skip(2)` or `.skip_while(…)`?
///
/// Both spellings the tree uses are accepted, `crate::testutil::skip(` and a bare `skip(` under a
/// glob import, by asking only that the character before it is not a `.` and not part of a longer
/// identifier.
fn calls_skip(body: &[String]) -> bool {
    body.iter().any(|line| {
        line.match_indices("skip(").any(|(at, _)| {
            at == 0
                || !matches!(line.as_bytes()[at - 1], b'.' | b'_')
                    && !line.as_bytes()[at - 1].is_ascii_alphanumeric()
        })
    })
}

/// Does this line start a `print`/`eprint` macro whose text mentions skipping?
///
/// The macro call may be wrapped over several lines, so the whole call is joined before it is read —
/// which is what the first attempt at this got wrong, matching only the sites whose message fitted
/// on one line and reporting the multi-line ones as already converted.
fn bare_skip_notices(body: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, line) in body.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        if !(t.starts_with("eprintln!") || t.starts_with("println!")) {
            continue;
        }
        let mut end = i;
        while end < body.len() && !body[end].trim_end().ends_with(");") {
            end += 1;
        }
        let call = body[i..=end.min(body.len() - 1)].join(" ");
        if call.to_lowercase().contains("skip") {
            out.push(i);
        }
    }
    out
}

/// Every bare `return` in these lines, by index. Trailing comments are stripped first.
///
/// That stripping is not a nicety. `src/diff.rs` skips with `return; // git not available in this
/// environment`, and a scan that matched only the exact text `return;` reported it as clean — the
/// one site in the sweep whose whole reason lives in a comment, missed by the check written to find
/// exactly that.
fn bare_returns(body: &[String]) -> Vec<usize> {
    body.iter()
        .enumerate()
        .filter(|(_, line)| {
            let t = line.trim();
            if t.starts_with("//") {
                return false;
            }
            let code = t.split("//").next().unwrap_or("").trim();
            code == "return;" || code == "return"
        })
        .map(|(i, _)| i)
        .collect()
}

/// The scanner reads whole test bodies, which every gate below rests on.
///
/// **What makes it fail:** a span that runs past its own test's end swallows the next `#[test]`,
/// which is exactly what brace counting did here. Replacing the indent rule with a brace counter
/// fails this with a four-figure list.
#[test]
fn the_library_test_scanner_reads_whole_bodies() {
    let tests = lib_tests();
    assert!(
        tests.len() > 500,
        "the scanner found {} tests in src/, which is far too few — it has stopped recognising \
         `#[test]`, and every gate below it is then green about a tree it cannot see",
        tests.len()
    );
    let swallowed: Vec<String> = tests
        .iter()
        .filter(|t| t.body.iter().any(|l| l.trim() == "#[test]"))
        .map(|t| format!("{}:{} {}", t.file, t.line, t.name))
        .collect();
    assert!(
        swallowed.is_empty(),
        "these spans ran past the end of their own test and swallowed the next one, so what the \
         gates below read as one test's body is really several: {swallowed:?}"
    );
}

/// No library test announces a skip the no-skip switch cannot refuse.
///
/// This is the gate that makes the fix durable. The fifteen sites SKEIN-790 converted all had the
/// same shape — `eprintln!("skipping: …"); return;` — and a sixteenth written that way tomorrow
/// fails HERE rather than passing quietly under `SKEIN_TESTS_NO_SKIP=1`.
///
/// **What makes it fail:** turning any `crate::testutil::skip("…")` in `src/` back into an
/// `eprintln!` that says "skipping". Proved by doing it.
#[test]
fn no_library_test_announces_a_skip_the_switch_cannot_refuse() {
    let mut bare = Vec::new();
    for t in lib_tests() {
        if UNREFUSABLE
            .iter()
            .any(|(f, n, _)| *f == t.file && *n == t.name)
        {
            continue;
        }
        for i in bare_skip_notices(&t.body) {
            bare.push(format!(
                "src/{} :{} in {} — {}",
                t.file,
                t.line + i + 1,
                t.name,
                t.body[i].trim()
            ));
        }
    }
    assert!(
        bare.is_empty(),
        "these library tests say they are skipping by printing it, which `$SKEIN_TESTS_NO_SKIP` \
         cannot refuse and `cargo test` hides — a skipped test PASSES, so the notice is invisible \
         in exactly the run that asked for no skips:\n  {}\n\nUse `crate::testutil::skip(\"why\")`, \
         which prints the same reason on an ordinary run and panics under the variable.",
        bare.join("\n  ")
    );
}

/// A library test that returns early does it through `skip`, so the reason survives the run.
///
/// The broader half of the pair. The shape above is the one that has actually recurred, but the
/// sweep behind SKEIN-790 also turned up skips with no message at all — a bare `return` under a
/// guard, where not even a reader watching the output learns the test did nothing. Those cannot be
/// found by grepping for "skip", which is why this asks the opposite question: an early return in a
/// test body is a claim that the test need not run, and a claim like that has to be sayable.
///
/// **What makes it fail:** deleting the `crate::testutil::skip(…)` line from any converted guard and
/// leaving its `return`. Proved by doing it.
#[test]
fn every_library_test_that_returns_early_says_why_through_skip() {
    let mut silent = Vec::new();
    for t in lib_tests() {
        if UNREFUSABLE
            .iter()
            .any(|(f, n, _)| *f == t.file && *n == t.name)
        {
            continue;
        }
        if bare_returns(&t.body).is_empty() || calls_skip(&t.body) {
            continue;
        }
        silent.push(format!("src/{} :{} in {}", t.file, t.line, t.name));
    }
    assert!(
        silent.is_empty(),
        "these library tests return early without going through `crate::testutil::skip`, so on a \
         machine that takes the guard they report success having proved nothing, and say nothing a \
         reader or `$SKEIN_TESTS_NO_SKIP` could notice:\n  {}\n\nIf the return is not a skip, add \
         it to UNREFUSABLE in this file with the reason.",
        silent.join("\n  ")
    );
}

/// The exemptions are still true — the other direction, or the list rots into folklore.
///
/// An entry that names a test which no longer exists, or one that has since been converted, is a
/// hole in the two gates above that nobody is watching. Failing here is how it gets closed.
///
/// The question it asks is exactly the one the two gates above would ask if the entry were gone:
/// would either of them flag this test? Asking anything looser is how an entry outlives its reason.
/// It did: the first spelling accepted any bare early return, so the entry for a test since
/// CONVERTED — `skip("…"); return;`, where the `return` is still bare and both gates are already
/// satisfied — read as still true. Measured against SKEIN-825's four, that spelling missed the two
/// that were converted, `place.rs` and `diff.rs`, and caught only the two whose guard went away
/// entirely; the two entries it missed could have sat here for ever describing finished work.
///
/// **What makes it fail:** re-adding any of those four entries beside its finished guard. Proved by
/// doing it, entry by entry, and by re-running the four against the old spelling to learn which two
/// it let through.
#[test]
fn the_library_skip_exemptions_are_still_true() {
    let tests = lib_tests();
    for (file, name, why) in UNREFUSABLE {
        assert!(
            !why.trim().is_empty(),
            "{file}::{name} is exempt with no reason"
        );
        let Some(t) = tests.iter().find(|t| t.file == *file && t.name == *name) else {
            panic!(
                "UNREFUSABLE names src/{file}::{name}, which no longer exists — delete the entry, \
                 or correct it if the test was renamed"
            );
        };
        let would_be_flagged = !bare_skip_notices(&t.body).is_empty()
            || (!bare_returns(&t.body).is_empty() && !calls_skip(&t.body));
        assert!(
            would_be_flagged,
            "src/{file}::{name} is exempted from the library skip gates and no longer needs to be — \
             neither gate above would say anything about it with this entry deleted, which is what \
             deleting it is for"
        );
    }
}

/// The library binary declares what this machine needs, and still skips.
///
/// Both directions, the same bargain `every_binary_that_skips_declares_what_this_machine_needs`
/// makes for `tests/*.rs`: a surface that can skip has to say what it wants, and an entry that
/// claims a surface skips has to be true. The library was in neither direction until SKEIN-790.
///
/// **What makes it fail:** removing the `LIB` entry from `common::REQUIREMENTS`. Proved by doing it.
#[test]
fn the_library_binary_declares_what_this_machine_needs() {
    let tools: Vec<&str> = REQUIREMENTS
        .iter()
        .find(|(n, _)| *n == LIB)
        .map(|(_, t)| t.iter().map(|t| t.name).collect())
        .unwrap_or_else(|| {
            panic!(
                "common::REQUIREMENTS does not name `{LIB}`, so what the `cargo test --lib` binary \
                 needs is written down nowhere — which is the state SKEIN-790 found it in"
            )
        });
    assert!(!tools.is_empty(), "the library is declared needing nothing");

    let tests = lib_tests();
    assert!(
        tests.iter().any(|t| calls_skip(&t.body)),
        "common::REQUIREMENTS says the library needs {tools:?}, but nothing in src/ skips any more \
         — either the guards were lost or the entry is stale"
    );
    // Derivable, so derived — the same one-directional check clause 3 makes, through the same two
    // helpers and for the same two reasons. A capability probe NAMES its tool while a bare
    // `Command::new("jq")` does not; and the needle is read over `code_only`, because a comment in a
    // library test that merely explains a probe is a mention and not a call (SKEIN-908).
    let bodies: Vec<String> = tests
        .iter()
        .map(|t| code_only(&t.body.join("\n")))
        .collect();
    for (probe, tool) in capability_probes() {
        let called = bodies.iter().any(|b| b.contains(&format!("{probe}()")));
        assert!(
            !called || tools.contains(&tool.as_str()),
            "a library test calls `{probe}()`, this suite's probe for `{tool}`, and \
             common::REQUIREMENTS does not declare `{tool}` for the library binary"
        );
    }
}

/// The library and the suite name the SAME variable, which is the whole value of the switch.
///
/// There are two `skip` implementations — `tests/common/mod.rs` for the integration binaries and
/// `src/testutil.rs` for the library — and they cannot be one, because the second is `#[cfg(test)]`
/// inside the crate and no integration binary can reach it. Sharing would mean making test
/// scaffolding `pub` in the shipped library. So the duplication stays and the part that could
/// silently diverge is checked instead: a rename on one side leaves `SKEIN_TESTS_NO_SKIP=1` still
/// looking like it covers the tree while covering half of it, and nothing would go red.
///
/// **What makes it fail:** renaming the constant's value in `src/testutil.rs` alone. Proved by doing
/// it.
#[test]
fn the_library_and_the_suite_ask_for_no_skips_with_the_same_variable() {
    let testutil = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testutil.rs");
    let text = std::fs::read_to_string(&testutil).expect("src/testutil.rs is readable");
    assert!(
        text.contains(&format!("\"{}\"", common::NO_SKIP)),
        "src/testutil.rs does not name `{}` anywhere, so the library's skips answer to a different \
         variable than the suite's — or to none. One `SKEIN_TESTS_NO_SKIP=1` has to mean `no skips \
         anywhere`, or a green run under it is a claim about a fraction of the tree",
        common::NO_SKIP
    );
    // And that it is still WIRED, not merely mentioned. `skip` deliberately does not read the
    // variable in the code path its own test drives — writing it from a test would let one thread
    // silence another thread's guard mid-run, which is the defect, not a way to test it — so this
    // one line is covered here or nowhere.
    assert!(
        text.contains("var_os(NO_SKIP)"),
        "src/testutil.rs names {} but never reads it, so every library skip is unrefusable again \
         and a run under the variable would be green having skipped whatever it skipped",
        common::NO_SKIP
    );
}

/// What each job in `.github/workflows/ci.yml` installs with `apt-get install`, by job name.
///
/// Read as text rather than parsed as YAML, which this crate has no parser for: a job is a
/// two-space-indented `name:` under `jobs:`, and a package is a word after `apt-get install` up to
/// the end of that command.
fn ci_apt_packages() -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let ci = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"),
    )
    .expect("the CI workflow");
    let mut out = std::collections::BTreeMap::new();
    let mut job: Option<String> = None;
    let mut in_jobs = false;
    for line in ci.lines() {
        if line.starts_with("jobs:") {
            in_jobs = true;
            continue;
        }
        if in_jobs && line.starts_with("  ") && !line.starts_with("   ") {
            let name = line.trim().trim_end_matches(':');
            if line.trim().ends_with(':') && !name.contains(' ') {
                job = Some(name.to_string());
                continue;
            }
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        let (Some(job), Some(rest)) = (&job, line.split("apt-get install").nth(1)) else {
            continue;
        };
        let packages: &mut std::collections::BTreeSet<String> = out.entry(job.clone()).or_default();
        for word in rest.split_whitespace() {
            if word == "&&" || word == "||" || word.starts_with(';') {
                break;
            }
            if !word.starts_with('-') {
                packages.insert(word.to_string());
            }
        }
    }
    out
}

/// **The `check` job installs at least what the `coverage` job does** (SKEIN-1094).
///
/// The coverage job installs `bubblewrap tmux` so its number is measured where nothing skipped;
/// `check` is where `noskip-check` runs, and it installed `bubblewrap` alone — so the tmux-dependent
/// binaries could skip there on a runner image without tmux, and nothing recorded whether the image
/// had it. Stated as an inclusion rather than as "tmux is on the line", so the next package the
/// coverage job needs cannot be added to one job and not the other.
///
/// **What makes it fail:** the `check` job's apt line without `tmux`, which is how it was.
#[test]
fn the_check_job_installs_every_package_the_coverage_job_does() {
    let jobs = ci_apt_packages();
    let coverage = jobs
        .get("coverage")
        .filter(|p| !p.is_empty())
        .expect("the coverage job installs nothing with apt, so this compares nothing");
    let check = jobs
        .get("check")
        .expect("no `check` job installs anything with apt");
    let missing: Vec<&String> = coverage.difference(check).collect();
    assert!(
        missing.is_empty(),
        "the coverage job installs {missing:?} and the check job does not, so the binaries that need \
         them can skip where noskip-check runs: check has {check:?}, coverage has {coverage:?}"
    );
}

/// The `publish` job's shell block, out of `.github/workflows/release.yml`, with the step's own
/// indentation removed — the script GitHub hands to `bash -e`.
///
/// Read as text for the reason `ci_apt_packages` gives: the block is the first `- run: |` after the
/// job's `  publish:` line, and it ends at the first line indented less than its body.
fn release_publish_script() -> String {
    let release = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .expect("the release workflow");
    let mut lines = release
        .lines()
        .skip_while(|l| *l != "  publish:")
        .skip_while(|l| l.trim() != "- run: |")
        .skip(1);
    let body = "          ";
    let mut script = String::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            script.push('\n');
        } else if let Some(rest) = line.strip_prefix(body) {
            script.push_str(rest);
            script.push('\n');
        } else {
            break;
        }
    }
    assert!(
        script.contains("gh release"),
        "found no `gh release` in the publish job's run block, so this reads the wrong block:\n{script}"
    );
    script
}

/// Run the publish block against a `gh` that records its arguments, one call per line, and answers
/// `release view` as told. Returns those calls.
fn run_release_publish(dir: &Path, release_exists: bool) -> Vec<String> {
    let stub = dir.join("bin");
    let dist = dir.join("dist");
    std::fs::create_dir_all(&stub).unwrap();
    std::fs::create_dir_all(&dist).unwrap();
    for f in [
        "skein-v0.0.0-example.tar.gz",
        "skein-v0.0.0-example.tar.gz.sha256",
    ] {
        std::fs::write(dist.join(f), "example").unwrap();
    }
    let gh = stub.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"$GH_LOG\"\n\
         if [ \"$1 $2\" = 'release view' ] && [ -z \"$GH_RELEASE_EXISTS\" ]; then\n\
           echo 'release not found' >&2; exit 1\n\
         fi\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = dir.join("gh.log");
    let path = format!(
        "{}:{}",
        stub.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-e")
        .arg("-c")
        .arg(release_publish_script())
        .current_dir(dir)
        .env("PATH", path)
        .env("GH_LOG", &log)
        .env("GITHUB_REF_NAME", "v0.0.0")
        .env("GITHUB_REPOSITORY", "example/thing")
        .env_remove("GH_RELEASE_EXISTS");
    if release_exists {
        cmd.env("GH_RELEASE_EXISTS", "1");
    }
    let out = cmd.output().expect("bash");
    assert!(
        out.status.success(),
        "the publish block failed with release_exists={release_exists}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// **The release's `publish` step uploads into a release that exists, and creates one that does
/// not.** v0.1.0's run passed every gate and both builds and then failed in `publish` with `a
/// release with the same tag name already exists: v0.1.0` (run 36413315833): the owner had written
/// the release by hand first, and `gh release create` refuses an existing one. A tag cannot be
/// pushed to test the workflow, so its block is run here against a stub `gh`, both ways.
///
/// **What makes it fail:** the block back to an unconditional `gh release create` (the "exists"
/// half sees `create`, not `upload`); `--clobber` dropped from the upload, so a re-run cannot
/// replace an archive it already attached; `--verify-tag` dropped from the create; or the upload
/// given `--title`/`--notes`, which would overwrite what the owner wrote.
#[test]
fn the_release_publish_step_uploads_into_an_existing_release_and_creates_a_missing_one() {
    let dir = common::Scratch::temp("skein-release-publish");
    let files = "dist/skein-v0.0.0-example.tar.gz dist/skein-v0.0.0-example.tar.gz.sha256";

    let exists = run_release_publish(&dir.join("exists"), true);
    assert_eq!(
        exists,
        vec![
            "release view v0.0.0 --repo example/thing".to_string(),
            format!("release upload v0.0.0 {files} --repo example/thing --clobber"),
        ],
        "with the release already there, publish must upload into it with --clobber and nothing \
         else — no create, which refuses, and no title or notes, which are the owner's"
    );

    let absent = run_release_publish(&dir.join("absent"), false);
    assert_eq!(
        absent,
        vec![
            "release view v0.0.0 --repo example/thing".to_string(),
            format!(
                "release create v0.0.0 {files} --repo example/thing --verify-tag \
                 --title skein v0.0.0 --generate-notes"
            ),
        ],
        "with no release yet, publish must create it as it always did, --verify-tag included"
    );
}
