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
pub(crate) fn env_lock() -> EnvGuard {
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    EnvGuard {
        before: env::vars_os().collect(),
        lock,
    }
}

/// The lock, plus the environment as it stood when the lock was taken — **put back on drop, on the
/// unwinding path as well as the returning one.**
///
/// This is a BACKSTOP and not the mechanism a test should reach for; [`EnvPins`] below is that, and
/// the difference is what each one can promise. `EnvPins` restores the names a test names, where the
/// test names them, so a variable stops pointing at a `TempDir` *before* that directory is removed.
/// This restores everything, at the one point every writer already passes through, so it cannot be
/// forgotten — and being un-forgettable is the entire argument, because a pairing that has to be
/// remembered is one that is sometimes forgotten and a forgotten one matches no grep.
///
/// It buys two things `EnvPins` cannot, both measured rather than supposed:
///
/// * **The panicking path of every test that has not been converted.** The repair that closed
///   SKEIN-696 was a `remove_var` on a test's last line, and most of this crate's env-touching
///   scopes are still in that shape — `python3 tools/env-lock-check.py` counts them on its last
///   line, as the `#[test]`s that "set one by hand" less the ones that "pin through `env_pins()`".
///   A failing assertion unwinds straight past a trailing `remove_var`, so a test in that shape
///   restores the environment exactly when it passes and leaks exactly when it fails — the ordinary
///   case while developing, and the case where the next test's result is least likely to be
///   believed.
/// * **The scopes the gate cannot read.** The same line counts the `#[test]`s it declines to judge,
///   "having a `remove_var` whose name is not a literal". A textual gate cannot follow those;
///   `Drop` does not have to.
///
/// **Restoring to the state at acquire cannot be worse than that state**, which is what makes a
/// blanket restore safe next to the SKEIN-626 guard. `config::skein_home` panics rather than answer
/// a test that has not pinned `$SKEIN_HOME`, and the fear is that putting a variable *back* re-arms
/// a fallback the guard exists to deny. It does not: the value restored is the one the process
/// started under, and if that value were dangerous it was already answering every unpinned test
/// before any of them ran. What the restore removes is the strictly newer hazard — one test's
/// `$SKEIN_HOME` still naming a `TempDir` that has since been deleted, which is SKEIN-705 itself.
///
/// **Restoring on acquire instead was considered and does not fix it.** Cleaning the environment as
/// the lock is handed over needs no change of return type, so it would have cost no call site
/// anything — but the victim in SKEIN-705 never takes the lock. It is an unpinned test that reads
/// `config::skein_home` and is handed a dead directory instead of the panic that would tell it to
/// pin, and no amount of tidying at the next acquire ever runs on its behalf. The restore has to be
/// on drop to reach it.
///
/// The whole environment rather than the names written under the lock: reading `env::vars_os()`
/// costs one allocation of a few dozen short strings per acquire and cannot miss a name. Tracking
/// only what was touched would mean intercepting every write, which means routing all of them
/// through this type — `grep -rc 'env_lock()' src/` says how many would have to be found and
/// changed to catch the ones that were not. The cheap thing that cannot miss beats the exact thing
/// that can.
///
/// Poisoning is ignored here exactly as it was before, and now it matters more rather than less: a
/// panicking test is precisely the one whose environment needs putting back, so a guard that
/// refused a poisoned lock would decline to clean up in the only case the cleanup was written for.
/// `unwrap_or_else(|e| e.into_inner())` keeps that. `Drop` must not panic while unwinding — that
/// aborts — so the restore never unwraps: the names come from `vars_os()`, which yields only names
/// that are already legal to set.
pub(crate) struct EnvGuard {
    before: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// Held so the restore below runs while this crate's writers are still locked out. A `Drop::drop`
    /// BODY runs before any of the value's fields are dropped, so the environment is whole again
    /// before the mutex is released; there is no window in which another test sees half of it.
    #[allow(dead_code)]
    lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let now: std::collections::HashMap<std::ffi::OsString, std::ffi::OsString> =
            env::vars_os().collect();
        for (name, _) in now
            .iter()
            .filter(|(n, _)| !self.before.iter().any(|(b, _)| b == *n))
        {
            env::remove_var(name);
        }
        for (name, was) in &self.before {
            if now.get(name).map(|v| v != was).unwrap_or(true) {
                env::set_var(name, was);
            }
        }
    }
}

