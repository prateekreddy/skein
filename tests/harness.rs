//! The shared test harness, tested — because everything else in this directory now trusts it.
//!
//! `tests/common/mod.rs` carries two behaviours that a reader has to be able to rely on without
//! reading its source, and both are the kind that fail silently:
//!
//! * a scratch directory removes itself, **except after a failure**, where it is the only evidence
//!   left. Getting the first half wrong fills a contributor's `/var/tmp` — 1,082 directories and
//!   1.6 GB on the box this was found on. Getting the second half wrong is worse: it makes every
//!   failure in this suite undebuggable, and nothing would say so.
//! * a skip announces, and turns into a failure under `$SKEIN_TESTS_NO_SKIP`. That variable is the
//!   whole answer to "was anything skipped in that green run" — `cargo test` captures a passing
//!   test's output, and a skipped test passes, so reading the log cannot be the check. A variable
//!   that quietly stopped being consulted would take the answer with it.

mod common;

use common::Scratch;
use std::path::PathBuf;

/// The ordinary path: the directory goes when the test that made it is done with it.
///
/// **What makes it fail:** a `Drop` for `Scratch` that returns without removing.
#[test]
fn a_scratch_directory_is_removed_when_the_test_that_made_it_passes() {
    let path: PathBuf = {
        let scratch = Scratch::temp("skein-harness-passing");
        std::fs::write(scratch.join("something"), "x").unwrap();
        assert!(
            scratch.join("something").exists(),
            "the fixture never wrote anything, so what this asserts afterwards is about nothing"
        );
        scratch.to_path_buf()
    };
    assert!(
        !path.exists(),
        "{} outlived the test that made it — this is the leak that put 1.6 GB in one box's \
         /var/tmp",
        path.display()
    );
}

/// And the exception, which is the half that matters more.
///
/// A failing test's scratch directory is the box's tree, its logs, and what the fake `sbx`
/// recorded. Tidying it away leaves a failure nobody can look into, so `Drop` keeps it while the
/// thread is panicking.
///
/// **What makes it fail:** a `Drop` that removes unconditionally — which is what
/// `tests/turn_state_probe.rs` and `tests/isolation_bwrap.rs` used to do.
#[test]
fn a_scratch_directory_survives_the_test_that_failed_because_that_is_the_evidence() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let path = std::panic::catch_unwind(|| {
        let scratch = Scratch::temp("skein-harness-failing");
        std::fs::write(scratch.join("evidence"), "what went wrong").unwrap();
        let path = scratch.to_path_buf();
        // Deliberate: this is the whole point of the test, and it unwinds through `Drop`.
        std::panic::panic_any(path);
    })
    .expect_err("the fixture did not panic, so nothing was proved");
    std::panic::set_hook(hook);
    let path = path
        .downcast::<PathBuf>()
        .expect("the path it panicked with");

    assert_eq!(
        std::fs::read_to_string(path.join("evidence")).unwrap_or_default(),
        "what went wrong",
        "a failing test's scratch directory was swept, so its failure cannot be looked into: {}",
        path.display()
    );
    // This test's own leftovers, once they have been read.
    let _ = std::fs::remove_dir_all(&*path);
}

/// What had to be stopped is stopped either way — only the directory's removal is conditional.
///
/// `tests/fleet_move.rs` is why: its supervisor loops on a file inside the scratch directory, so
/// keeping the directory after a failure and stopping nothing would keep the supervisor restarting
/// the server for ever. Four such processes were found by the leaked-process gate on 2026-08-31.
///
/// **What makes it fail:** a `Drop` that runs the quiesce only on the non-panicking path.
#[test]
fn whatever_a_scratch_had_to_stop_is_stopped_even_when_the_test_failed() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let path = std::panic::catch_unwind(|| {
        let scratch = Scratch::temp("skein-harness-quiesce").quiesce_with(|root| {
            // Stands in for "remove the loop's exit condition, then kill the tmux server".
            let _ = std::fs::remove_file(root.join("keeps-the-loop-going"));
        });
        std::fs::write(scratch.join("keeps-the-loop-going"), "").unwrap();
        let path = scratch.to_path_buf();
        std::panic::panic_any(path);
    })
    .expect_err("the fixture did not panic, so nothing was proved");
    std::panic::set_hook(hook);
    let path = path
        .downcast::<PathBuf>()
        .expect("the path it panicked with");

    assert!(
        path.exists(),
        "the directory was removed, so this test cannot tell a quiesce that ran from one that did \
         not: {}",
        path.display()
    );
    assert!(
        !path.join("keeps-the-loop-going").exists(),
        "the quiesce did not run on the failing path, so what the fixture started is still running"
    );
    let _ = std::fs::remove_dir_all(&*path);
}

/// A skip is invisible in a green run by construction, and this is what makes it visible.
///
/// `cargo test` captures a passing test's output and a skipped test passes, so grepping a run's log
/// for skip notices cannot work — that is precisely why eighteen of forty skip sites in this suite
/// were silent and nobody noticed. `$SKEIN_TESTS_NO_SKIP` inverts it: every skip becomes a panic,
/// cargo cannot hide a failing test, and a green run under that variable is a proof that nothing was
/// skipped which needs nobody to read anything.
///
/// **What makes it fail:** `common::skip` no longer consulting the variable — or the variable being
/// renamed on one side only.
#[test]
fn a_skip_becomes_a_failure_when_the_run_asked_for_a_run_with_no_skips() {
    let _env = common::env_lock();
    let was = std::env::var_os(common::NO_SKIP);
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    // The control first: without the variable a skip is an ordinary early return, or every suite on
    // a machine missing one tool would go red.
    std::env::remove_var(common::NO_SKIP);
    let quiet = std::panic::catch_unwind(|| common::skip("a tool this machine does not have"));
    assert!(
        quiet.is_ok(),
        "an ordinary skip panicked, which would make every machine without jq fail this suite"
    );

    std::env::set_var(common::NO_SKIP, "1");
    let loud = std::panic::catch_unwind(|| common::skip("a tool this machine does not have"));
    std::panic::set_hook(hook);
    match was {
        Some(v) => std::env::set_var(common::NO_SKIP, v),
        None => std::env::remove_var(common::NO_SKIP),
    }

    let said = loud
        .expect_err("a skip stayed silent under the variable that exists to forbid silent skips");
    let said = said
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_else(|| "<not a string>".into());
    assert!(
        said.contains("a tool this machine does not have") && said.contains("tests/harness.rs"),
        "the failure has to name the reason and the guard that took it, or it says no more than \
         `ignored` would: {said}"
    );
}
