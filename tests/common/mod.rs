//! The helpers every integration binary in this directory needs, in one place.
//!
//! Cargo builds **one binary per `tests/*.rs`**, and `src/testutil.rs` is `#[cfg(test)] mod` inside
//! the library (`src/lib.rs:71-72`), so no integration test can reach it. The answer to that had
//! been a copy per file: seven byte-identical `have()`s, five `env_lock()`s carrying the same
//! eleven-line comment, twelve scratch-directory helpers. `tests/common/mod.rs` is the shape cargo
//! gives for this — a module, not a `tests/common.rs`, which would be compiled as a test binary of
//! its own with no tests in it.
//!
//! Everything here is `pub` and most of it is unused in any given binary, which is what the
//! `dead_code` allow is for: the module is compiled once per binary that declares `mod common;`,
//! and `-D warnings` would otherwise fail every binary that uses half of it.
//!
//! # What a machine needs to run this suite
//!
//! `REQUIREMENTS` below is the list, per binary, and `tests/platform_gates.rs` checks it against
//! the code rather than trusting it. A machine without one of these does not fail the suite — it
//! **skips**, through `skip()`, which is the other half of this file's job.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

// ---------------------------------------------------------------------------------------------
// Is this machine able to run the thing under test?
// ---------------------------------------------------------------------------------------------