/// Environment variables a test pins, **put back when the test ends however it ends.**
///
/// The lock above and this are two different guarantees, and having one has repeatedly been read as
/// having the other. [`env_lock`] stops a *concurrent* test seeing a half-written environment, and
/// since SKEIN-705 it also puts the environment back when it is released. What it cannot do is put a
/// variable back at the right *moment*: its guard is bound at the top of a test and therefore drops
/// last, after the `TempDir` the variable names has already been removed. This drops where the test
/// says it does, so `$SKEIN_HOME` stops naming a directory before that directory goes.
///
/// **So [`EnvGuard`] is the floor and this is the fix.** The floor exists because a test that leaks
/// is by definition a test nobody noticed leaking: SKEIN-696 is `src/repos.rs` leaking a fleet root,
/// a later test reading it instead of failing for want of a pin of its own, and two defects
/// cancelling out into a green suite — `tools/env-lock-check.py` passed all day, because the lock
/// was held. Restoring at the lock boundary makes that particular pair impossible. It does not make
/// a test that names its own variables unnecessary, and the gate still asks for one.
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

/// **No warden**, for the width of the returned guard — `$SKEIN_WARDEN` pinned at an address
/// where nothing listens.
///
/// [`crate::warden_client::Warden::send_within`] refuses a test process that has not said which
/// warden to ask, rather than opening a connection to `host.docker.internal:7879` — whatever
/// warden the machine running the tests can reach (SKEIN-762). Most of the tests that reach it
/// never meant to: they destroy a fixture box, and `sandbox::destroy_box` reports the destroy into
/// the host audit log (§9.5 R6). Thirty-one lib tests were doing that when the guard went in.
///
/// What those tests want is *no warden at all*, and that is what this is. Port 1 on loopback is
/// refused by the kernel before a packet leaves the machine, so the call fails in microseconds,
/// nothing outside the fixture is asked anything, and a warden that accepted and stalled cannot
/// hold the suite open. It is the address `warden_client`'s own
/// `an_unreachable_warden_names_itself_and_the_fix` already uses for the same reason.
///
/// A test that is *about* the warden points `$SKEIN_WARDEN` at its own fake instead, or builds
/// one with [`crate::warden_client::Warden::at`], which is explicit and therefore never refused.
pub(crate) fn no_warden() -> EnvPins {
    let mut pins = env_pins();
    pins.set("SKEIN_WARDEN", "127.0.0.1:1");
    pins
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
/// [`tempdir`] and the server binary's copy of its naming (`scratch_dir` in
/// `src/bin/skein-server/main.rs`, which cannot reach this module) creates — and only what is older than
/// [`STALE`], so a run beside this one is safe.
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

/// A box-like namespace a test started, and everything inside it that must die with it — **on the
/// panicking path as much as the returning one.**
///
/// Both in-crate fixtures that make one spawn a `bwrap` whose `bash -c` execs a `sleep`, with no
/// `--unshare-pid` — so the sleeping anchor is an ordinary process in this pid namespace that bwrap
/// is merely *waiting on*. Killing the bwrap first does not end it, it orphans it, and it then runs
/// out its full minute at `ppid=1` (SKEIN-1005). The ordering in the drop below is that fix; this
/// type is the half SKEIN-1005 left, because the teardown it wrote was four statements at the bottom
/// of a test body with four assertions and an `expect` between them and the spawn. A run that PASSED
/// left nothing behind and a run that FAILED stranded both — the worst possible distribution, since
/// the reader is already looking at a red and `tests/ui/harness/leaks.mjs` then puts a second red on
/// top of the one they came to read (SKEIN-913).
///
/// **One caller so far, and the other is filed rather than assumed.**
/// `place::tests::a_crossing_in_the_fleet_enters_the_box_without_sbx` uses this;
/// `fleet::stop::tests::a_stop_reaches_what_walked_out_of_the_tmux_tree` is the second fixture of the same
/// shape and still has the two trailing statements — three processes to strand rather than one, and
/// its `while :; do sleep 0.5; done` has no minute to run out at all. That is SKEIN-1011, and the
/// `Vec` below is a `Vec` for it rather than for the single anchor today's caller records.
///
/// `tests/ui/lift.mjs`'s `boxlikeNamespace` has had this since SKEIN-861: it registers its `stop`
/// with `quiesceOnExit`, which runs on a throw and on a Ctrl-C as well as on a normal return. This
/// is that coverage in the Rust tier, where `Drop` is the only thing a panic is guaranteed to run.
///
/// **A recorded pid is killed only while it is still the process that was recorded.** Pids recycle,
/// this box is shared between checkouts, and a `pkill` here once reaped 72 tmux servers whose owners
/// could not afterwards be named — which `fixtureScopes` in `tests/ui/harness/leaks.mjs` records.
/// So [`Self::inside`] reads the pid's `starttime` as it records it, and the drop reads it again at
/// the moment of killing: a number that has changed means a stranger wears that pid now, and nothing
/// is sent. Reading it **once**, here, is also what stops the stamp a caller writes into a placement
/// record from differing from the stamp compared against at kill time — `inside` hands back the
/// number it recorded, so there is one read and no second parse of field 22 to drift from it.
///
/// **Nothing in the drop can panic**, which matters more here than usual: a panic in a `Drop` during
/// an unwind aborts the process, so a guard that unwrapped would turn a clean test failure into an
/// abort that says nothing about the assertion that failed — the failure this exists to make legible
/// made illegible instead. Every fallible step there is consumed rather than unwrapped, by way of
/// [`started_at`] — `read_to_string().ok()`, then [`crate::place::parse_proc_starttime`], which is
/// already an `Option`, then `unwrap_or(0)`, where `0` is a value the comparison rejects rather than
/// a value it trusts. The child's `kill` and `wait` are `let _ =`, and `libc::kill` reports by
/// return value and panics on nothing. There is no indexing, no slicing and no assertion.
pub(crate) struct BoxlikeNamespace {
    /// The `bwrap` itself — this process's own child, so it can be both signalled and reaped.
    outer: std::process::Child,
    /// `(pid, starttime)` for every process *inside* the namespace the fixture has named. Killed in
    /// the order they were recorded, and all of them before `outer`.
    held: Vec<(u32, u64)>,
}

impl BoxlikeNamespace {
    /// Take ownership of a spawned `bwrap`.
    ///
    /// Do this **at the spawn**, before the wait for the anchor to report itself: that wait ends in
    /// a `panic!` of its own when the namespace never starts, and until the child is in here that
    /// panic strands the very bwrap it is complaining about.
    pub(crate) fn holding(outer: std::process::Child) -> Self {
        Self {
            outer,
            held: Vec::new(),
        }
    }

    /// Record a process inside the namespace, and answer with the `starttime` recorded for it.
    ///
    /// `0` when `/proc` would not say — which is also the value that makes a placement record
    /// unprovable, so a caller that stamps an address with it is refused rather than handed a false
    /// one. A `0` is never killed on either: there is nothing to tell the process from a stranger
    /// that inherited its pid.
    pub(crate) fn inside(&mut self, pid: u32) -> u64 {
        let started = started_at(pid);
        self.held.push((pid, started));
        started
    }
}

/// Field 22 of `/proc/<pid>/stat`, or `0` — read through the parser the crossing's own anchor guard
/// uses, so the fixture cannot disagree with production about what a stamp is.
fn started_at(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| crate::place::parse_proc_starttime(&stat))
        .unwrap_or(0)
}

