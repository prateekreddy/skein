//! Fixtures shared by every module's tests.
//!
//! `#[cfg(test)]` only, so none of this is in a release build. It lives in its own module because
//! the tests moved out of `lib.rs` to sit beside the code they exercise, and three of these — the
//! env lock especially — are process-global and must be *the same* value for every test in the
//! crate, not a per-module copy.

use crate::place::{record_place, PlaceRecord};
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
        },
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

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
