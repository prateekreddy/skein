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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Env vars are process-global, and skein reads a dozen of them. Every test that sets one takes
/// this first, so a parallel run can't have one test's `$SKEIN_HOME` leak into another's.
///
/// One lock for the whole crate: a per-module lock would serialize each module against itself and
/// nothing else, which is the failure mode that looks like a flaky test.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh temp directory, unique per process and per call.
pub(crate) fn tempdir() -> PathBuf {
    let d = env::temp_dir().join(format!(
        "skein-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&d).unwrap();
    d
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
