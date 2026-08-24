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
}
