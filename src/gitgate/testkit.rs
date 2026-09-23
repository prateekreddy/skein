//! Test helpers shared by more than one of this module's test files.

pub(super) fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-08-13T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// A fresh `$SKEIN_HOME`, plus the guards that put it back.
///
/// The pins come **last** so they drop **first**: bindings from one `let` are dropped in
/// reverse, so `$SKEIN_HOME` stops naming the temp directory before the temp directory is
/// removed, and it is put back on the unwinding path as well as the passing one.
pub(super) fn fresh_home() -> (
    crate::testutil::EnvGuard,
    crate::testutil::TempDir,
    crate::testutil::EnvPins,
) {
    let lock = crate::testutil::env_lock();
    let home = crate::testutil::tempdir();
    let mut env = crate::testutil::env_pins();
    env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
    (lock, home, env)
}
