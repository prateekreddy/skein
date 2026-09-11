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

/// Every `#[cfg(target_os = "linux")] #[test]` in the library, by name.
fn gated_in_source() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(&src)
        .expect("src is readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
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
fn integration_sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/ is readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    for file in files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        out.push((name, std::fs::read_to_string(&file).unwrap_or_default()));
    }
    out
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
#[test]
fn every_binary_that_skips_declares_what_this_machine_needs() {
    let skips = "return skip(";
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

    // 1. A binary that can skip is a binary with a requirement, and it has to be written down.
    let undeclared: Vec<&String> = sources
        .iter()
        .filter(|(name, src)| src.contains(skips) && !declared.contains(&name.as_str()))
        .map(|(name, _)| name)
        .collect();
    assert!(
        undeclared.is_empty(),
        "these binaries skip tests and are not in common::REQUIREMENTS, so what they need is \
         written down nowhere: {undeclared:?}"
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
        assert!(
            src.contains(skips),
            "common::REQUIREMENTS says tests/{name}.rs needs {tools:?}, but nothing in it skips — \
             either the guard was lost or the entry is stale"
        );
        assert!(
            !tools.is_empty(),
            "tests/{name}.rs is declared needing nothing"
        );
    }

    // 3. Every tool a file actually gates on is in that file's list. One-directional on purpose:
    //    `have("x")` is derivable, while `chromium_ready()`, `real_git()` and a bare
    //    `Command::new("python3")` are not, so those are declared and this cannot check them.
    for (name, src) in &sources {
        let tools: Vec<&str> = REQUIREMENTS
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.to_vec())
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
        if src.contains("bwrap_works()") {
            assert!(
                tools.contains(&"bwrap"),
                "tests/{name}.rs asks whether bwrap can make a namespace and does not declare it"
            );
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
const UNREFUSABLE: &[(&str, &str, &str)] = &[
    (
        "fleet.rs",
        "creating_a_fleet_is_asked_of_the_warden_and_never_run_here",
        "NOT a skip at all: the `return` is a stub server thread leaving its accept loop when the \
         listener is gone. It is here because the scanner reads `return` and cannot read intent",
    ),
    (
        "place.rs",
        "a_crossing_in_the_fleet_enters_the_box_without_sbx",
        "an announced skip that SKEIN-790 could not convert: another lane held src/place.rs for the \
         whole of that change. One line, the same shape as the fifteen — convert it and delete this",
    ),
    (
        "ai.rs",
        "narrate_uses_stubbed_claude_and_respects_kill_switch",
        "a skip of the OTHER shape the sweep turned up: `if Command::new(\"sh\")…is_err() { return }` \
         with no notice at all, so unlike the fifteen there is nothing a reader sees either way",
    ),
    (
        "diff.rs",
        "git_range_handles_repo_and_nonrepo",
        "the same silent shape — `return; // git not available in this environment`, where the \
         reason is in a comment the run never prints",
    ),
    (
        "sandbox.rs",
        "resume_batch_holds_real_decisions_when_ai_on",
        "the same silent shape again, guarding on whether `sh` can be spawned",
    ),
];

/// One `#[test]` in the library: where it is, and the text of its body.
struct LibTest {
    file: String,
    name: String,
    line: usize,
    body: Vec<String>,
}

/// Every `#[test]` in `src/`, with its body — spans found by INDENT, not by counting braces.
///
/// Brace counting is the obvious way and it does not work on this tree. `src/fleet.rs` embeds whole
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
    let mut files: Vec<_> = std::fs::read_dir(&src)
        .expect("src is readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();

    let mut found = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
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
        assert!(
            !bare_returns(&t.body).is_empty() || !bare_skip_notices(&t.body).is_empty(),
            "src/{file}::{name} is exempted from the library skip gates and no longer needs to be — \
             it has neither a bare early return nor an unrefusable skip notice. Delete the entry, \
             which is the point of it being written down"
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
        .map(|(_, t)| t.to_vec())
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
    // Derivable, so derived — the same one-directional check the integration gate makes, and for the
    // same reason: `bwrap_works()` names its tool, while a bare `Command::new("jq")` does not.
    let uses_bwrap = tests
        .iter()
        .any(|t| t.body.iter().any(|l| l.contains("bwrap_works()")));
    assert!(
        !uses_bwrap || tools.contains(&"bwrap"),
        "a library test asks whether bwrap can make a namespace and common::REQUIREMENTS does not \
         declare it"
    );
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
