//! Whether the skein you are running is the newest one, and the button that makes it so.
//!
//! # Two updates, and they were not the same shape
//!
//! The agent CLIs had this and skein did not. [`crate::fleet::runtime_updates`] has asked npm what
//! it would install since SKEIN-405 — the owner's words there are *"show that in the bar when there
//! is an update. You check if new version is out regularly."* — while skein's own revision was
//! stamped by `build.rs`, reported by `--version`, `skein doctor` and `/api/health`, and **compared
//! to nothing**. A person found out they were behind by updating and noticing the string changed.
//!
//! The doing already existed: `bootstrap.sh` fetches, builds and installs, and
//! [`crate::fleet::build_server_in_sandbox`] runs those exact bytes with
//! `SKEIN_BOOTSTRAP_STOP_AFTER=build`. What was missing was the telling.
//!
//! # Three revisions, because two of them are usually the same and the third is the question
//!
//! - **running** — [`crate::health::BUILD_REVISION`], stamped into the binary that is answering.
//! - **source** — `HEAD` of the checkout at [`crate::fleet::skein_source_path`], which is what a
//!   rebuild would compile if it fetched nothing.
//! - **remote** — what GitHub says the tracked ref is at.
//!
//! Keeping them apart is what makes the answer actionable rather than merely true. `remote !=
//! source` is *there is something to fetch*; `source != running` is *the binary is older than the
//! checkout it was built from*, which is a rebuild that did not finish or a build from a tree
//! somebody edited. One says press the button, the other says the button already ran and something
//! went wrong, and a single "you are behind" cannot say which.
//!
//! # Why the remote is asked and not derived
//!
//! skein keeps a mirror of its own repo and comparing against that would cost no network at all —
//! and it would answer a different question: *is the mirror ahead of me*, where the mirror is only
//! as fresh as the last `skein pull`. The owner asked for GitHub directly, and the reason holds:
//! "there is a newer skein" must not be able to go stale for the same reason the fleet did.
//!
//! **Asked on skein's own clock, never on a poller's.** The rule is [`crate::health`]'s, about its
//! own AI field — "a polled endpoint is the wrong place to spawn a process to find out whether a
//! binary runs" — and this endpoint is polled by an open settings pane. So a caller gets what is
//! remembered, including nothing at all on the first call, and the refresh runs behind it.
//!
//! # Why the run is a detached log and not a held request
//!
//! The build takes minutes and **ends by replacing the binary that is serving the page**. An SSE
//! stream over that is a stream that dies at the moment of success, indistinguishable from one that
//! died at the moment of failure. So the script writes to a file, the run is detached under `tmux`
//! exactly as the server and the agent already are, and the page reads the file by offset. A reader
//! that comes back after the swap resumes where it stopped, which is the behaviour the reconnect
//! needs and the one a socket cannot give.

use crate::config::skein_home;
use crate::fleet::skein_source_path;
use crate::util::sh_quote;
use std::time::{Duration, Instant};

/// How long a remembered reading of the remote stays good.
///
/// Ten minutes rather than the six hours [`crate::fleet::runtime_updates`] uses, and the difference
/// is the audience: that one draws a line in a bar nobody opened, this one is read by somebody who
/// has deliberately opened a pane called Update and is deciding whether to press a button. Being
/// told about a commit from six hours ago is the failure mode there.
const REMOTE_FRESH: Duration = Duration::from_secs(10 * 60);

static REMOTE: std::sync::Mutex<Option<(Instant, Result<String, String>)>> =
    std::sync::Mutex::new(None);
static ASKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the Update pane draws.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Available {
    /// The revision stamped into the running binary.
    pub running: String,
    /// `HEAD` of the checkout a rebuild would compile, or empty when there is no checkout.
    pub source: String,
    /// What the tracked ref is at on GitHub, or empty when it could not be asked.
    pub remote: String,
    /// Why `remote` is empty. Empty itself when it is not.
    pub why: String,
    /// There is something to fetch: the remote and the checkout disagree, and both are known.
    pub behind: bool,
    /// The running binary was built from an edited tree — `git describe --dirty` said so.
    pub dirty: bool,
    /// The binary is not the checkout it sits beside: a build that did not finish, or was never run.
    pub unbuilt: bool,
    /// Which ref is tracked, as a person would type it. Empty means the remote's own default.
    pub tracking: String,
    /// The repository being tracked.
    pub url: String,
}

