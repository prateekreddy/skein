//! Fixtures shared by every module's tests.
//!
//! `#[cfg(test)]` only, so none of this is in a release build. It lives in its own module because
//! the tests moved out of `lib.rs` to sit beside the code they exercise, and three of these — the
//! env lock especially — are process-global and must be *the same* value for every test in the
//! crate, not a per-module copy.

use crate::Sandbox;
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
/// **Two directories a run still survive this**, and they are not removal failures — instrumenting
/// `drop` showed `remove_dir_all` never erroring. They are *recreated* after the guard has removed
/// them, by work that outlives the test that started it: [`crate::Gate`] refreshes behind its
/// caller on a spawned thread, and a thread still running when the test ends writes through an
/// env var that still names the deleted path. Bounded and understood, against ~180 a run before.
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
        if path.is_dir() && !path.is_symlink() {
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

pub(crate) fn tempdir() -> TempDir {
    let d = env::temp_dir().join(format!(
        "skein-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&d).unwrap();
    TempDir(d)
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
#[cfg(unix)]
pub(crate) fn write_claude_stub(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("claude-stub.sh");
    fs::write(
        &p,
        "#!/bin/sh\nfor last; do :; done\ncase \"$last\" in\n  *'wire it up'*) echo ROUTINE ;;\n  *'which database'*) echo DECISION ;;\n  *Summarise*) echo 'It wired up the parser.' ;;\n  *) echo '?' ;;\nesac\n",
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
