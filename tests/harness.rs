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

/// The test marker reaches an integration binary — which is the half `cfg!(test)` cannot do.
///
/// The library is compiled once, without `--cfg test`, and every `tests/*.rs` binary links that
/// build: inside `skein::…` here, `cfg!(test)` is false. So the guard in `config::skein_home` rests
/// on `$SKEIN_TEST`, and `$SKEIN_TEST` rests on `.cargo/config.toml`'s `[env]` table. Nothing else
/// in the suite would notice that table being deleted — every test would simply stop being guarded
/// and go on passing, which is the failure this file exists to make impossible for the two
/// behaviours above it.
///
/// **What makes it fail:** removing `SKEIN_TEST` from `.cargo/config.toml`, or renaming it on one
/// side only. (Running the binary by hand rather than through cargo fails it too, and correctly:
/// the marker really is absent there.)
#[test]
fn the_test_marker_arrives_in_a_binary_where_cfg_test_is_false() {
    let _env = common::env_lock();
    assert_eq!(
        std::env::var(skein::util::TEST_MARKER).ok().as_deref(),
        Some("1"),
        "${} is not set in this binary — `.cargo/config.toml`'s [env] table is the only thing that \
         sets it, and without it `config::skein_home` answers a test with the real ~/.skein",
        skein::util::TEST_MARKER
    );
    assert!(
        skein::util::in_test(),
        "the marker is set and the library still does not believe it is under test"
    );

    // And the asymmetry itself, measured rather than asserted from memory: with the marker taken
    // away the library stops believing it is under test, which it could not do if its `cfg!(test)`
    // were true in this binary. (`cfg!(test)` written HERE is true — the integration crate is
    // built with it. That is the trap this whole marker exists to step around.)
    std::env::remove_var(skein::util::TEST_MARKER);
    let without = skein::util::in_test();
    std::env::set_var(skein::util::TEST_MARKER, "1");
    assert!(
        !without,
        "the library's own cfg!(test) is true in an integration binary after all — then this \
         marker is unnecessary, and `util::TEST_MARKER`'s reasoning needs rewriting, not deleting"
    );
}

/// **An ambient `$SKEIN_HOME` or `$SKEIN_FLEET_ROOT` never reaches a test.** The refusals below fire
/// only on an unset variable, and inside a skein box `$SKEIN_HOME` is exported and names the
/// owner's live store — so an unpinned test there was answered with it rather than refused, and
/// wrote into it (SKEIN-1213). `.cargo/config.toml` forces both to empty, which both readers treat
/// as unset.
///
/// **What makes it fail:** removing either line from `.cargo/config.toml` — on any machine, since
/// the variable then arrives absent (or ambient), not empty.
#[test]
fn an_inherited_home_or_fleet_root_never_reaches_a_test() {
    let _env = common::env_lock();
    for var in ["SKEIN_HOME", "SKEIN_FLEET_ROOT"] {
        assert_eq!(
            std::env::var(var).ok().as_deref(),
            Some(""),
            "${var} did not arrive forced empty — `.cargo/config.toml`'s [env] table is what \
             forces it, and without that a box's own ${var} answers an unpinned test with the \
             live fleet instead of the refusal"
        );
    }
}

/// And what the marker buys: an unpinned `$SKEIN_HOME` is refused, not answered.
///
/// On a developer box the fallback resolves through `/boxes/.skein/skein-home` to the fleet's real
/// home, so this is the difference between a fixture writing into a temp directory and writing into
/// live box state (SKEIN-626). Asserted from an integration binary on purpose — the unit-test side
/// of the guard could hold while this side was dead and nothing would say so.
///
/// **What makes it fail:** deleting the `assert!` from `config::skein_home`.
#[test]
fn an_unpinned_home_is_refused_rather_than_answered() {
    let _env = common::env_lock();
    let was = std::env::var_os("SKEIN_HOME");
    std::env::remove_var("SKEIN_HOME");
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let answered = std::panic::catch_unwind(skein::config::skein_home);
    std::panic::set_hook(hook);
    if let Some(v) = was {
        std::env::set_var("SKEIN_HOME", v);
    }

    let said = answered.map_err(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "<not a string>".into())
    });
    let said = match said {
        Ok(path) => panic!(
            "an unpinned test was answered with {} instead of being refused",
            path.display()
        ),
        Err(said) => said,
    };
    assert!(
        said.contains("SKEIN_HOME"),
        "the refusal has to name the variable to set, or it tells a contributor nothing: {said}"
    );
}

