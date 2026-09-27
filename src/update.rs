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
//! [`crate::fleet::build_script_for_update`] is those exact bytes. What was missing was the
//! telling.
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

static REMOTE: std::sync::Mutex<Option<(Instant, Asked)>> = std::sync::Mutex::new(None);
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
    /// What the server binary installed on disk says it is — its `--version` stamp — or empty when
    /// there is none there, or it would not say (SKEIN-1029).
    pub installed: String,
    /// A different build is installed than the one answering, so "Restart on new build" would
    /// bring it up. Compared as whole stamps: a `-dirty` build and a clean one of the same commit
    /// are different binaries, and the page confirms the restart by the same exact comparison.
    pub restartable: bool,
    /// GitHub refused the stored token with a 401, so `remote` (or `why`) is the answer to the
    /// same question asked without it (SKEIN-1172). True whatever that second answer was: the
    /// token needs replacing even when the check itself came out fine.
    pub token_refused: bool,
}

/// One reading of the remote: what GitHub said, and whether the stored token was refused on the
/// way to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    answer: Result<String, String>,
    token_refused: bool,
}

/// Everything the pane needs, with the remote taken from the last reading rather than asked now.
///
/// `token` is passed in rather than fetched because this module has no business holding a
/// credential policy — the server has one already, and an `update` that reached for a token would
/// need an edge to the module that owns them.
pub fn available(token: Option<crate::secret::Secret>) -> Available {
    let running = crate::health::BUILD_REVISION.to_string();
    let source = source_revision();
    let asked = remembered(token);
    let token_refused = asked.token_refused;
    let (remote, why) = match asked.answer {
        Ok(sha) => (sha, String::new()),
        Err(why) => (String::new(), why),
    };
    let known = |a: &str, b: &str| !a.is_empty() && !b.is_empty();
    let installed = installed_revision();
    Available {
        restartable: differs(&installed, &running),
        token_refused,
        installed,
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

/// The last `--version` read of the installed binary, keyed by what would change its answer.
type InstalledReading = ((u64, Option<std::time::SystemTime>, u64), String);
static INSTALLED: std::sync::Mutex<Option<InstalledReading>> = std::sync::Mutex::new(None);

/// The revision the server binary installed at [`crate::fleet::server_path`] reports — the build a
/// restart would bring up (SKEIN-1029).
///
/// **Asked of the binary, once per file.** `/api/update` is polled while the pane is open, and a
/// polled endpoint is the wrong place to spawn a process (this module's own rule, above). But the
/// only thing that changes the answer is a new file at that path, and an install renames one into
/// place — so the length, mtime and inode are the key, a stat is all a poll costs, and the binary
/// is run once per install. Bounded, because a file at that path is whatever was put there.
fn installed_revision() -> String {
    use std::os::unix::fs::MetadataExt;
    let path = crate::fleet::server_path();
    let Ok(meta) = std::fs::metadata(&path) else {
        return String::new();
    };
    let key = (meta.len(), meta.modified().ok(), meta.ino());
    let mut held = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((was, rev)) = held.as_ref() {
        if *was == key {
            return rev.clone();
        }
    }
    let said = crate::util::output_with_timeout(
        std::process::Command::new(&path).arg("--version"),
        Duration::from_secs(10),
    )
    .filter(|out| out.status.success())
    .map(|out| stamp_of(&String::from_utf8_lossy(&out.stdout)))
    .unwrap_or_default();
    *held = Some((key, said.clone()));
    said
}

/// The revision out of `skein-server 0.1.0 (<rev>)` — the shape `skein-server --version` prints.
fn stamp_of(version: &str) -> String {
    version
        .trim()
        .rsplit_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// What holds the cockpit's port, as facts for the page to put into words (SKEIN-1029).
///
/// Facts rather than a sentence because every sentence the restart puts in front of a person is in
/// one object in the page (`RESTART_WORDS`), for the owner to read as a whole.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PortHolder {
    /// The cockpit's port inside the sandbox.
    pub port: u16,
    /// The doorway holding it, judged by its stamp exactly as [`crate::fleet::door_pid`] judges
    /// it; `None` when no doorway provably does.
    pub doorway: Option<u32>,
    /// The process answering this request, and the build it is.
    pub server: u32,
    pub build: String,
    /// Whether this server is the doorway's child — the only server a reload replaces.
    pub behind_doorway: bool,
    /// The reload, as a person would type it.
    pub reload: String,
    /// Where the doorway says why a server it started did not come up.
    pub look: String,
}

/// Who holds the port right now, asked by the server that is answering.
pub fn port_holder(sandbox: &str) -> PortHolder {
    let port = crate::fleet::server_sandbox_port();
    let doorway = crate::fleet::door_pid(sandbox, port);
    let parent = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("PPid:"))
                .and_then(|p| p.trim().parse::<u32>().ok())
        });
    PortHolder {
        port,
        behind_doorway: doorway.is_some() && doorway == parent,
        doorway,
        server: std::process::id(),
        build: crate::health::BUILD_REVISION.to_string(),
        reload: crate::fleet::reload_command(),
        look: crate::fleet::doorway_pane_command(),
    }
}

