//! The helpers every integration binary in this directory needs, in one place.
//!
//! Cargo builds **one binary per `tests/*.rs`**, and `src/testutil.rs` is `#[cfg(test)] mod` inside
//! the library (`src/lib.rs:69-70`), so no integration test can reach it. The answer to that had
//! been a copy per file: seven byte-identical `have()`s, five `env_lock()`s carrying the same
//! eleven-line comment, twelve scratch-directory helpers. `tests/common/mod.rs` is the shape cargo
//! gives for this — a module, not a `tests/common.rs`, which would be compiled as a test binary of
//! its own with no tests in it.
//!
//! Everything here is `pub` and most of it is unused in any given binary, which is what the
//! `dead_code` allow is for: the module is compiled once per binary that declares `mod common;`,
//! and `-D warnings` would otherwise fail every binary that uses half of it.
//!
//! # What a machine needs to run this suite
//!
//! `REQUIREMENTS` below is the list, per binary, and `tests/platform_gates.rs` checks it against
//! the code rather than trusting it. A machine without one of these does not fail the suite — it
//! **skips**, through `skip()`, which is the other half of this file's job.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

// ---------------------------------------------------------------------------------------------
// Is this machine able to run the thing under test?
// ---------------------------------------------------------------------------------------------

/// Is `tool` on this machine's PATH?
pub fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Can bwrap actually make a namespace here — which `have("bwrap")` does not ask.
///
/// `have` answers "is it on PATH". On `ubuntu-24.04`, which is what CI runs, bubblewrap installs
/// cleanly and `kernel.apparmor_restrict_unprivileged_userns=1` then refuses the unprivileged user
/// namespace it needs, so the two answers differ exactly where it matters. Hosting a box needs the
/// namespace, not the binary — and a guard that asked the weaker question left two tests dead on CI
/// for 27 days.
///
/// Deliberately identical to `src/testutil.rs::bwrap_works`. The crate boundary is why there are
/// two, not a difference of opinion, and `.github/workflows/ci.yml` runs this exact command as a
/// step of its own before the suites so that a green CI run means no guard here was taken.
pub fn bwrap_works() -> bool {
    Command::new("bwrap")
        .args(["--dev-bind", "/", "/", "--", "/bin/true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Skipping, out loud
// ---------------------------------------------------------------------------------------------

/// The variable that turns every skip in the suite into a failure.
///
/// Set it on a machine that is supposed to have everything — CI, or a release check — and a green
/// run then means "everything ran", which a green run otherwise does not.
pub const NO_SKIP: &str = "SKEIN_TESTS_NO_SKIP";

/// Say that this test is not running, and why. Written to be used as the whole of the statement:
///
/// ```ignore
/// if !have("jq") {
///     return common::skip("jq is not installed");
/// }
/// ```
///
/// **Why this is not enough on its own, and what makes it enough.** `cargo test` captures a passing
/// test's output, and a skipped test passes — so a notice printed here is invisible in exactly the
/// run where it matters, which is how eighteen silent skips went unnoticed long enough to be worth
/// a finding. Grepping the log for these lines cannot be the check. `$SKEIN_TESTS_NO_SKIP` is: with
/// it set, this panics, the test fails, and cargo cannot hide a failing test's output. A green run
/// under that variable is a proof that nothing was skipped, and it needs nobody to read anything.
///
/// `#[track_caller]` so the message names the guard rather than this function.
#[track_caller]
pub fn skip(why: &str) {
    let at = std::panic::Location::caller();
    let where_ = format!("{}:{}", at.file(), at.line());
    if std::env::var_os(NO_SKIP).is_some() {
        panic!(
            "SKIPPED at {where_}: {why}\n{NO_SKIP} is set, which asks for a run where nothing is \
             skipped — install what this needs (tests/common/mod.rs lists it per binary) or unset \
             the variable"
        );
    }
    eprintln!("SKIPPED at {where_}: {why}");
}

/// What each test binary needs on the machine, beyond a Rust toolchain.
///
/// Checked against the code by `tests/platform_gates.rs`: a binary that skips must be listed here,
/// a listed binary must still skip, and every tool named must still be guarded on inside that file.
/// So this cannot quietly become fiction, which is the state it would otherwise reach — the tools
/// this suite needs were written down in no file at all before it existed.
///
/// `bwrap` means `bwrap_works()`, not the binary: see its doc comment.
pub const REQUIREMENTS: &[(&str, &[&str])] = &[
    ("browser_suites", &["node", "chromium"]),
    ("docker_watchdog", &["python3"]),
    ("fleet_launch", &["bwrap", "tmux", "git"]),
    ("fleet_move", &["tmux", "python3"]),
    ("git_write_request", &["jq", "git"]),
    ("isolation_bwrap", &["bwrap", "python3"]),
    ("mail_provenance", &["jq", "flock"]),
    ("server", &["python3"]),
    ("substrate_request", &["jq"]),
    ("turn_state_probe", &["jq"]),
    ("warden_roundtrip", &["cargo"]),
];

// ---------------------------------------------------------------------------------------------
// The env lock
// ---------------------------------------------------------------------------------------------

/// One lock per test binary, taken by every test that writes a process-global environment variable.
///
/// `std::env::set_var` writes a table shared by every thread in the process, and cargo runs a
/// binary's tests as parallel threads of ONE process — so a test that sets `SKEIN_HOME` is writing
/// into the middle of whatever else is running. The symptom is never at the site: SKEIN-307 was a
/// GitHub request-count assertion in `src/prq.rs` that failed once, passed on re-run, and had
/// nothing wrong with it. `tools/env-lock-check.py` is what keeps this taken as tests are added.
///
/// **One lock, not two.** Five files here held a second `Mutex<()>` of their own — `alone()`,
/// `serialize()` — and every test in them took both, back to back, in the same order, with no
/// helper taking either. Two mutexes acquired in one order by one set of callers exclude exactly
/// what one does; the pair could only have differed if some test took one without the other, and
/// none did. Collapsing them can only widen exclusion, never narrow it, and there is no
/// re-entrancy to deadlock because nothing below a `#[test]` takes this.
///
/// Poisoning is ignored, for the reason `src/testutil.rs` gives: the guarded data is `()`, and
/// cascading the first panic into every other test buries the real failure.
pub fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Scratch directories that go away
// ---------------------------------------------------------------------------------------------

/// Keep every scratch directory, whatever the outcome. For a passing test you want to look inside.
pub const KEEP: &str = "SKEIN_TESTS_KEEP_SCRATCH";

/// A scratch directory that removes itself — **except when the test failed.**
///
/// Half the files here removed their directory on the last line of the test, which a failing
/// assertion unwinds straight past, and the other half never removed one at all. On this box that
/// came to 1,082 directories and 1.6 GB in `/var/tmp`, every one of them a copy of `/usr/bin/git`
/// left by `tests/git_write_request.rs`.
///
/// **A `Drop` that always removed would be worse than the leak**: the directory is the only
/// evidence a failure leaves — the box's tree, its logs, what the fake `sbx` recorded — and a test
/// that tidies it away is a test nobody can debug. So `Drop` keeps the directory when the thread is
/// panicking and says where it is; `$SKEIN_TESTS_KEEP_SCRATCH` keeps it always.
///
/// **`quiesce` runs either way**, and that is not a detail. `tests/fleet_move.rs` starts a
/// supervisor whose loop condition is a file *inside* this directory, so keeping the directory
/// after a failure would keep restarting the server it supervises — which is how four leaked
/// processes were found on 2026-08-31. Whatever has to stop, stops here; only the removal is
/// conditional.
/// Whatever has to stop before the directory could go — see `Scratch`.
type Quiesce = Box<dyn Fn(&Path) + Send + Sync>;

pub struct Scratch {
    path: PathBuf,
    quiesce: Option<Quiesce>,
}

impl Scratch {
    /// `<root>/<prefix>-<pid>`, emptied first so a re-run in the same process starts clean.
    ///
    /// The pid suffix is the convention the sweep below reads, so it is applied here rather than
    /// spelled at each call site. Prefixes are each file's own and are deliberately unchanged:
    /// the leaked-process gate greps `ps` for `skein-fleet-it-` and `skein-move-it-`, and renaming
    /// them would turn that count into a zero that means nothing.
    pub fn at(root: impl AsRef<Path>, prefix: &str) -> Scratch {
        let root = root.as_ref().to_path_buf();
        sweep_abandoned(&root);
        let path = root.join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch {
            path,
            quiesce: None,
        }
    }

    /// Under `/var/tmp`, which is where anything that launches a box has to live: a box binds its
    /// own directories over `/tmp` and `$HOME`, so a box root beneath either is unreadable from
    /// outside and `box-session.sh` refuses it outright.
    pub fn boxes(prefix: &str) -> Scratch {
        Scratch::at("/var/tmp", prefix)
    }

    /// Under the ordinary temp directory, for the tests that never start a namespace.
    pub fn temp(prefix: &str) -> Scratch {
        Scratch::at(std::env::temp_dir(), prefix)
    }

    /// Something to run before the directory would be removed, on every path including a panic.
    pub fn quiesce_with(mut self, f: impl Fn(&Path) + Send + Sync + 'static) -> Scratch {
        self.quiesce = Some(Box::new(f));
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// So a scratch directory can be handed straight to `Command::env` and `Command::current_dir`,
/// which is what most of the call sites do with it.
impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(q) = self.quiesce.take() {
            q(&self.path);
        }
        if std::thread::panicking() || std::env::var_os(KEEP).is_some() {
            eprintln!(
                "kept {} — a failing test's scratch directory is the evidence; \
                 remove it by hand, or unset ${KEEP}",
                self.path.display()
            );
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Remove the scratch directories of runs that are over, once per binary.
///
/// A directory here is named `<something>-<pid>`, so "is that process still alive" is the whole
/// question, and it is asked of `/proc` rather than of a clock: two `cargo test` runs at once on
/// this box is the normal state, and an age rule would either sweep a live run's directory or leave
/// a dead one for hours. `skein-test-*` is left alone — those belong to `src/testutil.rs`, which
/// sweeps its own.
fn sweep_abandoned(root: &Path) {
    // Once per ROOT, not once per binary: a binary that uses both `/var/tmp` and the ordinary temp
    // directory would otherwise sweep whichever it reached first and leave the other for ever.
    static SWEPT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    {
        let mut swept = SWEPT.lock().unwrap_or_else(|e| e.into_inner());
        if swept.iter().any(|p| p == root) {
            return;
        }
        swept.push(root.to_path_buf());
    }
    {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("skein-") || name.starts_with("skein-test-") {
                continue;
            }
            let Some((_, tail)) = name.rsplit_once('-') else {
                continue;
            };
            let Ok(pid) = tail.parse::<u32>() else {
                continue;
            };
            if pid == std::process::id() || Path::new(&format!("/proc/{pid}")).exists() {
                continue;
            }
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}