/// And the same for the fleet root, which is the other half of the same fixture.
///
/// `util::fleet_root` falls back to `/boxes`, which on any machine running skein is the owner's
/// LIVE fleet — so an unpinned test read real boxes' state and disks, and in two measured cases
/// wrote to them: `tests/server.rs` spawned a `skein-server` whose `heal_fleet` rewrote
/// `/boxes/.skein/box-session.sh` (SKEIN-685), and five earlier tests installed uncommitted code
/// onto it (SKEIN-530). `health::tests::a_missing_tool_is_one_fault_and_not_five` merely READ, and
/// passed or failed on how full the real machine's disk was while its message accused the code
/// (SKEIN-690).
///
/// **Both directions, in one test.** The pinned call has to be answered and the unpinned one
/// refused: a check that only ever exercises the pinned path would still pass with the `assert!`
/// deleted, which is the whole failure mode. From an integration binary for the reason the sibling
/// above gives — the unit-test side could hold while this side was dead.
///
/// **What makes it fail:** deleting the `assert!` from `util::fleet_root`.
#[test]
fn an_unpinned_fleet_root_is_refused_rather_than_answered() {
    let _env = common::env_lock();
    let was = std::env::var_os("SKEIN_FLEET_ROOT");

    // Pinned: answered, and answered with what it was given.
    std::env::set_var("SKEIN_FLEET_ROOT", "/tmp/a-fixture-fleet");
    assert_eq!(
        skein::util::fleet_root(),
        "/tmp/a-fixture-fleet",
        "a pinned root was not the one handed back, so the refusal below would be the only \
         behaviour this function had left"
    );

    // Unpinned: refused.
    std::env::remove_var("SKEIN_FLEET_ROOT");
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let answered = std::panic::catch_unwind(skein::util::fleet_root);
    std::panic::set_hook(hook);
    if let Some(v) = was {
        std::env::set_var("SKEIN_FLEET_ROOT", v);
    }

    let said = match answered {
        Ok(root) => panic!("an unpinned test was answered with {root} instead of being refused"),
        Err(e) => e
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "<not a string>".into()),
    };
    assert!(
        said.contains("SKEIN_FLEET_ROOT"),
        "the refusal has to name the variable to set, or it tells a contributor nothing: {said}"
    );
}

/// And the third guarantee this directory now rests on: a variable a test pins goes back, **even
/// when the test fails.**
///
/// `Scratch` and `skip` above are the two behaviours a reader has to be able to trust without
/// reading the source; `common::env_pins` is the third, and it fails silently in the same way. A
/// test that pins `$SKEIN_HOME` and never puts it back does not fail — it answers the *next* test in
/// the binary that pinned none of its own, and both go green (SKEIN-696). Nothing at the site of
/// either says so.
///
/// **The panicking path is the whole point**, and the happy path proves nothing about it. Every
/// repair of this class in this tree before `Drop` was a `remove_var` on a test's last line, which a
/// failing assertion unwinds straight past — so those tests restored when they passed and leaked
/// when they failed, which is the case where the next test's result is least likely to be believed.
/// The panic here is deliberate and caught; the hook is silenced so it does not read as a failure in
/// the output, and both are put back before any assertion below runs.
///
/// Every name is spelled as a literal, here and in the inner guard's own `set`/`unset` calls
/// below, so `tools/env-lock-check.py` can read exactly what this test touches. The fixture's own
/// before/after state is pinned through `EnvPins` too, for the same reason the rest of the suite
/// is (SKEIN-723): a bare trailing `remove_var` here would be unwound past by a failing assertion
/// below it, leaking these two names into whatever test runs next in this binary — which would be
/// more than a little ironic in the test that proves that exact remedy.
///
/// **What makes it fail:** emptying `EnvPins`'s `Drop`, or giving it the
/// `if std::thread::panicking() { return }` that `Scratch` above correctly has — for `Scratch` the
/// kept directory is the evidence, and there is no evidence in a leaked variable. Restoring an
/// absent variable as `""` fails the second assertion; dropping the `.rev()` fails the third.
#[test]
fn a_pin_taken_in_an_integration_test_goes_back_when_that_test_panics() {
    let _env = common::env_lock();
    let mut outer = common::env_pins();
    outer
        .set("SKEIN_HARNESS_PIN_HELD", "before")
        .set("SKEIN_HARNESS_PIN_TWICE", "before")
        .unset("SKEIN_HARNESS_PIN_ABSENT");

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(|| {
        let mut env = common::env_pins();
        env.set("SKEIN_HARNESS_PIN_HELD", "during")
            .set("SKEIN_HARNESS_PIN_ABSENT", "during");
        env.set("SKEIN_HARNESS_PIN_TWICE", "once")
            .set("SKEIN_HARNESS_PIN_TWICE", "twice");
        assert_eq!(
            std::env::var("SKEIN_HARNESS_PIN_HELD").unwrap(),
            "during",
            "the pin did not take, so what this test asserts afterwards is about nothing"
        );
        panic!("as a failing assertion would");
    });
    std::panic::set_hook(hook);
    assert!(outcome.is_err(), "the closure was supposed to unwind");

    assert_eq!(
        std::env::var("SKEIN_HARNESS_PIN_HELD").unwrap(),
        "before",
        "a panicking test leaked its pin — which is the whole class: the next test in this binary \
         is then answered out of a fixture it never asked for"
    );
    assert!(
        std::env::var_os("SKEIN_HARNESS_PIN_ABSENT").is_none(),
        "a variable that was ABSENT came back set — empty is not absent, and every skein reader \
         tests presence"
    );
    assert_eq!(
        std::env::var("SKEIN_HARNESS_PIN_TWICE").unwrap(),
        "before",
        "a variable pinned twice was restored to the intermediate value, not the original"
    );
}

