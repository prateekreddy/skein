//! Small, dependency-free helpers shared across skein: shell quoting, atomic writes, bounded
//! subprocess capture, and string trimming.
//!
//! Nothing here knows what a box is. If a helper needs to know, it belongs in the module that
//! owns that concept — this one stays safe to call from anywhere.
//!
//! `valid_name` is the one that looks like an exception and is not: what it enforces is that a
//! string is safe to join onto a path, and every caller happens to be passing a box name. It lives
//! here so the check is available to code that must not depend on the registry to make it.

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// The marker that says this process is a test run, and the reason it is an environment variable.
///
/// `cfg!(test)` is **false inside this library when it is linked into a `tests/*.rs` integration
/// binary** — the library is compiled once, without `--cfg test`, and every integration binary
/// links that build. So a `cfg!(test)` guard is absent from exactly the suites that drive the most
/// fleet machinery. (`fleet::fleet_disk_usage`'s `if cfg!(test)` already has that asymmetry, and
/// its cache is therefore live under every `tests/*.rs`.)
///
/// `.cargo/config.toml` sets it in the `[env]` table, so a plain `cargo test` in this tree carries
/// it with nothing to remember — which is the point, since the failure this guards was a `cargo
/// test` run by somebody who had not been told to export anything. `tests/harness.rs` asserts it
/// arrives in an integration binary, where `cfg!(test)` cannot.
///
/// It lives here rather than in `config`, where it was written, because [`fleet_root`] below needs
/// it and `config` is the module that depends on this one. The alternative was a second copy of
/// `cfg!(test) || env::var_os(..)` in this file, and the second implementation of a rule is the one
/// that goes wrong — so the predicate moved down to the module both guards can reach, and `config`
/// now reads it along the edge it already had (SKEIN-690).
pub const TEST_MARKER: &str = "SKEIN_TEST";

/// Is this a test process? [`TEST_MARKER`], or `cfg!(test)` for the crate's own unit tests, which
/// have it whether or not cargo was invoked from this tree.
pub fn in_test() -> bool {
    cfg!(test) || env::var_os(TEST_MARKER).is_some_and(|v| !v.is_empty())
}

/// The fleet root: `$SKEIN_FLEET_ROOT`, or `/boxes` — and **a test that has not pinned it is
/// refused rather than answered**, exactly as [`crate::config::skein_home`] refuses an unpinned
/// `$SKEIN_HOME` (SKEIN-626).
///
/// One definition, because there were two — this one and a byte-identical copy in `place`, which
/// derived the in-sandbox agent's socket from it — and two copies of a default is two places for it
/// to stop agreeing. `place` cannot reach `fleet`, which is why the shared one lives here.
///
/// **This doc used to argue the guard did not belong here**, on the ground that ~29 tests read the
/// root only to build a string they never act on and a panic would fail all of them for a hazard
/// none of them has. That premise did not survive being measured. The readers that never act on the
/// string are cheap to pin — `src/fleet.rs` alone already pins the variable in 95 places — while
/// the ones that do act on it were found by damage, one at a time, six times: five tests installed
/// uncommitted code onto the owner's live fleet (SKEIN-530), `tests/server.rs` spawned a real
/// `skein-server` whose `main` runs `heal_fleet` against whatever root it resolves (SKEIN-685), and
/// `health::tests::a_missing_tool_is_one_fault_and_not_five` passed or failed on how full the real
/// machine's disk was, while its message accused the code (SKEIN-690). A pin that is merely
/// recommended is a pin that is sometimes missing, and the missing one is invisible: it matches no
/// grep, because the defect is the absence.
///
/// The refusal names both variables for the same reason `skein_home`'s does — a test that resolves
/// a fleet path usually needs the home as well, and being told about one variable at a time costs
/// two runs.
pub fn fleet_root() -> String {
    if let Some(root) = env::var("SKEIN_FLEET_ROOT").ok().filter(|v| !v.is_empty()) {
        return root;
    }
    assert!(
        !in_test(),
        "$SKEIN_FLEET_ROOT is unset in a test process (${TEST_MARKER}). Refusing to fall back to \
         /boxes: on any machine running skein that is the owner's LIVE fleet, and a test that \
         resolves a fleet path there reads real boxes' state and disks — or writes to them, which \
         five tests did (SKEIN-530, SKEIN-685, SKEIN-690). Set $SKEIN_FLEET_ROOT to this test's \
         own temp directory — and $SKEIN_HOME with it, since anything resolving a fleet path \
         almost certainly resolves a home too."
    );
    "/boxes".to_string()
}

pub fn with_lock<T>(lock_path: &Path, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use fs2::FileExt;
    if let Some(dir) = lock_path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| format!("opening {}: {e}", lock_path.display()))?;
    lock.lock_exclusive()
        .map_err(|e| format!("locking {}: {e}", lock_path.display()))?;
    let out = f();
    // Explicit rather than left to the drop: the unlock has to happen whichever way `f` went, and
    // saying so is cheaper to check than tracing the lifetime of a file handle.
    let _ = FileExt::unlock(&lock);
    out
}

/// The lock file that guards a declared file: beside it, and named for it.
///
/// One function rather than the same `format!` written wherever a lock is taken, because two
/// writers that agree on the FILE and disagree about which lock guards it is the lost update
/// [`with_lock`] exists to prevent, with more moving parts and nothing to see in either diff.
/// [`update_json`] takes this lock, and so does the locked read-modify-write in
/// [`crate::prwork`], whose three files keep their own refusal wording and therefore cannot go
/// through `update_json` itself (SKEIN-414).
///
/// Beside the file rather than one lock for the directory: `review/<repo>/` holds the stops, the
/// assignments and the journal, and a shared lock would make a journal entry wait on a merge.
pub(crate) fn lock_beside(path: &Path) -> Result<std::path::PathBuf, String> {
    let dir = path.parent().ok_or("no directory to write into")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("unusable file name")?;
    Ok(dir.join(format!(".{name}.lock")))
}

