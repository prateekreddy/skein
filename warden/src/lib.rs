//! The host warden: the small service that owns fleet create and destroy.
//!
//! It exists because **create and destroy both terminate skein** — create because skein does not
//! exist yet, destroy because it will not afterwards — so fleet lifecycle cannot live inside the
//! fleet, permanently (architecture §8). That is a boundary rather than a limitation.
//!
//! A separate crate, with no dependency on `skein`. §14 gives this module an empty depends-on column
//! and the note "(separate binary)", and the reason is the whole point of the component: the warden
//! is what a compromised skein has to get past. A shared library is a shared blast radius.

pub mod approval;
pub mod audit;
pub mod capability;
pub mod doer;
pub mod flooding;
pub mod outcome;
pub mod secret;
pub mod serve;
pub mod sightings;
pub mod wire;

/// Where the warden's state lives: `$SKEIN_WARDEN_HOME` if set and non-empty — the explicit
/// override, for tests and development — else `warden/` under the volume root, `$SKEIN_HOME` or
/// `~/.skein`.
///
/// **Derived from the volume root rather than fixed at `~/.skein/warden`, because of the secret
/// in it.** Architecture §9.5 R5 puts the secret "under the cover of requirement 2", and that
/// cover is derived over `$SKEIN_HOME` — so a fixed home held R5 only while the volume sat at its
/// default. Point the volume elsewhere (the documented backup flow does: `SKEIN_HOME=…
/// skein repoint`) and the cover moved while the secret stayed behind, uncovered. Deriving the
/// home from the same root the cover is derived over makes R5 hold for every volume location by
/// construction. Setting `$SKEIN_WARDEN_HOME` is the operator explicitly stepping outside the
/// covered world; nothing re-derives a cover over it.
///
/// The same chain is spelled in skein's `warden_client` (`src/warden_client.rs`) — two copies on
/// purpose, because this crate depends on nothing of skein's (see the crate note above), and the
/// roundtrip test in `tests/warden_roundtrip.rs` is what fails if the two ends stop agreeing.
pub fn home() -> std::path::PathBuf {
    if let Some(overridden) = std::env::var_os("SKEIN_WARDEN_HOME").filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(overridden);
    }
    std::env::var_os("SKEIN_HOME")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
                .join(".skein")
        })
        .join("warden")
}

/// One test at a time where the environment is the thing under test.
///
/// `$SKEIN_WARDEN_LS_CMD` is process-wide, and two tests setting and removing it in parallel is a
/// flake that looks like a failure of whatever they were actually testing — here it read as "a flood
/// of proposals made skein unable to start", which is alarming and was untrue.
#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A test's own directory under the system temp dir, **removed when the test ends — unless it
/// panicked**, in which case it is kept and its path printed (SKEIN-557).
///
/// Every fixture in this crate used to be a bare `temp_dir().join(..)` that nothing removed: one
/// directory per test per run, and `/tmp` on the owner's box held more than five thousand
/// `warden-secret-*` alone. The integration suite's sweep tidied some of them as a side effect,
/// which works until somebody runs only these tests.
///
/// Kept on failure for the rule `tests/common/mod.rs`'s `Scratch` follows: the directory a failing
/// test leaves is the evidence of what it did, and deleting it on the way out of the panic is a
/// debugging session lost.
///
/// Named `<prefix>-<pid>-t<thread>`, as the fixtures it replaces were near enough, so a name still
/// says which test left it. **Bind it to a name** — `Scratch::new(..).join(..)` drops the guard at the
/// end of that statement and removes the directory before the test has used it.
#[cfg(test)]
pub(crate) struct Scratch(std::path::PathBuf);

#[cfg(test)]
impl Scratch {
    /// A fresh directory, emptied of anything a killed earlier run left under the same name.
    pub(crate) fn new(prefix: &str) -> Scratch {
        let dir = Scratch::fresh(prefix);
        std::fs::create_dir_all(&dir.0).expect("creating a test's scratch directory");
        dir
    }

