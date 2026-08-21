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