/// A JSON file's contents, with "not there" and "there and unreadable" kept apart.
///
/// The generic form of `repos::read_repos_or_why` and `tracking::read_connections`, and the one
/// distinction `fs::read_to_string(..).ok().and_then(|t| serde_json::from_str(&t).ok())` destroys:
/// it answers `None` to both, and a caller that then writes what it was handed replaces a file it
/// merely could not parse with a default nobody chose.
///
/// - `Ok(None)` — nobody has written this file yet. An empty opinion, and the one case where
///   `T::default()` is the truth.
/// - `Ok(Some(value))` — it is there and it parses.
/// - `Err(why)` — it is there and it could not be read: a read error, a parse error, or the
///   zero-length file a crash between [`write_atomic`]'s write and its rename used to leave behind.
///   Every entry somebody put in it is still on disk, and `why` names the file so the person told
///   about it can go and look.
pub fn read_json_or_why<T>(path: &Path) -> Result<Option<T>, String>
where
    T: serde::de::DeserializeOwned,
{
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// What skein says instead of writing a default over a file it could not read.
///
/// One wording for every caller of [`update_json`], for the reason `repos::unreadable_refusal`
/// gives for its two: the paths differ only in which file is about to be lost, and the sentence a
/// person needs is the same one — what would have been destroyed, and that the file was left alone.
///
/// Private: the only way to provoke it is to try the write and be refused, which is the shape a
/// caller should be in anyway. A module wanting its own wording writes its own, as `repos` and
/// `tracking` do — theirs name what is in the file, and a generic sentence cannot.
fn unreadable_refusal(path: &Path, why: &str) -> String {
    format!(
        "not writing over {} — skein cannot read it ({why}). Writing now would replace everything \
         in that file with a default nobody chose. The file is left alone; fix or move it, then \
         try again.",
        path.display()
    )
}

/// What [`update_json_on`] does about a file that is there and will not parse.
enum OnUnreadable {
    /// Refuse the write and say so. The default answer, and what every caller wants unless it can
    /// argue otherwise at the call site.
    Refuse,
    /// Take `T::default()` and write over it. Only for a file whose whole content is disposable —
    /// see [`update_json_lossy`], which is the only way to ask for this.
    TakeTheDefault,
}

/// Read a JSON file, change it, write it back — with an exclusive lock held across all three, and
/// **refusing on a file it could not read**.
///
/// The generic form of [`with_lock`], for the declared files that are lists or maps rather than one
/// struct: grants, the package manifest, stored write credentials. Every one of them is a
/// read-modify-write over a whole collection, so a lost update is a lost *entry*, not a lost field.
///
/// **The refusal is the whole point, and this used to be on the other side of it.** A missing file
/// reads as `T::default()`, which for a collection is the truth — an absent grants file is no
/// grants. An *unreadable* one used to read as `T::default()` too, and that is a different act: the
/// file is still there, still holds every entry somebody put in it, and the write that follows
/// replaces the lot. SKEIN-347 found those three lines behind grants, the package manifest and the
/// attempt leases, with [`write_atomic`] above as the mechanism that manufactured the unparseable
/// file; SKEIN-359 turned the helper itself round, so that a caller added tomorrow inherits the
/// refusal rather than the loss. `config::save_config`, `repos::update_repos` and
/// `tracking::update_connections` refuse in their own words, for their own files, and this is the
/// same refusal.
///
/// A caller for whom the file's contents genuinely are disposable says so at the call, through
/// [`update_json_lossy`].
///
/// The lock file sits beside the target, named for it, so two different files never contend.
pub fn update_json<T, R>(
    path: &Path,
    f: impl FnOnce(&mut T) -> Result<R, String>,
) -> Result<R, String>
where
    T: serde::de::DeserializeOwned + serde::Serialize + Default,
{
    update_json_on(path, f, OnUnreadable::Refuse)
}

/// [`update_json`], but an unreadable file is taken as `T::default()` and written over.
///
/// **Named, so that choosing it is visible in the diff that chooses it.** There is exactly one
/// caller — `attempt`'s lease file — and its argument is that the file holds nothing durable: a
/// lease is a claim with a deadline on it, a lease nobody can parse cannot be honoured, and
/// refusing here would block that one operation for ever on a file no human ever looks at. Anything
/// whose contents somebody would miss uses [`update_json`] and is told instead.
pub fn update_json_lossy<T, R>(
    path: &Path,
    f: impl FnOnce(&mut T) -> Result<R, String>,
) -> Result<R, String>
where
    T: serde::de::DeserializeOwned + serde::Serialize + Default,
{
    update_json_on(path, f, OnUnreadable::TakeTheDefault)
}

fn update_json_on<T, R>(
    path: &Path,
    f: impl FnOnce(&mut T) -> Result<R, String>,
    unreadable: OnUnreadable,
) -> Result<R, String>
where
    T: serde::de::DeserializeOwned + serde::Serialize + Default,
{
    let dir = path
        .parent()
        .ok_or("no directory to write into")?
        .to_path_buf();
    with_lock(&lock_beside(path)?, || {
        let read = read_json_or_why::<T>(path);
        let mut current: T = match (read, &unreadable) {
            (Ok(found), _) => found.unwrap_or_default(),
            (Err(why), OnUnreadable::Refuse) => return Err(unreadable_refusal(path, &why)),
            (Err(_), OnUnreadable::TakeTheDefault) => T::default(),
        };
        let out = f(&mut current)?;
        let bytes = serde_json::to_vec_pretty(&current).map_err(|e| e.to_string())?;
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        write_atomic(path, &dir, &bytes)?;
        Ok(out)
    })
}

/// Load a local `.env` (searched from the cwd upward) so the registry/repo paths and `*_CMD`
/// templates needn't be passed on every invocation. Variables already set in the real
/// environment win — dotenv never overrides — so a command-line `VAR=… skein …` still takes
/// precedence. A missing file is fine and silent; a *malformed* file is reported on stderr
/// rather than silently dropping every line after the bad one (which once made a quoting slip
/// look like a "command not found"). The binaries call this once at startup.
pub fn load_dotenv() {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("skein: ignoring malformed .env — {e}"),
    }
}

/// Write `bytes` to `path` atomically: a temp file in the same dir, then rename (POSIX-atomic),
/// so a concurrent reader sees either the old or the new whole file, never a truncated one. `dir`
/// must be `path`'s parent (same filesystem) for the rename to be atomic.
///
/// **Atomic against a reader was not atomic against a crash**, and the gap between those two is
/// where SKEIN-347's corrupt file came from. `fs::write` + `fs::rename` journals the rename and
/// leaves the bytes in the page cache; on ext4 a power loss, a host reboot or a hard kill of the
/// sandbox in that window classically leaves the renamed file present and **zero-length**. Zero
/// length is not a short read anybody notices — every caller here parses JSON, and an empty file is
/// unparseable, which is precisely the "unreadable" input that a read-modify-write used to answer
/// with `T::default()` and then write back over the survivors. `grep -rn 'sync_all' src/` returned
/// nothing before this line existed. So the bytes reach the disk *before* the rename, not after.
///
/// The directory flush is deliberately best-effort. Without it the rename itself can be lost, and
/// the file that comes back is the whole OLD one — a write that did not happen, never a corrupt
/// one — so a filesystem that refuses to fsync a directory must not fail every write skein makes.
pub(crate) fn write_atomic(path: &Path, dir: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    // pid + per-call counter: a pid-only temp name let two threads of the same process writing
    // into the same dir clobber each other's temp mid-write and rename the wrong bytes into place.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".skein.tmp.{}.{n}", std::process::id()));
    // The temp is removed on every failure below, not only on a failed rename: a half-written temp
    // left behind is a file nothing will ever rename into place and nothing will ever clean up.
    let flushed = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = flushed {
        let _ = fs::remove_file(&tmp);
        return Err(format!("writing temp: {e}"));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("renaming into place: {e}")
    })?;
    let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}

/// Run a command with a hard wall-clock bound: kill + reap on expiry, `None` on timeout/spawn
/// failure. Pipes are drained on their own threads so a chatty child can't fill the pipe buffer
/// and deadlock against the polling loop. Dependency-free; callers are all off the async runtime
/// (blocking pool / CLI).
///
/// Most callers only need "did it work", which is why this stays an `Option`. When the difference
/// between *not there* and *too slow* is the whole answer, ask [`output_with_timeout_why`].
pub(crate) fn output_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> Option<std::process::Output> {
    output_with_timeout_why(cmd, timeout).ok()
}

/// The same run, saying which way it failed.
///
/// The two failures this separates are indistinguishable in an `Option` and want opposite responses
/// from a person: a binary that is not on **this process's** PATH is a launcher problem, and one
/// that ran out of time is a sick daemon. Collapsing them cost a real debugging session — `sbx ls`
/// worked in a terminal while the server said the fleet had not answered, and nothing skein printed
/// could tell those apart, because a server started from a desktop session or a unit file does not
/// have the PATH the shell that started it by hand does.
///
/// So the PATH is *in* the message. It is the fact that settles it, and the one thing the person
/// reading the message cannot look up afterwards — by the time they check, they are checking their
/// shell's PATH, which is the one that works.
pub(crate) fn output_with_timeout_why(
    cmd: &mut Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    run_bounded(cmd, None, timeout)
}