    /// The same, but NOT created — for a test whose subject is what happens where nothing is yet.
    pub(crate) fn fresh(prefix: &str) -> Scratch {
        // The thread's number and not its `Debug` form: `ThreadId(7)` puts parentheses in a path
        // that several of these tests write, unquoted, into a shell script.
        let thread: String = format!("{:?}", std::thread::current().id())
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-t{thread}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Scratch(dir)
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "skein-warden test: kept {} for the failure above",
                self.0.display()
            );
            return;
        }
        // Best-effort, and it must not panic: a test that passed is not made to fail by its
        // cleanup. Modes first, because a directory a test made unwritable cannot be emptied.
        reopen(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Make a tree removable again. Symlinks are skipped: `set_permissions` follows one, and the target
/// need not be inside the tree — `src/testutil.rs`'s `reopen` learned that the expensive way.
#[cfg(test)]
fn reopen(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if dir.is_symlink() {
        return;
    }
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            reopen(&path);
        }
    }
}

#[cfg(test)]
impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(test)]
impl AsRef<std::path::Path> for Scratch {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(test)]
impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

/// Where the warden's RECORD lives — the audit log and the outcomes beside it — which is
/// deliberately **not** where its secret lives.
///
/// `$SKEIN_WARDEN_AUDIT` if set and non-empty, else `$SKEIN_WARDEN_HOME` when the operator has
/// overridden the whole home (tests and development keep one scratch directory rather than two),
/// else `~/.skein-warden` on the host — beside the volume, never inside it.
///
/// **Why the home splits at all.** [`home()`] follows the volume root so the secret stays under
/// the cover derived over that root (§9.5 R5). Delivery step 4c mounts that volume INTO the fleet
/// sandbox, where skein runs — so a home that is right for the secret is exactly wrong for the log:
/// §5 argues skein cannot audit itself, and architecture.md's queue table says it in one line — "a
/// record of an approval must not live where the thing being audited can reach it". The two halves
/// want opposite things from the same directory, so they no longer share one.
///
/// `~/.skein-warden` rather than `~/.skein/warden-log`: the volume is what gets mounted, and a
/// sibling of it is outside by construction wherever the volume is pointed. A path *inside*
/// `~/.skein` would be back on the volume the day somebody repointed it.
///
/// The outcomes go with the log rather than with the secret because they are the same kind of
/// thing — what happened, written by the approving side — and because nothing of skein's reads
/// them off disk: it asks the warden over the socket (`src/warden_client.rs`).
pub fn audit_home() -> std::path::PathBuf {
    if let Some(explicit) = std::env::var_os("SKEIN_WARDEN_AUDIT").filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(explicit);
    }
    if let Some(overridden) = std::env::var_os("SKEIN_WARDEN_HOME").filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(overridden);
    }
    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
        .join(".skein-warden")
}