impl Drop for BoxlikeNamespace {
    /// The anchor goes first and bwrap second, because killing bwrap first is precisely what orphans
    /// the anchor (SKEIN-1005): bwrap is what was *waiting* on it.
    fn drop(&mut self) {
        for &(pid, started) in &self.held {
            if started == 0 || started_at(pid) != started {
                continue;
            }
            // SAFETY: `kill` has no memory effects, and `pid` is one a fixture in this process read
            // out of its own namespace, checked on the line above to still be the same process it
            // was when it was recorded.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        let _ = self.outer.kill();
        let _ = self.outer.wait();
    }
}

// ---------------------------------------------------------------------------------------------
// Skipping, out loud — and refusable
// ---------------------------------------------------------------------------------------------

/// The variable that turns every skip in the **library's** tests into a failure.
///
/// The same name `tests/common/mod.rs` uses, and that is load-bearing rather than a coincidence:
/// one `SKEIN_TESTS_NO_SKIP=1` has to mean "no skips anywhere" or it means very little.
/// `tests/platform_gates.rs` reads both files and fails when the two spellings drift apart, because
/// a rename on one side leaves a switch that still looks like it covers the whole tree.
pub(crate) const NO_SKIP: &str = "SKEIN_TESTS_NO_SKIP";

/// Say that this test is not running, and why. Written to be the whole of the guard:
///
/// ```ignore
/// if !have_jq() {
///     skip("no jq, so the queue cannot be read at all");
///     return;
/// }
/// ```
///
/// **Deliberately a second implementation of `tests/common/mod.rs::skip` rather than a shared one.**
/// The crate boundary is the reason, exactly as it is for [`bwrap_works`] a few lines up: this
/// module is `#[cfg(test)] mod testutil` inside the library, so no integration binary can reach it,
/// and the only way to serve both from one copy would be to make test scaffolding `pub` in the
/// shipped library. What cannot be de-duplicated is instead *checked* — `tests/platform_gates.rs`
/// holds the two to the same variable name, which is the part that could silently diverge and the
/// part that matters, since the whole value of the switch is that one setting covers everything.
///
/// **Why printing is not enough.** `cargo test` captures a passing test's output and a skipped test
/// passes, so the notice below is invisible in precisely the run where it matters. Not hypothetical:
/// this switch reached only `tests/*.rs` from the day it was written, so the library's own binary —
/// the largest test surface in the tree — answered a run that had asked for no skips with fifteen of
/// them and reported nothing (SKEIN-790). With the variable set this panics, the test fails, and
/// cargo cannot hide a failing test's output.
///
/// **The reason still reaches a reader on an ordinary run.** Making skips refusable must not make
/// them silent, so the unset path prints what the bare `eprintln!`s it replaced printed.
///
/// `#[track_caller]` so the message names the guard rather than this function.
#[track_caller]
pub(crate) fn skip(why: &str) {
    let at = std::panic::Location::caller();
    let where_ = format!("{}:{}", at.file(), at.line());
    refuse_or_say(std::env::var_os(NO_SKIP).is_some(), &where_, why);
}

/// The whole of [`skip`]'s behaviour, with the one thing it reads from the world passed in.
///
/// **Split out so its own test does not have to write `$SKEIN_TESTS_NO_SKIP` to drive it**, which
/// sounds like tidiness and is not. Cargo runs a binary's tests as threads of ONE process; the guards
/// that call `skip` do not take [`env_lock`] and could not usefully be made to, since most already
/// hold it. A test that cleared the variable to prove the quiet arm would therefore open a window in
/// which a *concurrent* guard printed instead of panicking — and the run would then report a green
/// `SKEIN_TESTS_NO_SKIP=1`, meaning "nothing was skipped", having skipped something. That is
/// precisely the defect this function exists to close, so its test must not be able to cause it.
///
/// The line this leaves untested by behaviour is the `var_os` read in `skip` above, and that is
/// checked textually instead, by
/// `tests/platform_gates.rs::the_library_and_the_suite_ask_for_no_skips_with_the_same_variable`.
fn refuse_or_say(asked_for_no_skips: bool, where_: &str, why: &str) {
    if asked_for_no_skips {
        panic!(
            "SKIPPED at {where_}: {why}\n{NO_SKIP} is set, which asks for a run where nothing is \
             skipped — install what this needs (tests/common/mod.rs lists it under `lib`) or unset \
             the variable"
        );
    }
    eprintln!("SKIPPED at {where_}: {why}");
}

/// The server binary's source, EVERY file of it, for the library tests that read it as text.
///
/// It was one file, and they read it with `include_str!`. It is a directory now (SKEIN-1103), and
/// most of those reads are a `!contains` — a field that must not come back, a second caller that
/// must not appear — which a read of one of its files would pass over the others. So the directory
/// is read rather than a list of it, and a read that finds `main.rs` alone refuses rather than let
/// a `!contains` pass over too little text. `main.rs` first and the rest by name; the binary's own
/// tests read it the same way, through its own `server_source`.
pub(crate) fn server_source() -> &'static str {
    static SOURCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SOURCE.get_or_init(|| {
        let dir =
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bin/skein-server"));
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".rs"))
            .collect();
        names.sort_by_key(|n| (n != "main.rs", n.clone()));
        assert!(
            names.len() > 1 && names[0] == "main.rs",
            "read {names:?} out of {} — that is not the server's source, and every source \
             assertion made over it would be about nothing",
            dir.display()
        );
        names
            .iter()
            .map(|n| fs::read_to_string(dir.join(n)).unwrap_or_else(|e| panic!("{n}: {e}")))
            .collect()
    })
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
        // Every name is spelled as a literal, here and in the inner guard's own `set`/`unset`
        // calls below, so that `tools/env-lock-check.py` can read exactly what this test touches.
        // The fixture's own before/after state is pinned through `EnvPins` for the same reason the
        // rest of the suite is (SKEIN-723): a bare trailing `remove_var` here would be unwound past
        // by a failing assertion below it, leaking these two names into whatever test runs next —
        // which would be more than a little ironic in the test that proves that exact remedy.
        let _lock = env_lock();
        let mut outer = env_pins();
        outer
            .set("SKEIN_TESTUTIL_PIN_HELD", "before")
            .set("SKEIN_TESTUTIL_PIN_TWICE", "before")
            .unset("SKEIN_TESTUTIL_PIN_ABSENT");

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
    }

    /// Is this pid still the process that was stamped, and still doing something?
    ///
    /// Two halves, and dropping either makes this lie in a different direction. `SIGKILL` makes a
    /// **zombie** of anything whose parent has not reaped it yet, and `/proc/<pid>` outlives the
    /// process by exactly that window — so presence alone would report a killed process as running.
    /// And the **stamp** is the pid-reuse guard the rest of this file turns on: a pid stops naming
    /// the same thing the moment it is free, so a bare number would report a stranger as ours.
    fn still_running(pid: u32, stamp: u64) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        if crate::place::parse_proc_starttime(&stat) != Some(stamp) {
            return false;
        }
        stat.rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z")
    }

    /// **A panicking test takes the process its fixture started with it**, which is the whole of
    /// [`BoxlikeNamespace`].
    ///
    /// A happy-path version would pass against exactly what this replaces. SKEIN-1005's teardown
    /// was four statements at the bottom of a test body, and it reaps perfectly for as long as
    /// nothing above it fails; what it cannot do is reap on the path that matters. The failing run
    /// is both the one that still has something running and the one whose reader can least afford a
    /// second red on top of the one they came to read (SKEIN-913). So the panic is the fixture here
    /// rather than decoration.
    ///
    /// **The shape is the real one and not a convenient one.** The `sleep` is backgrounded and
    /// reports its OWN pid, so the guard's child is the shell and the recorded process is a
    /// GRANDCHILD — which is the arrangement that makes an anchor a separate thing to kill at all,
    /// because killing the shell alone only reparents the sleep to pid 1 to run out its clock. No
    /// `bwrap`: what is under test is the guard and not the namespace, and a bwrap here would make
    /// this skip on every machine that refuses an unprivileged user namespace, which is the machine
    /// CI runs on.
    ///
    /// **The concrete change that makes it fail**, run before this sentence was written: emptying
    /// the `for` loop in `Drop for BoxlikeNamespace` — or moving that kill back out into a
    /// statement after the `panic!` — leaves the sleep alive, and the assertion fires naming its
    /// pid. Deleting the `impl Drop` outright fails the same assertion for the same reason.
    ///
    /// It cleans up **before** it asserts, so a failing run of this test is not itself the leak it
    /// is about.
    #[test]
    fn a_panicking_test_takes_the_process_its_boxlike_namespace_started() {
        let dir = tempdir();
        let pidfile = dir.join("inner");
        let shell = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 60 & echo $! > {}; wait", pidfile.display()))
            // Nulled for the reason the bwrap fixtures null theirs: a process that outlives this
            // holding an inherited pipe open makes `cargo test` look like a hang long afterwards.
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start a shell that backgrounds a sleep");
        let mut held = BoxlikeNamespace::holding(shell);
        let inner: u32 = {
            let mut found = None;
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(&pidfile) {
                    if let Ok(pid) = text.trim().parse() {
                        found = Some(pid);
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // Panicking here is safe in the sense this whole type is about: `held` already owns the
            // shell, so the guard drops on the way out and the fixture does not survive its own
            // failure to report.
            found.expect("the fixture never reported the pid it backgrounded")
        };
        // Stamped by this test from `/proc` **before** the guard is told, and the two compared:
        // everything below observes the process through this test's own number, so a guard that
        // recorded the wrong one fails the assertion that says so rather than the premise. (Found
        // by sabotaging `inside` to record `started_at(pid) + 1`: with a shared number, the
        // corruption surfaced as "was not running to begin with", which names the wrong defect.)
        let stamp = started_at(inner);
        assert!(
            stamp > 0,
            "/proc would not stamp pid {inner}, so this test cannot tell it from a stranger later \
             and proves nothing"
        );
        assert_eq!(
            held.inside(inner),
            stamp,
            "the guard recorded a different start time than /proc reports for pid {inner} — it \
             would then decline to kill its own anchor, mistaking it for a recycled pid"
        );
        assert!(
            still_running(inner, stamp),
            "the fixture's own process was not running to begin with"
        );

        // Caught rather than allowed to fail the test, and the hook silenced so the deliberate
        // panic does not read as a failure in the output — the idiom
        // [`a_test_that_panics_still_puts_the_environment_back`] above uses, for the same reason.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = held;
            panic!("as a failing assertion would");
        }));
        std::panic::set_hook(hook);
        assert!(outcome.is_err(), "the closure was supposed to unwind");

        // Read by PID and stamp, never by matching a program name: a pattern over names is how a
        // check came to answer `0` on a box carrying 195 matching processes (SKEIN-647).
        let mut gone = false;
        for _ in 0..100 {
            if !still_running(inner, stamp) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !gone {
            // SAFETY: `kill` has no memory effects, and `inner` is a pid this test's own shell
            // reported into this test's own temporary directory, checked on the line above to still
            // be the same process it was when it was stamped.
            unsafe { libc::kill(inner as libc::pid_t, libc::SIGKILL) };
        }
        assert!(
            gone,
            "the process this fixture started (pid {inner}) outlived the panic that unwound past \
             its guard — a test that leaks when it fails poisons every later run in the same \
             worktree, which is SKEIN-1008 back"
        );
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

    /// [`EnvGuard`] puts a variable back however the scope that took the lock ended — **including
    /// when it ended by panicking**, which is the half no trailing `remove_var` has ever covered.
    ///
    /// **Why it tampers with `$SKEIN_TEST` of all things, and why that is inert.** Proving that a
    /// value is *restored* rather than merely removed needs a variable that already had one when the
    /// lock was taken, and the only writes that survive to be observed here are the ones the process
    /// started with: a value this test set for itself beforehand would have to be written OUTSIDE the
    /// lock, and any concurrent guard's restore would erase it between the write and the assertion.
    /// That is not hypothetical — it is how the first draft of this test failed, once, in a full
    /// parallel run. `$SKEIN_TEST` is process-ambient (`.cargo/config.toml`'s `[env]` table puts it
    /// in every `cargo test` binary, and `tests/harness.rs` asserts it arrives), and nothing in this
    /// crate writes it. Tampering with its VALUE cannot mislead [`crate::util::in_test`] either:
    /// that reads `cfg!(test) || …is_some_and(|v| !v.is_empty())`, which is already true in this
    /// binary on the first term, and the value written below is non-empty regardless.
    ///
    /// The ambient value is read rather than assumed, so this still asserts something when the
    /// marker is absent or set to something else — a run straight from the binary, as
    /// `tools/alone-check.py` does, rather than through cargo.
    ///
    /// Taking the lock around the whole test instead is not an option: `Mutex` is not re-entrant, so
    /// an outer guard would deadlock against the inner ones this test exists to watch drop.
    ///
    /// Each assertion was named against the change that breaks it, and each was watched failing
    /// while the other two passed:
    ///
    ///   · *restored after returning* — empty out `EnvGuard::drop`'s body; it reads the tampered
    ///     value.
    ///   · *restored after panicking* — return early from `EnvGuard::drop` when
    ///     `std::thread::panicking()`; the other two stay green.
    ///   · *left unset* — delete the loop in `EnvGuard::drop` that removes names the guard did not
    ///     snapshot; it reads `added`, and the other two stay green.
    #[test]
    fn the_env_lock_guard_puts_the_environment_back_however_the_scope_ended() {
        const FRESH: &str = "SKEIN_ENVGUARD_PROBE_FRESH";

        // Read under the lock, so it is the very value the guard below snapshotted.
        let ambient = {
            let _g = env_lock();
            let was = env::var_os(crate::util::TEST_MARKER);
            env::set_var(crate::util::TEST_MARKER, "tampered-by-this-probe");
            assert_eq!(
                env::var(crate::util::TEST_MARKER).unwrap(),
                "tampered-by-this-probe",
                "the write itself did not land, so nothing below is testing a restore"
            );
            was
        };
        assert_eq!(
            env::var_os(crate::util::TEST_MARKER),
            ambient,
            "a scope that took the env lock and RETURNED left the environment changed — every \
             later test in this binary that reads that variable is now reading this one's \
             leftovers, which is SKEIN-705"
        );

        // The hook is silenced so the deliberate panic does not read as a failure in the output,
        // and put back before any assertion runs, exactly as the pins test above does.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let fell = std::panic::catch_unwind(|| {
            let _g = env_lock();
            env::set_var(crate::util::TEST_MARKER, "tampered-while-unwinding");
            panic!("as a failing assertion would");
        });
        std::panic::set_hook(hook);
        assert!(
            fell.is_err(),
            "the scope was supposed to unwind and did not"
        );
        assert_eq!(
            env::var_os(crate::util::TEST_MARKER),
            ambient,
            "a scope that PANICKED left the environment changed. This is the case a trailing \
             `remove_var` cannot cover, because a failing assertion unwinds straight past it — so \
             one red test would poison every test after it as well"
        );

        {
            let _g = env_lock();
            env::set_var(FRESH, "added");
        }
        assert!(
            env::var_os(FRESH).is_none(),
            "a variable that was unset when the lock was taken came back SET, as {:?}. Restoring \
             a name to empty rather than removing it reads as PRESENT to `env::var_os`, which is \
             how `config::skein_home`'s SKEIN-626 refusal gets quietly answered instead of raised",
            env::var_os(FRESH)
        );
    }

    /// A library skip is invisible in a green run by construction, and this is what makes it visible.
    ///
    /// The mirror of `tests/harness.rs`'s
    /// `a_skip_becomes_a_failure_when_the_run_asked_for_a_run_with_no_skips`, and it has to be a
    /// mirror rather than a second caller of the same test: that file is an integration binary, the
    /// library is compiled for it *without* `--cfg test`, and `crate::testutil` therefore does not
    /// exist over there at all. So the only place the library half of this switch can be proved is
    /// inside the library.
    ///
    /// **Both arms, because only the pair is the behaviour.** Without the variable a skip must stay
    /// an ordinary early return, or every machine missing one tool goes red; with it set the same
    /// call must fail the test. A test that only checked the second arm would pass against a `skip`
    /// that panicked unconditionally.
    ///
    /// **What makes it fail:** deleting the `if asked_for_no_skips` branch from [`refuse_or_say`] —
    /// the `loud` arm's `expect_err` then fires. Making that branch unconditional fails the `quiet`
    /// arm instead. Both were run, and each broke only its own assertion.
    ///
    /// **It drives [`refuse_or_say`] rather than [`skip`], and does not touch the environment.** The
    /// reason is written out at that function; in short, a test that cleared the variable here could
    /// make a concurrent guard in another thread skip quietly during a run that had asked for no
    /// skips, which is the failure being fixed rather than a way to test it.
    #[test]
    fn a_library_skip_becomes_a_failure_when_the_run_asked_for_a_run_with_no_skips() {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let at = "src/testutil.rs:1";
        let quiet = std::panic::catch_unwind(|| {
            refuse_or_say(false, at, "a tool this machine does not have")
        });
        let loud = std::panic::catch_unwind(|| {
            refuse_or_say(true, at, "a tool this machine does not have")
        });
        std::panic::set_hook(hook);

        assert!(
            quiet.is_ok(),
            "an ordinary skip panicked, which would fail this crate's tests on every machine \
             without jq, node or tmux rather than skipping the handful that need them"
        );
        let said = loud.expect_err(
            "a skip stayed silent under the variable that exists to forbid silent skips",
        );
        let said = said
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "<not a string>".into());
        assert!(
            said.contains("a tool this machine does not have") && said.contains("src/testutil.rs"),
            "the failure has to name the reason and the guard that took it, or it says no more than \
             `ignored` would: {said}"
        );
    }
}