/// The same run, with `feed` written to the child's stdin and the pipe then closed.
///
/// **This is how a payload too big for `execve` reaches a program** (SKEIN-684). Linux caps a
/// *single* argv element at `MAX_ARG_STRLEN` — 32 pages, independent of the much larger `ARG_MAX`
/// total: 524,288 bytes on a 16 KiB-page machine and 131,072 on a 4 KiB-page one, measured here by
/// spawning `/bin/true` with one argument of each length (524,287 ran, 524,288 was `E2BIG`, the
/// cap counting the terminating NUL, so the boundary is 32 pages exactly). Past
/// it the spawn fails before the program is reached, and it fails as "could not start", which is a
/// sentence about the binary rather than about the size. A pipe has no such ceiling.
///
/// It is also what keeps the payload out of `ps`: argv is `/proc/<pid>/cmdline`, readable by
/// anything that can see the process for as long as it runs, and stdin is not. SKEIN-516's rule —
/// no secret on argv or in a URL — was written about credentials, and a diff of a private
/// repository is the same class of thing.
///
/// **The write is on its own thread**, for the reason the two reads are: the child can fill the
/// stdout pipe while this side is still filling its stdin, and both sides blocking on a full pipe
/// is a deadlock neither timeout can see, because the child is alive and so is skein. A write that
/// ends in `EPIPE` — the child exited, or was killed on the timeout — is not an error here: what
/// the child did with what it was given is the caller's answer, and it is already in the exit code
/// and the two streams.
pub(crate) fn output_with_timeout_fed(
    cmd: &mut Command,
    feed: Vec<u8>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    run_bounded(cmd, Some(feed), timeout)
}

/// The body both of the above share: spawn, drain, wait, kill on expiry.
///
/// `feed` decides what the child's stdin is — `None` is `/dev/null`, which is what every caller
/// but the model call wants, and `Some` is a pipe carrying exactly those bytes and then EOF.
fn run_bounded(
    cmd: &mut Command,
    feed: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    use std::io::Read as _;
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(match feed {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .spawn()
        .map_err(|e| spawn_failure(cmd, &e))?;
    let (Some(mut out_pipe), Some(mut err_pipe)) = (child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{} started without pipes", program_of(cmd)));
    };
    if let Some(bytes) = feed {
        let Some(mut in_pipe) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} started without a stdin pipe", program_of(cmd)));
        };
        std::thread::spawn(move || {
            let _ = in_pipe.write_all(&bytes);
            let _ = in_pipe.flush();
            // Dropped here rather than at the end of the closure only for emphasis: the child is
            // waiting for EOF, and EOF is this handle closing.
            drop(in_pipe);
        });
    }
    let out_h = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out_pipe.read_to_end(&mut v);
        v
    });
    let err_h = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err_pipe.read_to_end(&mut v);
        v
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not finish within {} and was killed",
                    program_of(cmd),
                    budget(timeout)
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for {}: {e}", program_of(cmd)));
            }
        }
    };
    Ok(std::process::Output {
        status,
        stdout: out_h.join().unwrap_or_default(),
        stderr: err_h.join().unwrap_or_default(),
    })
}

/// A timeout as a person would say it. Seconds read as "0s" below a second, which is the one case
/// where the number is the whole point of the sentence.
fn budget(timeout: Duration) -> String {
    match timeout.as_secs() {
        0 => format!("{}ms", timeout.as_millis()),
        secs => format!("{secs}s"),
    }
}

/// How the command names itself in a failure message.
fn program_of(cmd: &Command) -> String {
    format!("`{}`", cmd.get_program().to_string_lossy())
}

/// Why a spawn failed, in terms of the thing the reader can act on.
///
/// `NotFound` gets the PATH spelled out, because that is the case where the reader's own shell will
/// contradict the message and they need to see *which* PATH skein had. Everything else (a permission
/// bit, a broken interpreter line) is reported as the OS put it.
///
/// **Shared, because skein should say one thing about a program it could not start.** `act::begin`
/// says it about the `sh` it runs a command under, and said it from a word-for-word copy of this
/// paragraph until SKEIN-429 — a copy is at best identical on the day it is made, and the next
/// person to improve the wording improves one of the two.
pub(crate) fn spawn_failure(cmd: &Command, e: &std::io::Error) -> String {
    let program = program_of(cmd);
    if e.kind() != std::io::ErrorKind::NotFound {
        return format!("{program} could not be started: {e}");
    }
    format!(
        "{program} is not on this process's PATH ({}). A shell you start by hand may well find it — \
         what matters is the PATH the server was started with.",
        std::env::var("PATH").unwrap_or_else(|_| "unset".into())
    )
}

pub(crate) fn bounded_output(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    output_with_timeout(cmd, timeout).ok_or_else(|| {
        format!(
            "{label} failed to start or exceeded the {}s timeout",
            timeout.as_secs()
        )
    })
}

/// Wrap a string for safe single-quoting in a POSIX shell. Used to quote every value substituted
/// into a `*_CMD` template before it reaches `sh -c`, so a branch/box name can't inject commands.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ───────────────────────────── skein-owned repo registry ─────────────────────────────
//
// skein is no longer single-repo. `~/.skein/repos.json` lists every repo skein manages; each box
// is `<repo-id>-<branch>` and maps back to its repo by id-prefix. This is skein's OWN config —
// distinct from the per-box `sandboxes.json` we dropped — and it's what makes "add a repo URL and
// go" work without the repo shipping anything for skein.

pub(crate) fn program_on_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|dir| {
            let candidate = dir.join(name);
            // Executable, not merely present. This decides which DIAGNOSIS a person is shown — "sbx
            // is installed" versus "sbx is not on this process's PATH" — so a non-executable file of
            // the right name reading as installed sends them to look for the wrong fault.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(&candidate)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            candidate.is_file()
        })
    })
}

/// Expand a leading `~/` to `$HOME` (ssh-add doesn't do shell tilde expansion when called directly).
pub fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

/// Sbx sandbox names can't carry every branch character (notably `/`), so the box name is a *slug* of
/// the branch: anything outside `[A-Za-z0-9._-]` becomes `-`, runs collapse, ends trimmed. The real
/// branch (`feat/auth`) is preserved separately (launch spec → `git checkout`); only the *name* is
/// slugged (`<repo>-feat-auth`). Same branch ⇒ same name (stable), so reconnect/lookup are consistent.
pub fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Host component of an SSH git URL (for the network-policy hint). `None` if unparseable.
pub(crate) fn host_of(url: &str) -> Option<&str> {
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.split(':').next();
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map(|(_, h)| h).unwrap_or(rest);
        return rest.split(['/', ':']).next();
    }
    None
}