/// Why a restart was not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRestarted {
    /// The build answering is the one installed: there is nothing newer to bring up.
    NothingNewer,
    /// No doorway holds the port, or this server is not the one behind it — so a reload would
    /// replace nothing that is answering. The holder says what does.
    NoDoorway(PortHolder),
}

/// "Restart on new build": the doorway's in-place reload, once the answer to this request is out.
///
/// **The reload is the doorway's `SIGUSR1`** ([`crate::fleet::reload_server`]) — the same one an
/// install sends, which is gapless by design: the doorway ends this server and re-execs itself
/// across the listening socket it never closes, then starts whatever is installed. So the page
/// loses its connection for the time a server takes to start and never finds the port free.
///
/// **Sent after a beat, on a thread of its own**, because the process it replaces is this one: sent
/// inline, the doorway's `SIGTERM` can land before the response does, and the page would be told
/// nothing about a restart that worked. The checks that decide whether to send it are made first
/// and synchronously, so every refusal still reaches the page.
///
/// **Refused unless this server is the doorway's child.** A server started some other way is not
/// what a reload replaces: the doorway would restart a server nobody is talking to, and this one
/// would go on answering as the old build — the "did not take" case, known in advance.
pub fn restart(sandbox: &str) -> Result<PortHolder, NotRestarted> {
    if !available_restartable() {
        return Err(NotRestarted::NothingNewer);
    }
    let holder = port_holder(sandbox);
    if !holder.behind_doorway {
        return Err(NotRestarted::NoDoorway(holder));
    }
    let sandbox = sandbox.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        crate::fleet::reload_server(&sandbox);
    });
    Ok(holder)
}

/// [`Available::restartable`] without asking GitHub anything.
fn available_restartable() -> bool {
    differs(&installed_revision(), crate::health::BUILD_REVISION)
}

/// [`Available::restartable`]'s rule: both known, and not the same stamp.
fn differs(installed: &str, running: &str) -> bool {
    !installed.is_empty() && !running.is_empty() && installed != running
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
fn remembered(token: Option<crate::secret::Secret>) -> Asked {
    let known = REMOTE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let due = match &known {
        Some((at, _)) => at.elapsed() >= REMOTE_FRESH,
        None => true,
    };
    if due && !ASKING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(move || {
            let answer = ask(token, ask_github);
            *REMOTE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), answer));
            ASKING.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    match known {
        Some((_, asked)) => asked,
        None => Asked {
            answer: Err("not asked yet".to_string()),
            token_refused: false,
        },
    }
}

/// Ask with the stored token, and once more without it when GitHub refuses the token (SKEIN-1172).
///
/// No credential is the ordinary case here — the repository is public — so an empty `Secret`
/// stands in for none, and `github::call` sends no `Authorization` header for it. A token that
/// GitHub answers with a 401 is one that was rotated or revoked at GitHub, and a public repository
/// does not need it: the anonymous answer is the one used, and `token_refused` is what tells the
/// pane the token needs replacing. Only a 401 does this. A 403 or 404 is an answer about the
/// repository or the quota, which asking without a credential could only make worse.
///
/// `github` is [`ask_github`], passed in so a test can stand a GitHub up behind it.
fn ask(
    token: Option<crate::secret::Secret>,
    github: impl Fn(&crate::secret::Secret) -> Result<String, String>,
) -> Asked {
    let none = crate::secret::Secret::new("");
    let token = token.filter(|t| !t.is_empty());
    let Some(token) = token else {
        return Asked {
            answer: github(&none),
            token_refused: false,
        };
    };
    match github(&token) {
        Err(why) if refused_credentials(&why) => Asked {
            answer: github(&none),
            token_refused: true,
        },
        answer => Asked {
            answer,
            token_refused: false,
        },
    }
}

/// Whether `why` is `github::get_json` reporting a 401 — the status GitHub gives a credential it
/// does not accept, in either of the two sentences that module words a non-2xx with.
fn refused_credentials(why: &str) -> bool {
    why.starts_with("GitHub said 401:") || why.starts_with("GitHub answered 401")
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
        || CANCELLING.load(std::sync::atomic::Ordering::SeqCst)
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
    /// It ended because somebody pressed Cancel. Meaningless while `done` is false.
    pub cancelled: bool,
    /// Which run this is, for [`cancel`] to be handed back. Empty when there has never been one.
    pub run: String,
    /// Whole seconds since the log was last written, while the run is going; 0 once it has ended.
    pub quiet: u64,
    /// `quiet` has reached [`QUIET_TOO_LONG`]: the pane says so and offers Cancel.
    pub stalled: bool,
}

