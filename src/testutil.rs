//! Fixtures shared by every module's tests.
//!
//! `#[cfg(test)]` only, so none of this is in a release build. It lives in its own module because
//! the tests moved out of `lib.rs` to sit beside the code they exercise, and three of these — the
//! env lock especially — are process-global and must be *the same* value for every test in the
//! crate, not a per-module copy.

use crate::place::{record_place, PlaceRecord};
use crate::registry::Sandbox;
use chrono::Utc;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Env vars are process-global, and skein reads a dozen of them. Every test that sets one takes
/// this first, so a parallel run can't have one test's `$SKEIN_HOME` leak into another's.
///
/// One lock for the whole crate: a per-module lock would serialize each module against itself and
/// nothing else, which is the failure mode that looks like a flaky test.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Take the env lock, ignoring poisoning.
///
/// A plain `.lock().unwrap()` turns *one* failing test into a cascade: the panic poisons the mutex,
/// every other test then panics taking it, and the real failure is buried in fifty identical ones.
/// Observed exactly that — one stale assertion here read as fifty broken tests. The data this
/// guards is `()`; there is no invariant for a panic to have corrupted.
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Environment variables a test pins, **put back when the test ends however it ends.**
///
/// The lock above and this are two different guarantees, and having one has repeatedly been read as
/// having the other. [`env_lock`] stops a *concurrent* test seeing a half-written environment. It
/// says nothing about what the environment looks like once the lock is released — so a test that
/// pins `$SKEIN_FLEET_ROOT`, holds the lock perfectly, and never puts it back has answered every
/// *later* test in the process that pinned none of its own. That is SKEIN-696: `src/repos.rs` leaked
/// a fleet root, a later test read it instead of failing for want of a pin, and two defects
/// cancelled out into a green suite. `tools/env-lock-check.py` passed all day, because the lock was
/// held.
///
/// **The trailing `remove_var` is not the fix, and that is the whole reason this type exists.** The
/// repair for SKEIN-696 was a `remove_var` on the last line of the test — what all 23 of
/// `src/repos.rs`'s did, and what every such repair in this tree has done. A failing assertion unwinds
/// straight past it. So a test in that shape restores the environment exactly when it passes and
/// leaks exactly when it fails: the ordinary case while developing, and the case where the next
/// test's result is least likely to be believed.
///
/// `Drop` runs on both paths, which is the shape [`TempDir`] here and `Scratch` in
/// `tests/common/mod.rs` already use for directories. Restoration is in reverse order of pinning, so
/// a variable pinned twice returns to what it held before the *first* pin; a variable that was unset
/// before is unset after, not set to empty, which reads as present to `env::var_os`.
///
/// It deliberately does **not** take [`env_lock`]. The two are separate lines at a call site:
///
/// ```ignore
/// let _lock = env_lock();
/// let mut env = env_pins();
/// env.set("SKEIN_HOME", &home);
/// ```
///
/// **Bind it after the directory it points at.** Locals drop in reverse order of declaration, so
/// `let home = tempdir(); let mut env = env_pins();` unpins the variable and then removes the
/// directory. The other order leaves `$SKEIN_HOME` naming a directory that is already gone for the
/// width of one drop, which is worse for whatever reads it next than naming nothing at all — the
/// same ordering `tests/common/mod.rs` spells out as field order on its `Env` struct.
///
/// Folding the lock in would deadlock the moment a fixture that pins reached another that also pins,
/// which is the arrangement most of this crate's env-touching tests are already in — `Mutex` is not
/// re-entrant, and a partial conversion is exactly where that would bite. `tools/env-lock-check.py`
/// counts an `env_pins()` call as touching the environment, so a converted test still has to hold
/// the lock and still fails the gate if it stops.
pub(crate) struct EnvPins(Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>);

/// Start pinning environment variables. See [`EnvPins`].
pub(crate) fn env_pins() -> EnvPins {
    EnvPins(Vec::new())
}

impl EnvPins {
    /// Pin `name` to `value`, remembering what it held.
    pub(crate) fn set(&mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) -> &mut EnvPins {
        self.remember(name);
        env::set_var(name, value);
        self
    }

    /// Pin `name` to *absent*, remembering what it held.
    ///
    /// Needed as often as [`set`](EnvPins::set): a test that proves what happens with no `$GH_TOKEN`
    /// has to unset one the environment may already carry, and unsetting it without recording the
    /// old value is the same leak in the other direction.
    pub(crate) fn unset(&mut self, name: &str) -> &mut EnvPins {
        self.remember(name);
        env::remove_var(name);
        self
    }