/// Run `git` in `dir` with an identity and the file transport allowed, and hand back its stdout —
/// or fail naming the command, since a fixture that could not be built is not a finding.
fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=example",
            "-c",
            "user.email=example@example.invalid",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **A fresh worktree is provisioned before the gates judge it, and nothing else is touched**
/// (SKEIN-885).
///
/// The gate runner, not a test harness — but the same kind of thing: everything a lane concludes
/// from a green run trusts it. `git worktree add` leaves every submodule uninitialised, so every
/// lane's first full run failed `submodule-check` about how the tree was made rather than about
/// the change. `tools/gates.sh` now initialises exactly those, and this holds both halves in a
/// throwaway repository: a worktree's never-initialised submodule is checked out, and a submodule
/// deliberately AHEAD of its pin — the documented upgrade window — is left where it is.
///
/// **What makes it fail:** a `provision` that does nothing fails the first assertion; one that runs
/// `git submodule update --init` over every submodule, initialised or not, moves the ahead-of-pin
/// checkout back and fails the second.
#[test]
fn the_gate_runner_checks_out_a_worktrees_uninitialised_submodule_and_moves_no_other() {
    let scratch = Scratch::temp("skein-gates-provision");
    let root = scratch.to_path_buf();
    let upstream = root.join("upstream");
    let main = root.join("main");
    let lane = root.join("lane");
    for d in [&upstream, &main] {
        std::fs::create_dir_all(d).unwrap();
        git_in(d, &["init", "-q"]);
    }
    std::fs::write(upstream.join("a"), "one").unwrap();
    git_in(&upstream, &["add", "a"]);
    git_in(&upstream, &["commit", "-qm", "one"]);
    git_in(
        &main,
        &[
            "submodule",
            "add",
            "-q",
            upstream.to_str().unwrap(),
            "vendored",
        ],
    );
    git_in(&main, &["commit", "-qm", "vendor it"]);
    git_in(&main, &["worktree", "add", "-q", lane.to_str().unwrap()]);
    assert!(
        git_in(&lane, &["submodule", "status"]).starts_with('-'),
        "the fixture's worktree came with its submodule already checked out, so this proves nothing"
    );

    // The file transport is refused by default for submodules; the runner inherits git's config
    // from the environment, so this is how the fixture's own `-c` reaches the git it runs.
    let gates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/gates.sh");
    let provision = |at: &std::path::Path| {
        let out = std::process::Command::new("bash")
            .arg(&gates)
            .arg("--provision")
            .arg(at)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "protocol.file.allow")
            .env("GIT_CONFIG_VALUE_0", "always")
            .output()
            .expect("bash runs the gate runner");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let said = provision(&lane);
    let status = git_in(&lane, &["submodule", "status"]);
    assert!(
        status.starts_with(' ') && said.contains("vendored"),
        "a fresh worktree's submodule is still not checked out after the runner provisioned it, \
         so its first gate run fails submodule-check about how it was made: status {status:?}, \
         the runner said {said:?}"
    );

    // Ahead of the pin on purpose, as `git submodule update --remote` leaves it mid-upgrade.
    let checkout = main.join("vendored");
    std::fs::write(checkout.join("a"), "two").unwrap();
    git_in(&checkout, &["commit", "-qam", "two"]);
    let ahead = git_in(&checkout, &["rev-parse", "HEAD"]);
    let said = provision(&main);
    assert_eq!(
        git_in(&checkout, &["rev-parse", "HEAD"]),
        ahead,
        "the runner moved a submodule that was already checked out back to its pin — which undoes \
         the upgrade `src/store/sync/UPSTREAM.md` walks through (it said {said:?})"
    );
}

