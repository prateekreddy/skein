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
pub fn available(token: Option<crate::secret::Secret>) -> Available {
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
fn remembered(token: Option<crate::secret::Secret>) -> Result<String, String> {
    let known = REMOTE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let due = match &known {
        Some((at, _)) => at.elapsed() >= REMOTE_FRESH,
        None => true,
    };
    if due && !ASKING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(move || {
            // No credential is the ordinary case here — the repository is public — so an empty
            // `Secret` stands in for one, exactly as the empty `&str` used to.
            let answer = ask_github(&token.unwrap_or_else(|| crate::secret::Secret::new("")));
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
fn ask_github(token: &crate::secret::Secret) -> Result<String, String> {
    let slug = slug_of(&crate::fleet::skein_source_url())?;
    // An empty ref means the remote's own default branch, which is what a bare clone takes and what
    // `skein_source_ref` documents. `HEAD` is the API's spelling of that, and it is a ref the remote
    // always has — the same argument `bootstrap.sh` makes for fetching `HEAD`.
    let reference = match crate::fleet::skein_source_ref() {
        r if r.trim().is_empty() => "HEAD".to_string(),
        r => r,
    };
    // `reference` is `$SKEIN_SOURCE_REF`, and `slug` is cut out of `$SKEIN_SOURCE_URL` by
    // `slug_of`, which checks that it has two halves and nothing about what is inside them. Both
    // are encoded rather than interpolated: a ref may carry a `?` or a `#`, and either would turn
    // "what is this ref at" into a question about a different commit.
    let value = crate::github::get_json(
        &format!(
            "{}/commits/{}",
            crate::github::repo_path(&slug),
            crate::github::path_segments(&reference)
        ),
        token,
    )?;
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

/// The detached session the run lives in.
///
/// One constant because two things need the name and they must agree: [`start`] creates the
/// session, [`settle`] asks after it, and a second spelling would be a run nobody could find — the
/// state below would then be permanent for a build that was going perfectly well.
const SESSION: &str = "skein-update";

/// How often the liveness of a believed-running build is actually asked.
///
/// The log is polled roughly once a second while a build is watched, and each ask is a round trip
/// into the sandbox. Three seconds keeps that off the poll's back without letting a dead run sit
/// visible for long: nobody can tell the difference, and a build takes minutes.
const LIVENESS_EVERY: Duration = Duration::from_secs(3);
static ASKED_AT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Set while [`start`] is between clearing the marker and having a session.
///
/// **[`settle`] has a window to fall into and this is the shutter.** `start` removes the marker and
/// writes an empty log *before* it launches anything, so for the moment it takes to write a 35 KB
/// script into the sandbox and ask tmux for a session, the state on disk is exactly the state
/// `settle` reads as "a run that died" — and a poll landing there would bury a run a fraction of a
/// second before it began.
static LAUNCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the two files claim: a log with no marker beside it is a run that has not ended.
///
/// True for a build that is compiling, and equally true for one whose session was killed an hour
/// ago — telling those apart is [`settle`]'s job and cannot be done from here.
fn believed_running() -> bool {
    log_path().exists() && !done_path().exists()
}

/// Turn a run that died without saying so into a run that failed.
///
/// **Without this there is no way out of a run that stopped, short of deleting a file by hand.**
/// [`running`] is a claim about two files and not about a process, and the marker is written by the
/// script itself — so every way a run can end without reaching its last line leaves the log present
/// and the marker absent, for ever. The button is disabled while that holds and [`start`] refuses
/// every press with "an update is already running", about a run that no longer exists. A script
/// that would not parse did it on 2026-08-31; a killed session, a sandbox restarted mid-build, or a
/// machine rebooted during one all do the same thing.
///
/// Rate-limited and skipped entirely when nothing is believed to be running, so the cost is one
/// cheap question every few seconds during an actual build and nothing at all the rest of the time.
/// An empty `sandbox` is not asked about: there is nowhere to ask, and the server refuses to start
/// an update without one.
pub fn settle(sandbox: &str) {
    if sandbox.is_empty() || !believed_running() {
        return;
    }
    {
        let mut at = ASKED_AT.lock().unwrap_or_else(|e| e.into_inner());
        if at.is_some_and(|t| t.elapsed() < LIVENESS_EVERY) {
            return;
        }
        *at = Some(Instant::now());
    }
    settle_with(|| crate::fleet::detached_alive(sandbox, SESSION));
}

/// [`settle`] with the question injected, which is the only way to test the answer to it.
///
/// The rule the parameter exists to enforce: **only `Some(false)` — tmux answered, and the session
/// is not there — ends a run.** `None` is "could not ask", and that is what a sandbox says while it
/// is being restarted by the very update being watched.
fn settle_with(alive: impl FnOnce() -> Option<bool>) {
    // The shutter is read here rather than in `settle` so that it is part of the decision this
    // function makes, and therefore part of what a test of this function can hold it to.
    if LAUNCHING.load(std::sync::atomic::Ordering::SeqCst)
        || !believed_running()
        || alive() != Some(false)
    {
        return;
    }
    // Read the marker again, after the answer. The script writes it strictly before its last
    // command returns and therefore strictly before its session can end, so a run that finished in
    // the instant between the two reads above is a run that left a marker — and this is what keeps
    // a perfectly successful update from being written down as a death.
    if done_path().exists() {
        return;
    }
    // Appended, never written over: what the build managed to say before it stopped is the only
    // evidence of where it stopped, and this note is worth nothing beside it.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        use std::io::Write;
        let _ = writeln!(
            f,
            "\nskein: the update stopped without finishing — its {SESSION} session is gone and it \
             never recorded an exit status, so whatever ended it did not come from the build. \
             Press Update again."
        );
    }
    // Last, as on every other path, and non-zero because this did not succeed.
    let _ = std::fs::write(done_path(), b"1");
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
pub fn log_from(sandbox: &str, from: u64) -> Reading {
    // Before the read, not after: a reader that settled afterwards would hand back one more
    // "still going" for a run this call already knows is over.
    settle(sandbox);
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
///
/// [`settle`] first, because the two files alone cannot tell a build that is compiling from one
/// whose session died — and answering `true` for the second is what disables the button for ever.
pub fn running(sandbox: &str) -> bool {
    settle(sandbox);
    believed_running()
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
    if running(sandbox) {
        return Err("an update is already running".to_string());
    }
    let log = log_path();
    let done = done_path();
    let _ = std::fs::create_dir_all(skein_home());

    // The shutter is held across the whole launch rather than around the `detach_named` alone,
    // because the state `settle` would misread is created by the first line inside.
    LAUNCHING.store(true, std::sync::atomic::Ordering::SeqCst);
    let out = launch(sandbox, &log, &done);
    LAUNCHING.store(false, std::sync::atomic::Ordering::SeqCst);

    let Err(why) = out else {
        return Ok(());
    };
    // **A launch that never happened must not read as a run in progress.**
    //
    // [`running`] is "the log is there and the marker is not", and both of those were arranged
    // above, before anything could fail. So a `detach_named` that refused used to leave the cockpit
    // saying "updating…" with an empty log and the button disabled — for ever, because [`start`]
    // then refuses every later press with "an update is already running". Found live on 2026-08-31:
    // tmux answered `command too long` for a 35 KB script and the pane reported an update that had
    // not begun. The other half of that is fixed in `fleet::detached_script_path`.
    //
    // The failure goes INTO the log rather than merely clearing it, because the log is the one
    // place this pane shows a person what happened, and an update that vanished without a word is
    // the thing this module's own note says the log exists to prevent.
    let why = format!("starting the update in {sandbox}: {why}");
    let _ = std::fs::write(&log, format!("skein: {why}\n"));
    // Written last, exactly as the successful path writes it last, and non-zero because this run
    // did not succeed — `log_from` reads `ok` from this and the pane says so.
    let _ = std::fs::write(&done, b"1");
    Err(why)
}

/// Clear the two files and get a session going, or say why not.
///
/// Split out of [`start`] only so the shutter above can be closed on every path out of it,
/// including the one that gives up on writing the log.
fn launch(sandbox: &str, log: &std::path::Path, done: &std::path::Path) -> Result<(), String> {
    // Both cleared before the session starts, and the marker first: `believed_running` reads the
    // marker's absence as "in progress", so clearing the log first would make a stale marker
    // describe a run that had not begun.
    let _ = std::fs::remove_file(done);
    std::fs::write(log, b"").map_err(|e| format!("preparing {}: {e}", log.display()))?;
    let script = run_script(&log.to_string_lossy(), &done.to_string_lossy());
    crate::fleet::detach_named(sandbox, SESSION, &script)
}

/// The shell the run is, as bytes a shell will actually parse.
///
/// **A function so that it can be syntax-checked**, which is the entire reason it is not written
/// inline in [`launch`]: the version this replaces did not parse *at all*, and nothing in the suite
/// could see that, because reaching it needs a sandbox to talk to and a build to run.
///
/// **The newlines are load-bearing.** [`crate::fleet::build_script_for_update`] ends with a
/// heredoc, and a heredoc's terminator has to be the last thing on its line — so the build script
/// always ends in a newline, and the old `{{ …; }}` put its `;` at the start of a line, where no
/// shell accepts one. Measured on 2026-08-31, bash 5.2 and dash alike: `syntax error near
/// unexpected token ';'`, and the file rejected whole.
///
/// **What that cost, and why it was invisible.** A parse error happens before anything runs — so
/// the redirect was never applied and the last line was never reached. The run therefore wrote
/// *nothing*: an empty log, no marker, and [`running`] true for ever. tmux exits 0 having created
/// the session, so [`start`] returned `Ok` and the cockpit reported an update in progress that had
/// already failed. It had been that way since the pane was written; the 35 KB command ceiling
/// refused the launch first and hid it. A `}` at the start of a line needs no separator at all.
///
/// # And nothing swapped the cockpit onto what it built
///
/// `bootstrap.sh` under `SKEIN_BOOTSTRAP_STOP_AFTER=build` installs both binaries and returns —
/// deliberately, because [`crate::fleet::build_server_in_sandbox`] runs the same bytes while a
/// fleet is being made and must not restart anything. So an update that fetched, compiled and
/// installed perfectly left the *old* binary serving, the page reloaded onto it, and the revision
/// never moved. The button's own caption already promised "restarts the cockpit"; the code never
/// did it, and every update so far has been finished by hand.
///
/// The swap is skein's existing one rather than a second mechanism: `SIGUSR1` to the doorway,
/// which re-execs across the same descriptor so the port is never free — exactly what
/// `start-door.sh` does when it finds a cockpit already running. **After the marker, never
/// before**: the pane stops reading the log the moment the marker says the run ended, so a line
/// written after it is a line nobody sees, and the toast and its reload are timed against the swap
/// that follows.
fn run_script(log: &str, done: &str) -> String {
    run_script_with(
        &crate::fleet::build_script_for_update(),
        log,
        done,
        &crate::fleet::server_door_stamp_path(),
    )
}

/// [`run_script`] with the build and the doorway named, which is what makes the tail testable.
///
/// The real build takes minutes and ends by installing binaries, so nothing can run it in a test —
/// but everything that matters here is what happens *around* it, and a substitute build that
/// merely exits with a chosen status exercises all of it.
///
/// **A subshell, not a brace group — and that gap was a fourth defect.** `bootstrap.sh` is inlined
/// here rather than invoked, and under `SKEIN_BOOTSTRAP_STOP_AFTER=build` it ends `exit 0`, with
/// seven `exit 1`s on its error paths. `exit` inside `{ … }` exits the **shell**, not the group. So
/// on 2026-09-03 an update fetched, compiled and installed `569dfcf` — the binaries are on disk,
/// timestamped — and then stopped at the closing brace: no `rc`, no marker, no signal, and the
/// session gone. [`settle`] was right about every word it said. `( … )` scopes the `exit` to the
/// build, which is the only thing it was ever meant to end.
fn run_script_with(build: &str, log: &str, done: &str, door: &str) -> String {
    format!(
        "(\n{build}\n) > {log} 2>&1\n\
         rc=$?\n\
         if [ \"$rc\" = 0 ]; then\n\
         printf 'skein: the build finished; swapping the running cockpit onto it\\n' >> {log}\n\
         fi\n\
         printf '%s' \"$rc\" > {done}\n\
         if [ \"$rc\" = 0 ]; then\n\
         kill -USR1 \"$(cut -d' ' -f1 < {door})\" 2>/dev/null \\\n\
         || printf 'skein: the new skein is installed, but the running cockpit could not be \
         swapped onto it — the Update pane will say the binary is not its checkout until \
         something restarts it\\n' >> {log}\n\
         fi\n",
        log = sh_quote(log),
        done = sh_quote(done),
        door = sh_quote(door),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An update that never started does not report itself as running** — the live failure of
    /// 2026-08-31, in a test.
    ///
    /// What the owner saw: the button said something about a command being too long, and the pane
    /// then showed an update in progress. It was not in progress. [`start`] writes an empty log and
    /// removes the marker BEFORE it launches anything, and [`running`] is "the log is there and the
    /// marker is not" — so a launch that failed left exactly the state a launch that succeeded
    /// leaves, for ever, with an empty log and the button disabled. Every later press then answered
    /// "an update is already running", which was the only true thing said and was about a run that
    /// did not exist. Recovering it meant deleting a file by hand.
    ///
    /// **What would make this fail:** removing either write on the failure path. Without the marker
    /// `running` stays true; without the log line the pane says an update finished and shows
    /// nothing about why, which is the silence this module's log exists to prevent.
    #[test]
    fn a_launch_that_failed_is_not_left_looking_like_a_run_in_progress() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        assert!(!running(""), "a fresh home cannot have a run in it");
        // No sandbox to reach, so the launch cannot happen — which is the point: what is under
        // test is what skein is left holding when it does not.
        let out = start("");
        assert!(out.is_err(), "a launch with nowhere to go reported success");

        assert!(
            !running(""),
            "the cockpit would report an update in progress that never began, and refuse every \
             later press"
        );
        let said = log_from("", 0);
        assert!(said.done, "the run was left unfinished");
        assert!(
            !said.ok,
            "a launch that failed was reported as a successful update"
        );
        assert!(
            said.text.contains("starting the update"),
            "the log says nothing about why the update did not happen: {:?}",
            said.text
        );

        // And a person can press again, which is the whole recovery: no file to delete by hand.
        assert!(
            !start("").is_err_and(|why| why.contains("already running")),
            "the second press was refused on behalf of a run that never existed"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **The shell the update runs has to be shell**, and for the life of this pane it was not.
    ///
    /// What the owner saw on 2026-08-31: the button reported success, no update happened, and the
    /// pane then said "updating…" for ever. The script was `{ <build>\n; } > log 2>&1; printf …` —
    /// the build script ends in a newline because it ends in a heredoc, which put the `;` at the
    /// start of a line, which no shell parses. The whole file was rejected before one command ran,
    /// so the redirect never applied and the marker line was never reached: an empty log, no
    /// marker, and a button disabled for ever. It had always been that way. The 35 KB command
    /// ceiling refused the launch first, so it never got far enough to be seen.
    ///
    /// **What would make this fail:** putting the `;` back before the `}`, or joining the lines.
    /// `sh -n` on the real bytes is the check — the run assembles its script once and this is the
    /// same call, so nothing here can agree with itself about a shape the shell disagrees with.
    #[test]
    fn the_script_the_update_runs_is_one_a_shell_can_parse() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        std::env::set_var("SKEIN_HOME", &dir);
        let script = dir.join("run.sh");
        let text = run_script("/a home/update.log", "/a home/update.done");
        std::fs::write(&script, &text).unwrap();

        for shell in ["sh", "bash"] {
            let out = std::process::Command::new(shell)
                .arg("-n")
                .arg(&script)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{shell} cannot parse the script the update runs, so pressing the button writes \
                 nothing at all and leaves the pane saying it is updating: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        // And it is still the script that records how it went — a version that parsed by dropping
        // the last line would pass the check above and jam the pane exactly as the old one did.
        assert!(
            text.contains("printf '%s' \"$rc\" > '/a home/update.done'"),
            "the run no longer records its exit status: {}",
            &text[text.len().saturating_sub(200)..]
        );
        // Quoted, because a home with a space in it is the ordinary case on a Mac.
        assert!(
            text.contains("> '/a home/update.log' 2>&1"),
            "the log path is unquoted"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// **A build that finished swaps the cockpit onto it, and one that failed leaves it alone.**
    ///
    /// The third defect in the same button. `bootstrap.sh` under `STOP_AFTER=build` installs the
    /// binaries and returns without restarting anything — correctly, because a fleet being created
    /// runs the same bytes — so an update that compiled and installed perfectly left the old binary
    /// serving and the revision never moved. The caption said "restarts the cockpit"; nothing did.
    ///
    /// Run rather than pattern-matched, with a stand-in build that exits with a chosen status and a
    /// stand-in doorway that is a real process: `SIGUSR1` has no handler on a `sleep`, so it dies,
    /// and whether it is still there afterwards is the whole question, asked of the operating
    /// system rather than of a string.
    ///
    /// **What would make this fail:** sending the signal unconditionally kills the doorway under a
    /// build that failed; sending it before the marker is written puts the swap ahead of the page's
    /// last read, so the marker arrives from a server that is already going down; dropping it
    /// entirely leaves the cockpit on the binary it was already running, which is the bug.
    #[test]
    fn a_finished_build_swaps_the_running_cockpit_and_a_failed_one_leaves_it_alone() {
        let dir = crate::testutil::tempdir();
        let log = dir.join("update.log");
        let done = dir.join("update.done");
        let door = dir.join("server.door");

        // (what the build does, what it prints, the status the run should record, does the doorway
        //  survive it)
        let rows: [(&str, u32, bool); 4] = [
            ("printf 'Compiling skein v0.1.0\\n'", 0, false),
            (
                "printf 'error: could not compile\\n' >&2\n( exit 3 )",
                3,
                true,
            ),
            // **The two rows that end in a bare `exit`, which is what the real build does.** The
            // two above cannot fail under a brace group — one never exits at all and the other
            // exits inside a subshell of its own — so for as long as they were the whole table the
            // stand-in was unfaithful in precisely the way that hid the defect.
            ("printf 'Compiling skein v0.1.0\\n'\nexit 0", 0, false),
            ("printf 'error: could not compile\\n' >&2\nexit 3", 3, true),
        ];
        for (build, status, doorway_lives) in rows {
            let mut stand_in = std::process::Command::new("sleep")
                .arg("5")
                .spawn()
                .expect("a stand-in doorway");
            std::fs::write(&door, format!("{} 7878\n", stand_in.id())).unwrap();
            let _ = std::fs::remove_file(&done);

            let script = dir.join("run.sh");
            std::fs::write(
                &script,
                run_script_with(
                    build,
                    &log.to_string_lossy(),
                    &done.to_string_lossy(),
                    &door.to_string_lossy(),
                ),
            )
            .unwrap();
            let ran = std::process::Command::new("sh")
                .arg(&script)
                .status()
                .expect("the run");
            assert!(
                ran.success(),
                "the run itself fell over on a build exiting {status}"
            );

            assert_eq!(
                std::fs::read_to_string(&done).unwrap_or_default(),
                status.to_string(),
                "the marker does not carry the build's own exit status"
            );
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(
                said.contains("skein") || said.contains("error"),
                "the build's output did not reach the log: {said:?}"
            );

            // The signal is delivered and acted on by another process, so give it a moment before
            // concluding anything — a poll rather than a sleep, so the passing case stays quick.
            let mut alive = true;
            for _ in 0..100 {
                if stand_in.try_wait().ok().flatten().is_some() {
                    alive = false;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if alive {
                let _ = stand_in.kill();
                let _ = stand_in.wait();
            }
            assert_eq!(
                alive, doorway_lives,
                "a build exiting {status} left the doorway alive={alive}; the update either did \
                 not swap the cockpit onto what it built, or restarted it after a failed build"
            );
            if !doorway_lives {
                assert!(
                    said.contains("swapping the running cockpit onto it"),
                    "the swap happened without the log saying so: {said:?}"
                );
                // **The order, asserted on the script rather than on the race.** An earlier
                // version of this test asked the stand-in's own signal handler whether the marker
                // was there yet — and it could not fail, because the run writes the marker within
                // microseconds of `kill` returning and the handler is scheduled whenever the
                // kernel gets to it. The ordering is a property of the program being generated, so
                // it is checked where it is decided.
                let script = run_script_with("true", "/l", "/d", "/o");
                assert!(
                    script.find("> '/d'").unwrap() < script.find("kill -USR1").unwrap(),
                    "the cockpit is signalled before the marker is written — the page stops \
                     reading the log at the marker, so it would be asking a server that is \
                     already going down for the answer that says the update worked:\n{script}"
                );
            }
        }
    }

    /// **A run that died without saying so is a failed run, not an eternal one.**
    ///
    /// This is the other half of the same live failure. The marker is written by the script, so
    /// every way a run can stop before its last line — a script that would not parse, a killed
    /// session, a sandbox restarted mid-build — leaves the log present and the marker absent, which
    /// is exactly the state a healthy build is in. `running` said yes for ever, the button stayed
    /// disabled, and `start` refused every press on behalf of a run that did not exist. The only
    /// recovery was deleting a file by hand.
    ///
    /// **What would make each row fail**, in order: reading `None` as death buries a build whose
    /// sandbox merely did not answer — and that is the state a *successful* update puts its own
    /// sandbox in, so it is the most damaging of the three; dropping the marker write leaves
    /// `running` true and nothing changes; overwriting the log instead of appending throws away the
    /// build output, which is the only evidence of where it stopped.
    #[test]
    fn a_run_whose_session_vanished_is_reported_as_failed_rather_than_running_for_ever() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &*home);
        std::fs::write(log_path(), b"   Compiling skein v0.1.0\n").unwrap();
        let _ = std::fs::remove_file(done_path());
        assert!(believed_running(), "the fixture is not a run in progress");

        // The window `start` opens: the marker is gone and the log is empty before there is any
        // session to find, so for that instant a run being born is indistinguishable from one that
        // died. Burying it there would re-enable the button under a build that is about to run, and
        // the next press would start a second one.
        LAUNCHING.store(true, std::sync::atomic::Ordering::SeqCst);
        settle_with(|| Some(false));
        LAUNCHING.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(
            believed_running(),
            "a run was buried in the instant between clearing the marker and having a session"
        );

        settle_with(|| Some(true));
        assert!(believed_running(), "a build that is still going was buried");

        settle_with(|| None);
        assert!(
            believed_running(),
            "a sandbox that could not be asked was read as a dead run — which is what a \
             successful update's own restart looks like from here"
        );

        settle_with(|| Some(false));
        assert!(
            !believed_running(),
            "the pane would still say it is updating"
        );
        let said = log_from("", 0);
        assert!(said.done, "the run was left unfinished");
        assert!(
            !said.ok,
            "a run that was cut off reported itself as a successful update"
        );
        assert!(
            said.text.contains("stopped without finishing"),
            "the log does not say why the update ended: {:?}",
            said.text
        );
        assert!(
            said.text.contains("Compiling skein"),
            "the recovery threw away what the build had managed to say: {:?}",
            said.text
        );

        // And the last word stays with the run: a marker that arrived between the question and the
        // answer is a run that finished, and settling must not overwrite it with a failure.
        std::fs::write(log_path(), b"done\n").unwrap();
        std::fs::write(done_path(), b"0").unwrap();
        settle_with(|| Some(false));
        assert!(
            log_from("", 0).ok,
            "an update that had already succeeded was recorded as a death"
        );

        std::env::remove_var("SKEIN_HOME");
    }

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

        assert_eq!(log_from("", 0).text, "three");
        assert_eq!(log_from("", 0).at, 5);
        assert_eq!(log_from("", 5).text, "");
        // Past the end — a stale offset from before a truncation.
        assert_eq!(log_from("", 4096).text, "");
        assert_eq!(log_from("", 4096).at, 5);
        assert!(
            !log_from("", 0).done,
            "a run with no marker read as finished"
        );

        std::fs::write(done_path(), b"0").unwrap();
        let ended = log_from("", 0);
        assert!(
            ended.done && ended.ok,
            "a zero marker did not read as success"
        );
        std::fs::write(done_path(), b"101").unwrap();
        assert!(!log_from("", 0).ok, "a non-zero exit read as success");
    }

    /// **The slug and the ref this asks about reach GitHub as encoded path segments** (SKEIN-633).
    ///
    /// Neither value is skein's. `slug` comes out of `$SKEIN_SOURCE_URL` through [`slug_of`], which
    /// checks that it has two halves and nothing whatever about what is inside them, and
    /// `reference` is `$SKEIN_SOURCE_REF` verbatim — "a branch, tag or sha, whatever
    /// `git checkout` takes". Interpolated raw, `release#2` made
    /// `GET /repos/…/commits/release#2`, and curl never puts a fragment on the wire: the answer
    /// came back about a different commit, and the update pane then said the fleet was behind (or
    /// level) on the strength of it.
    ///
    /// **What would make this fail:** putting either value into the `format!` unencoded. The stub
    /// then records `/repos/acme/skein?x=1/commits/release`, and both assertions below name what
    /// that is missing.
    #[test]
    fn the_slug_and_ref_this_asks_about_reach_github_as_encoded_segments() {
        let _g = crate::testutil::env_lock();
        let (base, heard) = recording_github();
        std::env::set_var("SKEIN_GITHUB_API", &base);
        std::env::set_var("SKEIN_SOURCE_URL", "https://github.com/acme/skein?x=1.git");
        std::env::set_var("SKEIN_SOURCE_REF", "release#2");

        let _ = ask_github(&crate::secret::Secret::new("skein-test-github-token"));

        let said = heard.lock().unwrap().clone();
        let line = said
            .first()
            .unwrap_or_else(|| panic!("nothing was asked of GitHub"))
            .clone();
        assert_eq!(
            line.split(' ').nth(1).unwrap_or_default(),
            "/repos/acme/skein%3Fx%3D1/commits/release%232",
            "the source URL and ref did not reach GitHub as encoded segments: {line}"
        );

        for key in ["SKEIN_GITHUB_API", "SKEIN_SOURCE_URL", "SKEIN_SOURCE_REF"] {
            std::env::remove_var(key);
        }
    }

    /// A GitHub that records the request line and answers one canned commit.
    ///
    /// A listener rather than a mock, for [`crate::prwork::testkit::github`]'s reason: the question
    /// is what skein PUT ON THE WIRE, and only something that reads the socket can answer it. Its
    /// own copy because that one is `pub(super)` to `prwork` and this is the only thing in
    /// `update` that talks to GitHub at all.
    fn recording_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                seen.lock()
                    .unwrap()
                    .push(said.lines().next().unwrap_or_default().to_string());
                let answer = r#"{"sha":"0123456789abcdef0123456789abcdef01234567"}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (base, heard)
    }
}