/// Run a program in the repo dir ($SKEIN_REPO, else cwd); returns (stdout, stderr, exit-code).
pub(crate) fn run_capture(prog: &str, args: &[&str]) -> Result<(String, String, i32), String> {
    let timeout = env::var("SKEIN_ACTION_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30));
    run_capture_for(prog, args, timeout)
}

/// Run a program with the terminal attached: the child owns stdin/stdout/stderr, and there is no
/// timeout. For steps that are interactive *and* slow — `sbx create` asks for confirmation and then
/// boots a microVM. Capturing its output closes stdin, so the prompt reads EOF and sbx aborts with
/// "user cancelled operation": a question nobody was shown, reported as a refusal.
pub(crate) fn run_attached(prog: &str, args: &[&str]) -> Result<i32, String> {
    run_attached_env(prog, args, &[])
}

/// [`run_attached`] with extra environment for the child.
///
/// Some of what sbx can be told is not a flag: the sandbox's disk sizes are read from the
/// environment (`DOCKER_SANDBOXES_ROOT_SIZE`), not from `sbx create`'s argv. Setting them in *this*
/// process instead would leak into every other child skein spawns for the rest of the run.
pub(crate) fn run_attached_env(
    prog: &str,
    args: &[&str],
    extra_env: &[(String, String)],
) -> Result<i32, String> {
    let mut c = Command::new(prog);
    c.args(args);
    c.envs(extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    if let Ok(repo) = env::var("SKEIN_REPO") {
        if !repo.is_empty() {
            c.current_dir(repo);
        }
    }
    let status = c
        .status()
        .map_err(|e| format!("{prog} failed to start: {e}"))?;
    Ok(status.code().unwrap_or(-1))
}

/// [`run_capture`] with the timeout named at the call site, for work the 30s action budget doesn't fit.
pub(crate) fn run_capture_for(
    prog: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(String, String, i32), String> {
    run_capture_for_env(prog, args, timeout, &[])
}

/// [`run_capture_for`] with extra environment for the child — see [`run_attached_env`].
pub(crate) fn run_capture_for_env(
    prog: &str,
    args: &[&str],
    timeout: Duration,
    extra_env: &[(String, String)],
) -> Result<(String, String, i32), String> {
    let mut c = Command::new(prog);
    c.args(args);
    c.envs(extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    if let Ok(repo) = env::var("SKEIN_REPO") {
        if !repo.is_empty() {
            c.current_dir(repo);
        }
    }
    // The classified runner, so "not installed" never reads as "timed out". Every tool skein drives
    // is one someone has to have — `gh`, `git`, `sbx`, `curl` — and a missing one reported as
    // "failed to start or exceeded the timeout" sends people looking at a network or a daemon for a
    // binary that is simply not there.
    let out = output_with_timeout_why(&mut c, timeout)?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    ))
}

pub(crate) fn run_shell(cmd: &str) -> Result<(String, String, i32), String> {
    run_capture("sh", &["-c", cmd])
}

// ───────────────────────── asking the sandbox without making it worse ─────────────────────────
//
// Everything skein knows about a *running* fleet it learns by spawning a subprocess: `sbx ls` for
// the sandboxes, `sbx exec` for the questions only the guest can answer. The board asks every 2s,
// per open tab, forever — so these are not occasional calls, they are a permanent load, and the way
// they were written turned a slow daemon into a stuck one.
//
// Two properties are missing from a plain "remember it for 1.5s", and both matter only when things
// are already going wrong, which is exactly when they matter:
//
// **Single flight.** Check-then-act means every caller that misses the cache together spawns its
// own subprocess, because the cache is only written when the first one *returns*. One browser tab
// was one call; five tabs were five simultaneous calls, every tick, and a sick daemon got five
// times the load a healthy one did.
//
// **Backoff.** On failure the old code re-armed at the same 1.5s and asked again — so a daemon that
// had gone slow was asked more often than it could answer, and every attempt was SIGKILLed at its
// timeout with the guest-side work left running. Nothing in that loop lets it recover; the only
// event that ever broke it was the user restarting the daemon, which is the one thing that makes
// these calls fail *fast*. Doubling the interval per consecutive failure lets a struggling daemon
// drain its backlog instead of being handed a fresh one every second and a half.

/// A path with every symlink in it resolved, as far as the filesystem can answer.
///
/// **Why any of this exists.** On macOS `/var` is a symlink to `/private/var`, and `$TMPDIR` lives
/// under it. So a volume whose marker was written canonically says `/private/var/…` while the store
/// paths in its own `repos.json` say `/var/…` — the same directory, sharing not one byte of prefix.
/// Every `starts_with` below then answers "no", and the consequences are the opposite of harmless:
/// `skein repoint` reports "0 paths repointed" and rewrites nothing, and `opened_where_it_was_written`
/// finds nothing stale and quietly ADOPTS the copy. The one check standing between somebody and a
/// copied volume that goes on writing to the original disarms itself.
///
/// Not macOS-only, and that is why this is a resolve rather than a special case: a `$SKEIN_HOME`
/// reached through any symlinked component — a home directory on another disk, a linked `~/work`,
/// `/tmp` on several systems — is the same shape.
///
/// **The deepest ancestor that exists is resolved, and the rest is kept verbatim.** A path under a
/// volume that has been moved away from no longer exists, and `canonicalize` on it fails outright —
/// which is exactly when a repoint needs to reason about it.
pub(crate) fn resolved(path: &str) -> String {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut at = Path::new(path);
    loop {
        if let Ok(real) = at.canonicalize() {
            let mut out = real;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out.to_string_lossy().to_string();
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                at = parent;
            }
            // Nothing on this path exists, so there is nothing to resolve it against. The string
            // itself is the best available answer and is what the old code always used.
            _ => return path.to_string(),
        }
    }
}

/// A remembered answer to a question only a subprocess can answer: fresh for a while, asked by one
/// caller at a time, and asked progressively less often while the answers keep failing.
pub struct Gate<T> {
    cell: std::sync::Mutex<Asked<T>>,
    /// Consecutive failures — the backoff exponent, reset by any success.
    fails: std::sync::atomic::AtomicU32,
    /// Held for the duration of an ask, so concurrent callers wait for that one answer instead of
    /// each starting their own.
    lane: std::sync::Mutex<()>,
    /// Set while a refresh runs behind a caller, so the ones arriving during it serve the remembered
    /// answer rather than each spawning a thread that would only queue on the lane.
    refreshing: std::sync::atomic::AtomicBool,
}

struct Asked<T> {
    /// When the last ask *finished*, successfully or not — the clock the interval runs against.
    at: Option<std::time::Instant>,
    /// The last answer actually obtained. Sticky across failures on purpose: it is what a caller
    /// gets while the sandbox is unreachable, so the board keeps its last picture of the fleet
    /// instead of blanking on one slow tick.
    good: Option<T>,
}

/// However bad it gets, keep asking this often — a daemon that came back must be noticed.
pub(crate) const GATE_MAX_INTERVAL: Duration = Duration::from_secs(30);

impl<T: Clone> Gate<T> {
    pub(crate) const fn new() -> Self {
        Gate {
            cell: std::sync::Mutex::new(Asked {
                at: None,
                good: None,
            }),
            fails: std::sync::atomic::AtomicU32::new(0),
            lane: std::sync::Mutex::new(()),
            refreshing: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The answer, asking `take` for a new one only when the remembered one has aged out.
    ///
    /// **A caller waits only when there is nothing to serve it.** Once the gate holds an answer,
    /// ageing out costs the caller nothing: it gets the remembered one immediately and the ask runs
    /// behind it, so the next caller finds a fresh one. That is the difference between a wedged
    /// subprocess making the cockpit *slightly stale* and making it *stop*: measured against a hung
    /// sbx daemon, a board refresh walked four gates in series and took 31 seconds, because each one
    /// made its caller sit out the full timeout before handing back the very answer it already had.
    /// Nothing about that wait improved the answer — the value returned was the remembered one
    /// either way.
    ///
    /// Waiting is still right in exactly two cases, and both are preserved:
    ///
    /// * **nothing remembered yet** — a cold gate has nothing to hand back, so the first caller has
    ///   to go and find out;
    /// * **[`invalidate`](Self::invalidate)d** — skein has just changed the thing being asked about
    ///   (started a box, stopped one) and *knows* the remembered answer is wrong. Serving it while a
    ///   refresh runs behind would show a box as stopped immediately after starting it.
    ///
    /// The two are one condition in the data: no clock (`at == None`) means either never asked or
    /// deliberately expired, and both must block. An aged-out clock means merely old, which must not.
    ///
    /// A `fresh` of zero disables the gate entirely — no remembering, no single flight. Unit tests
    /// swap the underlying command per case and run in parallel, so one test's fleet must never be
    /// served to another.
    pub(crate) fn get(
        &'static self,
        fresh: Duration,
        take: impl FnOnce() -> Option<T> + Send + 'static,
    ) -> Option<T>
    where
        T: Send + 'static,
    {
        if fresh.is_zero() {
            return take();
        }
        if let Some(remembered) = self.remembered(fresh) {
            return remembered;
        }
        if let Some(good) = self.servable_while_stale() {
            self.refresh_behind(fresh, take);
            return Some(good);
        }
        self.ask(fresh, take)
    }

    /// The remembered answer when it is merely old, or `None` when the caller must wait for a real
    /// one. See [`get`](Self::get) for why those are the same two cases.
    fn servable_while_stale(&self) -> Option<T> {
        let cell = self.cell.lock().unwrap_or_else(|e| e.into_inner());
        cell.at.and(cell.good.clone())
    }

    /// Run the ask on a thread of its own, at most one at a time.
    ///
    /// The flag rather than the lane: a caller that finds the lane held could simply return, but it
    /// would have paid for a thread to discover that. Since every caller arriving during a refresh
    /// takes this path, that is a thread per caller per tick against a daemon that is, by
    /// construction, already the slow thing.
    fn refresh_behind(
        &'static self,
        fresh: Duration,
        take: impl FnOnce() -> Option<T> + Send + 'static,
    ) where
        T: Send + 'static,
    {
        use std::sync::atomic::Ordering;
        if self.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        std::thread::spawn(move || {
            self.ask(fresh, take);
            self.refreshing.store(false, Ordering::Release);
        });
    }

    /// Ask, and remember the answer. Blocking, single-flighted, and the only writer of the clock.
    fn ask(&self, fresh: Duration, take: impl FnOnce() -> Option<T>) -> Option<T> {
        let _lane = self.lane.lock().unwrap_or_else(|e| e.into_inner());
        // Whoever held the lane may have just answered this for us while we waited.
        if let Some(remembered) = self.remembered(fresh) {
            return remembered;
        }
        let taken = take();
        let mut cell = self.cell.lock().unwrap_or_else(|e| e.into_inner());
        match taken {
            Some(value) => {
                self.fails.store(0, std::sync::atomic::Ordering::Relaxed);
                cell.good = Some(value);
            }
            // A failed ask keeps the last good answer rather than reporting the fleet gone.
            None => {
                self.fails
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        // Re-armed either way — that is what makes the *next* ask the backed-off one.
        cell.at = Some(std::time::Instant::now());
        cell.good.clone()
    }

    /// How long the current answer stands: `fresh` while the sandbox is answering, doubling per
    /// consecutive failure up to [`GATE_MAX_INTERVAL`].
    pub(crate) fn interval(&self, fresh: Duration) -> Duration {
        let fails = self
            .fails
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(16);
        fresh
            .saturating_mul(1u32 << fails)
            .clamp(fresh, GATE_MAX_INTERVAL.max(fresh))
    }

    /// The standing answer while it is still young enough to serve, or `None` meaning "go ask".
    /// Nested, because "we asked and got nothing" is itself an answer worth not re-asking for.
    fn remembered(&self, fresh: Duration) -> Option<Option<T>> {
        let interval = self.interval(fresh);
        let cell = self.cell.lock().unwrap_or_else(|e| e.into_inner());
        cell.at
            .filter(|at| at.elapsed() < interval)
            .map(|_| cell.good.clone())
    }

    /// Whether the last ask failed — so the cockpit can say it is showing a remembered answer
    /// rather than present stale data as live.
    pub(crate) fn degraded(&self) -> bool {
        self.fails.load(std::sync::atomic::Ordering::Relaxed) > 0
    }

    /// Expire the standing answer, so the next caller asks — and *waits* for the reply rather than
    /// being handed the old one while a refresh runs behind it. For the moments when skein itself
    /// has just changed the thing being asked about — started a box, stopped one — and therefore
    /// knows the remembered answer to be wrong, as opposed to merely old. Ageing out is the other
    /// case and is deliberately cheaper; see [`get`](Self::get).
    ///
    /// Expires the *clock*, not the last good answer: if the ask that follows fails, falling back
    /// to what was true a moment ago still beats reporting that the fleet has gone. And it leaves
    /// the failure count alone, because that records whether the sandbox is answering, which skein
    /// having changed something says nothing about. Any successful ask clears it.
    pub(crate) fn invalidate(&self) {
        self.cell.lock().unwrap_or_else(|e| e.into_inner()).at = None;
    }
}

// ───────────────────────────── attachments: paste / drop into a box ─────────────────────────────
//
// The agent runs *inside* the sandbox: it can't see the user's clipboard, their Downloads folder, or
// anything else on the host. Anything the user wants to hand it — a screenshot, a PDF, a video, a
// whole folder of samples — has to be copied into the box first, then referenced by its in-box path.
// One drop (paste, drag-and-drop, file picker) becomes one `/tmp/skein-drop-<batch>/` directory:
// per-batch so a folder keeps its structure and the agent can be handed the directory itself, and so
// same-named files from different drops never clobber each other.

/// Sanitise one browser-supplied path component into a plain, single-segment filename. Letters and
/// digits of any script are kept — `née deed.pdf` and CJK names stay readable rather than turning into
/// hyphen soup — and everything else collapses to `-`: no separator, quote, glob, space, or control
/// character survives, so the name is safe both as a path and as a bare token pasted into a prompt.
/// Leading dots are stripped (kills `..` and dotfiles that would hide the drop) and the name is capped
/// at 80 chars **keeping its extension**, since the suffix is what tells the agent it got an `.mp4`.
pub(crate) fn safe_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.starts_with('.') {
        out.remove(0);
    }
    if out.chars().count() > 80 {
        let ext = out
            .rsplit_once('.')
            .map(|(_, e)| e)
            .filter(|e| !e.is_empty() && e.chars().count() <= 8)
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        // char-wise, not `truncate`: a multibyte name would panic on a byte boundary.
        let stem: String = out.chars().take(80 - ext.chars().count()).collect();
        out = stem + &ext;
    }
    out
}

/// Percent-decode a header value. Filenames are arbitrary UTF-8 (`née.pdf`, CJK, emoji) but HTTP
/// headers are ASCII, so the UI sends `encodeURIComponent(name)` and this reverses it. Invalid
/// escapes are left verbatim rather than erroring — `safe_component` sanitises whatever comes out.
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// JSON-quote a string without building a Value for it.
pub(crate) fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The head of `text` within a **byte** budget, never splitting a character.
///
/// The byte-budgeted sibling of [`clip`], and the distinction is the reason both exist: `clip` caps
/// what a person will *read*, so it counts characters; this caps what will be *transferred*, so it
/// counts bytes. Counting characters for a transfer budget would also mean walking a multi-megabyte
/// diff to decide where to cut it.
///
/// `String::truncate` and a raw slice both panic on a byte index inside a character, and a large
/// diff is the likeliest place of all to meet one at an arbitrary offset. Rounds the cut *inward*,
/// so the result stays within budget rather than growing to keep a character whole.
pub(crate) fn clip_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Cut a string to `max` chars on a char boundary, marking that it was cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// [`clip`] from the other end: keep the **last** `max` chars, marking that the start was dropped.
///
/// For the sources where the news is at the end — a journal, a log tail — so the cap has to fall on
/// the part already read rather than the part just written.
///
/// Chars rather than bytes, and that is the whole reason this is a function rather than a slice.
/// `&s[s.len() - max..]` reads like a size budget and is a panic waiting for its first non-ASCII
/// character: it aborts the moment the cut lands inside one. It did — a box whose journal ran past
/// the cap and happened to contain an `…` took a server worker thread down with "byte index 18674
/// is not a char boundary", and every caller of the digest with it. A cap is a display concern and
/// must never be able to fail.
pub(crate) fn keep_tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count - max).collect();
    format!("…{kept}")
}

/// First non-empty line of `s`, whitespace-collapsed and capped — the inbox headline. None when
/// `s` is blank.
pub(crate) fn first_line(s: &str) -> Option<String> {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    const CAP: usize = 120;
    let out = if collapsed.chars().count() > CAP {
        let mut t: String = collapsed.chars().take(CAP).collect();
        t.push('…');
        t
    } else {
        collapsed
    };
    Some(out).filter(|s| !s.is_empty())
}

pub fn shorten(p: &str) -> String {
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() && p.starts_with(&home) {
            return format!("~{}", &p[home.len()..]);
        }
    }
    p.to_string()
}

/// A duration in seconds as the fleet says it: `12s ago`, `4m ago`, `3h ago`, `2d ago`.
///
/// One copy, because three had grown — the fleet row, the verify chip and now provenance — and
/// three spellings of "how old is this" is how two of them end up disagreeing about the same
/// moment. Negative input (a clock that moved) clamps to zero rather than rendering nonsense.
pub fn ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

/// How long ago a file was last written, formatted by [`ago`]. `None` when the path is unreadable
/// or the filesystem's timestamp is in the future by more than rounding.
pub fn file_ago(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let secs = std::time::SystemTime::now()
        .duration_since(modified)
        .ok()?
        .as_secs();
    Some(ago(secs as i64))
}

/// A box name is a registry key, a path component **and a shell word**. It is the last of those
/// that this used to get wrong.
///
/// The old rule was "reject what could escape a filesystem join" — `..`, separators, NUL — which is
/// correct for a path and far too weak for a shell. A name is interpolated into generated scripts in
/// several places (`fleet::apply_box_limits`'s `sudo tee`, the several `echo 'skein: {name} …'`
/// refusals), and while most of those quote it, they did not all quote it, and the ones that did
/// not executed whatever a `"` or a `$(` opened. Proven with the box name `x"; id; echo "`, which
/// the old rule accepted.
///
/// The generator it was proven *through* — the fleet liveness sweep's shell — is gone (SKEIN-615),
/// deleted with the `sbx exec` deployment it belonged to. The proof moved; the exposure did not,
/// which is why this rule stays an allow-list rather than relaxing back to the path question.
///
/// **So the rule is now an allow-list, not a deny-list**, and it is deliberately the character class
/// [`slug`] already produces: a real box name is `<repo-id>-<slug(branch)>`, so every name skein has
/// ever made is inside it. Verified against a live fleet before narrowing — 16 box names, 30 state
/// directories and 8 repo ids, all already conforming.
///
/// A deny-list has to anticipate every metacharacter of every language a name is ever pasted into;
/// an allow-list is true for the ones nobody has thought of yet. That is the whole reason this is
/// the fix rather than quoting the nine sites: quoting is a discipline somebody must keep, and this
/// is a property the type system of the string enforces once.
///
/// Rejected beyond the class: a leading `-`, which argv-parses as a flag wherever a name reaches a
/// command; and a **leading dot**, which is three problems at once and used to be none of them
/// (SKEIN-742).
///
/// The leading-dot rule replaces a narrower one that rejected a name of *only* dots, on the grounds
/// that `Path::join(".")` resolves to the parent. That case is a subset of this one — a non-empty
/// all-dots name begins with a dot — so nothing it caught is let through, and `.` and `..` are
/// still refused for exactly the reason they always were.
///
/// What the narrower rule missed is that a leading dot is not only a path question. `.skein` is the
/// **substrate directory** in the fleet root, so a box of that name would be created at the path
/// skein keeps its own installation at. And the two places that tell boxes from substrate —
/// [`crate::fleet::local_disk_usage`] and [`crate::fleet::live_box_names`] — both drop a dotted
/// entry, because the `du -sxm <root>/*/` oracle the first reproduces is a shell glob and a glob
/// does not match a leading dot. So a box called `.anything` would be invisible to both: absent
/// from the disk map, and absent from the live-box list that `fleet::substrate_strays` subtracts to
/// decide what is unattributed — which would then offer the owner an `rm -rf` for a running box's
/// own directories.
///
/// Both of those filters used to carry a comment saying no box name may begin with a dot, citing
/// this function. That was false when written and stayed false for two releases; measuring it is
/// what produced this rule. The comments now cite the rule below, and the rule is now real.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, env_pins, sb, secs_ago, tempdir};

    /// A command that could not run says **which** way it could not run.
    ///
    /// Both of these arrive as `None` through [`output_with_timeout`], and a caller holding that
    /// `None` writes the same sentence for either — which is how "sbx did not answer" got printed on
    /// a machine whose `sbx ls` worked in a terminal. The two need opposite responses: a PATH the
    /// server was started with, or a daemon that has gone slow.
    #[test]
    fn a_command_that_could_not_run_says_which_way_it_failed() {
        let mut missing = Command::new("skein-no-such-program-1a2b3c");
        let why = output_with_timeout_why(&mut missing, Duration::from_secs(5))
            .expect_err("a program that does not exist cannot have run");
        assert!(why.contains("not on this process's PATH"), "{why}");
        // The PATH itself, because the reader's own shell will contradict the message and the only
        // thing that settles it is which PATH skein had. They cannot look it up afterwards.
        assert!(
            why.contains(&std::env::var("PATH").unwrap_or_default()),
            "the message never says which PATH: {why}"
        );

        let mut slow = Command::new("sleep");
        slow.arg("30");
        let why = output_with_timeout_why(&mut slow, Duration::from_millis(200))
            .expect_err("a 30s sleep cannot finish inside 200ms");
        assert!(why.contains("did not finish within 200ms"), "{why}");
        assert!(
            !why.contains("PATH"),
            "a slow command was reported as a missing one: {why}"
        );
    }

    #[test]
    fn ages_read_the_way_the_fleet_says_them() {
        assert_eq!(ago(0), "0s ago");
        assert_eq!(ago(59), "59s ago");
        assert_eq!(ago(60), "1m ago");
        assert_eq!(ago(3599), "59m ago");
        assert_eq!(ago(3600), "1h ago");
        assert_eq!(ago(86_399), "23h ago");
        assert_eq!(ago(86_400), "1d ago");
        // A clock that moved backwards must not render "-3s ago" on the board.
        assert_eq!(ago(-5), "0s ago");
    }

    #[test]
    fn a_byte_budget_never_cuts_a_character_in_half() {
        // The shape that panicked in the field: the budget lands inside a 3-byte '…'. A raw slice
        // at these indices is `start byte index N is not a char boundary`, not a mis-render.
        let s = "abc…def"; // 3 + 3 + 3 bytes
        assert_eq!(s.len(), 9);
        for max in 0..=s.len() {
            // The real assertion is that it does not panic for any budget — every index through the
            // '…' is one `String::truncate` would have aborted on.
            assert!(clip_bytes(s, max).len() <= max, "over budget at {max}");
            assert!(s.starts_with(clip_bytes(s, max)));
        }
        // Rounding goes *inward*, so a budget that splits the '…' drops it whole rather than
        // keeping a fragment that busts the budget it was given.
        assert_eq!(clip_bytes(s, 4), "abc");
        // Exact fits and over-budget inputs are returned untouched.
        assert_eq!(clip_bytes(s, 6), "abc…");
        assert_eq!(clip_bytes(s, 99), s);
    }

    #[test]
    fn a_files_age_comes_from_the_file_and_is_absent_when_it_cannot() {
        let dir = std::env::temp_dir().join(format!("skein-ago-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x");
        fs::write(&f, "hi").unwrap();
        assert_eq!(file_ago(&f).as_deref(), Some("0s ago"));
        assert!(file_ago(&dir.join("nope")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A cap is a display concern, so it must not be able to fail — and as a byte slice it could.
    ///
    /// The journal that found this was an ordinary one: long enough to cut, with an `…` where the
    /// cut landed. `&s[s.len() - 4000..]` then aborted the thread doing the cutting rather than the
    /// request that asked, so the whole cockpit lost a worker to one box's punctuation.
    #[test]
    fn a_cap_falls_between_characters_however_wide_they_are() {
        assert_eq!(keep_tail("short", 10), "short");
        assert_eq!(keep_tail("abcdefghij", 4), "…ghij");
        // The head is what goes; `clip` is the same rule from the other end.
        assert_eq!(clip("abcdefghij", 4), "abcd…");
        // Multi-byte, and cut at every offset through it: each one is a byte index the naive slice
        // would have panicked on, and the answer is always a whole character.
        let wide = "…é日本語…ü";
        for max in 0..=wide.chars().count() + 2 {
            let out = keep_tail(wide, max);
            assert!(
                wide.ends_with(out.trim_start_matches('…')),
                "the tail is kept whole at {max}: {out}"
            );
        }
        // The shape that panicked, reproduced: an ellipsis straddling the cut.
        let journal = format!("{}… did: reset onto master", "x".repeat(5000));
        let out = keep_tail(&journal, 4000);
        assert_eq!(
            out.chars().count(),
            4001,
            "the marker plus the cap: {out:.40}"
        );
        assert!(out.ends_with("did: reset onto master"));
    }

    #[test]
    fn valid_name_guards_paths() {
        assert!(valid_name("thing-feature"));
        assert!(valid_name("box_123"));
        for bad in ["", "../etc", "a/b", "a\\b", "..", "x..y", "a\0b"] {
            assert!(!valid_name(bad), "should reject {bad:?}");
        }
        assert!(!valid_name(&"x".repeat(200)));
    }

    /// A box named `.skein` would be created at the path skein keeps its own substrate at, and
    /// then be invisible to everything that tells a box from the substrate.
    ///
    /// The concrete change that makes this fail is removing `!name.starts_with('.')` from
    /// [`valid_name`] — which is exactly the state this repo shipped in for two releases, while
    /// two comments in `fleet.rs` asserted the opposite (SKEIN-742).
    ///
    /// The bite is not the collision on its own. `fleet::live_box_names` drops dotted entries, and
    /// `fleet::substrate_strays` subtracts that list from what is under `.skein` to decide what is
    /// unattributed — so a live box whose name began with a dot would have its own directories
    /// reported to the owner as strays, under a copyable `rm -rf`. The advice would be wrong in
    /// the most expensive direction advice can be wrong.
    ///
    /// `.` and `..` are in here too, because this rule replaced the narrower all-dots one and the
    /// point of the replacement is that it loses nothing.
    #[test]
    fn a_name_that_would_shadow_the_substrate_is_not_a_box_name() {
        for shadow in [".skein", ".mybox", ".", "..", ".."] {
            assert!(
                !valid_name(shadow),
                "{shadow:?} was accepted as a box name. A box created under it sits where the \
                 substrate lives, and every filter that tells the two apart drops it — so it \
                 would not appear in the disk map, and `substrate_strays` would offer the owner \
                 an `rm -rf` for a running box's own build output"
            );
        }
        // A dot that is not leading is ordinary: `slug` produces them from branch names like
        // `v1.2`, so rejecting those would rename real boxes.
        for real in ["a.b", "repo-v1.2", "thing_1.0-x"] {
            assert!(valid_name(real), "{real:?} is a shape `slug` produces");
        }
    }

    /// A box name reaches a shell, so the characters a shell reads must not be in one.
    ///
    /// The first entry is not a hypothetical: fed to the fleet liveness sweep's generated shell,
    /// the old rule let it generate `answered="$answeredx"; id; echo " ";` — a command substitution
    /// that ran. That generator has since been deleted (SKEIN-615), so the proof cannot be re-run
    /// against it; what it established about this allow-list is why the list is here. Every entry
    /// below is a metacharacter of the shell the generated scripts are written in, and the concrete
    /// change that makes this fail is putting the old deny-list back.
    #[test]
    fn a_name_that_a_shell_would_read_as_more_than_a_word_is_refused() {
        for hostile in [
            r#"x"; id; echo ""#, // proven to execute through the sweep shell, since deleted
            "a$(id)b",
            "a`id`b",
            "a;id",
            "a b",
            "a'b",
            "a|b",
            "a&b",
            "a>b",
            "a*b",
            "a~b",
            "-rf", // argv-parses as a flag
            ".",   // `Path::join(".")` is the parent directory
        ] {
            assert!(
                !valid_name(hostile),
                "a shell reads {hostile:?} as more than one word, so it is not a box name"
            );
        }
        // And the class every real name is already in stays accepted, or this fix would have
        // renamed the fleet. These are the shapes `repos::box_name` actually produces.
        for real in [
            "gadget-demo-invoice-line-numbering",
            "example-work",
            "PROJ-S8",
            "bridge-one-two-master",
            "box_123",
            "a.b-c_d",
        ] {
            assert!(valid_name(real), "{real:?} is a name skein makes");
        }
    }

    /// The property the board's tick depends on: however many callers arrive together, the sandbox
    /// is asked once. Check-then-act gave every browser tab its own subprocess, because the answer
    /// was only remembered once the first one returned.
    #[test]
    fn concurrent_callers_ask_the_sandbox_once_between_them() {
        static GATE: Gate<u32> = Gate::new();
        static ASKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        GATE.invalidate();
        ASKS.store(0, std::sync::atomic::Ordering::Relaxed);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    GATE.get(Duration::from_secs(60), || {
                        ASKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // Long enough that the others are certainly waiting on the lane rather
                        // than having missed each other by luck.
                        std::thread::sleep(Duration::from_millis(50));
                        Some(1)
                    })
                });
            }
        });
        assert_eq!(
            ASKS.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "eight simultaneous callers must cost one subprocess, not eight"
        );
    }

    /// What a caller pays when the sandbox is wedged. Measured against a hung sbx daemon, a board
    /// refresh walked four gates in series and took 31 seconds to hand back the answers it already
    /// had — so the property is that ageing out costs the caller *nothing* once the gate holds one.
    #[test]
    fn an_aged_out_answer_is_served_at_once_and_refreshed_behind_the_caller() {
        static GATE: Gate<u32> = Gate::new();
        static ASKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        use std::sync::atomic::Ordering::Relaxed;
        const HUNG: Duration = Duration::from_millis(300);
        let fresh = Duration::from_millis(20);
        GATE.invalidate();
        ASKS.store(0, Relaxed);

        // Cold: nothing to serve, so this caller does have to wait.
        assert_eq!(GATE.get(fresh, || Some(1)), Some(1));
        assert_eq!(
            ASKS.load(Relaxed),
            0,
            "the cold ask is the one under test next"
        );

        // Now aged out, with the sandbox hung. The caller must not wait on it.
        std::thread::sleep(fresh * 2);
        let began = std::time::Instant::now();
        let answer = GATE.get(fresh, || {
            ASKS.fetch_add(1, Relaxed);
            std::thread::sleep(HUNG);
            Some(2)
        });
        assert_eq!(
            answer,
            Some(1),
            "the remembered answer, not a wait for a new one"
        );
        assert!(
            began.elapsed() < HUNG / 2,
            "a stale gate must not make its caller sit out the timeout: waited {:?}",
            began.elapsed()
        );

        // …and the refresh really did run, so the *next* caller finds a fresh answer.
        std::thread::sleep(HUNG * 2);
        assert_eq!(
            ASKS.load(Relaxed),
            1,
            "exactly one refresh, behind the caller"
        );
        assert_eq!(
            GATE.get(fresh, || Some(3)),
            Some(2),
            "refreshed to the new value"
        );
    }

    /// The exception, and why it is one: `invalidate` means skein has just *changed* the thing being
    /// asked about, so the remembered answer is wrong rather than merely old. Serving it while a
    /// refresh ran behind would show a box as stopped immediately after starting it.
    #[test]
    fn a_gate_skein_has_invalidated_makes_its_caller_wait_for_the_truth() {
        static GATE: Gate<u32> = Gate::new();
        let fresh = Duration::from_millis(20);
        GATE.invalidate();
        assert_eq!(GATE.get(fresh, || Some(1)), Some(1));

        GATE.invalidate();
        assert_eq!(
            GATE.get(fresh, || Some(2)),
            Some(2),
            "an invalidated gate must return what it just asked for, not what it remembered"
        );
    }

    /// The property that lets a struggling daemon recover: consecutive failures space the attempts
    /// out instead of re-arming at the same interval, and one success puts it straight back.
    #[test]
    fn repeated_failure_asks_less_often_and_success_restores_the_cadence() {
        static GATE: Gate<u32> = Gate::new();
        let gate = &GATE;
        gate.invalidate();
        let fresh = Duration::from_millis(100);
        assert_eq!(gate.interval(fresh), fresh, "healthy: ask at the full rate");

        for expected in [200u64, 400, 800] {
            gate.invalidate();
            gate.get(fresh, || None);
            assert_eq!(gate.interval(fresh), Duration::from_millis(expected));
        }
        // Capped, so a daemon that comes back is still noticed within half a minute.
        for _ in 0..20 {
            gate.invalidate();
            gate.get(fresh, || None);
        }
        assert_eq!(gate.interval(fresh), GATE_MAX_INTERVAL);

        gate.invalidate();
        gate.get(fresh, || Some(7));
        assert_eq!(gate.interval(fresh), fresh);
    }

    #[test]
    fn age_buckets() {
        assert!(sb("", &secs_ago(5)).age().ends_with("s ago"));
        assert!(sb("", &secs_ago(120)).age().ends_with("m ago"));
        assert!(sb("", &secs_ago(7200)).age().ends_with("h ago"));
        assert_eq!(sb("", "nope").age(), "?");
    }

    #[test]
    fn sh_quote_escapes() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("x'; rm -rf ~"), "'x'\\''; rm -rf ~'");
    }

    #[test]
    fn shorten_replaces_home() {
        let _g = env_lock();
        // `$HOME` of all names: left set to `/home/me`, every later test in this binary that read a
        // home read a directory that exists on no machine.
        let mut env = env_pins();
        env.set("HOME", "/home/me");
        assert_eq!(shorten("/home/me/work/x"), "~/work/x");
        assert_eq!(shorten("/other/x"), "/other/x");
    }

    #[test]
    fn safe_component_caps_length_but_keeps_extension() {
        let long = format!("{}.mp4", "n".repeat(200));
        let s = safe_component(&long);
        assert_eq!(s.chars().count(), 80);
        assert!(
            s.ends_with(".mp4"),
            "extension tells the agent the type: {s}"
        );
        // a multibyte name must truncate on a char boundary, never panic
        let s = safe_component(&format!("{}.pdf", "é".repeat(120)));
        assert_eq!(s.chars().count(), 80);
        assert!(s.ends_with(".pdf"));
    }

    #[test]
    fn safe_component_keeps_readable_names() {
        // letters of any script survive; only the shell/path-hostile characters collapse to '-'
        assert_eq!(safe_component("née deed.pdf"), "née-deed.pdf");
        assert_eq!(safe_component("契約書.docx"), "契約書.docx");
        assert_eq!(safe_component("a'b\"c;d|e$f*g.txt"), "a-b-c-d-e-f-g.txt");
        assert_eq!(safe_component(".hidden"), "hidden");
    }

    #[test]
    fn pct_decode_recovers_unicode_filenames() {
        assert_eq!(pct_decode("n%C3%A9e%20deed.pdf"), "née deed.pdf");
        assert_eq!(pct_decode("plain.txt"), "plain.txt");
        assert_eq!(pct_decode("100%"), "100%"); // dangling escape left verbatim
        assert_eq!(pct_decode("a%zz"), "a%zz");
    }

    /// **The bytes reach the disk before the rename, and a failed write leaves nothing behind.**
    ///
    /// The durability half cannot be asserted by running code: proving a crash between the write
    /// and the rename leaves a whole file needs a crash. So it is asserted where it can be — on the
    /// source of [`write_atomic`] itself, which is the same thing `grep -rn 'sync_all' src/` was
    /// asked and answered *nothing* to when SKEIN-347 was found. That absence is the mechanism: an
    /// ext4 crash after a rename with the data still in the page cache classically leaves the file
    /// present and zero-length, and zero-length is unparseable JSON for every caller here.
    ///
    /// The rest is behaviour, and it is the reason `fs::write` could not simply be kept: the flush
    /// adds two more ways to fail after the temp file exists, and a temp nothing will ever rename
    /// into place is litter that nothing will ever clean up either.
    #[test]
    fn an_atomic_write_is_flushed_before_the_rename_and_leaves_no_temp_behind() {
        let src = include_str!("util.rs");
        let at = src
            .find("pub(crate) fn write_atomic(")
            .expect("no `write_atomic` in this file");
        let body = &src[at..at + src[at..].find("\n}\n").expect("a fn with no end")];
        let rename = body
            .find("fs::rename")
            .expect("write_atomic no longer renames — re-read this test");
        // Before the rename, and asked that way rather than as "is `sync_all` in here anywhere":
        // the directory is flushed AFTER the rename, and a search of the whole body is satisfied
        // by that one — which guarantees nothing at all about the bytes.
        assert!(
            body[..rename].contains("sync_all()"),
            "write_atomic renames bytes it never flushed; a crash after the rename leaves a \
             zero-length file, which every caller reads as unparseable"
        );

        let dir = tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let path = dir.join("thing.json");
        write_atomic(&path, dir, b"{\"kept\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"kept\":true}");

        // The one failure a caller can provoke without a broken disk: a target that cannot be
        // renamed onto, because it is a non-empty directory.
        let occupied = dir.join("occupied");
        std::fs::create_dir_all(occupied.join("child")).unwrap();
        assert!(write_atomic(&occupied, dir, b"x").is_err());
        let leftovers: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".skein.tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed atomic write left its temp file behind: {leftovers:?}"
        );
    }

    /// **"Not there" and "there and unreadable" are different answers.**
    ///
    /// The one distinction `read_to_string(..).ok().and_then(|t| from_str(&t).ok())` destroys, and
    /// the reason it destroys anything: every caller of it is a read-modify-write, so the answer it
    /// gives to the second case is written back over the file it could not read.
    #[test]
    fn a_json_file_that_is_missing_is_not_one_that_is_unreadable() {
        let dir = tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let path = dir.join("list.json");

        assert_eq!(
            read_json_or_why::<Vec<String>>(&path).unwrap(),
            None,
            "a file nobody has written yet has to be readable as an empty opinion"
        );

        fs::write(&path, b"[\"alpha\"]").unwrap();
        assert_eq!(
            read_json_or_why::<Vec<String>>(&path).unwrap(),
            Some(vec!["alpha".to_string()])
        );

        // The crash artifact, exactly: present, zero-length, unparseable.
        fs::write(&path, b"").unwrap();
        let why = read_json_or_why::<Vec<String>>(&path)
            .expect_err("a zero-length file was read as a list");
        assert!(
            why.contains("list.json"),
            "the reason has to name the file somebody must go and look at: {why}"
        );
    }

    /// **A JSON file skein cannot read is never written over, and the one caller that wants it to
    /// be says so by name.**
    ///
    /// SKEIN-359, and the inversion at the heart of it: `update_json` used to answer
    /// `T::default()` for a file that was merely unparseable and then write that default back, so a
    /// grants file corrupted by a crash lost every grant in it to the next approval. The default is
    /// still available — `attempt`'s lease file genuinely wants it — but it is now the named,
    /// argued form rather than what a caller gets for not thinking about it.
    ///
    /// Asserted on the BYTES on disk rather than on the returned error, for `repos`' reason: the
    /// error is the nice half, and the half that matters is that the file is still there.
    #[test]
    fn a_json_file_skein_cannot_read_is_refused_by_update_json_and_taken_only_by_the_lossy_form() {
        let dir = tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let path = dir.join("list.json");
        let push = |what: &str| {
            let what = what.to_string();
            move |all: &mut Vec<String>| {
                all.push(what);
                Ok(())
            }
        };

        // A first write with no file to read: missing is empty, which is the case that must keep
        // working or nobody can ever write their first entry.
        update_json(&path, push("alpha")).unwrap();

        for corrupt in [&b""[..], &b"[\"alpha\""[..]] {
            fs::write(&path, corrupt).unwrap();
            let why = update_json(&path, push("beta"))
                .expect_err("update_json wrote a default over a file it could not read");
            assert!(
                why.contains("cannot read") && why.contains("list.json"),
                "the refusal has to name the file and say it could not be read: {why}"
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                corrupt,
                "the unreadable file was replaced by the update"
            );
        }

        // And the lossy form does what its name says, so the choice is visible where it is made.
        update_json_lossy(&path, push("gamma")).unwrap();
        assert_eq!(
            read_json_or_why::<Vec<String>>(&path).unwrap(),
            Some(vec!["gamma".to_string()]),
            "the lossy form has to still be able to take the default, or `attempt` blocks for ever"
        );
    }
}