/// Everything the pane needs, with the remote taken from the last reading rather than asked now.
///
/// `token` is passed in rather than fetched because this module has no business holding a
/// credential policy — the server has one already, and an `update` that reached for a token would
/// need an edge to the module that owns them.
pub fn available(token: Option<String>) -> Available {
    let running = crate::health::BUILD_REVISION.to_string();
    let source = source_revision();
    let (remote, why) = match remembered(token) {
        Ok(sha) => (sha, String::new()),
        Err(why) => (String::new(), why),
    };
    let known = |a: &str, b: &str| !a.is_empty() && !b.is_empty();
    Available {
        behind: known(&remote, &source) && !same_revision(&remote, &source),
        // The stamp is `git describe --always --dirty`, so it is the source revision with a suffix
        // when the tree was clean. Compared by prefix for that reason, and only when both are known.
        unbuilt: known(&running, &source) && !same_revision(&running, &source),
        dirty: running.ends_with("-dirty"),
        running,
        source,
        remote,
        why,
        tracking: crate::fleet::skein_source_ref(),
        url: crate::fleet::skein_source_url(),
    }
}

/// Whether two revisions name the same commit, one of which may be abbreviated.
///
/// GitHub answers with a full 40-character sha and `git describe --always` gives an abbreviation
/// whose length is git's choice, so `==` would report every fleet as behind, for ever. Prefix on the
/// shorter, and `-dirty` stripped first: a dirty tree is a different fact, reported by its own
/// field, and folding it in here would make an edited tree look like a commit nobody has.
fn same_revision(a: &str, b: &str) -> bool {
    let bare = |r: &str| r.trim().trim_end_matches("-dirty").to_string();
    let (a, b) = (bare(a), bare(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

/// `HEAD` of the checkout skein builds from, or empty when there is not one.
///
/// Read with `git rev-parse` rather than by opening `.git/HEAD`, because a checkout can be on a
/// detached `FETCH_HEAD` — which is exactly what `bootstrap.sh` leaves behind — and the file then
/// holds a sha in one case and a ref in the other.
fn source_revision() -> String {
    let src = skein_source_path();
    let out = std::process::Command::new("git")
        .args(["-C", &src, "rev-parse", "HEAD"])
        .output();
    match out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// The last reading of the remote, refreshing behind the caller when it has gone stale.
fn remembered(token: Option<String>) -> Result<String, String> {
    let known = REMOTE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let due = match &known {
        Some((at, _)) => at.elapsed() >= REMOTE_FRESH,
        None => true,
    };
    if due && !ASKING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(move || {
            let answer = ask_github(token.as_deref().unwrap_or_default());
            *REMOTE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), answer));
            ASKING.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    match known {
        Some((_, answer)) => answer,
        None => Err("not asked yet".to_string()),
    }
}

/// `owner/repo` out of whatever `SKEIN_SOURCE_URL` is set to.
///
/// Both spellings, because both install: `https://github.com/owner/repo.git` is what the README
/// gives and `git@github.com:owner/repo.git` is what somebody with an SSH remote will have. A URL
/// this cannot read is an error naming the URL rather than a guess.
pub fn slug_of(url: &str) -> Result<String, String> {
    let rest = url
        .trim()
        .trim_end_matches('/')
        .rsplit_once("github.com")
        .map(|(_, rest)| rest.trim_start_matches([':', '/']))
        .ok_or_else(|| format!("{url} is not a github.com URL, so there is nothing to ask"))?;
    let slug = rest.trim_end_matches(".git");
    match slug.split('/').filter(|s| !s.is_empty()).count() {
        2 => Ok(slug.to_string()),
        _ => Err(format!("{url} does not name an owner and a repository")),
    }
}

/// What the tracked ref is at, straight from the API.
fn ask_github(token: &str) -> Result<String, String> {
    let slug = slug_of(&crate::fleet::skein_source_url())?;
    // An empty ref means the remote's own default branch, which is what a bare clone takes and what
    // `skein_source_ref` documents. `HEAD` is the API's spelling of that, and it is a ref the remote
    // always has — the same argument `bootstrap.sh` makes for fetching `HEAD`.
    let reference = match crate::fleet::skein_source_ref() {
        r if r.trim().is_empty() => "HEAD".to_string(),
        r => r,
    };
    let value = crate::github::get_json(&format!("/repos/{slug}/commits/{reference}"), token)?;
    value
        .get("sha")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("GitHub answered about {slug}@{reference} without a sha"))
}

/// Where the run writes, and where the page reads.
///
/// Beside the volume rather than in it: the log is about the fleet's own installation, it is read
/// after the server that wrote it has been replaced, and a person looking for "what did that
/// update do" wants it in the same place as everything else skein keeps.
pub fn log_path() -> std::path::PathBuf {
    skein_home().join("update.log")
}

fn done_path() -> std::path::PathBuf {
    skein_home().join("update.done")
}

/// What a reader has of the run so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Reading {
    /// Bytes read so far, to hand back on the next call.
    pub at: u64,
    /// What arrived since the caller's offset.
    pub text: String,
    /// The run has ended, one way or the other.
    pub done: bool,
    /// It ended by succeeding. Meaningless while `done` is false.
    pub ok: bool,
}

/// Read the log from `from`, and say whether the run has finished.
///
/// **The marker is written last and read first**, in that order, because the alternative races: a
/// reader that checked the file size and then the marker can see a complete marker and a short log,
/// and would stop reading before the last line — which is the line saying what went wrong.
pub fn log_from(from: u64) -> Reading {
    let ended = std::fs::read_to_string(done_path()).ok();
    let all = std::fs::read(log_path()).unwrap_or_default();
    let from = from.min(all.len() as u64);
    let text = String::from_utf8_lossy(&all[from as usize..]).to_string();
    Reading {
        at: all.len() as u64,
        text,
        done: ended.is_some(),
        ok: ended.is_some_and(|s| s.trim() == "0"),
    }
}

/// Whether a run is going on right now, so a second press cannot start a second build.
pub fn running() -> bool {
    log_path().exists() && !done_path().exists()
}

/// Fetch, build and install — the same bytes `bootstrap.sh` runs, detached, writing to the log.
///
/// **Detached, because this ends by replacing the process serving the request.** The redirect is
/// inside the script rather than around the call for the same reason the log exists at all: what a
/// caller would have collected is lost at exactly the moment the binary is swapped.
///
/// The marker is written by the same shell, after the build, from the build's own exit status —
/// `$?` and not the tmux session's, which is 0 whenever tmux itself started.
pub fn start(sandbox: &str) -> Result<(), String> {
    if running() {
        return Err("an update is already running".to_string());
    }
    let log = log_path();
    let done = done_path();
    let _ = std::fs::create_dir_all(skein_home());
    // Both cleared before the session starts, and the marker first: `running()` reads the marker's
    // absence as "in progress", so clearing the log first would make a stale marker describe a run
    // that had not begun.
    let _ = std::fs::remove_file(&done);
    std::fs::write(&log, b"").map_err(|e| format!("preparing {}: {e}", log.display()))?;

    let script = format!(
        "{{ {build}; }} > {log} 2>&1; printf '%s' \"$?\" > {done}",
        build = crate::fleet::build_script_for_update(),
        log = sh_quote(&log.to_string_lossy()),
        done = sh_quote(&done.to_string_lossy()),
    );
    crate::fleet::detach_named(sandbox, "skein-update", &script)
        .map_err(|e| format!("starting the update in {sandbox}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short revision and a full sha are the same commit, and saying otherwise reports every
    /// fleet as behind for ever.
    ///
    /// **What would make this fail**: comparing with `==`. GitHub answers with 40 characters and
    /// `git describe --always` abbreviates to whatever git thinks is unambiguous, so the two are
    /// never equal on a fleet that is perfectly up to date — the button would glow permanently and
    /// stop meaning anything, which is the failure this whole pane exists to avoid.
    #[test]
    fn an_abbreviated_revision_and_a_full_sha_are_the_same_commit() {
        let full = "8236e7a1c0ffee1234567890abcdef1234567890";
        assert!(same_revision(full, "8236e7a"));
        assert!(same_revision("8236e7a", full));
        // The dirty marker is a different fact with its own field, and folding it in here would
        // make an edited tree look like a commit that does not exist.
        assert!(same_revision("8236e7a-dirty", full));
        assert!(!same_revision("aaaaaaa", full));
        // Unknown is never "the same", or a fleet that could not be asked would read as current.
        assert!(!same_revision("", full));
        assert!(!same_revision(full, ""));
    }

    /// Both spellings of a GitHub remote name the same repository.
    #[test]
    fn a_remote_is_read_the_same_however_it_was_written() {
        for url in [
            "https://github.com/prateekreddy/skein.git",
            "https://github.com/prateekreddy/skein",
            "git@github.com:prateekreddy/skein.git",
            "https://github.com/prateekreddy/skein/",
        ] {
            assert_eq!(slug_of(url).as_deref(), Ok("prateekreddy/skein"), "{url}");
        }
        // And a URL this cannot read says so rather than guessing half of one.
        for url in ["https://gitlab.com/a/b.git", "https://github.com/onlyowner"] {
            assert!(slug_of(url).is_err(), "{url} was read as a repository");
        }
    }

    /// **Not knowing is not being behind**, which is the one way this pane could lie.
    ///
    /// The remote is empty whenever GitHub has not been asked yet, could not be reached, or
    /// refused. If `behind` were `remote != source` without the both-known test, every one of those
    /// would light the update button on a fleet that is current — and a person who pressed it would
    /// rebuild for nothing and learn to ignore the light.
    #[test]
    fn a_remote_that_could_not_be_asked_is_never_reported_as_an_update() {
        let unknown = Available {
            running: "8236e7a".into(),
            source: "8236e7a".into(),
            remote: String::new(),
            why: "not asked yet".into(),
            ..Default::default()
        };
        let behind = |a: &Available| {
            !a.remote.is_empty() && !a.source.is_empty() && !same_revision(&a.remote, &a.source)
        };
        assert!(!behind(&unknown), "an unanswered check read as an update");
        // The same for a checkout that is not there, which is a fleet whose source was never
        // cloned: nothing to compare, and nothing to claim.
        assert!(!behind(&Available {
            source: String::new(),
            remote: "abc1234".into(),
            ..unknown.clone()
        }));
        // And when both are known and differ, it IS an update — or the test above passes by
        // asserting nothing ever lights up.
        assert!(behind(&Available {
            source: "8236e7a".into(),
            remote: "ffffffff".into(),
            ..unknown.clone()
        }));
    }

    /// The reading hands back an offset a later call can resume from, and never a negative slice.
    ///
    /// The offset comes from a browser, so it arrives as whatever the page last saw — including a
    /// value from *before* the log was truncated by a new run. Clamping is what keeps that from
    /// panicking the server on a slice out of bounds.
    #[test]
    fn a_reader_that_is_ahead_of_the_log_is_clamped_rather_than_panicking() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &*home);
        std::fs::write(log_path(), b"three").unwrap();
        let _ = std::fs::remove_file(done_path());

        assert_eq!(log_from(0).text, "three");
        assert_eq!(log_from(0).at, 5);
        assert_eq!(log_from(5).text, "");
        // Past the end — a stale offset from before a truncation.
        assert_eq!(log_from(4096).text, "");
        assert_eq!(log_from(4096).at, 5);
        assert!(!log_from(0).done, "a run with no marker read as finished");

        std::fs::write(done_path(), b"0").unwrap();
        let ended = log_from(0);
        assert!(
            ended.done && ended.ok,
            "a zero marker did not read as success"
        );
        std::fs::write(done_path(), b"101").unwrap();
        assert!(!log_from(0).ok, "a non-zero exit read as success");
    }
}
