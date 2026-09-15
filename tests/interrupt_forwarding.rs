//! **A Ctrl-C at `skein` ends the command, and what the command started.**
//!
//! `a456c41` put every child of `util::run_bounded` in a process group of its own, so that a
//! deadline ends the WORK and not just the shell in front of it. The same move takes that child out
//! of the terminal's foreground group, and the terminal then stops delivering Ctrl-C to it — one
//! leak closed and another opened, on the path where a person is watching. `src/bin/skein.rs`
//! installs a `SIGINT` handler that forwards; this is the proof that it does, through the real
//! binary, on the production call the decision was taken for.
//!
//! **The command under the interrupt is a real one.** `skein doctor` asks whether review summaries
//! work, which runs the model binary from PATH through `ai.rs:1037` ->
//! `util::output_with_timeout_fed` -> `run_bounded` with a 30s budget. So the fixture is a `claude`
//! of this test's own on PATH, and it is a shell that backgrounds its work and waits — the shape
//! every wrapper skein runs has, and the shape that makes the difference visible: POSIX has a
//! non-interactive shell set `SIGINT` to ignored in any job it backgrounds, so forwarding the
//! signal alone would kill the wrapper and leave the work. The wrapper's whole GROUP has to end.
//!
//! **Why 30s matters to the reading.** The deadline would end the group too. An assertion that
//! waited for it would pass against a skein that forwards nothing, so everything below is timed:
//! the interrupt lands about a second in, and what it claims has to have happened seconds before
//! the budget could have.

mod common;

use common::{processes_under, Scratch};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The model call this test interrupts, as it appears in a process table.
const WRAPPER: &str = "claude";

/// What the wrapper starts and abandons — the process a forwarded `SIGINT` does not reach by itself.
const WORK: &str = "sleep";

#[test]
fn a_ctrl_c_ends_the_model_call_and_the_work_it_started() {
    let scratch = Scratch::temp("skein-sigint-it").quiesce_with(end_everything_under);
    let mut skein = start_skein_doctor(scratch.path());

    // ---- PRESENT first (SKEIN-833) ----
    // An absence that was never a presence proves nothing, and here it would prove nothing twice
    // over: `doctor` reaching the model at all is what puts a bounded child in flight, and if it
    // stopped doing so this test would be interrupting a skein with nothing to forward to and
    // reading the empty table afterwards as success.
    let began = Instant::now();
    let running = wait_for(scratch.path(), &[WRAPPER, WORK], Duration::from_secs(20));
    assert!(
        running.len() >= 2,
        "`skein doctor` never reached the model call this test interrupts — the process table under \
         {} held {running:#?}, and nothing below was observed",
        scratch.path().display()
    );

    // ---- the Ctrl-C ----
    // To skein's process group and to nothing else, which is exactly what a terminal sends the
    // foreground job. The bounded child is NOT in that group — that is the whole subject here — so
    // nothing but skein's own handler can carry the signal any further.
    interrupt_group(&skein);
    let ended = Instant::now();
    let status = skein.wait().expect("wait for skein");

    // ---- skein left the way a killed process leaves ----
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(
        status.signal(),
        Some(libc::SIGINT),
        "skein did not exit as a process killed by SIGINT ({status:?}); a handler that swallows \
         the interrupt is worse than none, because the shell it returns to cannot tell"
    );
    assert!(
        ended.elapsed() < Duration::from_secs(5),
        "skein took {:?} to leave after the interrupt — long enough that the 30s model budget, \
         rather than the Ctrl-C, could be what ended anything below",
        ended.elapsed()
    );

    // ---- and it took the work with it ----
    let left = settle(scratch.path(), &[WRAPPER, WORK], Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "the interrupt ended skein and left its model call running: {left:#?}. Before a456c41 the \
         terminal delivered Ctrl-C to both, because the child shared skein's foreground group; \
         after it, the child has a group of its own and only skein's own handler can reach it."
    );
    assert!(
        began.elapsed() < Duration::from_secs(25),
        "the whole run took {:?}, which is inside the model call's own 30s deadline — a pass here \
         would not distinguish the interrupt from the timeout",
        began.elapsed()
    );
}

#[test]
fn a_second_ctrl_c_leaves_at_once_and_still_takes_the_work_with_it() {
    let scratch = Scratch::temp("skein-sigint-twice").quiesce_with(end_everything_under);
    let mut skein = start_skein_doctor(scratch.path());
    let running = wait_for(scratch.path(), &[WRAPPER, WORK], Duration::from_secs(20));
    assert!(
        running.len() >= 2,
        "`skein doctor` never reached the model call, so nothing below was observed: {running:#?}"
    );

    // The first interrupt buys the command a grace to end itself in; the second says the person has
    // stopped waiting. What must NOT happen is the second one buying a second grace, which is what
    // a handler that simply repeats itself would do.
    interrupt_group(&skein);
    std::thread::sleep(Duration::from_millis(50));
    interrupt_group(&skein);
    let ended = Instant::now();
    let status = skein.wait().expect("wait for skein");
    let waited = ended.elapsed();

    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(
        status.signal(),
        Some(libc::SIGINT),
        "skein did not exit as a process killed by SIGINT after two interrupts ({status:?})"
    );
    assert!(
        waited < Duration::from_millis(400),
        "the second interrupt waited {waited:?} — a person who asks twice is asking to leave now, \
         and the grace a first Ctrl-C buys is 500ms"
    );
    // Leaving at once is not licence to leave the work behind: the second interrupt ends the groups
    // outright rather than asking them.
    let left = settle(scratch.path(), &[WRAPPER, WORK], Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "two Ctrl-Cs left skein's model call running: {left:#?}"
    );
}