/// Is `tool` on this machine's PATH?
pub fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Can bwrap actually make a namespace here — which `have("bwrap")` does not ask.
///
/// `have` answers "is it on PATH". On `ubuntu-24.04`, which is what CI runs, bubblewrap installs
/// cleanly and `kernel.apparmor_restrict_unprivileged_userns=1` then refuses the unprivileged user
/// namespace it needs, so the two answers differ exactly where it matters. Hosting a box needs the
/// namespace, not the binary — and a guard that asked the weaker question left two tests dead on CI
/// for 27 days.
///
/// Deliberately identical to `src/testutil.rs::bwrap_works`. The crate boundary is why there are
/// two, not a difference of opinion, and `.github/workflows/ci.yml` runs this exact command as a
/// step of its own before the suites so that a green CI run means no guard here was taken.
pub fn bwrap_works() -> bool {
    Command::new("bwrap")
        .args(["--dev-bind", "/", "/", "--", "/bin/true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Skipping, out loud
// ---------------------------------------------------------------------------------------------

/// The variable that turns every skip in the suite into a failure.
///
/// Set it on a machine that is supposed to have everything — CI, or a release check — and a green
/// run then means "everything ran", which a green run otherwise does not.
pub const NO_SKIP: &str = "SKEIN_TESTS_NO_SKIP";

/// Say that this test is not running, and why. Written to be used as the whole of the statement:
///
/// ```ignore
/// if !have("jq") {
///     return common::skip("jq is not installed");
/// }
/// ```
///
/// **Why this is not enough on its own, and what makes it enough.** `cargo test` captures a passing
/// test's output, and a skipped test passes — so a notice printed here is invisible in exactly the
/// run where it matters, which is how eighteen silent skips went unnoticed long enough to be worth
/// a finding. Grepping the log for these lines cannot be the check. `$SKEIN_TESTS_NO_SKIP` is: with
/// it set, this panics, the test fails, and cargo cannot hide a failing test's output. A green run
/// under that variable is a proof that nothing was skipped, and it needs nobody to read anything.
///
/// `#[track_caller]` so the message names the guard rather than this function.
#[track_caller]
pub fn skip(why: &str) {
    let at = std::panic::Location::caller();
    let where_ = format!("{}:{}", at.file(), at.line());
    if std::env::var_os(NO_SKIP).is_some() {
        panic!(
            "SKIPPED at {where_}: {why}\n{NO_SKIP} is set, which asks for a run where nothing is \
             skipped — install what this needs (tests/common/mod.rs lists it per binary) or unset \
             the variable"
        );
    }
    eprintln!("SKIPPED at {where_}: {why}");
}

/// The library's own `#[cfg(test)]` tests — the `cargo test --lib` binary — as named in
/// [`REQUIREMENTS`].
///
/// Not a `tests/*.rs`, and that is the whole reason it needs a name here. `$SKEIN_TESTS_NO_SKIP` and
/// the requirement list both grew up around this directory, so for as long as they existed the
/// crate's own tests were outside both: fifteen of them skipped through a bare `eprintln!` and an
/// early `return`, which the switch cannot refuse and cargo hides because a skipped test PASSES. A
/// run that had asked for no skips got fifteen and was told nothing — a status display reporting on
/// something other than what it names (SKEIN-790).
///
/// `src/testutil.rs::skip` is the library's half of the switch, and `tests/platform_gates.rs` is
/// what keeps a sixteenth from being added the old way.
pub const LIB: &str = "lib";

/// What each test binary needs on the machine, beyond a Rust toolchain.
///
/// Checked against the code by `tests/platform_gates.rs`: a binary that skips must be listed here,
/// a listed binary must still skip, and every tool named must still be guarded on inside that file.
/// So this cannot quietly become fiction, which is the state it would otherwise reach — the tools
/// this suite needs were written down in no file at all before it existed.
///
/// `bwrap` means `bwrap_works()`, not the binary: see its doc comment.
///
/// [`LIB`] is in here and is not a `tests/*.rs`. It is the largest test surface in the tree and it
/// was in no list at all until SKEIN-790 — see that constant for why that mattered.
pub const REQUIREMENTS: &[(&str, &[&str])] = &[
    (
        LIB,
        &["bwrap", "du", "git", "jq", "node", "python3", "tmux"],
    ),
    ("browser_suites", &["node", "chromium"]),
    ("fleet_launch", &["bwrap", "tmux", "git"]),
    ("fleet_move", &["tmux", "python3"]),
    ("git_write_request", &["jq", "git"]),
    ("isolation_bwrap", &["bwrap", "python3"]),
    ("mail_provenance", &["jq", "flock"]),
    // **`tmux` was always needed here and was written down nowhere** (SKEIN-765). Every spawn in
    // `tests/server.rs` runs the real `main`, whose `heal_fleet` reaches `fleet::start_server` — a
    // `tmux new-session` — before the port is bound, so a machine without tmux has been running
    // these twelve tests against a server that silently healed nothing. It is listed now because
    // one of them gates on it and says so.
    //
    // `bwrap` joined it when the upload test stopped standing the `nsenter` hop in and started
    // making a real namespace to cross into (SKEIN-832): the anchor it enters is a `bwrap` process,
    // so on a machine without bwrap that test has no box and skips.
    ("server", &["python3", "tmux", "bwrap"]),
    ("substrate_request", &["jq"]),
    ("turn_state_probe", &["jq"]),
    ("warden_roundtrip", &["cargo"]),
];

// ---------------------------------------------------------------------------------------------
// The env lock
// ---------------------------------------------------------------------------------------------

/// One lock per test binary, taken by every test that writes a process-global environment variable.
///
/// `std::env::set_var` writes a table shared by every thread in the process, and cargo runs a
/// binary's tests as parallel threads of ONE process — so a test that sets `SKEIN_HOME` is writing
/// into the middle of whatever else is running. The symptom is never at the site: SKEIN-307 was a
/// GitHub request-count assertion in `src/prq.rs` that failed once, passed on re-run, and had
/// nothing wrong with it. `tools/env-lock-check.py` is what keeps this taken as tests are added.
///
/// **One lock, not two.** Five files here held a second `Mutex<()>` of their own — `alone()`,
/// `serialize()` — and every test in them took both, back to back, in the same order, with no
/// helper taking either. Two mutexes acquired in one order by one set of callers exclude exactly
/// what one does; the pair could only have differed if some test took one without the other, and
/// none did. Collapsing them can only widen exclusion, never narrow it, and there is no
/// re-entrancy to deadlock because nothing below a `#[test]` takes this.
///
/// Poisoning is ignored, for the reason `src/testutil.rs` gives: the guarded data is `()`, and
/// cascading the first panic into every other test buries the real failure.
pub fn env_lock() -> MutexGuard<'static, ()> {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Scratch directories that go away
// ---------------------------------------------------------------------------------------------

/// Keep every scratch directory, whatever the outcome. For a passing test you want to look inside.
pub const KEEP: &str = "SKEIN_TESTS_KEEP_SCRATCH";

/// A scratch directory that removes itself — **except when the test failed.**
///
/// Half the files here removed their directory on the last line of the test, which a failing
/// assertion unwinds straight past, and the other half never removed one at all. On this box that
/// came to 1,082 directories and 1.6 GB in `/var/tmp`, every one of them a copy of `/usr/bin/git`
/// left by `tests/git_write_request.rs`.
///
/// **A `Drop` that always removed would be worse than the leak**: the directory is the only
/// evidence a failure leaves — the box's tree, its logs, what the fake `sbx` recorded — and a test
/// that tidies it away is a test nobody can debug. So `Drop` keeps the directory when the thread is
/// panicking and says where it is; `$SKEIN_TESTS_KEEP_SCRATCH` keeps it always.
///
/// **`quiesce` runs either way**, and that is not a detail. `tests/fleet_move.rs` starts a
/// supervisor whose loop condition is a file *inside* this directory, so keeping the directory
/// after a failure would keep restarting the server it supervises — which is how four leaked
/// processes were found on 2026-08-31. Whatever has to stop, stops here; only the removal is
/// conditional.
/// Whatever has to stop before the directory could go — see `Scratch`.
type Quiesce = Box<dyn Fn(&Path) + Send + Sync>;

pub struct Scratch {
    path: PathBuf,
    quiesce: Option<Quiesce>,
}

impl Scratch {
    /// `<root>/<prefix>-<pid>`, emptied first so a re-run in the same process starts clean.
    ///
    /// The pid suffix is the convention the sweep below reads, so it is applied here rather than
    /// spelled at each call site. Prefixes are each file's own and are deliberately unchanged:
    /// the leaked-process gate greps `ps` for `skein-fleet-it-` and `skein-move-it-`, and renaming
    /// them would turn that count into a zero that means nothing.
    pub fn at(root: impl AsRef<Path>, prefix: &str) -> Scratch {
        let root = root.as_ref().to_path_buf();
        sweep_abandoned(&root);
        let path = root.join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch {
            path,
            quiesce: None,
        }
    }

    /// Under `/var/tmp`, which is where anything that launches a box has to live: a box binds its
    /// own directories over `/tmp` and `$HOME`, so a box root beneath either is unreadable from
    /// outside and `box-session.sh` refuses it outright.
    pub fn boxes(prefix: &str) -> Scratch {
        Scratch::at("/var/tmp", prefix)
    }

    /// Under the ordinary temp directory, for the tests that never start a namespace.
    pub fn temp(prefix: &str) -> Scratch {
        Scratch::at(std::env::temp_dir(), prefix)
    }

    /// Something to run before the directory would be removed, on every path including a panic.
    pub fn quiesce_with(mut self, f: impl Fn(&Path) + Send + Sync + 'static) -> Scratch {
        self.quiesce = Some(Box::new(f));
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// So a scratch directory can be handed straight to `Command::env` and `Command::current_dir`,
/// which is what most of the call sites do with it.
impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(q) = self.quiesce.take() {
            q(&self.path);
        }
        if std::thread::panicking() || std::env::var_os(KEEP).is_some() {
            eprintln!(
                "kept {} — a failing test's scratch directory is the evidence; \
                 remove it by hand, or unset ${KEEP}",
                self.path.display()
            );
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Remove the scratch directories of runs that are over, once per binary.
///
/// A directory here is named `<something>-<pid>`, so "is that process still alive" is the whole
/// question, and it is asked of `/proc` rather than of a clock: two `cargo test` runs at once on
/// this box is the normal state, and an age rule would either sweep a live run's directory or leave
/// a dead one for hours. `skein-test-*` is left alone — those belong to `src/testutil.rs`, which
/// sweeps its own.
fn sweep_abandoned(root: &Path) {
    // Once per ROOT, not once per binary: a binary that uses both `/var/tmp` and the ordinary temp
    // directory would otherwise sweep whichever it reached first and leave the other for ever.
    static SWEPT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    {
        let mut swept = SWEPT.lock().unwrap_or_else(|e| e.into_inner());
        if swept.iter().any(|p| p == root) {
            return;
        }
        swept.push(root.to_path_buf());
    }
    {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("skein-") || name.starts_with("skein-test-") {
                continue;
            }
            // `<something>-<pid>`, or `<something>-<pid>-ThreadId(n)`, which is what the unit-test
            // fixtures in `src/` and `warden/src/` spell (`warden/src/outcome.rs:398`,
            // `src/testutil.rs`). 2,460 of the second shape were on this box, so reading past the
            // thread id is the difference between sweeping them and leaving them for ever.
            let mut parts = name.rsplit('-');
            let last = parts.next().unwrap_or_default();
            let tail = if last.starts_with("ThreadId(") {
                parts.next().unwrap_or_default()
            } else {
                last
            };
            let Ok(pid) = tail.parse::<u32>() else {
                continue;
            };
            if pid == std::process::id() || Path::new(&format!("/proc/{pid}")).exists() {
                continue;
            }
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Environment variables that go back
// ---------------------------------------------------------------------------------------------

/// Environment variables a test pins, **put back when the test ends however it ends.**
///
/// A copy of `src/testutil.rs`'s `EnvPins`, deliberately, for the reason this file's header gives
/// about `env_lock()` and `Scratch`: `src/testutil.rs` is a `#[cfg(test)] mod` inside the library
/// (`src/lib.rs:71-72`), so nothing in this directory can reach it however it is spelled. The
/// alternatives were both worse. Making the library's copy reachable means `pub mod testutil`
/// behind a cargo feature — a test fixture in the released crate's public surface, a `[current]`
/// row in `docs/modules.toml` for `module-check.py`, and `--features` on every `cargo test`
/// invocation in the tree and in CI. A third workspace crate for forty lines is heavier still; the
/// workspace's second crate is `warden` because it is a separate trust boundary, not because
/// splitting is cheap.
///
/// **[`env_lock`] and this are two different guarantees**, and having one has repeatedly been read
/// as having the other. The lock stops a *concurrent* test seeing a half-written environment; it
/// says nothing about what the environment looks like once the lock is released. A test that pins
/// `$SKEIN_HOME`, holds the lock perfectly and never puts it back has answered every *later* test in
/// the binary that pinned none of its own — SKEIN-696, where two defects cancelled out into a green
/// suite.
///
/// **The trailing `remove_var` is not the fix, and that is the whole reason this type exists.** A
/// failing assertion unwinds straight past the last line of a test, so a test repaired that way
/// restores the environment exactly when it passes and leaks exactly when it fails: the ordinary
/// case while developing, and the case where the next test's result is least likely to be believed.
/// `Drop` runs on both paths.
///
/// **Not the `Scratch` shape.** [`Scratch`] keeps its directory when the thread is panicking,
/// because a failed test's directory is the only evidence the failure leaves. Copying that
/// `if std::thread::panicking()` here would disable this type in exactly the case it exists for —
/// there is no evidence in a leaked variable, only the next test being answered out of it.
///
/// **Bind it after the [`Scratch`] it points at.** Locals drop in reverse order of declaration, so
/// `let dir = Scratch::temp(..); let mut env = env_pins();` unpins the variable and then removes the
/// directory. The other order leaves `$SKEIN_HOME` naming a directory that is already gone, which is
/// worse for whatever reads it next than naming nothing at all. It is the same ordering rule
/// `tests/review_queue.rs` spells out as field order on its `Env` struct, and it applies to the lock
/// too:
///
/// ```ignore
/// let _lock = env_lock();
/// let dir = Scratch::temp("skein-something");
/// let mut env = env_pins();
/// env.set("SKEIN_HOME", &dir)
///     .set("SKEIN_FLEET_ROOT", dir.join("boxes"));
/// ```
///
/// **The example pins both, and that is the point of it rather than a flourish.** `$SKEIN_HOME`
/// unpinned means the real `~/.skein`; `$SKEIN_FLEET_ROOT` unpinned means `/boxes`, which on any
/// machine running skein is the owner's LIVE fleet. `skein_home` and `fleet_root` each refuse a
/// test that has not pinned theirs — but only when the test's path actually resolves one, so
/// pinning a single variable passes for as long as nothing reaches the other, and then fails in a
/// test nobody edited, the day something inside the library moves that read onto its path. Six
/// files in this directory were in exactly that state. `tools/fleet-pin-check.py` is the gate that
/// keeps the pair together; a deliberate `unset` of one counts as saying something about it, which
/// is what the tests proving those refusals need.
///
/// It deliberately does **not** take [`env_lock`] itself. `Mutex` is not re-entrant, and most
/// env-touching tests here already hold the lock before they reach a fixture that would pin, so
/// folding it in would deadlock on the first such call. `tools/env-lock-check.py` counts an
/// `env_pins()` call as touching the environment, so a converted test still has to hold the lock and
/// still fails that gate if it stops.
pub struct EnvPins(Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>);

/// Start pinning environment variables. See [`EnvPins`].
pub fn env_pins() -> EnvPins {
    EnvPins(Vec::new())
}

impl EnvPins {
    /// Pin `name` to `value`, remembering what it held.
    pub fn set(&mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) -> &mut EnvPins {
        self.remember(name);
        put(std::ffi::OsStr::new(name), Some(value.as_ref()));
        self
    }

    /// Pin `name` to *absent*, remembering what it held.
    ///
    /// Needed as often as [`set`](EnvPins::set): a test that proves what happens with no `$GH_TOKEN`
    /// has to unset one the environment may already carry, and unsetting it without recording the
    /// old value is the same leak in the other direction.
    pub fn unset(&mut self, name: &str) -> &mut EnvPins {
        self.remember(name);
        put(std::ffi::OsStr::new(name), None);
        self
    }

    fn remember(&mut self, name: &str) {
        self.0.push((name.into(), std::env::var_os(name)));
    }
}

impl Drop for EnvPins {
    fn drop(&mut self) {
        // Reverse, so the FIRST pin of a name is the last one undone and therefore the one that
        // wins. Forward order would leave a twice-pinned variable holding its intermediate value.
        for (name, prior) in self.0.drain(..).rev() {
            put(&name, prior.as_deref());
        }
    }
}

/// Set `name` to `value`, or remove it when there is no value.
///
/// **The one place in this file that writes the process environment**, and pinning and restoring
/// both go through it. Written once rather than twice because the two are the same operation read
/// in opposite directions, and the half that is easy to get wrong is `None`: restoring an absent
/// variable by setting it to `""` reads as *present* to `env::var_os`, and every skein reader tests
/// presence. A second copy of this `match` in `Drop` is a second place for that to be got wrong.
///
/// It is also what leaves this file with a single env-touching scope for `tools/env-lock-check.py`,
/// which waves one through on the grounds that a `tests/*.rs` file is its own process. That
/// reasoning does not literally hold for `tests/common/mod.rs` — it is compiled into every binary
/// that declares `mod common;` — but the verdict is right for a different reason the gate cannot
/// reach: this is a helper whose callers are in other files, and rule one resolves callers within
/// one file only.
fn put(name: &std::ffi::OsStr, value: Option<&std::ffi::OsStr>) {
    match value {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}

// ---------------------------------------------------------------------------------------------
// One fake GitHub, for the two integration binaries that need one
// ---------------------------------------------------------------------------------------------

/// A parsed HTTP request, as handed to a [`fake_github`] handler.
pub struct GhRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// Start a GitHub-shaped HTTP server on `127.0.0.1` and hand every request on it to `handler`.
///
/// This is **half** of "one fake GitHub" — the half this crate boundary allows. The audit that
/// asked for a single `testutil::fake_github` counted 25 hand-rolled TCP servers
/// (`grep -rh 'TcpListener::bind' src/prq.rs src/prwork/ src/github.rs | wc -l` → 25, as 10 + 9 + 6), but every
/// one of those 25 is a `#[cfg(test)] mod tests` inside `src/`, reachable only from unit tests in
/// that same crate — none of them are in `tests/*.rs`. `tests/review_queue.rs` and `tests/server.rs`
/// had their own pair (`stub_github`, `stub_github_for`), which is the actual count for this
/// directory: **2**, not 25. This function is what those two now share.
///
/// A `testutil::fake_github` for the 25 in `src/` would be the other half, and belongs in
/// `src/testutil.rs` — but changing it means touching `src/prq.rs`, `src/prwork/` and
/// `src/github.rs` to call it, and none of those are this slice's files. Reported, not done here.
///
/// Deliberately not shared with `src/testutil.rs` even in spirit beyond the transport shape: the
/// crate boundary is why there would be two `fake_github`s, not a difference of opinion — see
/// `bwrap_works` above for the same split on a smaller helper.
///
/// **What moved here and what did not.** Only the transport — one thread, one connection at a
/// time, read the request line and headers, read exactly `Content-Length` bytes of body, write the
/// status and payload back. What each server *answers* is unchanged: `stub_github`'s GraphQL
/// alias-splitting, its `fail-<term>`/`dead-request`/`too-heavy` fixtures, and its request-counting
/// `hits.log`, and `stub_github_for`'s PR-count fixture and `reading`-gated `/files`/`/pulls/`
/// branches, all moved into their call sites' handler closures unchanged. Neither call site's
/// assertions changed shape; only the loop and the wire format did.
///
/// Returns the base URL (`http://127.0.0.1:<port>`) to point `$SKEIN_GITHUB_API` at.
pub fn fake_github(handler: impl Fn(&GhRequest) -> (u16, String) + Send + 'static) -> String {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            gh_respond_once(stream, &handler);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Read one HTTP/1.1 request off `stream`, hand it to `handler`, write the answer back, then
/// return — the connection is closed on drop, which is why `fake_github` serves one at a time.
fn gh_respond_once(
    mut stream: std::net::TcpStream,
    handler: &(impl Fn(&GhRequest) -> (u16, String) + ?Sized),
) {
    use std::io::{BufRead, BufReader, Read, Write};
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok();
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let (status, payload) = handler(&GhRequest { method, path, body });
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
}
