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

/// Is Playwright's chromium actually installed — which `have("chromium")` does not ask, and cannot.
///
/// The second capability in this file, and it is here for the same reason as the first: what the
/// browser tier needs is not a binary on PATH. Playwright downloads its browser into a cache of its
/// own and never puts it on PATH, so `command -v chromium` answers `false` on a machine that has it
/// — **including the CI runner, where `.github/workflows/ci.yml` installs it on purpose.** A gate
/// that probes the declared tool name that way therefore scopes `browser_suites` report-only
/// exactly where the browser is present, and a skip in it is reported rather than failing the build
/// (SKEIN-899). That is under-blocking, not a false red, which is why it survived: it is visible in
/// every run's summary and still does nothing.
///
/// **Asked by resolving it the way the suites do.** `node_modules/playwright` can be present while
/// the browser it downloads separately is not, which fails at `chromium.launch()` minutes into a
/// run with a message about a missing executable rather than about setup.
///
/// **From `tests/ui`, not from the repo root.** That is where `node_modules` is — `tests/ui/package.json`
/// is its own — and asking from the root resolves nothing and reports "not installed" on a machine
/// that has it, which would skip the browser tier silently for ever: the exact shape of the bug
/// `tests/browser_suites.rs` exists to end.
///
/// It lives beside [`bwrap_works`] rather than in `tests/browser_suites.rs`, where it was written,
/// so that the capability and the requirement that means it are one file apart from nothing —
/// [`CHROMIUM`] names this function, so moving it away or deleting it does not compile.
pub fn chromium_ready() -> bool {
    Command::new("node")
        .args([
            "-e",
            "const fs = require('node:fs'); \
             import('playwright') \
               .then(p => process.exit(fs.existsSync(p.chromium.executablePath()) ? 0 : 1)) \
               .catch(() => process.exit(1))",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui"))
        .output()
        .is_ok_and(|out| out.status.success())
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

/// Assert that a refusal is the one this test pins — or SKIP, naming the OTHER real refusal that
/// answered instead.
///
/// **The defect this exists for is a class, not one test** (SKEIN-433, and the same shape as
/// SKEIN-396 and SKEIN-413). An assertion written as
///
/// ```ignore
/// assert!(err.contains("no registered repo"), "the refusal must say ...: {err}");
/// ```
///
/// says exactly one thing about every answer it did not expect: *this refusal is malformed*. When
/// the action it drives has more than one real refusal and a different one fires — correctly,
/// about something the test was not asking — the report sends the reader to the refusal's wording.
/// Observed in this file: the launch suite failed with "the refusal must say the sandbox was left
/// alone: copying the boxes out needs about 706 MiB and the host has 352 MiB free — resize aborted
/// with the sandbox untouched." Skein was right and the test was right; only the report was wrong,
/// and it was wrong in the expensive direction, because a reader goes looking at refusal wording
/// when the truth is the disk.
///
/// So a test that pins one refusal says which others it RECOGNISES, and an unrecognised one is
/// still a failure — that is the case the assertion was written for and it keeps it.
///
/// * `wanted` — the refusal this test is about, as substrings that must ALL be present.
/// * `others` — `(marker, what it means)` for each other legitimate refusal of the same action.
///   The marker is matched against the refusal; the meaning is what the skip says out loud.
/// * `covering` — what is not being covered when this skips, in the reader's terms.
///
/// Returns `true` when the pinned refusal fired and the caller should go on, `false` after
/// skipping. Skipping goes through [`skip`], so `$SKEIN_TESTS_NO_SKIP` refuses it like any other
/// skip and a machine that is supposed to have room says so rather than passing quietly.
///
/// **An empty `wanted` is a panic rather than a match.** `iter().all()` over nothing is `true`, so
/// a caller that derived its markers and derived none would pin every answer including the wrong
/// ones, and this helper would report a covered ordering that was never reached — the SKEIN-647
/// shape, one layer in. There is nothing to check against, so it says so instead.
#[track_caller]
pub fn pinned_refusal(err: &str, wanted: &[&str], others: &[(&str, &str)], covering: &str) -> bool {
    assert!(
        !wanted.is_empty(),
        "{covering}: pinned_refusal was given NO marker for the refusal it pins, and a pin that \
         matches everything would report this as covered whatever skein said. What skein said: \
         {err}"
    );
    if wanted.iter().all(|w| err.contains(w)) {
        return true;
    }
    if let Some((marker, meaning)) = others.iter().find(|(m, _)| err.contains(m)) {
        // One line, because `skip` writes one and `tools/noskip-check.py` reads them back per line.
        // The refusal's first line is the part that carries the numbers.
        let said = err.lines().next().unwrap_or(err);
        skip(&format!(
            "{covering}: skein refused for a different real reason — {meaning}. It said: {said} \
             (matched {marker:?}; this test pins {wanted:?})"
        ));
        return false;
    }
    panic!(
        "{covering}: the refusal is neither the one this test pins nor any this test recognises, \
         so it is the malformation this assertion exists for.\n  pinned: {wanted:?}\n  \
         recognised as other legitimate refusals: {:?}\n  what skein said: {err}",
        others.iter().map(|(m, _)| *m).collect::<Vec<_>>()
    );
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

/// One thing a test binary needs from the machine, and **how to ask whether it is here**.
///
/// The name alone is not the requirement, and this type is what SKEIN-915 is: a requirement used to
/// be a bare `&str`, and whether asking `command -v <name>` was the right question was decided by a
/// NAMING RULE — a nullary `pub fn <name>_<verb>() -> bool` beside it in this file meant "ask this
/// instead", and both gates rediscovered that rule by matching function names. The rule can only
/// see what is there. **Deleting [`chromium_ready`] outright left `chromium` a name with no probe,
/// which is indistinguishable from `jq`** — so both gates fell back to `command -v chromium`, which
/// exits 127 on a machine that has Playwright's browser, and `browser_suites` goes silently back to
/// report-only. That is the SKEIN-899 defect, and nothing could go red for it, because **the
/// absence of a probe carries no information**.
///
/// So the probe is carried here, as a function POINTER, in the declaration itself. Deleting the
/// probe is then a compile error at the tool that named it rather than a change of meaning; `None`
/// is a positive statement that PATH is the right question, written by someone, reviewable as a
/// diff. Both gates read this structure — see `tests/platform_gates.rs::capability_probes` and
/// `tools/noskip-check.py::capabilities` — so a probe's NAME means nothing to either of them now.
pub struct Tool {
    /// What to say to a reader, and what to look for on PATH when there is no `probe`.
    pub name: &'static str,
    /// The question the suite's own guards ask, where PATH is the wrong one. `None` means PATH is
    /// the right one, asked with [`have`].
    pub probe: Option<fn() -> bool>,
}

/// **A capability, not a PATH lookup**: `bwrap` installs cleanly on `ubuntu-24.04` and is then
/// refused the user namespace it needs, which left two tests dead on CI for 27 days (SKEIN-549).
pub const BWRAP: Tool = Tool {
    name: "bwrap",
    probe: Some(bwrap_works),
};

/// **A capability, not a PATH lookup**: Playwright keeps its browser in a cache of its own and
/// never puts it on PATH, so `command -v chromium` is `false` on the CI runner that installs it
/// deliberately — and `browser_suites` was left report-only in exactly the place it should have
/// been blocking (SKEIN-899).
pub const CHROMIUM: Tool = Tool {
    name: "chromium",
    probe: Some(chromium_ready),
};

// The rest are on PATH or they are not, and `probe: None` says so in the declaration rather than by
// being silent: an entry that wants asking some other way has somewhere to say it.
pub const CARGO: Tool = Tool {
    name: "cargo",
    probe: None,
};
pub const CURL: Tool = Tool {
    name: "curl",
    probe: None,
};
pub const DU: Tool = Tool {
    name: "du",
    probe: None,
};
pub const FLOCK: Tool = Tool {
    name: "flock",
    probe: None,
};
pub const GIT: Tool = Tool {
    name: "git",
    probe: None,
};
pub const JQ: Tool = Tool {
    name: "jq",
    probe: None,
};
pub const NODE: Tool = Tool {
    name: "node",
    probe: None,
};
pub const PYTHON3: Tool = Tool {
    name: "python3",
    probe: None,
};
pub const TMUX: Tool = Tool {
    name: "tmux",
    probe: None,
};

/// What each test binary needs on the machine, beyond a Rust toolchain.
///
/// Checked against the code by `tests/platform_gates.rs`: a binary that skips must be listed here,
/// a listed binary must still skip, and every tool named must still be guarded on inside that file.
/// So this cannot quietly become fiction, which is the state it would otherwise reach — the tools
/// this suite needs were written down in no file at all before it existed.
///
/// Each entry is a [`Tool`], not a name, so **a requirement that is a CAPABILITY carries the probe
/// that answers it** — [`BWRAP`] and [`CHROMIUM`] do, and each says in its own doc comment what the
/// weaker question cost. Anything probing this list asks what the entry says to ask, and a probe
/// cannot be deleted out from under it without failing to compile.
///
/// [`LIB`] is in here and is not a `tests/*.rs`. It is the largest test surface in the tree and it
/// was in no list at all until SKEIN-790 — see that constant for why that mattered.
pub const REQUIREMENTS: &[(&str, &[Tool])] = &[
    (LIB, &[BWRAP, DU, GIT, JQ, NODE, PYTHON3, TMUX]),
    ("browser_suites", &[NODE, CHROMIUM]),
    // `python3` because the launcher's credential leg IS python — `login_life`, `merge_login`, and
    // the onboarding flag that goes with a seeded login (SKEIN-957). It was always needed and was
    // written down nowhere, which is the same gap SKEIN-765 closed for `tmux` in `server`.
    ("fleet_launch", &[BWRAP, TMUX, GIT, PYTHON3]),
    ("fleet_move", &[TMUX, PYTHON3, RUSTUP]),
    // `curl` because the git shim's reachability probe IS a curl call: the two tests that drive the
    // blocked-egress hint guard on it, and without it here a machine with no curl skips them while
    // this list still says the binary needs only jq and git (SKEIN-548).
    ("git_write_request", &[JQ, GIT, CURL]),
    ("isolation_bwrap", &[BWRAP, JQ, PYTHON3]),
    ("mail_provenance", &[JQ, FLOCK]),
    // **`tmux` was always needed here and was written down nowhere** (SKEIN-765). Every spawn in
    // `tests/server.rs` runs the real `main`, whose `heal_fleet` reaches `fleet::start_server` — a
    // `tmux new-session` — before the port is bound, so a machine without tmux has been running
    // these twelve tests against a server that silently healed nothing. It is listed now because
    // one of them gates on it and says so.
    //
    // `bwrap` joined it when the upload test stopped standing the `nsenter` hop in and started
    // making a real namespace to cross into (SKEIN-832): the anchor it enters is a `bwrap` process,
    // so on a machine without bwrap that test has no box and skips.
    ("server", &[PYTHON3, TMUX, BWRAP]),
    ("substrate_request", &[JQ]),
    ("turn_state_probe", &[JQ]),
    ("warden_roundtrip", &[CARGO]),
    ("warden_failures", &[CARGO]),
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
    /// spelled at each call site. Prefixes are each caller's own, and this function is deliberately
    /// excluded from `tests/ui/harness/leaks.mjs`'s derivation: it reads the fixture prefixes it
    /// scans for out of the `Scratch::boxes("…")` / `Scratch::temp("…")` call sites themselves, and
    /// `prefix` here is a variable rather than a literal, so there is nothing for it to derive at
    /// this line. Renaming a call site's literal moves what the check matches; it refuses to run
    /// rather than silently matching zero when it derives nothing at all (SKEIN-647).
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

/// Remove the scratch directories of runs that are over, once per binary — **except the ones
/// something is still running out of.**
///
/// A directory here is named `<something>-<pid>`, so "is that process still alive" is the whole
/// question of whose directory it is, and it is asked of `/proc` rather than of a clock: two
/// `cargo test` runs at once on this box is the normal state, and an age rule would either sweep a
/// live run's directory or leave a dead one for hours. `skein-test-*` is left alone — those belong
/// to `src/testutil.rs`, which sweeps its own.
///
/// **The owner being gone does not make the directory empty of processes, and that is the whole of
/// this** (SKEIN-900). Removing it while a box's tmux server or a supervisor loop is still running
/// out of it manufactures, for every `tests/*.rs` in the repository, the one state nothing here can
/// attribute afterwards: the process is alive, the path it names does not exist, and nothing says
/// which run made it. `tests/ui/harness/leaks.mjs` then reports a process whose fixture is gone and
/// no reader can tell which suite to go and look at (SKEIN-884). It is a SECOND producer of that
/// state, independent of whichever binary leaked in the first place — `tests/fleet_launch/` fixed
/// its own producer and `Scratch` is shared by every binary here.
///
/// **So the directory is kept, and the reason is said out loud**, rather than the processes being
/// killed. A kept directory is then the evidence that identifies the orphan — the name carries the
/// pid of the run that made it — which is exactly what the deletion was destroying.
///
/// **The line between "a leak" and "a run in flight", which is the part that is easy to get wrong.**
/// [`processes_under`]'s caller in `tests/fleet_launch/fixture.rs` exempts a live DESCENDANT
/// of the test process, because an environment is inherited and killing one of those ends a sibling
/// test's own child. This sweep exempts **nothing**, and the difference is not an inconsistency: the
/// question there is *what may be killed* and the question here is *what may be deleted*. Deleting
/// the directory out from under a process is the same harm whoever owns that process, including
/// this one — so ownership is never asked, and the sweep is safe to run across runs precisely
/// because it never kills anything. The cost of being wrong is one stale directory surviving one
/// more sweep while saying why, which is the cheap direction.
///
/// Returns what it kept and why, `(directory, the processes still running out of it)`, so that the
/// stderr notice below and the decision cannot drift apart — and so a test can read the decision
/// without capturing stderr. An already-swept root returns empty, which is the memo and not a
/// finding.
pub fn sweep_abandoned(root: &Path) -> Vec<(PathBuf, Vec<(u32, String)>)> {
    // Once per ROOT, not once per binary: a binary that uses both `/var/tmp` and the ordinary temp
    // directory would otherwise sweep whichever it reached first and leave the other for ever.
    static SWEPT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    {
        let mut swept = SWEPT.lock().unwrap_or_else(|e| e.into_inner());
        if swept.iter().any(|p| p == root) {
            return Vec::new();
        }
        swept.push(root.to_path_buf());
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("skein-") || name.starts_with("skein-test-") {
            continue;
        }
        // `<something>-<pid>`, or `<something>-<pid>-ThreadId(n)`, which is what the unit-test
        // fixtures in `src/` and `warden/src/` spell (`warden/src/outcome.rs:398`,
        // `src/testutil.rs`). 2,460 of the second shape were on this box, so reading past the
        // thread id is the difference between sweeping them and leaving them for ever. The warden's
        // `Scratch` spells the thread `t<n>` since SKEIN-557 — parentheses in a path its tests write
        // into shell scripts — and removes its own directory unless the test panicked; the ones a
        // panic keeps are still this sweep's to collect once their run is gone.
        let mut parts = name.rsplit('-');
        let last = parts.next().unwrap_or_default();
        let a_thread = last.starts_with("ThreadId(")
            || last
                .strip_prefix('t')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let tail = if a_thread {
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
        candidates.push(entry.path());
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    // `/proc` is walked ONCE for the whole sweep rather than once per candidate. This runs before
    // the first scratch directory of every test binary in the repository, and this box has carried
    // thousands of abandoned directories at a time — a walk each would make the hygiene fix the
    // slowest thing in the suite, and a check people turn off is a check that does not exist.
    let table = process_table();
    let mut kept = Vec::new();
    for dir in candidates {
        let running = named_in(&table, &dir);
        if running.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        eprintln!(
            "kept {} — the run that owned it is gone, but {} process(es) are still running out of \
             it, and removing it would leave them with nothing naming what they belong to. \
             End them by pid (never `pkill -f`): {running:#?}",
            dir.display(),
            running.len()
        );
        kept.push((dir, running));
    }
    kept
}

// ---------------------------------------------------------------------------------------------
// What is still running out of a directory
// ---------------------------------------------------------------------------------------------

/// Every process on this machine still running out of `root`, found by **scanning `/proc`** rather
/// than by asking after pids that something wrote down.
///
/// **The two questions are different and only one of them is the one that matters** (SKEIN-834).
/// `kill -0 <recorded pid>` answers "is the thing I wrote down still running"; what leaks out of a
/// fixture is the kind of descendant nothing recorded — `fleet::start_server` starts a tmux server,
/// tmux forks a supervisor loop, the loop forks the doorway, and no caller ever held any of those
/// three pids.
///
/// **Both surfaces are read, because one of them is empty by the time it matters** (SKEIN-687). A
/// box's pane runs `exec sleep 400`, and `exec` replaces the image: its `cmdline` is the two bare
/// words `sleep 400` while its environment carries the fixture path six times over. A process whose
/// environment this user may not read is matched on its command line alone, the same concession
/// `tests/ui/harness/leaks.mjs` makes and announces.
///
/// This process is never in the answer. It is not an orphan from its own point of view, and every
/// test that pins `$SKEIN_FLEET_ROOT` would otherwise find itself. Ancestry beyond that is the
/// CALLER's policy and deliberately not decided here, because the two callers need opposite answers
/// — see [`sweep_abandoned`] for which and why.
pub fn processes_under(root: &Path) -> Vec<(u32, String)> {
    named_in(&process_table(), root)
}

/// `(pid, argv, the text that could name a fixture)` for every process this user can read.
fn process_table() -> Vec<(u32, String, String)> {
    let me = std::process::id();
    let mut table = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return table;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let read = |what: &str| {
            std::fs::read(format!("/proc/{pid}/{what}"))
                .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
                .unwrap_or_default()
        };
        let argv = read("cmdline").trim().to_string();
        let haystack = format!("{argv} {}", read("environ"));
        table.push((pid, argv, haystack));
    }
    table
}

/// The rows of `table` that name `dir` **as a directory**, not as the first characters of a longer
/// one.
///
/// A plain substring test is wrong here and the collision is not hypothetical: these directories are
/// named `<prefix>-<pid>`, pids on this box are five and six digits, and `skein-fleet-it-12345` is a
/// substring of `skein-fleet-it-123456`. A sweep that read it that way would keep a dead run's
/// directory for ever on the strength of a live run whose pid happens to start with the dead one's
/// — an absence of deletion that nothing would ever explain. So the character after the match has
/// to be one that cannot continue a path component.
fn named_in(table: &[(u32, String, String)], dir: &Path) -> Vec<(u32, String)> {
    let needle = dir.to_string_lossy().into_owned();
    table
        .iter()
        .filter(|(_, _, haystack)| {
            haystack.match_indices(&needle).any(|(at, _)| {
                match haystack[at + needle.len()..].chars().next() {
                    None => true,
                    Some(c) => !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'),
                }
            })
        })
        .map(|(pid, argv, _)| (*pid, argv.clone()))
        .collect()
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

/// Down here rather than beside the other [`Tool`]s so that nothing citing this file by line moves.
/// `fleet_move` needs it because bootstrap's toolchain-download bound is asked of the real rustup
/// (SKEIN-1090): a stand-in would prove only that bootstrap sets a variable, not that rustup reads it.
pub const RUSTUP: Tool = Tool {
    name: "rustup",
    probe: None,
};