/// How long since the log last grew, while a run is believed to be going.
///
/// The log's mtime rather than its size seen between two polls, because the reader is a page that
/// may have been open for five seconds or closed for an hour — and the file's own clock is the same
/// for both. `None` when no run is going, or the log cannot be read.
fn quiet_for() -> Option<Duration> {
    if !believed_running() {
        return None;
    }
    let written = std::fs::metadata(log_path()).ok()?.modified().ok()?;
    // A log dated in the future (a clock stepped back) is not quiet; it is simply not stalled.
    Some(
        std::time::SystemTime::now()
            .duration_since(written)
            .unwrap_or_default(),
    )
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
    let quiet = quiet_for().unwrap_or_default();
    Reading {
        at: all.len() as u64,
        text,
        done: ended.is_some(),
        ok: ended.as_deref().is_some_and(|s| s.trim() == "0"),
        cancelled: ended.as_deref().is_some_and(|s| s.trim() == CANCELLED),
        run: run_id(),
        quiet: quiet.as_secs(),
        stalled: quiet >= QUIET_TOO_LONG,
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
    let _control = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
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
    // Named before the log exists, so no reader can see this run's log under the last run's name.
    let run = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    std::fs::write(run_path(), &run)
        .map_err(|e| format!("preparing {}: {e}", run_path().display()))?;
    std::fs::write(log, b"").map_err(|e| format!("preparing {}: {e}", log.display()))?;
    let script = run_script(&log.to_string_lossy(), &done.to_string_lossy());
    crate::fleet::detach_named(sandbox, SESSION, &script)
}

/// Which run the log belongs to: a name [`launch`] writes before anything else, and [`cancel`]
/// is handed back.
///
/// **This is what keeps Cancel from ending somebody else's run.** The pane that offers Cancel is
/// looking at one run; by the time the press arrives that run may have ended and a second tab may
/// have started another, which lives in the same session name. A press carrying the name of the run
/// it was offered for can only ever stop that run.
fn run_path() -> std::path::PathBuf {
    skein_home().join("update.run")
}

/// The run the log belongs to, or empty when none has been started since this was introduced.
fn run_id() -> String {
    std::fs::read_to_string(run_path())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// What [`cancel`] writes into the marker in place of an exit status, so the pane can tell a run a
/// person stopped from one that failed.
const CANCELLED: &str = "cancelled";

/// How long the log may go unwritten, while a run is going, before the pane says so (SKEIN-1037).
///
/// The owner's number, 2026-09-23. It is not a timeout — nothing is stopped when it passes; the pane
/// says that nothing has been written for this long, shows where the log got to, and offers Cancel.
/// A cold release build writes a line per crate, so five silent minutes is long for a build and
/// ordinary for nothing except a single long link, which the pane's wording allows for.
pub const QUIET_TOO_LONG: Duration = Duration::from_secs(5 * 60);

/// Set while [`cancel`] is between stopping the session and writing the marker.
///
/// The same window as [`LAUNCHING`], from the other side: for that instant the session is gone and
/// the marker is not there yet, which is exactly what [`settle`] reads as a run that died — and it
/// would write its own "stopped without finishing" over a run a person stopped on purpose.
static CANCELLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Held by [`start`] and [`cancel`] for their whole length, so a press of one cannot land in the
/// middle of the other — a cancel that checked the run's name and then killed the session a second
/// start had just created is the one way it could stop a run it was not offered for.
static CONTROL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How a press of Cancel came out, when it reached the run at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancelled {
    /// The run was stopped, and the log and the marker say so.
    Stopped,
    /// It had already ended by itself — the marker it wrote is left exactly as it wrote it.
    AlreadyEnded,
}

/// Stop the run named `run`, and only that run (SKEIN-1037).
///
/// **Nothing broader than the one session this module started is touched.** The session is
/// addressed as `=skein-update` — tmux's exact-match form; a bare `-t skein-update` also matches
/// any session whose name merely *starts* with that — and what is signalled is that session's own
/// process group: its pane's process is a session leader, so the build it started and everything
/// under it share the group, and nothing outside it does. No `pkill`, no pattern.
///
/// Refused unless `run` names the run the log belongs to and that run is believed to be going, so a
/// press that arrives after its run ended, or after another began, stops nothing.
pub fn cancel(sandbox: &str, run: &str) -> Result<Cancelled, String> {
    let _control = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if sandbox.is_empty() {
        return Err("there is no fleet sandbox to stop an update in".to_string());
    }
    if !believed_running() || LAUNCHING.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(Cancelled::AlreadyEnded);
    }
    // Both empty is allowed and is one run: one started by a skein from before runs were named,
    // which is still the only run there is.
    if run != run_id() {
        return Err(
            "that update has already ended, and a different one is running now".to_string(),
        );
    }
    CANCELLING.store(true, std::sync::atomic::Ordering::SeqCst);
    // Through `fleet`, which already owns the session's other two verbs (`detach_named`,
    // `detached_alive`): which sandbox a command runs in is not this module's business
    // (docs/modules.toml, `[current.update]`).
    let out = crate::fleet::stop_detached(sandbox, SESSION);
    let ended = out.map(|()| finish_cancelled());
    CANCELLING.store(false, std::sync::atomic::Ordering::SeqCst);
    ended
}

/// Write down that the run was cancelled — unless it wrote its own ending first.
///
/// The same last-word rule as [`settle_with`]: the build writes its marker strictly before its
/// session ends, so a marker present now is a run that finished in the instant before the press
/// reached it, and overwriting it would record a success as a cancel.
fn finish_cancelled() -> Cancelled {
    if done_path().exists() {
        return Cancelled::AlreadyEnded;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        use std::io::Write;
        let _ = writeln!(
            f,
            "\nskein: you cancelled this update from the Update pane. Its {SESSION} session was \
             stopped, so nothing after the last line above ran. Press Update skein to start again."
        );
    }
    let _ = std::fs::write(done_path(), CANCELLED);
    Cancelled::Stopped
}

/// The shell the run is, as bytes a shell will actually parse.
///
/// **A function so that it can be syntax-checked**, which is the entire reason it is not written
/// inline in [`launch`]: the version this replaces did not parse *at all*, and nothing in the suite
/// could see that, because reaching it needs a sandbox to talk to and a build to run. Public so that
/// `tests/fleet_move.rs` can run these exact bytes against a fixture fleet, which is the only way to
/// see what the button does after the build without a sandbox.
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
/// # The swap, and the proof of it, are bootstrap's — and the marker waits for both
///
/// This used to run bootstrap only as far as the build, then send its own bare `kill -USR1` to the
/// doorway *after* writing the marker. Two things were wrong with that (SKEIN-1031). A doorway that
/// ignored the signal left the old build serving, and nothing checked, so the button was less safe
/// than the script it runs — bootstrap has, since SKEIN-1020, asked the port which build answers and
/// stopped a stale cockpit it can prove is its own. And because the pane stops reading at the
/// marker, anything said after the swap was said to nobody.
///
/// Now the run is all of `bootstrap.sh` ([`crate::fleet::build_script_for_update`]): its
/// `start-door.sh` reloads the doorway in place first — the same `SIGUSR1`, so the port is never
/// free — and its closing check then waits for the new build to answer, fixes what is provably its
/// own, and says what it found. The marker is written after all of that, from bootstrap's exit
/// status. **So the marker moved, and the pane is why that is safe:** `tailUpdate` in
/// `src/web/index.html` treats a poll that fails as "the swap, most likely" and asks again from the
/// same offset, so it reads the check's lines from whichever server answers next, and then the
/// marker. A successful marker therefore means the new build was seen answering, which is what
/// the pane's reload after it assumes.
///
/// `SKEIN_BOOTSTRAP_FROM=update` tells bootstrap its reader has a button rather than a shell, so
/// a failure says "press Update skein again" rather than "run bootstrap.sh again", and the
/// first-install epilogue is left out. Stdin is `/dev/null` because nothing in the run has anyone to
/// read from; bootstrap closes the prompts that go to the terminal instead (SKEIN-1032).
pub fn run_script(log: &str, done: &str) -> String {
    run_script_with(&crate::fleet::build_script_for_update(), log, done)
}

/// [`run_script`] with the build named, which is what makes the tail testable on its own.
///
/// **A subshell, not a brace group — and that gap was a fourth defect.** `bootstrap.sh` is inlined
/// here rather than invoked, and it ends `exit 0` or `exit 1` on many paths. `exit` inside `{ … }`
/// exits the **shell**, not the group. So on 2026-09-03 an update fetched, compiled and installed
/// `569dfcf` — the binaries are on disk, timestamped — and then stopped at the closing brace: no
/// `rc`, no marker, and the session gone. [`settle`] was right about every word it said. `( … )`
/// scopes the `exit` to the build, which is the only thing it was ever meant to end.
fn run_script_with(build: &str, log: &str, done: &str) -> String {
    format!(
        "(\n\
         SKEIN_BOOTSTRAP_FROM=update\n\
         export SKEIN_BOOTSTRAP_FROM\n\
         {build}\n\
         ) > {log} 2>&1 < /dev/null\n\
         rc=$?\n\
         printf '%s' \"$rc\" > {done}\n",
        log = sh_quote(log),
        done = sh_quote(done),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The installed build is read out of the line `skein-server --version` prints**
    /// (SKEIN-1029), and a restart is offered only for a different, known one.
    ///
    /// What would make each assertion fail: `stamp_of` taking the package version (`0.1.0`) rather
    /// than the parenthesised stamp, which is the same for every build and would never offer a
    /// restart; and `differs` comparing through `same_revision`, which strips `-dirty` and so would
    /// call a clean build and an edited-tree build of one commit the same binary.
    #[test]
    fn a_restart_is_offered_for_a_different_installed_build_and_only_then() {
        assert_eq!(
            stamp_of("skein-server 0.1.0 (ceb78a0-dirty)\n"),
            "ceb78a0-dirty"
        );
        assert_eq!(stamp_of("not a version line"), "");
        assert!(differs("ceb78a0", "463f028"));
        assert!(
            differs("ceb78a0-dirty", "ceb78a0"),
            "an edited-tree build is another binary"
        );
        assert!(!differs("ceb78a0", "ceb78a0"));
        assert!(
            !differs("", "ceb78a0"),
            "nothing installed is nothing to restart onto"
        );
    }

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
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Nothing
        // asserted below carries the root, so a fixture is the whole of what this needs.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

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

        std::env::remove_var("SKEIN_FLEET_ROOT");
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
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Nothing
        // asserted below carries the root, so a fixture is the whole of what this needs.
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
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
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **The run records how bootstrap ended, as the last thing it does, and swaps nothing itself.**
    ///
    /// The swap and the check that it took are `bootstrap.sh`'s now (SKEIN-1031) — the run is all
    /// of it — so the tail has three jobs and this holds each: tell bootstrap it is the button,
    /// carry bootstrap's own exit status into the marker, and write nothing after the marker. The
    /// last matters because the pane stops reading there: the old tail's bare `kill -USR1` sat
    /// after it, so a doorway that ignored the signal left the old build serving with nobody told.
    /// `tests/fleet_move.rs` runs the real bytes against exactly that doorway.
    ///
    /// Run rather than pattern-matched, with stand-in builds that end the ways the real one does —
    /// falling off the end, a failing command, and a bare `exit`, which is what bootstrap does and
    /// what a brace group once turned into a run with no marker at all.
    ///
    /// **What would make each assertion fail**, in order: a run that did not survive the build's
    /// `exit` (a brace group for the subshell); a marker from anything other than the build's `$?`;
    /// dropping the `SKEIN_BOOTSTRAP_FROM=update` line, which puts "run bootstrap.sh again" in
    /// front of a person holding a button; and any command after the marker — the old
    /// `kill -USR1` among them. (Dropping only the `export` fails nothing, and correctly: bootstrap
    /// is inlined into the same subshell, so it reads the variable as the shell's own. The export
    /// is for a future that runs it as a file.)
    #[test]
    fn the_run_records_how_bootstrap_ended_last_and_swaps_nothing_itself() {
        let dir = crate::testutil::tempdir();
        let log = dir.join("update.log");
        let done = dir.join("update.done");

        // (what the build does, the status the run should record)
        let rows: [(&str, u32); 4] = [
            ("printf 'from=%s\\n' \"$SKEIN_BOOTSTRAP_FROM\"", 0),
            (
                "printf 'from=%s\\n' \"$SKEIN_BOOTSTRAP_FROM\" >&2\n( exit 3 )",
                3,
            ),
            ("printf 'from=%s\\n' \"$SKEIN_BOOTSTRAP_FROM\"\nexit 0", 0),
            (
                "printf 'from=%s\\n' \"$SKEIN_BOOTSTRAP_FROM\" >&2\nexit 3",
                3,
            ),
        ];
        for (build, status) in rows {
            let _ = std::fs::remove_file(&done);
            let script = dir.join("run.sh");
            std::fs::write(
                &script,
                run_script_with(build, &log.to_string_lossy(), &done.to_string_lossy()),
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
                said.contains("from=update"),
                "bootstrap was not told the Update button started it, so a failure would tell the \
                 owner to run a script instead of pressing the button: {said:?}"
            );
        }

        let script = run_script_with("true", "/l", "/d");
        assert!(
            script.trim_end().ends_with("> '/d'"),
            "something runs after the marker is written — the pane stops reading the log at the \
             marker, so whatever it is happens where nobody can see it:\n{script}"
        );
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

    /// **A run whose log has not moved for five minutes reads as stalled — and only such a run**
    /// (SKEIN-1037).
    ///
    /// The pane's "no progress" state is this flag and nothing else, so the flag is what has to be
    /// right: quiet is measured from the log's own mtime, the line is the owner's five minutes, and
    /// a run that has ended is never stalled however old its log is.
    ///
    /// **What would make each assertion fail**, in order: a threshold of anything under five
    /// minutes (or `>` against a shorter one) fails the first; anything over it, or reading the
    /// size between two polls instead of the mtime, fails the second; dropping the
    /// `believed_running` gate in `quiet_for` fails the third, which is a finished update whose
    /// pane would offer to cancel it.
    #[test]
    fn a_log_quiet_for_five_minutes_while_a_run_is_going_reads_as_stalled_and_only_then() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &*home);
        std::fs::write(log_path(), b"   Compiling skein v0.1.0\n").unwrap();
        let _ = std::fs::remove_file(done_path());
        let aged = |secs: u64| {
            std::fs::File::options()
                .write(true)
                .open(log_path())
                .unwrap()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(secs))
                .unwrap();
        };

        // Literal seconds rather than the constant's, because the five minutes is the owner's
        // number: a test written in terms of `QUIET_TOO_LONG` passes whatever it is changed to.
        aged(290);
        let r = log_from("", 0);
        assert!(
            !r.stalled && r.quiet >= 289,
            "a log written under five minutes ago read as stalled (or its quiet was not measured \
             from the file): {r:?}"
        );

        aged(310);
        let r = log_from("", 0);
        assert!(
            r.stalled && r.quiet >= 310,
            "a log nobody has written to for over five minutes, under a run that has not ended, \
             did not read as stalled — the pane would say \"updating…\" for as long as it hangs: \
             {r:?}"
        );

        std::fs::write(done_path(), b"1").unwrap();
        aged(3000);
        let r = log_from("", 0);
        assert!(
            r.done && !r.stalled && r.quiet == 0,
            "a run that has ended read as stalled, so its pane would offer to cancel it: {r:?}"
        );
    }

    /// Whether `pid` is a live process — present and not a zombie waiting to be reaped.
    fn alive(pid: &str) -> bool {
        std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
            .ok()
            .and_then(|s| s.rsplit_once(") ").map(|(_, rest)| rest.to_string()))
            .is_some_and(|rest| !rest.starts_with('Z'))
    }

    /// Wait up to a few seconds for `f`; signals are delivered asynchronously.
    fn eventually(f: impl Fn() -> bool) -> bool {
        for _ in 0..60 {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        f()
    }

    /// **Cancel stops the run it was offered for, all of it, and nothing else** (SKEIN-1037).
    ///
    /// Run through [`cancel`] itself, against a real tmux on a socket of this test's own — the
    /// execution seam puts `TMUX_TMPDIR` in front of the exact command production sends, and drops
    /// the `$TMUX` a developer's shell carries, which would otherwise aim it at their own session.
    ///
    /// Beside the run: a session whose name merely begins `skein-update`, and a process of the same
    /// user outside any session. Both must survive.
    ///
    /// **What would make each assertion fail:** dropping the `=` from `fleet::stop_detached`'s targets — tmux then
    /// resolves `skein-update` to `skein-update-decoy` by prefix, and the first assertion fails
    /// with the decoy stopped; dropping the `kill -TERM` of the process group — `kill-session`
    /// alone sends SIGHUP, and the child that ignores SIGHUP (as anything under `nohup` does) is
    /// still running; dropping the run-name check — the wrongly-named press stops the run; and
    /// dropping the marker write — the pane never learns the run ended and says "updating…".
    #[test]
    fn cancel_stops_the_named_run_and_nothing_whose_name_merely_starts_the_same() {
        use std::process::Command;
        if Command::new("tmux").arg("-V").output().is_err() {
            crate::testutil::skip("no tmux here, so there is no session for Cancel to stop");
            return;
        }
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &*home);
        env.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        std::fs::write(home.join("config.json"), r#"{"fleet_sandbox":"example"}"#).unwrap();
        let sock_dir = home.join("t");
        std::fs::create_dir_all(&sock_dir).unwrap();
        let tmux = |args: &[&str]| {
            Command::new("tmux")
                .args(args)
                .env_remove("TMUX")
                .env("TMUX_TMPDIR", &sock_dir)
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        let dir = sock_dir.to_string_lossy().into_owned();
        let _stood_in = crate::place::seam::install(Box::new(move |argv: &[String]| {
            let mut v: Vec<String> = ["env", "-u", "TMUX"].map(String::from).to_vec();
            v.push(format!("TMUX_TMPDIR={dir}"));
            v.extend(argv.iter().cloned());
            Some(v)
        }));
        let pid_of = |name: &str| {
            let at = home.join(name);
            eventually(|| std::fs::read_to_string(&at).is_ok_and(|s| !s.trim().is_empty()));
            std::fs::read_to_string(&at).unwrap_or_default()
        };
        let believe = |run: &str| {
            let _ = std::fs::remove_file(done_path());
            std::fs::write(run_path(), run).unwrap();
            std::fs::write(log_path(), b"skein: fetching the default branch\n").unwrap();
        };

        let decoy = home.join("decoy.sh");
        std::fs::write(
            &decoy,
            format!(
                "echo $$ > '{}'\nexec sleep 300\n",
                home.join("decoy.pid").display()
            ),
        )
        .unwrap();
        assert!(tmux(&[
            "new-session",
            "-d",
            "-s",
            "skein-update-decoy",
            &format!("sh '{}'", decoy.display())
        ]));
        let decoy_pid = pid_of("decoy.pid");
        let mut outsider = Command::new("sleep").arg("300").spawn().unwrap();
        let outsider_pid = outsider.id().to_string();

        // The run's session is not there at all, and only the decoy is: a target that matched by
        // prefix would find it.
        believe("run-1");
        let _ = cancel("example", "run-1");
        assert!(
            alive(&decoy_pid) && tmux(&["has-session", "-t", "=skein-update-decoy"]),
            "Cancel stopped a session that is not the update's — its name only begins the same"
        );

        // Now the run itself: a pane whose child ignores SIGHUP, as anything under nohup does.
        believe("run-2");
        let run = home.join("run.sh");
        std::fs::write(
            &run,
            format!(
                "sh -c 'trap \"\" HUP; echo $$ > {hup}; exec sleep 300' &\n\
                 echo $$ > {pane}\n\
                 wait\n",
                hup = sh_quote(&home.join("hup.pid").to_string_lossy()),
                pane = sh_quote(&home.join("pane.pid").to_string_lossy()),
            ),
        )
        .unwrap();
        assert!(tmux(&[
            "new-session",
            "-d",
            "-s",
            "skein-update",
            &format!("sh '{}'", run.display())
        ]));
        let (pane_pid, hup_pid) = (pid_of("pane.pid"), pid_of("hup.pid"));
        assert!(
            alive(&pane_pid) && alive(&hup_pid),
            "the fixture run did not start"
        );

        assert!(
            cancel("example", "run-1").is_err(),
            "a press carrying another run's name was accepted"
        );
        assert!(
            alive(&pane_pid) && tmux(&["has-session", "-t", "=skein-update"]),
            "a press carrying another run's name stopped this run"
        );

        assert_eq!(cancel("example", "run-2"), Ok(Cancelled::Stopped));
        assert!(
            eventually(|| !alive(&pane_pid) && !tmux(&["has-session", "-t", "=skein-update"])),
            "the update's session is still there after Cancel"
        );
        assert!(
            eventually(|| !alive(&hup_pid)),
            "a process the update started is still running after Cancel — it ignored the SIGHUP \
             that ending the session sends, and nothing else was sent to it"
        );
        assert!(
            alive(&decoy_pid) && tmux(&["has-session", "-t", "=skein-update-decoy"]),
            "Cancel stopped a session that is not the update's"
        );
        assert!(
            alive(&outsider_pid),
            "Cancel stopped a process outside the update"
        );
        let said = log_from("", 0);
        assert!(
            said.done && said.cancelled && !said.ok,
            "the run was stopped but not recorded as cancelled, so the pane would go on saying \
             \"updating…\": {said:?}"
        );
        assert!(
            said.text.contains("fetching the default branch")
                && said.text.contains("you cancelled this update"),
            "the log lost what the run had said, or does not say it was cancelled: {:?}",
            said.text
        );

        // And a run that ended by itself keeps its own last word.
        believe("run-3");
        std::fs::write(done_path(), b"0").unwrap();
        assert_eq!(cancel("example", "run-3"), Ok(Cancelled::AlreadyEnded));
        assert!(
            log_from("", 0).ok,
            "Cancel overwrote the marker of an update that had already succeeded"
        );

        let _ = outsider.kill();
        let _ = outsider.wait();
        tmux(&["kill-server"]);
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

        // Put back what this set, here rather than leaving it to the lock. `env_lock`'s guard does
        // restore now (SKEIN-705), but it is bound above `home` and so drops after it — this line
        // is what stops `$SKEIN_HOME` naming a deleted directory in between. Measured, on the day
        // the guard was still a bare mutex: without this line a probe taking the lock next read
        // `SKEIN_HOME=/tmp/skein-test-<pid>-2` at a path that no longer exists, which is
        // `config::skein_home`'s SKEIN-626 refusal silently answered instead of raised — an
        // unpinned test after this one would have been handed a dead directory rather than the
        // panic that tells it to pin.
        std::env::remove_var("SKEIN_HOME");
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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_GITHUB_API", &base);
        env.set("SKEIN_SOURCE_URL", "https://github.com/acme/skein?x=1.git");
        env.set("SKEIN_SOURCE_REF", "release#2");

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
    }

    /// **A stored token GitHub refuses is dropped for one more ask, and the pane is told**
    /// (SKEIN-1172).
    ///
    /// The fake GitHub answers exactly as the real one did on 2026-09-26: a 401 "Bad credentials"
    /// to a rotated token, and the commit to a request with no `Authorization` header at all.
    ///
    /// **What would make each assertion fail:** `ask` returning the first answer instead of asking
    /// again fails the sha assertion with "GitHub said 401: Bad credentials"; `github::config`
    /// sending `Authorization: Bearer ` for the empty token (as it did) makes the second ask a 401
    /// too, and fails the same assertion and the header one; `token_refused: false` on that arm
    /// fails the flag.
    #[test]
    fn a_refused_stored_token_is_asked_again_without_it_and_the_pane_is_told() {
        let _g = crate::testutil::env_lock();
        let (base, heard) = token_github();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_GITHUB_API", &base);
        env.set("SKEIN_SOURCE_URL", "https://github.com/acme/skein.git");
        env.set("SKEIN_SOURCE_REF", "");

        let asked = ask(
            Some(crate::secret::Secret::new("skein-test-rotated-token")),
            ask_github,
        );
        assert_eq!(
            asked.answer,
            Ok(TOKEN_GITHUB_SHA.to_string()),
            "the check did not come back with the anonymous answer"
        );
        assert!(asked.token_refused, "the refused token was not reported");
        assert_eq!(
            heard.lock().unwrap().clone(),
            vec![
                "Bearer skein-test-rotated-token".to_string(),
                String::new()
            ],
            "the token was not tried first, or the second ask still carried an Authorization header"
        );
        // And the pane reads it by this name.
        let drawn = serde_json::to_value(Available {
            token_refused: asked.token_refused,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(drawn["token_refused"], serde_json::json!(true));
    }

    /// **Only a refused token is reported** (SKEIN-1172): a token GitHub accepts, and no token at
    /// all, are one ask each and no word to the pane.
    ///
    /// **What would make it fail:** setting `token_refused` whenever a token was present, or on
    /// every answer; or asking twice regardless, which the request count catches.
    #[test]
    fn a_token_github_accepts_and_no_token_at_all_are_one_ask_and_no_warning() {
        let _g = crate::testutil::env_lock();
        let (base, heard) = token_github();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_GITHUB_API", &base);
        env.set("SKEIN_SOURCE_URL", "https://github.com/acme/skein.git");
        env.set("SKEIN_SOURCE_REF", "");

        for token in [
            Some(crate::secret::Secret::new("skein-test-good-token")),
            None,
        ] {
            heard.lock().unwrap().clear();
            let asked = ask(token, ask_github);
            assert_eq!(asked.answer, Ok(TOKEN_GITHUB_SHA.to_string()));
            assert!(
                !asked.token_refused,
                "a token that was not refused was reported"
            );
            let said = heard.lock().unwrap().clone();
            assert_eq!(said.len(), 1, "{said:?}");
        }
    }

    /// **Only a 401 is a refused token** (SKEIN-1172). A token ask that fails for any other
    /// reason — GitHub's own 5xx, a 403 about the repository — is returned exactly as it came, with
    /// no second ask and no word about the token, because "your stored GitHub token was refused" would
    /// then be false.
    ///
    /// The failures are GitHub's real ones through `github::get_json`, so the sentences are the ones
    /// that function actually writes: `GitHub said 500: …` from a JSON `message`, and
    /// `GitHub answered 502: …` from a body that is not JSON. A 403 rate limit is left out on
    /// purpose: it engages the process-wide hold in `github`, which would refuse every later GitHub
    /// call in this test binary.
    ///
    /// **What would make it fail:** `ask` retrying on any error (`Err(_why) => Asked { …` in place of
    /// the `refused_credentials` guard). The second ask then answers with the sha, and the first
    /// assertion fails naming the status it swallowed.
    #[test]
    fn a_token_ask_that_fails_for_another_reason_is_returned_as_it_came_and_asked_once() {
        let _g = crate::testutil::env_lock();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_SOURCE_URL", "https://github.com/acme/skein.git");
        env.set("SKEIN_SOURCE_REF", "");
        for (status, body, said) in [
            (
                "500 Internal Server Error",
                r#"{"message":"Server Error"}"#,
                "GitHub said 500: Server Error",
            ),
            (
                "502 Bad Gateway",
                "<html>bad gateway</html>",
                "GitHub answered 502: <html>bad gateway</html>",
            ),
            (
                "403 Forbidden",
                r#"{"message":"Resource not accessible by personal access token"}"#,
                "GitHub said 403: Resource not accessible by personal access token",
            ),
        ] {
            let (base, heard) = answering_github(status, body);
            env.set("SKEIN_GITHUB_API", &base);
            let asked = ask(
                Some(crate::secret::Secret::new("skein-test-some-token")),
                ask_github,
            );
            assert_eq!(
                asked.answer,
                Err(said.to_string()),
                "a {status} to the token ask was not returned as it came"
            );
            assert!(
                !asked.token_refused,
                "a {status} was reported as a refused token"
            );
            let said = heard.lock().unwrap().clone();
            assert_eq!(
                said,
                vec!["Bearer skein-test-some-token".to_string()],
                "a {status} was followed by a second ask"
            );
        }
    }

    /// **Every way `github::get_json` words a 401 is recognised as one** (SKEIN-1172): a JSON
    /// `message` (`GitHub said 401: …`), a body that is not JSON (`GitHub answered 401: …`), and no
    /// body at all (`GitHub answered 401 with an empty body`). The sentences are produced by the
    /// real function against a GitHub that answers each way, not written out here.
    ///
    /// **What would make it fail:** `refused_credentials` dropping either prefix; the case it no
    /// longer matches then fails by name. And the non-401 answers, worded by the same function, must
    /// not match — a prefix loosened to `GitHub said 4` fails the second assertion.
    #[test]
    fn every_wording_get_json_gives_a_401_is_a_refused_token_and_no_other_status_is() {
        let _g = crate::testutil::env_lock();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_SOURCE_URL", "https://github.com/acme/skein.git");
        env.set("SKEIN_SOURCE_REF", "");
        let mut worded = |status: &'static str, body: &'static str| {
            let (base, _) = answering_github(status, body);
            env.set("SKEIN_GITHUB_API", &base);
            ask_github(&crate::secret::Secret::new("skein-test-some-token"))
                .expect_err("the fake GitHub answered a failure")
        };
        for (status, body) in [
            ("401 Unauthorized", r#"{"message":"Bad credentials"}"#),
            ("401 Unauthorized", "<html>unauthorized</html>"),
            ("401 Unauthorized", ""),
        ] {
            let why = worded(status, body);
            assert!(
                refused_credentials(&why),
                "get_json's 401 wording {why:?} (body {body:?}) was not read as a refused token"
            );
        }
        for (status, body) in [
            ("404 Not Found", r#"{"message":"Not Found"}"#),
            ("403 Forbidden", r#"{"message":"Must have admin rights"}"#),
            ("500 Internal Server Error", ""),
        ] {
            let why = worded(status, body);
            assert!(
                !refused_credentials(&why),
                "get_json's {status} wording {why:?} was read as a refused token"
            );
        }
    }

    /// A GitHub that answers `status` with `body` to any request carrying `Authorization`, and the
    /// commit to one carrying none. Records each request's `Authorization`, or "" for none.
    fn answering_github(
        status: &'static str,
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
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
                let auth = said
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                            .map(|(_, v)| v.trim().to_string())
                    })
                    .unwrap_or_default();
                seen.lock().unwrap().push(auth.clone());
                let (status, answer) = match auth.is_empty() {
                    true => ("200 OK", format!(r#"{{"sha":"{TOKEN_GITHUB_SHA}"}}"#)),
                    false => (status, body.to_string()),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (base, heard)
    }

    const TOKEN_GITHUB_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A GitHub with opinions about credentials: `skein-test-good-token` and no `Authorization`
    /// header get the commit, and any other token a 401 "Bad credentials". Records each request's
    /// `Authorization` value, or an empty string when it carried none.
    fn token_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
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
                let auth = said
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                            .map(|(_, v)| v.trim().to_string())
                    })
                    .unwrap_or_default();
                seen.lock().unwrap().push(auth.clone());
                let (status, answer) = match auth.as_str() {
                    "" | "Bearer skein-test-good-token" => {
                        ("200 OK", format!(r#"{{"sha":"{TOKEN_GITHUB_SHA}"}}"#))
                    }
                    _ => (
                        "401 Unauthorized",
                        r#"{"message":"Bad credentials","status":"401"}"#.to_string(),
                    ),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (base, heard)
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