#[cfg(test)]
mod tests {
    /// The home follows the volume, and the override steps outside it.
    ///
    /// The middle rung is the one this exists for: `$SKEIN_HOME` set and `$SKEIN_WARDEN_HOME` not
    /// is the repointed-volume case, where the old fixed default left the secret uncovered.
    #[test]
    fn the_home_is_derived_from_the_volume_root() {
        let _g = super::env_lock();
        let keep: Vec<_> = ["SKEIN_WARDEN_HOME", "SKEIN_HOME", "HOME"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();

        std::env::set_var("HOME", "/host/home");
        std::env::remove_var("SKEIN_WARDEN_HOME");
        std::env::remove_var("SKEIN_HOME");
        assert_eq!(
            super::home(),
            std::path::PathBuf::from("/host/home/.skein/warden"),
            "the default install must resolve exactly where it always has"
        );

        std::env::set_var("SKEIN_HOME", "/mnt/backup/.skein-backup");
        assert_eq!(
            super::home(),
            std::path::PathBuf::from("/mnt/backup/.skein-backup/warden"),
            "a repointed volume must carry the warden's home with it, or the secret sits \
             outside the cover (§9.5 R5)"
        );

        // Empty is unset — the same reading `skein_home()` gives `$SKEIN_HOME` (src/config.rs).
        std::env::set_var("SKEIN_WARDEN_HOME", "");
        assert_eq!(
            super::home(),
            std::path::PathBuf::from("/mnt/backup/.skein-backup/warden")
        );

        std::env::set_var("SKEIN_WARDEN_HOME", "/somewhere/deliberate");
        assert_eq!(
            super::home(),
            std::path::PathBuf::from("/somewhere/deliberate"),
            "the explicit override is the operator leaving the covered world, and it wins"
        );

        for (k, v) in keep {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// The record does NOT follow the volume, because at 4c the volume is inside the fleet
    /// (SKEIN-218).
    #[test]
    fn the_audit_home_is_never_inside_the_volume() {
        let _g = super::env_lock();
        let keep: Vec<_> = [
            "SKEIN_WARDEN_HOME",
            "SKEIN_WARDEN_AUDIT",
            "SKEIN_HOME",
            "HOME",
        ]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect();

        std::env::set_var("HOME", "/host/home");
        for k in ["SKEIN_WARDEN_HOME", "SKEIN_WARDEN_AUDIT", "SKEIN_HOME"] {
            std::env::remove_var(k);
        }
        assert_eq!(
            super::audit_home(),
            std::path::PathBuf::from("/host/home/.skein-warden"),
            "the record must sit beside the volume, not in it"
        );

        // The case the split exists for: a repointed volume moves the secret and must not move the
        // log with it — wherever the volume is pointed, that is a place skein can write.
        std::env::set_var("SKEIN_HOME", "/mnt/backup/.skein-backup");
        assert_eq!(
            super::home(),
            std::path::PathBuf::from("/mnt/backup/.skein-backup/warden")
        );
        assert_eq!(
            super::audit_home(),
            std::path::PathBuf::from("/host/home/.skein-warden"),
            "the volume moved and took the record with it — the audited thing can reach it there"
        );

        // One scratch directory for a test or a development run, both halves in it.
        std::env::set_var("SKEIN_WARDEN_HOME", "/somewhere/deliberate");
        assert_eq!(
            super::audit_home(),
            std::path::PathBuf::from("/somewhere/deliberate")
        );
        // …and the narrower variable wins over it, for an operator who wants only the record moved.
        std::env::set_var("SKEIN_WARDEN_AUDIT", "/var/log/skein-warden");
        assert_eq!(
            super::audit_home(),
            std::path::PathBuf::from("/var/log/skein-warden")
        );

        for (k, v) in keep {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// **A fixture removes itself when its test passes, and keeps itself when its test panics**
    /// (SKEIN-557).
    ///
    /// Both halves, because each is the other's counterfactual. What makes the first fail: a `Drop`
    /// that does not remove — every fixture this crate made before this one. What makes the
    /// second fail: a `Drop` that removes regardless, which is the tidy-looking version that
    /// deletes a failing test's only evidence. A file is written inside and a subdirectory is left
    /// at mode 000, so "removed" means removed with contents, not an empty `rmdir` that happened
    /// to succeed.
    #[test]
    fn a_scratch_directory_goes_when_its_test_passes_and_stays_when_it_panics() {
        use std::os::unix::fs::PermissionsExt;
        let passed = {
            let dir = super::Scratch::new("skein-warden-scratch-passes");
            std::fs::write(dir.join("left"), "by the test").unwrap();
            let shut = dir.join("shut");
            std::fs::create_dir(&shut).unwrap();
            std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
            dir.to_path_buf()
        };
        assert!(
            !passed.exists(),
            "{} outlived a test that passed — the fixture leaks one directory per run",
            passed.display()
        );

        let failed = std::sync::Mutex::new(None);
        let unwound = std::panic::catch_unwind(|| {
            let dir = super::Scratch::new("skein-warden-scratch-panics");
            std::fs::write(dir.join("evidence"), "what the failing test did").unwrap();
            *failed.lock().unwrap() = Some(dir.to_path_buf());
            panic!("a test failing, on purpose");
        });
        assert!(unwound.is_err());
        let failed = failed
            .into_inner()
            .unwrap()
            .expect("the panicking half ran");
        let kept = failed.join("evidence").exists();
        let _ = std::fs::remove_dir_all(&failed);
        assert!(
            kept,
            "{} was deleted on the way out of a panic — the failing test's evidence went with it",
            failed.display()
        );
    }
}