/// Every `skein-server` a test starts is told the warden is somewhere nothing listens.
///
/// A server asks the warden at every start now (SKEIN-1130), and one with no `$SKEIN_WARDEN` asks
/// the default address — the owner's warden on any machine running one. So this reads the pin off
/// the two commands `tests/common` hands out, then counts every place a test names the binary, in
/// every `.rs` file under `tests/`, and requires that the one place is `tests/common/mod.rs`: a
/// test that builds its own `Command` for the binary is a test whose environment nobody pinned,
/// and the browser suites, which start it from node, have their harness's own pin checked too.
///
/// **What makes this fail**: either helper losing its `.env("SKEIN_WARDEN", …)`; any test naming
/// the binary's `env!` path itself; or `tests/ui/harness/server.mjs` losing its pin.
#[test]
fn every_skein_server_a_test_starts_is_pinned_away_from_a_real_warden() {
    use std::ffi::OsStr;
    use std::path::Path;
    for (how, started) in [
        ("common::skein_server", common::skein_server()),
        (
            "common::skein_server_behind",
            common::skein_server_behind("python3", ["-c", "pass"]),
        ),
    ] {
        let pin = started
            .get_envs()
            .find(|(name, _)| *name == OsStr::new("SKEIN_WARDEN"))
            .and_then(|(_, value)| value);
        assert_eq!(
            pin,
            Some(OsStr::new(common::NO_WARDEN)),
            "{how} starts a skein-server whose $SKEIN_WARDEN is not pinned to {}",
            common::NO_WARDEN
        );
    }

    // Spelt in two halves so this file is not one of the places it counts.
    let needle = concat!("CARGO_BIN_EXE_", "skein-server");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir)
            .expect("tests/ is readable")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name() != Some(OsStr::new("node_modules")) {
                    walk(&path, found);
                }
            } else if path.extension() == Some(OsStr::new("rs")) {
                found.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root.join("tests"), &mut files);
    let mut named = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("a test file is readable");
        for (at, line) in text.lines().enumerate() {
            if line.contains(needle) && !line.trim_start().starts_with("//") {
                let shown = file
                    .strip_prefix(root)
                    .unwrap_or(file)
                    .display()
                    .to_string();
                named.push(format!("{shown}:{}", at + 1));
            }
        }
    }
    eprintln!(
        "read {} .rs file(s) under tests/; the binary is named at {named:?}",
        files.len()
    );
    assert!(
        files.len() > 1,
        "found no test files to read under {}",
        root.display()
    );
    assert_eq!(
        named.len(),
        1,
        "the skein-server binary is named outside `tests/common` — start it through \
         `common::skein_server` or `common::skein_server_behind`, which pin $SKEIN_WARDEN: {named:?}"
    );
    assert!(
        named[0].starts_with("tests/common/mod.rs:"),
        "the one place the binary is named is not tests/common/mod.rs: {named:?}"
    );

    let harness = std::fs::read_to_string(root.join("tests/ui/harness/server.mjs"))
        .expect("the browser suites' harness is readable");
    let pinned = format!("childEnv.SKEIN_WARDEN = \"{}\";", common::NO_WARDEN);
    assert!(
        harness.lines().any(|line| line.trim() == pinned),
        "tests/ui/harness/server.mjs no longer pins the server it starts with `{pinned}`"
    );
}