    fn remember(&mut self, name: &str) {
        self.0.push((name.into(), env::var_os(name)));
    }
}

impl Drop for EnvPins {
    fn drop(&mut self) {
        // Reverse, so the FIRST pin of a name is the last one undone and therefore the one that
        // wins. Forward order would leave a twice-pinned variable holding its intermediate value.
        for (name, prior) in self.0.drain(..).rev() {
            match prior {
                Some(v) => env::set_var(&name, v),
                None => env::remove_var(&name),
            }
        }
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh temp directory, unique per process and per call, **removed when the test ends**.
///
/// The guard is the whole point. Without it every `cargo test` run left its directories behind, and
/// they accumulated: measured in one box, 6,494 of them holding 5 GB — most of that box's disk, and
/// enough to abort a fleet resize, because among them is the mode-000 fixture below that `tar`
/// cannot read. A test's scratch space outliving the test is a leak like any other; it just takes
/// longer to notice.
///
/// Derefs to `Path`, so it is used exactly as the `PathBuf` it replaced. Bind it to a name —
/// `tempdir().join("x")` drops the guard on the spot and deletes the directory before the test has
/// used it, which the test then silently recreates.
///
/// **One directory a run still survives this**, and it is not a removal failure — instrumenting
/// `drop` showed `remove_dir_all` never erroring. It is *recreated* after the guard has removed it,
/// by work that outlives the test that started it: [`crate::util::Gate`] refreshes behind its caller on a
/// spawned thread, and a thread still running when the test ends writes through an env var that
/// still names the deleted path. Bounded and understood, against ~180 a run before.
///
/// It was two a run until [`reopen`] stopped chmodding through symlinks. The second was a genuine
/// removal failure and a far more expensive one: it left a store directory unreadable, and a single
/// unreadable path anywhere under `/boxes` makes `du` exit nonzero, which blanked the disk figure
/// for every box on the board. The survivor above is now the only one, and it is readable — which
/// is the property that actually matters, more than the count.
pub(crate) struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort: a test that has already failed must report *its* failure, not a cleanup
        // error on top of it. Modes are reset first because tests deliberately create unreadable
        // files, and a directory at mode 000 cannot be removed without opening it.
        reopen(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Make a tree removable again, since tests create files and directories that deliberately are not.
fn reopen(dir: &std::path::Path) {
    // The mode BEFORE the listing. A directory at 000 cannot be read, so asking first returns
    // nothing and leaves the tree exactly as unremovable as it was — which is how this leaked on
    // its first attempt: the one shape it existed to handle was the one it skipped.
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Symlinks are skipped outright, and this is the second bug this function has had.
        // `set_permissions` FOLLOWS a symlink, so chmodding what looked like a harmless link
        // rewrote the mode of whatever it pointed at — and skein's fixtures are full of them
        // (`tree/.claude` is a symlink to the store). The store became `drw-r--r--`, which nothing
        // can descend into, so `remove_dir_all` then failed and left the whole tree behind. The
        // function that exists to make a tree removable was what made it unremovable.
        //
        // Worse, it reaches outside: the target need not be under `dir` at all, so this could
        // silently re-mode a directory somewhere else entirely.
        //
        // Nothing is lost by skipping. A symlink's own mode is meaningless on Linux, and
        // `remove_dir_all` unlinks the link rather than following it.
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            reopen(&path);
        } else {
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
        }
    }
}

impl std::ops::Deref for TempDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for TempDir {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

/// # Reproducing macOS on Linux, in one command
///
/// This honours `$TMPDIR`, which is the whole of what is needed to reproduce the most expensive
/// class of bug this suite has had. On macOS `$TMPDIR` lives under `/var`, which is a **symlink** to
/// `/private/var` — so anything skein canonicalises and anything a fixture holds as a string are two
/// names for one directory, and every `starts_with` between them answers "no". That silently
/// disarmed `skein repoint` and the copied-volume guard (SKEIN-118), and nobody saw it because the
/// suite had only ever been run on Linux.
///
/// ```sh
/// mkdir -p /tmp/realtmp && ln -sfn /tmp/realtmp /tmp/linktmp
/// TMPDIR=/tmp/linktmp cargo test --lib
/// ```
///
/// **`--lib`, not `--workspace`**: `tests/isolation_bwrap.rs` cannot build a namespace through a
/// symlinked temp root, which is an artefact of this trick rather than anything macOS does — a Mac
/// runs no `bwrap` at all. What this reproduces is path comparison, and it reproduces it exactly.
pub(crate) fn tempdir() -> TempDir {
    sweep_stale_runs();
    let d = env::temp_dir().join(format!(
        "skein-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&d).unwrap();
    TempDir(d)
}

/// How old a leftover has to be before this run treats it as nobody's.
///
/// An hour, not a minute: a `cargo test --all` on a loaded machine runs for minutes, and several of
/// them can overlap on one box. Deleting a directory another process is still using is a worse
/// failure than leaving one behind, and it would look like a flaky test in a suite this one does not
/// even know about.
const STALE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Is this leftover this crate's, and from a run that is over?
///
/// Its own function so both halves can be asserted without arranging a filesystem: the name rule is
/// what keeps this from touching anything it did not create, and the age rule is what keeps it from
/// deleting the scratch of a run happening beside it. Getting either wrong is a test suite that
/// breaks another one, which is the hardest kind of failure to attribute.
fn nobodys(name: &str, age: std::time::Duration) -> bool {
    name.starts_with("skein-test-") && age > STALE
}

/// Remove scratch directories from runs that are over, once per process.
///
/// **The guard above is not enough, and this is the measurement rather than a worry.** One run in
/// this tree leaves about three behind — the doc on [`TempDir`] explains why: a background refresh
/// outliving the test recreates a path the guard has already removed. Three is nothing; three per
/// run for a month is not. Measured on this box: **7,933 directories holding 6.9 GB**, on a
/// filesystem that was then 98% full and failing a resize test for want of 273 MiB.
///
/// So the guard cleans up after a test and this cleans up after a *run*, and neither is redundant:
/// the guard is exact and cannot catch what outlives it, this is approximate and catches whatever
/// the guard missed however it got there.
///
/// Only this crate's own scratch — `skein-test-*` under the temp directory, which nothing but
/// [`tempdir`] creates — and only what is older than [`STALE`], so a run beside this one is safe.
fn sweep_stale_runs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(entries) = fs::read_dir(env::temp_dir()) else {
            return;
        };
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let age = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|at| now.duration_since(at).unwrap_or_default())
                .unwrap_or_default();
            if nobodys(name, age) {
                // Modes first, for the same reason the guard does it: a fixture deliberately at
                // 000 cannot be removed without being opened, and one of those is what made `du`
                // exit nonzero and blank the disk figure for every box on the board.
                reopen(&entry.path());
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    });
}

/// An RFC3339 timestamp `s` seconds in the past — for ageing a signal without sleeping.
pub(crate) fn secs_ago(s: i64) -> String {
    (Utc::now() - chrono::Duration::seconds(s)).to_rfc3339()
}

/// A registry entry with only the fields a test cares about set.
pub(crate) fn sb(status: &str, last_seen: &str) -> Sandbox {
    Sandbox {
        branch: "b".into(),
        dir: "/d".into(),
        last_seen: last_seen.into(),
        status: status.into(),
    }
}

/// A fake `claude` that answers the two prompts skein's AI paths send it, so the enrichment and
/// the Continue-N gate can be tested without spending a token or needing a login.
///
/// **It reads the prompt from stdin, and no longer from the last argument** (SKEIN-684). That is
/// where `ai::tried` puts it — a pipe has no `MAX_ARG_STRLEN` and does not show up in
/// `/proc/<pid>/cmdline` — so a stub that still walked argv would be answering `?` to a prompt it
/// had been handed in full. Deliberately with no argv fallback: the fixture matches the mechanism,
/// and a prompt put back on argv makes these tests fail too rather than silently still passing.
#[cfg(unix)]
pub(crate) fn write_claude_stub(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("claude-stub.sh");
    fs::write(
        &p,
        "#!/bin/sh\nasked=$(cat)\ncase \"$asked\" in\n  *'wire it up'*) echo ROUTINE ;;\n  *'which database'*) echo DECISION ;;\n  *Summarise*) echo 'It wired up the parser.' ;;\n  *) echo '?' ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// The session signal a box writes at turn end, as a store file.
#[cfg(unix)]
pub(crate) fn write_session(dir: &std::path::Path, name: &str, last_message: &str) {
    fs::create_dir_all(dir.join("sessions")).unwrap();
    let body =
        serde_json::json!({"ts":"2026-06-29T00:00:00Z","kind":"stop","lastMessage":last_message});
    fs::write(
        dir.join("sessions").join(format!("{name}.json")),
        body.to_string(),
    )
    .unwrap();
}

/// Give `name` a placement record, which is what makes it a box skein can address.
///
/// Needed by every test that builds an argv or execs into a box. It used to be needed by none of
/// them: an unplaced name resolved to "a sandbox called `name`", skein's per-VM model, so a test
/// could ask for a box's argv without there being a box. That fallback was a guess in production
/// too — any name at all, including a sandbox skein never made — so it is gone, and a fixture now
/// has to say the box exists.
pub(crate) fn placed(name: &str) {
    record_place(
        name,
        &PlaceRecord {
            sandbox: "skein-fleet".into(),
            ns_pid: std::process::id(), // alive, so the record is followed
            home: format!("/boxes/{name}/home"),
            tree: format!("/boxes/{name}/tree"),
            sock: format!("/boxes/{name}/session.sock"),
            // Stamped, because an unstamped record is not a placed box any more — it is a box whose
            // address skein refuses to use. A fixture that left this off would be testing the
            // refusal path while claiming to test the thing it refused.
            generation: "test-boot".into(),
            ns_start: 1,
            ..Default::default()
        },
    )
    .unwrap();
}

/// Can bwrap actually make a namespace here — not merely, is bwrap installed?
///
/// **The distinction cost a red CI run for 27 days.** The two callers used to ask
/// `Command::new("bwrap").arg("--version").output().is_err()`, which is `Err` only when the binary
/// cannot be *spawned*. On `ubuntu-24.04` — what `.github/workflows/ci.yml` runs on — bubblewrap
/// installs fine and `kernel.apparmor_restrict_unprivileged_userns=1` then refuses the unprivileged
/// user namespace it needs. So the guard passed, bwrap started and died, the fixture never reported
/// its anchor, and the test failed five seconds later with a message about an anchor rather than
/// about a namespace.
///
/// The cheapest possible namespace is the only honest question, and it is the one
/// `tests/isolation_bwrap.rs::bwrap_works` was already asking. This is that check, shared by the two
/// in-crate callers; the integration tests keep their own copies because they are separate crates.
///
/// **A skip here is not free**, and `ci.yml` treats it as a failure: a guard that quietly turns the
/// isolation cover into a no-op is the outcome the bubblewrap install exists to prevent.
pub(crate) fn bwrap_works() -> bool {
    std::process::Command::new("bwrap")
        .args(["--dev-bind", "/", "/", "--", "/bin/true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A test that panics still puts the environment back**, which is the only property
    /// [`EnvPins`] exists for.
    ///
    /// A happy-path version of this test would pass against the thing being replaced: a trailing
    /// `remove_var` restores the environment perfectly for as long as nothing fails. The failure it
    /// hides is a *failing* test that also leaks, so the panic is the fixture, not decoration.
    ///
    /// Four assertions, and the concrete change that makes each one fail:
    ///
    ///   · emptying `EnvPins::drop` — the `..._PIN_HELD` assertion fails, still reading `during`;
    ///   · guarding that `drop` with `if !std::thread::panicking()`, which is what `Scratch` does
    ///     for directories and what would be the natural thing to copy — the same assertion fails,
    ///     and *only* because the closure panicked;
    ///   · restoring an absent variable with `set_var(name, "")` instead of `remove_var` — the
    ///     `..._PIN_ABSENT` assertion fails, because empty and absent are different answers to
    ///     `var_os`;
    ///   · draining `self.0` forward instead of `.rev()` — the `..._PIN_TWICE` assertion fails,
    ///     the variable being left holding the intermediate value.
    ///
    /// All four were run, and each failed only its own assertion.
    #[test]
    fn a_test_that_panics_still_puts_the_environment_back() {
        // Every name is spelled as a literal rather than bound once and reused, so that
        // `tools/env-lock-check.py`'s restore rule can read this test at all: a `remove_var` whose
        // name is not a literal puts a whole scope beyond that rule, and the test that proves the
        // rule's remedy should not be one of the 84 it cannot see.
        let _lock = env_lock();
        env::set_var("SKEIN_TESTUTIL_PIN_HELD", "before");
        env::set_var("SKEIN_TESTUTIL_PIN_TWICE", "before");
        env::remove_var("SKEIN_TESTUTIL_PIN_ABSENT");

        // The panic is caught rather than allowed to fail the test, and the hook is silenced so the
        // deliberate one does not read as a failure in the output. Both are put back before any
        // assertion runs, so a failure below reports itself normally.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(|| {
            let mut env = env_pins();
            env.set("SKEIN_TESTUTIL_PIN_HELD", "during")
                .set("SKEIN_TESTUTIL_PIN_ABSENT", "during");
            env.set("SKEIN_TESTUTIL_PIN_TWICE", "once")
                .set("SKEIN_TESTUTIL_PIN_TWICE", "twice");
            assert_eq!(
                env::var("SKEIN_TESTUTIL_PIN_HELD").unwrap(),
                "during",
                "the pin did not take"
            );
            panic!("as a failing assertion would");
        });
        std::panic::set_hook(hook);
        assert!(outcome.is_err(), "the closure was supposed to unwind");

        assert_eq!(
            env::var("SKEIN_TESTUTIL_PIN_HELD").unwrap(),
            "before",
            "a panicking test leaked its pin — which is the whole class: the next test in this \
             process is then answered out of a fixture it never asked for"
        );
        assert!(
            env::var_os("SKEIN_TESTUTIL_PIN_ABSENT").is_none(),
            "a variable that was ABSENT came back set — empty is not absent, and every skein \
             reader tests presence"
        );
        assert_eq!(
            env::var("SKEIN_TESTUTIL_PIN_TWICE").unwrap(),
            "before",
            "a variable pinned twice was restored to the intermediate value, not the original"
        );

        env::remove_var("SKEIN_TESTUTIL_PIN_HELD");
        env::remove_var("SKEIN_TESTUTIL_PIN_TWICE");
    }

    /// The sweep touches this crate's own leftovers and nothing else, and only once a run is over.
    ///
    /// Both halves have a cost if they are wrong, and they are different costs. A name rule that is
    /// too loose deletes somebody else's data out of a shared temp directory. An age rule that is
    /// too eager deletes the scratch of a `cargo test` running beside this one, which shows up as a
    /// flaky failure in a suite this process has never heard of.
    #[test]
    fn the_sweep_takes_old_scratch_of_ours_and_nothing_else() {
        let old = STALE + std::time::Duration::from_secs(1);
        let fresh = std::time::Duration::from_secs(1);
        assert!(nobodys("skein-test-1234-0", old));
        assert!(
            !nobodys("skein-test-1234-0", fresh),
            "a run beside this one"
        );
        // Named by other things in the same directory — skein's own integration tests among them,
        // which reuse a fixed name per test and are not this function's to remove.
        for theirs in [
            "skein-it-routes-99",
            "skein-warden-rt-99",
            "skein-audit-said-99",
            "systemd-private-whatever",
            "",
        ] {
            assert!(!nobodys(theirs, old), "{theirs} is not ours to delete");
        }
    }

    /// `reopen` must never chmod through a symlink — including out of the tree it was handed.
    ///
    /// This is the bug as it happened, and it cost more than a stray directory. skein's fixtures
    /// symlink `tree/.claude` at the store, `set_permissions` follows symlinks, so runs left a store
    /// at `drw-r--r--` and a temp tree that could not be removed. Those accumulated to 152
    /// unreadable paths under `/boxes` — and one unreadable path anywhere is enough to make `du`
    /// exit nonzero, which blanked the disk figure for every box on the board.
    ///
    /// The target sits deliberately *outside* the tree being reopened. Inside, the test would depend
    /// on readdir order: reach the real directory after the link and it gets chmodded back, hiding
    /// the bug on some runs and not others. Outside, nothing can put it back.
    #[test]
    fn reopening_a_tree_never_chmods_through_a_symlink() {
        let elsewhere = tempdir();
        let target = elsewhere.join("store");
        fs::create_dir_all(target.join("memory")).unwrap();

        let dir = tempdir();
        std::os::unix::fs::symlink(&target, dir.join("link")).unwrap();
        reopen(&dir);

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "reopen re-moded a directory a symlink pointed at, and one outside its own tree at that"
        );
        assert!(
            fs::read_dir(&target).is_ok(),
            "the directory can no longer be descended into, so nothing can remove it"
        );
    }

    /// And the shape that actually leaked: a store, and a tree that links to it.
    #[test]
    fn a_fixture_that_symlinks_its_store_is_still_cleaned_up() {
        let path;
        {
            let dir = tempdir();
            path = dir.to_path_buf();
            fs::create_dir_all(dir.join("store/.claude/memory")).unwrap();
            fs::write(dir.join("store/.claude/memory/m.md"), "x").unwrap();
            fs::create_dir_all(dir.join("tree")).unwrap();
            std::os::unix::fs::symlink(dir.join("store/.claude"), dir.join("tree/.claude"))
                .unwrap();
        }
        assert!(
            !path.exists(),
            "the temp tree was left behind at {} — these accumulate, and an unreadable one \
             blanks every box's disk usage",
            path.display()
        );
    }
}