/// A real `skein doctor`, in a process group of its own, with a model binary this test wrote.
///
/// Everything it can be told to look at is inside `scratch`, so `processes_under` can recognise
/// what it started: the wrapper carries the path in its ARGV and the `sleep` it abandons carries it
/// only in its ENVIRONMENT, which is the surface SKEIN-687 was about.
fn start_skein_doctor(scratch: &Path) -> Child {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;

    for dir in ["skein", "boxes", "home", "bin"] {
        std::fs::create_dir_all(scratch.join(dir)).expect("make the fixture");
    }
    let model = scratch.join("bin").join(WRAPPER);
    std::fs::write(
        &model,
        // Backgrounds its work and waits, which is what makes the group the thing that has to end.
        format!("#!/bin/sh\n{WORK} 300 &\nwait\n"),
    )
    .expect("write the model binary");
    std::fs::set_permissions(&model, std::fs::Permissions::from_mode(0o755))
        .expect("make it runnable");

    Command::new(env!("CARGO_BIN_EXE_skein"))
        .arg("doctor")
        // The seam skein already has for exactly this: a test process that would spawn the real
        // agent CLI off `$PATH` is refused outright, because the real one bills the owner's real
        // login and a test that reached it would look like one that had not. So the model binary is
        // named rather than found.
        .env("SKEIN_CLAUDE_BIN", &model)
        .env("SKEIN_HOME", scratch.join("skein"))
        .env("SKEIN_FLEET_ROOT", scratch.join("boxes"))
        .env("HOME", scratch.join("home"))
        // Nothing in this test is about the warden or the fleet, and both would otherwise be
        // answered by whatever is really running on this box.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("SKEIN_LS_CMD", "echo '[]'")
        .env_remove("SANDBOX_VM_ID")
        .env_remove("SKEIN_SELF")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group and session, because that is what a terminal's foreground job is
        // and a Ctrl-C is a signal to that group. Without it the interrupt below would be delivered
        // to this test binary's own group, which is every test running beside it.
        .process_group(0)
        .spawn()
        .expect("start the real skein")
}

/// Send `SIGINT` to skein's group, the way a terminal sends it to the foreground job.
fn interrupt_group(skein: &Child) {
    let group = skein.id() as libc::pid_t;
    // SAFETY: `skein` was spawned with `process_group(0)` and has not been reaped, so its pid is
    // its own group's id and can be nothing else's. Never a `pkill -f` and never a bare negative
    // pid: this is the only group this test is allowed to signal.
    assert_eq!(
        unsafe { libc::kill(-group, libc::SIGINT) },
        0,
        "signal skein"
    );
}

/// Wait until every one of `names` is running out of `root`, and say what was found.
fn wait_for(root: &Path, names: &[&str], budget: Duration) -> Vec<(u32, String)> {
    let until = Instant::now() + budget;
    loop {
        let found = matching(root, names);
        if names
            .iter()
            .all(|n| found.iter().any(|(_, a)| a.contains(n)))
            || Instant::now() >= until
        {
            return found;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait until none of `names` is running out of `root` any more, and say what is left.
fn settle(root: &Path, names: &[&str], budget: Duration) -> Vec<(u32, String)> {
    let until = Instant::now() + budget;
    loop {
        let found = matching(root, names);
        if found.is_empty() || Instant::now() >= until {
            return found;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The processes running out of `root` whose argv names one of `names`.
///
/// `processes_under` is the repository's own finder and reads a process's ENVIRONMENT as well as
/// its command line, which is the half SKEIN-687 was about — the abandoned `sleep 300` carries this
/// fixture's path nowhere else.
fn matching(root: &Path, names: &[&str]) -> Vec<(u32, String)> {
    processes_under(root)
        .into_iter()
        .filter(|(_, argv)| names.iter().any(|n| argv.contains(n)))
        .collect()
}

/// End anything still running out of `root`, by pid, however the test ended.
///
/// A failing test keeps its scratch directory, because the directory is the evidence — but a kept
/// directory must not keep a `sleep 300` alive with it (SKEIN-645). Never a `pkill -f`: every pid
/// here came from the repository's own finder having matched THIS directory, and each one is
/// re-read from `/proc` and checked again before it is signalled, because a pid read a moment ago
/// may have been reaped and handed to somebody else since.
fn end_everything_under(root: &Path) {
    let named = root.display().to_string();
    for (pid, argv) in processes_under(root) {
        let read = |what: &str| {
            std::fs::read(format!("/proc/{pid}/{what}"))
                .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
                .unwrap_or_default()
        };
        if !read("cmdline").contains(&named) && !read("environ").contains(&named) {
            continue;
        }
        eprintln!("ending {pid} ({argv}) — this test started it and it outlived the test");
        // SAFETY: `kill` has no memory effects, and the pid was just re-read out of /proc as one
        // still naming this test's own scratch directory.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
}
