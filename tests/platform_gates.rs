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
//! running the suite on a Mac reads `box_side_tests_do_not_run_on_this_platform` in the list and
//! knows what they have and have not just proved.

mod common;

use common::REQUIREMENTS;
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
