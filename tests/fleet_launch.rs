//! The whole shared-sandbox launch, end to end, against a fake `sbx`.
//!
//! Everything the fleet path needs from the host is `sbx create` and `sbx exec`. But a sandbox is a
//! Linux machine with `bwrap`, `tmux` and `git` — and so is the machine running this test — so
//! `sbx exec <fleet> …` can simply mean "run it here" and the rest is genuinely exercised: a real
//! clone from a real remote, a real bwrap namespace, a real tmux server, real `nsenter` re-entry.
//!
//! What this deliberately does NOT cover is sbx's own behaviour — whether the flags are spelled
//! right, and where a workspace mount lands. Both were verified by hand against a real sandbox
//! instead (see `fleet::create_argv` and `fleet::fleet_workspace`), because no fake can answer them.
//!
//! Skipped rather than failed where the substrate is absent: this suite is about skein's logic, and
//! a machine without `bwrap` cannot host a box at all.

mod common;

use common::{bwrap_works, env_lock, env_pins, have, skip, Scratch};
use skein::config::{load_config, save_config, Config};
use skein::fleet::{
    anchor_from_launch, box_root, box_session_path, box_sock, box_state, clone_script,
    ensure_box_session, fleet_liveness, forget_fleet_liveness, heal_fleet, install_launcher,
    provision_script, resize_fleet, server_tmux_sock_in, session_script, snapshot_box, start_box,
};
use skein::kit::ensure_store;
use skein::place::{forget_place, own_sandbox, place_of, record_place, shared_record, PlaceRecord};
use skein::probes::ensure_probe_in;
use skein::repos::{branch_of, save_repos, Repo};
use skein::sandbox::{destroy_box, stop_box};
use skein::sbx::{fleet_boxes, Liveness};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const FLEET: &str = "test-fleet";
const BOX: &str = "web-main";

fn sh(script: &str) -> String {
    let out = Command::new("bash")
        .arg("-lc")
        .arg(script)
        .output()
        .expect("bash");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A stand-in for `sbx` that runs the guest command locally.
///
/// `exec` drops its flags and the sandbox name and execs the rest, so an `nsenter` hop reaches the
/// same namespace it would in a real sandbox. `create` only has to succeed — the sandbox in this
/// test is the machine itself.
fn write_fake_sbx(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    fs::write(
        &p,
        r#"#!/usr/bin/env bash
verb="$1"; shift
case "$verb" in
  create) exit 0 ;;
  # Destroying the sandbox is the one irreversible step, so the harness records that it happened
  # rather than trusting resize's own report of whether it got that far.
  rm) : > "$SBX_RM_MARKER"; exit 0 ;;
  exec)
    while [ $# -gt 0 ]; do case "$1" in -*) shift ;; *) break ;; esac; done
    shift          # the sandbox name
    exec "$@" ;;
  *) echo "fake sbx: unsupported verb $verb" >&2; exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A bare repo with one commit on `main`, standing in for the remote a box clones from.
fn write_remote(root: &Path) -> String {
    let remote = root.join("remote.git");
    let seed = root.join("seed");
    let git = "git -c user.email=t@example.com -c user.name=test -c init.defaultBranch=main";
    sh(&format!(
        "set -e; git init --bare -q -b main {r}; {git} init -q {s}; \
         cd {s}; echo hello > README.md; {git} add -A; {git} commit -qm seed; \
         {git} remote add origin {r}; {git} push -q origin main",
        r = remote.display(),
        s = seed.display(),
        git = git
    ));
    remote.to_string_lossy().into_owned()
}

/// Deliberately **not** under `/tmp` or `$HOME`: a box binds its own directories over both, so a
/// box root beneath either is unreadable from outside — and `box-session.sh` refuses it outright.
/// The first run of this test put the scratch in `/tmp` and was correctly turned away.
///
/// The literal before `{what}` is load-bearing and is why this is a helper rather than a
/// `Scratch::boxes` at each call site: `tests/ui/harness/leaks.mjs` reads the fixture prefixes it
/// scans for out of the `Scratch::boxes`/`Scratch::temp` call sites themselves, and `skein-fleet-it-`
/// is the one it derives from this line. Spelling the prefix with a variable in front of it would
/// leave the scan with nothing to derive here and make this whole binary invisible to it — which is
/// the failure the derivation replaced (SKEIN-647), not a reason to trust a hand-written list.
///
/// **And it quiesces.** See [`quiesce_fixture`]: a fixture *directory* is deliberately kept when a
/// test fails, because it is the only evidence a failure leaves, but the tmux servers inside it are
/// not evidence — they are a supervisor loop restarting a python every two seconds for as long as
/// anyone leaves it alone (SKEIN-645).
fn scratch_named(what: &str) -> Scratch {
    Scratch::boxes(&format!("skein-fleet-it-{what}")).quiesce_with(quiesce_fixture)
}

/// Every process still running out of `root` **that this binary is not itself an ancestor of.**
///
/// The scan itself is `common::processes_under`, which walks `/proc` and reads both `cmdline` and
/// `environ`; its doc comment carries why it is a scan rather than a list of recorded pids
/// (SKEIN-834) and why one surface is not enough (SKEIN-687). It lives there rather than here
/// because `common::sweep_abandoned` has to ask the same question about a directory it is about to
/// remove, and answers the ancestry question below *differently* — see that function for which way
/// round, and why the two are not in disagreement.
///
/// The needle is this fixture's absolute path, which ends in this process's pid, so it cannot match
/// another run's box or another lane's suite.
///
/// **What this adds is that a process this binary is still an ancestor of is not a leak, however
/// well it matches.** An environment is INHERITED, and `env_pins` writes a table shared by every
/// thread in the process — so while one test holds `$SKEIN_FLEET_ROOT` pinned at its fixture, every
/// child ANY OTHER test spawns carries that path too, having nothing whatever to do with a box.
/// Written without this clause, [`quiesce_fixture`] SIGKILLed the stub belonging to
/// `the_launchers_sudo_never_waits_for_a_password`, whose own scratch is in `/tmp`: 4 failures in 14
/// runs of this binary against 0 in 13 on the same tree without the change, and then caught in the
/// act — a `bash -c ensure_container_cgroup() …` whose ppid was this very process, carrying
/// `SKEIN_FLEET_ROOT=/var/tmp/skein-fleet-it-box-<that pid>/boxes`.
///
/// So ancestry is walked and a live descendant is left to whoever is running it — the same
/// distinction `tests/ui/harness/leaks.mjs` draws between a leak and a run in flight. Nothing this
/// has to catch is lost by it: what leaks here daemonizes and is `ppid=1` before this ever runs,
/// both the doorway's tmux server and the box's own, and a pane hangs off its server rather than
/// off this process.
fn fixture_processes(root: &Path) -> Vec<(u32, String)> {
    let me = std::process::id();
    common::processes_under(root)
        .into_iter()
        .filter(|(pid, _)| !descends_from(*pid, me))
        .collect()
}

/// Is `pid` below `ancestor` in the process tree?
///
/// The ppid is field 4 of `/proc/<pid>/stat`, and the field before it is `comm` in parentheses —
/// which may itself contain spaces and parentheses, so the fields are counted from past the LAST
/// `)` rather than split from the front. That is the same reading
/// `a_box_lives_and_dies_inside_the_fleet_sandbox` does for the anchor's start time.
///
/// Bounded, because this runs inside a `Drop`: a `/proc` read that disagrees with itself midway —
/// which it may, since the tree is changing while it is walked — must not turn a teardown into a
/// hang. Depth 64 is far past any tree this suite makes, and running out of it answers "no", which
/// is the answer that treats the process as a leak rather than the one that overlooks it.
fn descends_from(mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some((_, after_comm)) = stat.rsplit_once(") ") else {
            return false;
        };
        let Some(Ok(parent)) = after_comm.split_whitespace().nth(1).map(str::parse::<u32>) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        if parent <= 1 {
            return false;
        }
        pid = parent;
    }
    false
}

/// How long [`stop_fixture`] gives the `kill-server`s to take their supervisors with them.
///
/// Not a guess at how long that takes — `kill-server` SIGHUPs each pane's process group, so a
/// supervisor shell and whichever python its loop is on go with the tmux server in milliseconds.
/// It is a ceiling on a fixture that will not let go, and only a failure ever pays it:
/// [`until_none_under`] returns on the first clear scan, so a teardown that works costs one read
/// of `/proc`.
const KILL_WINDOW: Duration = Duration::from_secs(5);

/// How long each round of [`stop_fixture`]'s `SIGKILL` sweep waits for the signal to be delivered.
///
/// A second is already a hundred times what signal delivery costs; it is a bound, not a beat.
const SWEEP_WINDOW: Duration = Duration::from_secs(1);

/// What [`stop_fixture`]'s `kill-server` achieved, **measured before anything else could have.**
///
/// Two fields, because one of them is what makes the other mean anything. `left` is empty on a kill
/// that worked — and equally on a kill that did nothing at all, if the loop's exit condition had
/// been taken away first or a `SIGKILL` sweep had already run. That is not a hypothesis: it is what
/// `tests/server.rs` measured in SKEIN-920, and what this file was still doing until SKEIN-919.
struct Stopped {
    /// The doorway script — the supervisor loop's own `while [ -f … ]` exit condition — was still on
    /// disk when the wait that produced `left` finished, so for the whole of that wait nothing but
    /// the kill could have emptied it.
    script_was_there: bool,
    /// What still ran out of the fixture when the kill's wait gave up. Empty means the kill took it.
    left: Vec<(u32, String)>,
}

/// Poll until nothing runs out of `root`, or `within` elapses; whatever is still there.
///
/// **This is what replaces a sleep.** It returns on the first clear scan, so the passing path costs
/// one read of `/proc` rather than a fixed delay, and only a failure pays `within`. The 10 ms
/// between tries is a poll interval and not a beat: nothing is decided by it, and doubling or
/// halving it changes only how many scans a failure makes.
fn until_none_under(root: &Path, within: Duration) -> Vec<(u32, String)> {
    let deadline = Instant::now() + within;
    loop {
        let left = fixture_processes(root);
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// End everything this fixture is running, before its directory would go, **and report what the
/// kill did** — on every path out of a test, including a panic and a Ctrl-C'd `cargo test`.
///
/// Reached from `Scratch`'s `Drop` through [`quiesce_fixture`], which runs whether the directory is
/// kept or removed. That split is the point: a failing test's directory is the only evidence the
/// failure leaves, and a tmux server in it is not evidence at all — with `$SKEIN_TESTS_KEEP_SCRATCH`
/// set (which is what a kept directory looks like to the supervisor) a single passing run of this
/// binary left **nine** processes across its three fixtures, three of them restarting a python every
/// two seconds with nothing left that would ever stop them. That is SKEIN-645 exactly.
///
/// **The kill comes first and the script removal last, which is the reverse of what this used to
/// do** (SKEIN-919, correcting the order SKEIN-920 disproved). `fleet::start_server` wraps the
/// doorway in `while [ -f <fixture>/boxes/.skein/server-doorway.py ]` — [`skein::fleet`]'s
/// `supervised`, built by `start_server` — so that script is the loop's own exit condition. The old
/// order removed it first, on the belief that ending the session could otherwise lose a race with a
/// restart. Measured, that belief is false in both directions: removing the script does NOT stop a
/// supervisor already running, because `src/server-doorway.py` holds its socket for as long as it is
/// alive and the `while` never comes round to re-test its condition; and removing it first makes
/// every count taken afterwards empty for a `kill-server` that does nothing whatever. Under the old
/// order, `kill-server` replaced by `list-sessions` left this file's whole suite green.
///
/// So the kill is measured while the exit condition is still TRUE, and [`Stopped::script_was_there`]
/// reports that in the same breath as the count — a count nobody can date is exactly what went
/// wrong. The removal then happens unconditionally, so the end state is the one the old order left.
///
/// Four steps, and the order is the whole of it:
///
///   1. **Every tmux server in the fixture is told to end** — the fleet's doorway socket, and each
///      box's `session.sock`. Ending a server ends its panes, which is the only thing that reaches
///      a pane that `exec`ed and has no name left to be found by. The socket paths are DERIVED
///      (`server_tmux_sock_in`, and a `read_dir`) and never spelled: a literal here would not fail
///      when the socket moves, it would quietly stop killing anything (SKEIN-529).
///   2. **The wait, on the post-condition**, while the script is still on disk.
///   3. **Then the script**, unconditionally, so no further python is started.
///   4. **Then the scan, and `SIGKILL` by pid**, in rounds, because a scan of `/proc` is a sample.
///      Never `pkill -f`: a pattern kill on this box is how one lane killed another lane's test run
///      mid-flight, and the pattern would have to match a process whose argv is `sleep 400` anyway.
///
/// **The sweep is the fallback and never the measurement.** It runs after [`Stopped::left`] has been
/// recorded, so it cannot turn a failed kill into a clean count — which is what it did before, and
/// why nothing in this file could tell a working `kill-server` from one that had been sabotaged. It
/// is also not enough on its own to be relied upon as the mechanism: the scan reads `cmdline` and
/// `environ`, and on this box `node tests/ui/harness/leaks.mjs` reports ~97 of ~106 processes whose
/// environment this user may not read at all. A process that `exec`ed away its argv and will not
/// show its environment is invisible to the sweep and reachable only through its tmux server, which
/// is precisely the surface SKEIN-687 was about.
///
/// **It never panics.** `Drop` reaches this while the thread may already be unwinding, and a panic
/// there aborts the process — replacing a named assertion failure with a core dump. So a fixture
/// that will not let go is reported on stderr and the test's own result stands.
fn stop_fixture(root: &Path) -> Stopped {
    let fleet_root = root.join("boxes");
    let script = fleet_root.join(".skein/server-doorway.py");
    let mut socks = vec![server_tmux_sock_in(&fleet_root.to_string_lossy())];
    if let Ok(entries) = fs::read_dir(&fleet_root) {
        for entry in entries.flatten() {
            let sock = entry.path().join("session.sock");
            if sock.exists() {
                socks.push(sock.to_string_lossy().into_owned());
            }
        }
    }
    for sock in socks {
        let _ = Command::new("tmux")
            .args(["-S", &sock, "kill-server"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    // Taken in this order: the wait first, then whether the exit condition survived it. Read the
    // other way round, `script_was_there` would be a fact about a moment before the count rather
    // than about the whole of it.
    let stopped = Stopped {
        left: until_none_under(root, KILL_WINDOW),
        script_was_there: script.is_file(),
    };
    let _ = fs::remove_file(&script);
    for _ in 0..3 {
        let stragglers = fixture_processes(root);
        if stragglers.is_empty() {
            break;
        }
        for (pid, _) in &stragglers {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = until_none_under(root, SWEEP_WINDOW);
    }
    // A machine with no `kill(1)` on it, or a process in uninterruptible sleep, falls through to
    // here rather than quietly succeeding.
    for (pid, argv) in fixture_processes(root) {
        eprintln!(
            "quiesce: pid {pid} is still running out of {} and would not die: {argv}",
            root.display()
        );
    }
    stopped
}

/// [`stop_fixture`] as `Scratch` wants it: the measurement is for the test that asserts on it, and
/// a `Drop` has nowhere to put one.
fn quiesce_fixture(root: &Path) {
    let _ = stop_fixture(root);
}

/// **A sweep does not delete a dead run's fixture out from under what that run left running**
/// (SKEIN-900).
///
/// `common::sweep_abandoned` runs before the first scratch directory of every `tests/*.rs` in this
/// repository, and it used to remove a `<prefix>-<pid>` directory on the strength of that pid being
/// out of `/proc` alone. That is a *second* producer of the state this week was spent learning to
/// detect — process alive, fixture gone — independent of whichever binary leaked in the first place
/// (SKEIN-884): what is left names a path that does not exist, and nothing says which run made it.
///
/// It lives in this file rather than beside the code because this is where the `/proc` scanning it
/// shares with [`quiesce_fixture`] is already proven, and `tests/common/mod.rs` compiles no tests of
/// its own.
///
/// **Presence, then absence, and a control for each direction.** An absence that was never a
/// presence proves nothing (SKEIN-833), so the haunted directory is asserted to have something
/// running out of it *before* the sweep; and the quiet directory beside it, identically named and
/// identically dead, is asserted to be GONE afterwards — without which this test would pass just as
/// well against a sweep that had stopped deleting anything at all.
///
/// **The fixture path is put in the ghost's ENVIRONMENT and not in its argv**, because that is the
/// shape a real leak has: a box's pane runs `exec sleep 400` and keeps no name in its command line
/// at all (SKEIN-687). A scanner reading only `cmdline` passes every other assertion here and sees
/// nothing.
///
/// **The ghost is an ordinary child of this test, and that is deliberate.** [`fixture_processes`]
/// exempts a live descendant because it is about to SIGKILL what it finds; the sweep exempts nobody
/// because it is about to DELETE what it finds, and deleting the directory out from under a process
/// is the same harm whoever owns it. A ghost that daemonized to `ppid=1` would exercise a weaker
/// claim than this one does.
#[test]
fn a_sweep_keeps_a_dead_runs_fixture_while_anything_is_still_running_out_of_it() {
    // A root of this test's own. `sweep_abandoned` memoises per ROOT, so a shared one would answer
    // the second question with the first question's visit — and sweeping the real `/var/tmp` from
    // here would remove two other lanes' abandoned directories, which is somebody else's evidence.
    let scratch = Scratch::temp("skein-sweep-it");
    let root = scratch.path();

    // A pid that is genuinely gone rather than one picked to look dead: spawned, then reaped. If it
    // were recycled before the sweep, both directories would be skipped as a LIVE run's and the
    // removal assertion below fails — the loud direction, not the quiet one.
    let dead = {
        let mut done = Command::new("/bin/true").spawn().expect("/bin/true");
        let pid = done.id();
        done.wait().expect("reap it, so the pid is really free");
        pid
    };
    let haunted = root.join(format!("skein-sweep-haunted-{dead}"));
    let quiet = root.join(format!("skein-sweep-quiet-{dead}"));
    fs::create_dir_all(haunted.join("boxes")).unwrap();
    fs::create_dir_all(&quiet).unwrap();

    // `sleep` with a bounded argument, so that a failure anywhere below cannot leave this test's own
    // orphan running until somebody sweeps the box by hand.
    // Both coupled variables, which is what `tools/fleet-pin-check.py` requires of any scope that
    // says either — and what a real leaked box carries anyway: read off a live pane, its environment
    // names the fixture through `SKEIN_HOME`, `SKEIN_FLEET_ROOT`, `SKEIN_STATE`, `TMUX`, `PWD` and
    // `HOME`. Neither is process-global here: these are this child's environment and nothing else's.
    let mut ghost = Command::new("sleep")
        .arg("400")
        .env("SKEIN_HOME", &haunted)
        .env("SKEIN_FLEET_ROOT", haunted.join("boxes"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a process to haunt the dead run's fixture with");
    let ghost_pid = ghost.id();

    // Every observation is taken BEFORE the ghost is ended, and every assertion AFTER — so a failing
    // assertion unwinds past nothing that had to happen. This is the `Scratch` lesson applied to a
    // process: a trailing `kill` runs exactly when the test passes.
    let haunting = common::processes_under(&haunted);
    let beside_it = common::processes_under(&quiet);
    let quiet_was_there = quiet.exists();
    let kept = common::sweep_abandoned(root);
    let haunted_survived = haunted.exists();
    let quiet_swept = !quiet.exists();
    let _ = ghost.kill();
    let _ = ghost.wait();

    assert!(
        haunting.iter().any(|(pid, _)| *pid == ghost_pid),
        "nothing was found running out of {}, so the survival asserted below would be about a \
         fixture nobody was holding — and a scan that answers this way answers it for every real \
         leak too. Found: {haunting:#?}",
        haunted.display()
    );
    assert!(
        beside_it.is_empty(),
        "something is already running out of {}, so its removal below would prove nothing about \
         the empty case: {beside_it:#?}",
        quiet.display()
    );
    assert!(
        quiet_was_there && quiet_swept,
        "the control directory was {} before the sweep and {} after it. A sweep that removes \
         nothing passes every other assertion in this test",
        if quiet_was_there { "present" } else { "absent" },
        if quiet_swept { "gone" } else { "still there" }
    );
    assert!(
        haunted_survived,
        "the sweep removed {} while pid {ghost_pid} was still running out of it. That is the \
         expensive state exactly: the process outlives the only thing that explains it, and \
         `node tests/ui/harness/leaks.mjs` can then name it but not say which run to look at",
        haunted.display()
    );
    assert_eq!(
        kept.iter().map(|(dir, _)| dir.clone()).collect::<Vec<_>>(),
        vec![haunted.clone()],
        "the sweep kept a different set of directories than the one it should have reported on, \
         so the notice it printed and the decision it took are not the same thing: {kept:#?}"
    );
    assert!(
        kept[0].1.iter().any(|(pid, _)| *pid == ghost_pid),
        "the sweep kept {} without naming pid {ghost_pid} as the reason, which leaves a reader of \
         that notice with a directory and no orphan to attribute it to: {:#?}",
        haunted.display(),
        kept[0].1
    );
}

/// A home for the fleet sandbox, with an agent CLI in it where the real one lives.
///
/// Two things this replaces, both of which made the launch test depend on the machine it ran on.
/// `$HOME` was the developer's own — `sbx exec` here means "run it on this machine", so the box was
/// placed over a real home directory. And `command -v claude` inside the box was read as "the agent
/// survived the launch" when what it actually asked was "is Claude Code installed here": the suite
/// failed outright on a machine without it, and passed for the wrong reason on this one, where
/// `claude` is at `/usr/local/share/npm-global/bin/claude` — outside `$HOME` entirely, so replacing
/// `$HOME` wholesale would not have moved it.
///
/// The stub goes at `~/.local/bin/claude`, which is where Claude Code installs itself and therefore
/// the only placement under which that assertion means what it says: the launcher binds the box's
/// private home over `$HOME`, so a launch that replaced the home rather than binding into it takes
/// this path with it and `command -v claude` stops answering.
fn sandbox_home_with_agent(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = root.join("sandbox-home");
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\necho 'stub agent'\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    home
}

/// Block until `anchor` is gone from `/proc`, and say so loudly, naming the pid, if it never is.
///
/// **This is the real post-condition of ending a box.** `box-session.sh` reports the tmux SERVER's
/// pid as the anchor and says why in as many words — "box alive <=> server alive <=> namespace
/// joinable" — and `place::local_liveness` decides a box by asking `/proc/<anchor>/stat` for that
/// process's start time. So "the box is down" IS "that pid is gone", and every liveness assertion
/// in this file rests on it. It is not a proxy that happens to correlate; it is the fact the sweep
/// reads.
///
/// **And it is asynchronous, measured rather than assumed.** With a probe printed at the instant
/// `place.exec("tmux -S <sock> kill-server")` returned `Ok("")`, `/proc/<anchor>` still existed —
/// and the box's socket still accepted a connection — in **2 of 15 runs** on this box. The tmux
/// client's exit says the server took the command, not that it has finished ending its panes, its
/// cgroup and its namespace. On a bare tmux server with one `sleep` pane the same probe was clean
/// 200 times out of 200, which is why this looks synchronous until it is a box.
///
/// So the wait is on the POST-CONDITION and never on the subject. `fleet_liveness()` is still read
/// exactly once after this returns, so a sweep that reports a dead box as running still fails on
/// the first and only read, with nothing retried and no budget to run out. What this removes is
/// the other failure — the one where tmux had simply not finished — which is not a fact about
/// skein at all, and which an accidental `warden_client` round trip to the host used to hide by
/// costing a few milliseconds in between (SKEIN-739's lesson, at the sixth site).
///
/// It is also **faster to fail than what it replaces**. A box that genuinely stays up fails here,
/// naming the pid and what was expected of it, rather than at a `Some(true)` against `Some(false)`
/// sixty lines away that says nothing about why. The five seconds are the failure path only: the
/// spin is a millisecond and the loop is not entered at all once the pid is gone, so the ordinary
/// case costs one `Path::exists`.
fn anchor_gone(anchor: u32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let proc = format!("/proc/{anchor}");
    while Path::new(&proc).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the box's tmux server (pid {anchor}) is still in /proc five seconds after it was told \
             to end, so the box is still up — `place::local_liveness` reads exactly this, and every \
             liveness assertion after this point would be about a live box rather than about the \
             sweep"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// One box, from nothing to running to gone.
///
/// A single test rather than several: each step consumes the previous one's real side effects (the
/// anchor pid only exists once the session starts, and the placement only means anything while that
/// pid lives), so splitting them would mean either re-running the launch per assertion or sharing
/// mutable state between tests through the environment — which is exactly what makes suites flaky.
#[test]
fn a_box_lives_and_dies_inside_the_fleet_sandbox() {
    let _env = env_lock();
    // **The real crossing is this suite's subject**, so it says so rather than being refused:
    // `Place::spawning` turns a fleet-scope command into a panic in a test process that has
    // installed no stand-in (SKEIN-530), and a stand-in here would delete what the module note
    // above promises — a real clone, a real bwrap namespace, a real tmux server, real `nsenter`
    // re-entry. What keeps all of that inside the fixture is the `$SKEIN_FLEET_ROOT` these tests
    // pin at their own scratch tree.
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") || !have("git") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let root = scratch_named("box");
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);
    // Stand in for the SANDBOX's home, exactly as the sibling test below does and for the same
    // reason: `sbx exec` here means "run it on this machine", so a box placed over the real `$HOME`
    // is a box driving the developer's own home directory — and this one writes a credential file
    // into `~/.claude` (below) and reads `$HOME` into three `PlaceRecord`s.
    let sandbox_home = sandbox_home_with_agent(&root);

    // Bound after `root`, so every name stops pointing into the scratch tree before the tree is
    // removed — and `$HOME` in particular goes back on the failing path, where the `set_var` on
    // this test's last line used to be unwound past. A test that leaves `$HOME` naming a deleted
    // scratch directory is the worst of these to debug: everything after it in the binary reads
    // the developer's home as gone.
    //
    // **The fixture's `~/.local/bin` is deliberately NOT on this PATH** — only the fake `sbx` is.
    // It used to be, and that made the `command -v claude` assertion below satisfiable two ways:
    // by the box resolving its own home, or by the spawner's PATH riding through `nsenter` into the
    // box. The second is not the property, and while it was available the assertion could not tell
    // a crossing that lands on the box's PATH from one that lands on the caller's (SKEIN-832).
    // With it gone there is exactly one path by which `claude` can answer from the fixture: the
    // PATH `Place::wrap` builds from the box's OWN home.
    let mut pins = env_pins();
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("HOME", &sandbox_home)
    .set("SKEIN_HOME", root.join("skein"))
    // /boxes needs root to create; the seam exists so this path is testable at all.
    .set("SKEIN_FLEET_ROOT", root.join("boxes"));
    // **Named, because every address below is checked against it.** `Place` refuses an address for
    // a sandbox that is not the one this process is standing in — there is no `sbx` hop left to
    // reach another with (SKEIN-576) — and "the one it is standing in" is the configured fleet. A
    // fixture that left this at the default would be asking about somebody else's sandbox and
    // getting told so, which is correct and not what this test is about.
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");

    // **This fixture's box matches no repository, so it is one of the uncovered ones** — the
    // module note says so in its own words, "a sandbox that has never seen this repo", and nothing
    // here calls `save_repos`. `fleet::refuse_if_uncovered` turns that away now unless somebody has
    // said to, and `ensure_box_session` further down is on that path. Declared here, once, rather
    // than registering a repo: registering one would hand the launcher a manifest and change the
    // mounts this test is measuring, which is a different test. The refusal itself is asserted in
    // `an_uncovered_box_is_refused_until_it_is_allowed_and_then_says_so_where_someone_is_looking`.
    skein::fleet::allow_uncovered(BOX, true).expect("this fixture's box is uncovered on purpose");

    // ---- the launcher reaches a sandbox that has never seen this repo ----
    install_launcher(FLEET).expect("install box-session.sh");
    let launcher = box_session_path();
    assert!(
        Path::new(&launcher).exists(),
        "the launcher is embedded and installed over stdin, not served from a repo's store"
    );

    // ---- a checkout, from the remote at the base branch ----
    let place = own_sandbox(FLEET);
    place
        .exec(
            &clone_script(BOX, &remote, "main", "feat/auth", ""),
            Duration::from_secs(120),
        )
        .expect("clone");
    let tree = format!("{}/tree", box_root(BOX));
    assert_eq!(
        sh(&format!("git -C {tree} rev-parse --abbrev-ref HEAD")),
        "feat/auth",
        "the box starts on its own branch, cut from the remote base"
    );
    // A second box of the same name must not inherit this tree — it may hold uncommitted work.
    assert!(
        place
            .exec(
                &clone_script(BOX, &remote, "main", "feat/auth", ""),
                Duration::from_secs(60)
            )
            .is_err(),
        "an existing checkout is refused, not reused"
    );

    // ---- the session, and the anchor that outlives its launcher ----
    let launched = place
        .exec(
            &session_script(
                BOX,
                "skein-agent",
                // The agent records the environment it was STARTED with, which is the only place
                // that answer exists: a later `nsenter` gets a fresh environment, so asking the
                // running box would answer a different question. See the scratch assertion below.
                "printf '%s\\n' \"${CLAUDE_CODE_TMPDIR:-the shared /tmp}\" > /tmp/scratch.env; \
                 mkdir -p \"${CLAUDE_CODE_TMPDIR:-/tmp/nowhere}\"; \
                 echo agent-started > /tmp/agent.log; exec sleep 400",
            ),
            Duration::from_secs(60),
        )
        .expect("start the box");
    // Read off the launcher's own stdout, never out of the box's tree. The pidfile there is bound
    // read-write, so a box can put a sibling's server pid in it — and skein entering that would be
    // executing in the sibling's namespace with the box's name on it.
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    assert!(
        Path::new(&format!("/proc/{anchor}")).exists(),
        "the launcher has exited by now; the anchor must be the tmux server, which has not"
    );
    // And it is the same process the box was told to write down — the file stays for the box's own
    // use, so the two must agree while nobody is lying.
    let claimed = fs::read_to_string(skein::fleet::box_pidfile(BOX))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    assert_eq!(
        claimed,
        anchor.to_string(),
        "the launcher reported one pid and wrote another"
    );

    // ---- the ceiling that keeps one box from taking the fleet down ----
    // The limit is applied to the LAUNCHER before it execs bwrap, so the tmux server and everything
    // the agent forks inherit it. Moving the anchor pid afterwards would move one process and leave
    // its children outside — a limit that looks applied and holds nothing. Checked on the anchor
    // precisely because it is a process the launcher spawned, not the launcher itself.
    //
    // Skipped where the substrate can't do it: box-session.sh warns and runs the box uncapped rather
    // than refusing to start it, so the absence of cgroup delegation is not a test failure.
    // Which case this machine is in is read from the record the LAUNCH wrote, and not from a `sudo`
    // of the test's own. The probe here was `sudo mkdir -p /sys/fs/cgroup/skein` — with no `-n`, so
    // on a machine whose sudo wants a password it blocked on a prompt with `cargo test`'s output
    // captured and nothing on screen to answer, and on a machine with passwordless sudo it made a
    // root-owned cgroup on the developer's host to re-ask a question the launch had already
    // answered. `box-session.sh:1122-1160` writes `limits.state` as `capped <…>` or
    // `uncapped no-cgroup-delegation` on every start.
    //
    // Both branches assert, from opposite sides of the same agreement: whichever the launch says,
    // the anchor's own cgroup line has to say the same. Recording "uncapped" while the box IS in
    // its cgroup, or "capped" while it is not, is the failure either way — and the second is what
    // "nothing caps them" looked like before this existed.
    let cgroup_of_anchor = sh(&format!("cat /proc/{anchor}/cgroup 2>/dev/null"));
    let state = fs::read_to_string(format!("{}/limits.state", box_root(BOX))).unwrap_or_default();
    let in_its_cgroup = cgroup_of_anchor.contains(&format!("/skein/{BOX}"));
    if state.starts_with("capped ") {
        assert!(
            in_its_cgroup,
            "the launch recorded {state:?}, but the box's processes are outside its cgroup, so \
             nothing caps them: {cgroup_of_anchor}"
        );
        let limit = fs::read_to_string(format!("/sys/fs/cgroup/skein/{BOX}/memory.max"))
            .unwrap_or_default()
            .trim()
            .to_string();
        assert!(
            limit.parse::<u64>().map(|b| b > 0).unwrap_or(false),
            "the cgroup exists but holds no memory ceiling: {limit:?}"
        );
    } else {
        // Recorded, not merely logged: skein keeps a command's stdout and drops its stderr on
        // success, so "this box has no ceiling" would vanish precisely when the box started fine.
        // The file is how anything later can still ask — which is why its absence is a failure
        // rather than a second way of skipping.
        assert!(
            state.starts_with("uncapped "),
            "the launch left no readable answer to whether this box got a ceiling: {state:?}"
        );
        assert!(
            !in_its_cgroup,
            "the launch recorded {state:?} while the box sits in its own cgroup — the one record \
             anything later can read is wrong: {cgroup_of_anchor}"
        );
        eprintln!(
            "SKIPPED the cgroup ceiling assertions: this machine gave the launch no cgroup \
             delegation ({state})"
        );
    }

    // The stamp that makes the anchor an identity rather than a number, read the way skein reads
    // it — from this machine, which is the sandbox for this test.
    let generation = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .expect("a boot id")
        .trim()
        .to_string();
    let ns_start = sh(&format!(
        "sed -n 's/.*) //p' /proc/{anchor}/stat | cut -d' ' -f20"
    ))
    .trim()
    .parse::<u64>()
    .expect("the anchor's start time");

    record_place(
        BOX,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            // The sandbox's own HOME: a box no longer gets an empty private one. `claude` lives at
            // ~/.local/bin and its credentials at ~/.claude, so replacing HOME wholesale left a box
            // with no agent to run. Privacy comes from binding the few paths that must differ.
            home: std::env::var("HOME").unwrap_or_default(),
            tree: tree.clone(),
            sock: box_sock(BOX),
            generation: generation.clone(),
            ns_start,
            launcher: String::new(),
            ceiling: String::new(),
            ..Default::default()
        },
    )
    .unwrap();

    // The guard is not decoration, and this is the assertion that says so: the same box, one field
    // of its recorded identity wrong, is refused rather than entered. Every way of being wrong
    // means the box is gone — the pid still exists, and it belongs to something else.
    record_place(
        BOX,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            home: std::env::var("HOME").unwrap_or_default(),
            tree: tree.clone(),
            sock: box_sock(BOX),
            generation,
            ns_start: ns_start + 1,
            launcher: String::new(),
            ceiling: String::new(),
            ..Default::default()
        },
    )
    .unwrap();
    let refused = place_of(BOX)
        .expect("placed")
        .exec("pwd", Duration::from_secs(30))
        .expect_err("a recycled pid must not be entered");
    assert!(
        refused.contains("is gone") && refused.contains(&anchor.to_string()),
        "the refusal says the box is gone and names the pid: {refused}"
    );

    record_place(
        BOX,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            home: std::env::var("HOME").unwrap_or_default(),
            tree: tree.clone(),
            sock: box_sock(BOX),
            generation: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .unwrap()
                .trim()
                .to_string(),
            ns_start,
            launcher: String::new(),
            ceiling: String::new(),
            ..Default::default()
        },
    )
    .unwrap();

    // ---- and now every ordinary skein call lands inside that box ----
    let boxed = place_of(BOX).expect("placed");
    assert_eq!(
        boxed.sandbox, FLEET,
        "the box is not its own sandbox any more"
    );
    assert_eq!(
        boxed.exec("pwd", Duration::from_secs(30)).unwrap().trim(),
        tree,
        "scripts start at the repo root, which nsenter does not inherit"
    );
    // The agent and its credentials survive, because HOME is not replaced any more.
    //
    // The agent here is the stub `sandbox_home_with_agent` put at `~/.local/bin/claude`, which is
    // where Claude Code installs itself, and **the resolved path is what is asserted** rather than
    // "some claude answered".
    //
    // `command -v claude >/dev/null && echo yes` was the old spelling, and it asked the machine, not
    // the box: it fails on any machine without Claude Code installed, and on this one it stays green
    // with `.local` cut out of `share_paths` entirely, because `/usr/local/share/npm-global/bin/
    // claude` is still on the inherited PATH inside the box. The launcher binds the box's private
    // home over `$HOME` (`box-session.sh:939`) and binds `share_paths` back on top of it, so the
    // agent's own path is the one thing that says the share survived the bind.
    //
    // **It now asks about the PATH as well as the bind, because the fixture's bin directory is off
    // the spawner's PATH** (see the pin above). Two independent things have to hold for this to
    // answer: the share survived the bind, AND the crossing landed on a PATH derived from the box's
    // own home rather than on whatever the caller had. It caught the second failing on its own —
    // `Place::path_pin` put `FLEET_PATH` in front of a crossing and nothing set it back past the
    // hop, so a box answered `/usr/local/bin/claude`: the substrate's copy, with none of the
    // fleet's. That is exactly the confusion the resolved-path spelling exists to make visible.
    assert_eq!(
        boxed
            .exec("command -v claude", Duration::from_secs(30))
            .unwrap()
            .trim(),
        sandbox_home.join(".local/bin/claude").display().to_string(),
        "a box with no agent CLI cannot start one — this is what binding all of HOME broke"
    );
    // ...while the state that must differ per box really does. Two boxes sharing this file claim
    // work as the SAME agent, which silently defeats the atomic claim the tracker exists for.
    boxed
        .exec(
            "mkdir -p ~/.config/sync && echo mine > ~/.config/sync/env",
            Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(format!("{}/home/.config/sync/env", box_root(BOX)))
            .unwrap()
            .trim(),
        "mine",
        "the box's tracker identity landed in its own copy"
    );
    assert_eq!(
        boxed
            .exec("cat /tmp/agent.log", Duration::from_secs(30))
            .unwrap()
            .trim(),
        "agent-started",
        "the agent really ran inside the namespace"
    );

    // ...and it was started with a scratch directory of its own, rather than being left to derive
    // one from the shared /tmp.
    //
    // Claude Code puts its temp directory at `${os.tmpdir()}/claude-<uid>` and REFUSES to start
    // when that path is somebody else's. In a fleet that path is the sandbox's shared /tmp, and on
    // the owner's fleet something running as root got there first: every model call, and a login
    // whose OAuth had otherwise completed, came back `Temp directory /tmp/claude-1000 is owned by
    // uid 0` (SKEIN-289).
    //
    // Read off the file the AGENT wrote, never asked of the running box: `boxed.exec` enters the
    // namespace fresh through nsenter, so it would report its own environment and pass whatever
    // the launcher did. And driven through `session_script` + the real launcher + real bwrap,
    // because a grep for the string in `box-session.sh` proves the string is present, not that the
    // environment a box starts with carries it.
    let scratch = boxed
        .exec("cat /tmp/scratch.env", Duration::from_secs(30))
        .unwrap()
        .trim()
        .to_string();
    let box_home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(
        scratch,
        skein::fleet::model_scratch_dir(Path::new(&box_home))
            .display()
            .to_string(),
        "the box's agent starts in the shared /tmp, where anything that got there first stops the \
         runtime from starting at all"
    );
    // And the path is the box's OWN, not one every box in the sandbox shares: the launcher binds
    // the box's private home over $HOME, so the directory the agent made inside the namespace has
    // to land under the box's root out here.
    assert!(
        Path::new(&format!(
            "{}/home/{}",
            box_root(BOX),
            skein::fleet::MODEL_SCRATCH
        ))
        .is_dir(),
        "the agent's scratch directory is not in this box's private home, so every box in the \
         sandbox shares one — which is the thing a per-box path exists to prevent"
    );

    // ---- provisioning: the same script the kit runs, inside the box ----
    // A box that never got this comes up looking entirely healthy and simply never reports — no
    // hooks, no probe, no tracker. It is the one gap that cannot be seen from the outside, so it is
    // asserted from the inside, on the artefacts the script actually leaves.
    let store = root.join("skein/repos/web/store/.claude");
    ensure_store(&store).expect("a store to provision against");
    // Written by the ordinary launch path (`write_launch_spec_for_agent`), which the fleet shares —
    // keyed on the box name, which is exactly the identity `SKEIN_BOX` supplies inside the box.
    fs::write(
        store.join(format!("skein/launch/{BOX}.json")),
        r#"{"branch":"feat/auth","agent":"claude"}"#,
    )
    .unwrap();
    boxed
        .exec(
            &provision_script(BOX, &store.to_string_lossy()),
            Duration::from_secs(120),
        )
        .expect("provision the box");
    assert_eq!(
        boxed
            .exec("readlink .claude", Duration::from_secs(30))
            .unwrap()
            .trim(),
        store.to_string_lossy(),
        "the store link is what makes hooks, skills and the probe resolve at all"
    );
    // `shared` is scoped to a REPO, not to a sandbox — the two were one object when a box WAS a
    // sandbox. So it must be this box's own symlink into its own repo's store, not the fleet
    // sandbox's directory bound through to every box regardless of which repo they are checkouts of.
    // Binding it also failed closed: shared-home.sh refuses to replace a real path, and it gates
    // startup, so every box in the fleet would have failed to provision.
    assert_eq!(
        boxed
            .exec("readlink ~/shared", Duration::from_secs(30))
            .unwrap()
            .trim(),
        store.join("shared-home").to_string_lossy(),
        "a box's shared workspace must resolve to its own repo's store"
    );
    // The boot report is per BOX, not per sandbox. Every box here reports the same SANDBOX_VM_ID,
    // so without an explicit identity they would overwrite each other — one box's diagnosis
    // standing in for all of them, which is worse than none.
    let boot = store.join(format!("skein/boot/{BOX}.json"));
    let report = fs::read_to_string(&boot).expect("boot report at the box's own name");
    assert!(
        report.contains("\"claude_link\":\"linked\"")
            && report.contains("\"shared_home\":\"linked\""),
        "the report is how a dark box is diagnosed without entering it: {report}"
    );
    // The store is infrastructure that must never show up as a worktree change.
    assert_eq!(
        boxed
            .exec("git status --porcelain", Duration::from_secs(30))
            .unwrap()
            .trim(),
        "",
        "the linked store leaked into the box's diff"
    );
    // The isolation, from the other side: the box's /tmp is invisible to everyone else.
    assert!(
        !Path::new("/tmp/agent.log").exists(),
        "the box's /tmp leaked into the sandbox's"
    );
    // Bytes, not text: this is the path the Files tab serves images and PDFs down.
    boxed
        .write("cat > blob", &[0u8, 159, 146, 150], Duration::from_secs(30))
        .unwrap();
    assert_eq!(
        boxed.bytes("cat blob", Duration::from_secs(30)).unwrap(),
        vec![0u8, 159, 146, 150],
        "a lossy UTF-8 hop here corrupts every binary the box serves"
    );

    // ---- the snapshot that has to survive a resize ----
    // Changing the fleet's memory or CPUs means destroying the sandbox, and every box's checkout is
    // VM-local — that is what makes builds fast and what makes this the one path where a bug costs
    // real work. So each kind of not-yet-pushed state is made distinct here and checked separately:
    // a commit that is on no remote, a staged change, an unstaged change, and an untracked file.
    boxed
        .exec(
            "git -c user.email=t@e.com -c user.name=t commit -qm local --allow-empty \
             && echo staged > s.txt && git add s.txt \
             && echo hello-unstaged >> README.md \
             && echo loose > u.txt",
            Duration::from_secs(60),
        )
        .unwrap();
    let head = boxed
        .exec("git rev-parse HEAD", Duration::from_secs(30))
        .unwrap()
        .trim()
        .to_string();
    // A transcript and a credential, side by side in the box's private HOME exactly as the real
    // agent leaves them — so the snapshot has to distinguish them rather than take the directory.
    boxed
        .exec(
            "mkdir -p ~/.claude/projects/-boxes-web-main-tree \
             && echo '{\"type\":\"user\"}' > ~/.claude/projects/-boxes-web-main-tree/sess.jsonl \
             && echo 'SECRET-TOKEN' > ~/.claude/.credentials.json",
            Duration::from_secs(30),
        )
        .unwrap();

    let relative = snapshot_box(BOX, &store.to_string_lossy(), "resize-run").expect("snapshot");
    assert!(
        relative.starts_with("skein/handoff-snapshots/"),
        "the provisioning script refuses any path outside that prefix: {relative}"
    );
    let snap = store.join(&relative);
    for artifact in [
        "repo.bundle",
        "index.patch",
        "worktree.patch",
        "untracked.tgz",
    ] {
        assert!(
            snap.join(artifact).metadata().map(|m| m.len()).unwrap_or(0) > 0,
            "{artifact} is empty — one whole class of unpushed work would be lost"
        );
    }
    // Restore into a fresh clone, which is exactly what a resized box comes up as. `--all` rather
    // than `HEAD`: a bundle of HEAD alone drops every other local branch the box was carrying.
    let restored = root.join("restored");
    sh(&format!(
        "set -e; git clone -q --branch main {remote} {r}; cd {r}; \
         git fetch -q {s}/repo.bundle 'refs/heads/*:refs/remotes/snap/*'; \
         git checkout -q -B feat/auth snap/feat/auth; \
         git apply --binary --index {s}/index.patch; \
         git apply --binary {s}/worktree.patch; \
         tar -xzf {s}/untracked.tgz",
        r = restored.display(),
        s = snap.display()
    ));
    assert_eq!(
        sh(&format!("git -C {} rev-parse HEAD", restored.display())),
        head,
        "the unpushed commit did not survive the bundle"
    );
    assert_eq!(
        sh(&format!(
            "git -C {} diff --cached --name-only",
            restored.display()
        )),
        "s.txt",
        "the staged change came back unstaged, which loses the index"
    );
    assert!(
        sh(&format!("cat {}/README.md", restored.display())).contains("hello-unstaged"),
        "the unstaged change was lost"
    );
    assert_eq!(
        sh(&format!("cat {}/u.txt", restored.display())),
        "loose",
        "the untracked file was lost — no patch covers these"
    );

    // ---- the conversation travels; the credential does not ----
    // A rebuilt box must resume the session rather than open a new one against a familiar tree, and
    // the transcript is addressed by the cwd slug, which survives because the box comes back at the
    // same path. The credential must NOT travel: the store is host-side shared data, and the box is
    // re-seeded with auth from the sandbox anyway. An allowlist is what makes the second half hold
    // for files that do not exist yet.
    // The transcript is not merely snapshot-able, it is already on the HOST — bound in from
    // box_state, so it survives the sandbox dying rather than only surviving a planned resize. An
    // OOM or a hand-run `sbx rm` never runs a snapshot; this is what covers those.
    let host_transcript =
        PathBuf::from(box_state(BOX)).join("claude-projects/-boxes-web-main-tree/sess.jsonl");
    assert_eq!(
        fs::read_to_string(&host_transcript)
            .expect("the conversation must be readable from the host")
            .trim(),
        "{\"type\":\"user\"}",
        "the box wrote its transcript into VM-local disk, where a crash would take it"
    );
    // And the credential did NOT follow it out: only the record directories are host-bound.
    assert!(
        !PathBuf::from(box_state(BOX))
            .join("claude-projects/.credentials.json")
            .exists()
            && !fs::read_dir(box_state(BOX))
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().contains("credential")),
        "host-binding the record must not drag the credentials out with it"
    );

    // The snapshot carries only what is genuinely VM-local. The transcript is host-bound already,
    // so copying it out to the store and straight back would traverse virtiofs twice to arrive at
    // the file that never moved.
    let members = sh(&format!("tar -tzf {}/agent-state.tgz", snap.display()));
    assert!(
        !members.contains(".claude/projects"),
        "the host-bound transcript was copied redundantly through the store: {members}"
    );
    assert!(
        !members.contains("credentials"),
        "a credential reached the shared store: {members}"
    );

    // ---- a resize that cannot save a box must not destroy the sandbox ----
    // The entire safety property of resize_fleet is its ordering: everything comes out first, and a
    // single failure leaves the sandbox standing with every box still in it. A partial snapshot is
    // not a partial resize, it is lost work — and the box that loses it is precisely the one whose
    // state could not be read. Asserted against the marker the fake sbx writes, not against the
    // error message, because the failure being guarded is "it destroyed things anyway".
    let rm_marker = root.join("sbx-rm-happened");
    pins.set("SBX_RM_MARKER", &rm_marker);
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..load_config()
    })
    .unwrap();
    // This box belongs to no registered repo, so its work has nowhere to be saved.
    //
    // `drop_docker` so the refusal under test is the one this asserts: the fake sbx has no Docker to
    // ask, and skein reads silence from Docker as a reason to stop — correctly, but that would make
    // this test pass for the wrong reason and stop covering the ordering it exists for.
    let err = resize_fleet("8g", "4", "", true).expect_err("resize must refuse");
    // **Which refusal answered, and the other one this recognises** (SKEIN-433). `resize_fleet`
    // refuses in phase 1 for more than one real reason and the FIRST of them is space:
    // `room_to_copy_out` runs before the per-box census, because discovering the host is full
    // after the sandbox is gone would be the worst possible moment for it. So on a machine with
    // too little room the answer here is a correct refusal about the disk and not the ordering
    // this covers — and `err.contains("no registered repo")` reported that correct refusal as a
    // malformed one, sending the reader to the wording when the truth was 352 MiB free.
    // `common::pinned_refusal` carries the argument; this call only has to name the other answer.
    common::pinned_refusal(
        &err,
        &["no registered repo", "untouched"],
        &[(
            "copying the boxes out needs about",
            "this machine has too little free space to copy the boxes out, so the resize refused \
             on space before it reached the per-box census this covers",
        )],
        "the resize ordering assertions",
    );
    assert!(
        !rm_marker.exists(),
        "the sandbox was destroyed despite a box whose work could not be saved"
    );
    // The fleet's name is left set from here on. This used to blank it — harmless while an address
    // for another sandbox merely grew an `sbx exec` prefix — and blanking it now names a DIFFERENT
    // sandbox: `load_config` repairs an empty name to the default (SKEIN-484), and `Place` refuses
    // an address for any sandbox but the one this process is standing in, because there is no `sbx`
    // hop left to reach one with (SKEIN-576). Every call below addresses this fixture's fleet.

    // ---- liveness, without entering anything ----
    let sock = box_sock(BOX);
    assert!(
        sh(&format!(
            "tmux -S {sock} has-session -t skein-agent && echo yes"
        )) == "yes",
        "the socket sits outside the private mounts so the fleet can be listed from outside"
    );

    // ---- teardown through skein's own lifecycle, not a hand-rolled kill ----
    // stop_box used to run `sbx stop <box>`, which for a shared box either misses or stops an
    // unrelated sandbox carrying the same name. It must reach this box's server instead.
    stop_box(BOX).expect("stop the box");
    for _ in 0..40 {
        if !Path::new(&format!("/proc/{anchor}")).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !Path::new(&format!("/proc/{anchor}")).exists(),
        "killing the server must end the box"
    );
    forget_place(BOX);
    assert!(
        place_of(BOX).is_none(),
        "a forgotten box must resolve to nothing, not to a dead namespace — and not to a sandbox \
         named after it, which was the per-VM model and is gone"
    );

    // The cgroup outlives the box's filesystem — rmdir only succeeds once the server is gone, which
    // the wait above has already established. destroy_box does this for a real box; stop_box (used
    // here) deliberately does not, because a stopped box is meant to be startable again.
    //
    // `-n`, and only where the launch actually made a cgroup: a `sudo` that wants a password has
    // nothing to prompt on under `cargo test`, and there is nothing here worth blocking a suite to
    // tidy up.
    if state.starts_with("capped ") {
        let _ = Command::new("sudo")
            .args(["-n", "rmdir", &format!("/sys/fs/cgroup/skein/{BOX}")])
            .status();
    }
}

/// **A box's session gets the launcher's allow-list of the environment, and nothing else from the
/// process that started it** (SKEIN-972).
///
/// The cockpit starts every box, so whatever the cockpit was started with used to be in every box:
/// `SKEIN_HOME`, and `SKEIN_LISTEN_INHERITED_ONLY`, which the doorway sets for the cockpit alone.
/// `src/box-session.sh` now keeps only the names on `inherited_env`, each with its reason.
///
/// Driven through the real mechanism: this process's environment stands for the cockpit's, because
/// `Place::exec` spawns the launcher exactly as `skein-server` does, and the answer is read from a
/// file the box's own agent wrote from inside its namespace. Asking the running box instead would
/// be wrong: a crossing enters through `nsenter` with an environment of its own and reports that.
///
/// What would make each assertion fail:
///   * the canary: putting `SKEIN_TEST_LEAK_CANARY` on the list, or deleting the filter. A name no
///     list would carry is what tells an allow-list from a deny-list of the names noticed so far.
///   * the cockpit's three: deleting the filter.
///   * the exported function: deleting the launcher's `exec env -u …` that drops names bash cannot
///     unset.
///   * `SANDBOX_NAME`: a filter that removes everything, which would also take the proxy and the
///     credential placeholders every box needs.
///   * `SKEIN_BOX`: running the filter after the launcher's own exports rather than before them.
///
/// Only names and two chosen values leave the box: the fixture is kept when a test fails, and the
/// environment of a developer's box carries real credentials.
#[test]
fn a_box_session_inherits_only_its_allow_list() {
    let _env = env_lock();
    // The real crossing is the subject, as in the test above.
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so it cannot host a box",
        );
    }
    // Its own name, so its cgroup cannot be another test's, in this binary or in another run.
    let name = "envlist-main";
    let root = scratch_named("env");
    let sandbox_home = sandbox_home_with_agent(&root);

    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        // What must not arrive: a name nobody would list, and the cockpit's own three.
        .set("SKEIN_TEST_LEAK_CANARY", "skein-test-canary")
        .set("SKEIN_LISTEN_INHERITED_ONLY", "1")
        .set("SKEIN_IN_FLEET", "1")
        .set("BASH_FUNC_skein_leak%%", "() {  echo leaked\n}")
        // A name that is not an identifier, which bash can neither hold nor unset.
        .set("SKEIN_TEST_ODD-NAME", "1")
        // What must: one of the sandbox's own, which is on the list.
        .set("SANDBOX_NAME", "skein-test-allowed");
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    // Written to a temporary name and moved, so the poll below never reads half a report. The
    // canary's value is spelled in two pieces so this command line cannot be what matches it.
    let agent = "{ printf 'canary %s\\n' \"$(env | grep -c 'skein-test-cana''ry')\"; \
                 printf 'allowed %s\\n' \"${SANDBOX_NAME-}\"; \
                 if type skein_leak >/dev/null 2>&1; then echo 'fn defined'; else echo 'fn absent'; fi; \
                 env | cut -d= -f1 | sed 's/^/name /'; } > /tmp/env.tmp && mv /tmp/env.tmp /tmp/env.report; \
                 exec sleep 300";
    let launched = own_sandbox(FLEET)
        .exec(
            &session_script(name, "skein-agent", agent),
            Duration::from_secs(60),
        )
        .expect("start the box");
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");

    let report_path = PathBuf::from(format!("{}/tmp/env.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !report_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let report = fs::read_to_string(&report_path).unwrap_or_default();
    // And the environment of the box's tmux server, which every window and pane in the box is made
    // from. Needed beside the pane's own report because this box is uncovered, so its pane starts
    // behind the launcher's `sh -c` banner wrapper — and dash drops a variable whose name is not an
    // identifier, which an exported function's `BASH_FUNC_<name>%%` is not. A covered box has no
    // wrapper, so the pane alone would be asking a question this fixture answers by accident.
    // Names only, for the reason the report is.
    let server_env: Vec<String> = Command::new("tmux")
        .args(["-S", &box_sock(name), "show-environment", "-g"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split_once('=').map(|(n, _)| n.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let limits = fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
    // Ended before asserting, so a failure leaves no box running; `Scratch` would stop it too.
    let _ = Command::new("tmux")
        .args(["-S", &box_sock(name), "kill-server"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    anchor_gone(anchor);
    if limits.starts_with("capped ") {
        let _ = Command::new("sudo")
            .args(["-n", "rmdir", &format!("/sys/fs/cgroup/skein/{name}")])
            .status();
    }

    assert!(
        !report.is_empty(),
        "the box's agent never wrote its environment, so nothing below would be about a box"
    );
    let field = |key: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or("")
            .to_string()
    };
    let names: Vec<&str> = report
        .lines()
        .filter_map(|l| l.strip_prefix("name "))
        .collect();
    assert!(
        !names.contains(&"SKEIN_TEST_LEAK_CANARY") && field("canary") == "0",
        "a variable on no list reached the box from the process that started it, so a box still \
         inherits the cockpit's environment rather than the launcher's allow-list: {names:?}"
    );
    for cockpit in [
        "SKEIN_LISTEN_INHERITED_ONLY",
        "SKEIN_IN_FLEET",
        "SKEIN_HOME",
    ] {
        assert!(
            !names.contains(&cockpit),
            "{cockpit} is the cockpit's, and it reached the box: {names:?}"
        );
    }
    assert!(
        server_env.iter().any(|n| n == "HOME"),
        "the box's tmux server reported no environment, so the two checks below would be about \
         nothing: {server_env:?}"
    );
    assert!(
        !server_env.iter().any(|n| n == "SKEIN_TEST_LEAK_CANARY"
            || n.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))),
        "the box's tmux server, which every window in the box is made from, carries the canary, \
         an exported function or another name that is not an identifier from the cockpit's \
         environment: {server_env:?}"
    );
    assert_eq!(
        field("fn"),
        "absent",
        "an exported shell function from the cockpit's environment is defined in the box"
    );
    assert_eq!(
        field("allowed"),
        "skein-test-allowed",
        "SANDBOX_NAME is on the allow-list and did not arrive, so the filter is removing what a box \
         needs rather than what it was not given: {names:?}"
    );
    assert!(
        names.contains(&"SKEIN_BOX"),
        "the launcher exports SKEIN_BOX on purpose and the box does not have it, so the filter ran \
         over the launcher's own exports: {names:?}"
    );
}

/// The whole of `start_box`, rather than its pieces called in the right order by hand.
///
/// The test above assembles the launch itself — install, clone, session — and that is precisely why
/// it kept passing while every real launch failed. Each bug lived in the *seams*: the placement
/// recorded an empty HOME, so provisioning resolved `$HOME/shared` to `/shared`; no launch spec was
/// written, so the box stayed on the clone's default branch; the fleet root was never created. None
/// of it is visible unless the entry point itself is the thing under test.
#[test]
fn start_box_leaves_a_box_that_is_actually_usable() {
    let _env = env_lock();
    // **The real crossing is this suite's subject**, so it says so rather than being refused:
    // `Place::spawning` turns a fleet-scope command into a panic in a test process that has
    // installed no stand-in (SKEIN-530), and a stand-in here would delete what the module note
    // above promises — a real clone, a real bwrap namespace, a real tmux server, real `nsenter`
    // re-entry. What keeps all of that inside the fixture is the `$SKEIN_FLEET_ROOT` these tests
    // pin at their own scratch tree.
    let _real = skein::place::seam::real_crossings();
    // **`python3` is new in this guard and was always needed here** (SKEIN-957). The launcher's
    // whole credential leg is python — `login_life`, `merge_login`, and now the onboarding flag
    // asserted below — so on a machine without it a box is seeded with a copy and none of the
    // judgement, which is a different thing from what this test says it starts.
    if !bwrap_works() || !have("tmux") || !have("git") || !have("python3") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git/python3, so it cannot \
             host a box",
        );
    }
    let root = scratch_named("start");
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);
    let name = "demo-smoke";

    // Bound after `root`, so every name stops pointing into the scratch tree before the tree is
    // removed — `$HOME` below included, which the `set_var` on this test's last line put back only
    // when the test passed.
    let mut pins = env_pins();
    // And the warden, at an address where nothing listens: a box teardown reports the destroy
    // into the host audit log, and `warden_client` refuses a test process that has not said
    // which warden to ask rather than letting it reach the owner's (SKEIN-762).
    pins.set("SKEIN_WARDEN", "127.0.0.1:1");
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"))
    // No runtime installs: this harness's `sbx exec` runs on THIS machine, so the substrate step
    // would npm-install an agent runtime onto the developer's box. It did exactly that once.
    .set("SKEIN_RUNTIME_PACKAGES", "");
    // Stand in for the SANDBOX's home. Without this the fake `sbx` reports this machine's real
    // $HOME, and the launcher would seed a box from — and reconcile credentials back into — the
    // developer's own ~/.claude. A test must not be able to touch that; the first run of this test
    // read a real credential file, which is exactly how it was caught.
    let sandbox_home = root.join("sandbox-home");
    fs::create_dir_all(sandbox_home.join(".claude")).unwrap();
    // Shaped like the real thing, because the sync now reads it rather than moving it about: it
    // tells a login from the husk a logout leaves, and it carries only the login across. A
    // placeholder like `{"tok":"…"}` passed straight through the old copy and would say nothing
    // about either. The `mcpOAuth` block is what the sandbox must KEEP when a box's login arrives —
    // those grants are per-repo, and copying the file whole used to discard the receiver's.
    fs::write(
        sandbox_home.join(".claude/.credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"SEEDED","refreshToken":"r"},"mcpOAuth":{"sync|sandbox":{"accessToken":"GRANT-SHARED"}}}"#,
    )
    .unwrap();
    pins.set("HOME", &sandbox_home)
        // The fleet sandbox already exists, so `ensure_fleet` goes straight to substrate +
        // launcher.
        .set("SKEIN_LS_CMD", format!("echo '[{{\"name\":\"{FLEET}\"}}]'"));

    let store = root.join("store");
    fs::create_dir_all(&store).unwrap();
    ensure_store(&store).expect("seed the store");
    ensure_probe_in(&store).expect("seed the store's scripts");
    let repo = Repo {
        read_prs: false,
        id: "demo".into(),
        source: remote.clone(),
        store: store.to_string_lossy().into_owned(),
        agent: "claude".into(),
        plane_project: String::new(),
        sync_connection: String::new(),
        review_queue: true,
        sync_gateway_url: String::new(),
        ..Default::default()
    };
    save_repos(std::slice::from_ref(&repo)).expect("register the repo");
    let mut config = load_config();
    config.fleet_sandbox = FLEET.into();
    save_config(&config).expect("turn the fleet on");

    // ---- the gate, warmed before the act, so the act has something wrong to settle ----
    // Read once here and once after each of the three acts below, with no `forget_fleet_liveness()`
    // in between. That is the whole test: the gate serves its last good answer while it refreshes
    // behind the caller, so the only thing that can make the second read agree with the fleet is the
    // act having invalidated it. This is also the only place the check can live — `cfg!(test)` is
    // false for the library these tests link, so the gate is real here and disabled in unit tests.
    assert_eq!(
        fleet_liveness().get(name).copied(),
        None,
        "nothing is placed under this name yet, so the warm answer must not mention it"
    );

    start_box(
        name,
        &repo,
        "feat/smoke",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect("start the box");

    // Remove `start_box`'s settle and this reads back the map from before the launch — no entry at
    // all for a box that is up and whose row the person who pressed the button is looking at.
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(true),
        "starting a box must settle the liveness gate, or the board serves the pre-start picture"
    );

    // The placement must carry a real HOME: `Place::wrap` exports it, and an empty one sends every
    // `$HOME/…` path in provisioning to the filesystem root.
    let placed = shared_record(name).expect("the box is placed");
    assert!(
        placed.home.starts_with('/') && placed.home.len() > 1,
        "a placement with no HOME makes every box command write to /: {:?}",
        placed.home
    );

    // And which isolation it got. The whole chain, in one assertion, because every link is silent
    // on its own: `install_launcher` stamps the script it installs, the script reports the stamp it
    // was given, and `start_box` records what was reported. Break any of them and a box that IS
    // covered reads as uncovered forever — a restart that never clears the thing asking for it.
    //
    // A box keeps the mount namespace it was born with, so this is the only moment the answer
    // exists; there is nothing on the host to check it against afterwards, which is the whole
    // reason the value has to travel with the box.
    assert_eq!(
        placed.launcher,
        skein::fleet::launcher_revision(),
        "the box that this launcher just started does not know which launcher started it"
    );

    // And what bounds it. The value depends on the machine — a scratch fleet with no cgroup
    // delegation reports `uncapped no-cgroup-delegation` and that is the honest answer — so what is
    // asserted is that the box **said something**, which is the whole of the defect. Nothing on the
    // host can read `limits.state`; it is written inside the sandbox, so a box that reported nothing
    // is a box whose ceiling is unknowable, and it used to look exactly like a bounded one.
    assert!(
        !placed.ceiling.is_empty(),
        "the box did not say what bounds its memory, so an uncapped one is invisible again"
    );
    assert!(
        skein::fleet::is_capped(&placed.ceiling) || placed.ceiling.starts_with("uncapped "),
        "the ceiling state is neither capped nor a named reason: {:?}",
        placed.ceiling
    );

    // The launch spec is how the box, and skein, learn which branch this box is for. Asserting on
    // the checkout alone proves nothing: `clone_script` checks the branch out itself, so that stays
    // green with no spec at all — while the box's own restart falls back to the clone's default and
    // the board reports the wrong branch.
    let tree = format!("{}/tree", box_root(name));
    assert_eq!(
        sh(&format!("git -C {tree} rev-parse --abbrev-ref HEAD")),
        "feat/smoke",
        "the box is on its own branch, not the clone's default"
    );
    assert_eq!(
        branch_of(name).as_deref(),
        Some("feat/smoke"),
        "skein must be able to read the box's branch back from its launch spec"
    );

    // Provisioning ran *inside* the box: its shared home resolves under the store, not under /.
    let boxed = place_of(name).expect("placed");
    let link = boxed
        .exec("readlink \"$HOME/shared\" || true", Duration::from_secs(30))
        .expect("read the shared-home link");
    assert!(
        link.trim().starts_with(store.to_str().unwrap()),
        "shared home must point into the mounted store, got {link:?}"
    );

    // And the store is reachable from the checkout, which is what makes hooks and the probe work.
    let claude = boxed
        .exec(
            &format!("readlink -f {tree}/.claude || true"),
            Duration::from_secs(30),
        )
        .expect("resolve .claude");
    assert!(
        claude.trim().starts_with(store.to_str().unwrap()),
        "the box's .claude must resolve into the store, got {claude:?}"
    );

    // ---- the sandbox cycles: the tree survives, the session does not ----
    // Measured against a real sandbox, not imagined: after sbx restarted skein-fleet, the box's
    // checkout, private HOME and cgroup ceiling were all intact and its tmux server was gone. Every
    // such box was then unreachable, and what a user saw first was an nsenter error about a pid.
    let before = shared_record(name).unwrap().ns_pid;
    let place = own_sandbox(FLEET);
    // A re-login inside the box, made just before the session dies. It is newer than the sandbox's
    // copy and it STAYS HERE: the file a box writes is not evidence about itself, and a box that
    // could improve the fleet's copy could also poison it. See the launcher's direction rule.
    let box_cred = PathBuf::from(format!("{}/home/.claude/.credentials.json", box_root(name)));
    let seeded = fs::read_to_string(&box_cred).unwrap_or_default();
    assert!(
        seeded.contains("SEEDED"),
        "a new box inherits the sandbox's login rather than asking for its own: {seeded}"
    );
    // ---- and it is not then asked to log in anyway (SKEIN-957) ----
    // The invariant, over a box that was really started rather than over a fixture: a box skein has
    // handed a credential to must not meet Claude Code's onboarding screen, which is gated on
    // `hasCompletedOnboarding` in `~/.claude.json` and not on the credential. Derived over every
    // home under the fleet root that the launcher's own `login_life` calls a login, so a seed path
    // that acquires a credential some other way and forgets the flag fails here too. The sandbox's
    // copy in this fixture has no `.claude.json` at all, which is the state that produced the bug:
    // the flag cannot have arrived by being copied down.
    let carrying = homes_carrying_a_login(&root.join("boxes"));
    assert!(
        !carrying.is_empty(),
        "no box under the fleet root carries a login, so this assertion is about nothing — the \
         launch above did not seed the credential it was given"
    );
    for home in &carrying {
        assert_eq!(
            onboarding_flag(home),
            Some(serde_json::Value::Bool(true)),
            "{} holds a working credential and would still be asked to onboard — which is the \
             login screen on every new box",
            home.display()
        );
    }
    // ---- nor then asked to trust the tree skein cloned for it (SKEIN-959) ----
    // The same invariant over a really-started box: every box under the fleet root has ITS OWN
    // tree trusted in its own `~/.claude.json`, under the exact path its session starts in. No
    // login condition here, on purpose — trust is about the tree, not the credential.
    let mut trusted_boxes = 0;
    for at in fs::read_dir(root.join("boxes"))
        .expect("the fleet root has a boxes directory after a start")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|at| at.join("home").is_dir() && at.join("tree").is_dir())
    {
        let tree = at.join("tree").display().to_string();
        assert_eq!(
            trust_of(&at.join("home"), &tree),
            Some(serde_json::Value::Bool(true)),
            "{} was started by skein and would still ask the person to trust the tree skein cloned \
             for it",
            at.display()
        );
        trusted_boxes += 1;
    }
    assert!(
        trusted_boxes > 0,
        "no box under the fleet root has a home and a tree, so the trust assertion is about nothing"
    );
    std::thread::sleep(Duration::from_millis(1100)); // mtime granularity, not a race
                                                     // A re-login, and a grant of this box's own alongside it. Both are written here because the two
                                                     // must travel differently: the login belongs to the person and goes everywhere, the grant
                                                     // belongs to this box's repository and goes nowhere.
    fs::write(
        &box_cred,
        br#"{"claudeAiOauth":{"accessToken":"RELOGIN","refreshToken":"r"},"mcpOAuth":{"sync|box":{"accessToken":"GRANT-MINE"}}}"#,
    )
    .unwrap();
    // **Asserted, not discarded.** This used to be `.ok()`, which threw the kill's result away —
    // so a `tmux kill-server` that never reached the box left it running, and the assertion below
    // then reported `Some(true)`: correct about a live box, and silent about the sweep this test
    // is for. A kill that failed has to fail here, where the message names the kill.
    place
        .exec(
            &format!("tmux -S {} kill-server", box_sock(name)),
            Duration::from_secs(30),
        )
        .expect("the kill must reach the box's tmux server");
    // And waited out on the anchor, because the kill is not synchronous — see `anchor_gone`, which
    // is where the measurement is. One act, then one read.
    anchor_gone(before);
    // The server was killed behind skein's back, which is the one thing the gate cannot know. A warm
    // gate hands back its last picture immediately and refreshes behind the caller — deliberately,
    // so the board never blanks on a slow tick — so without this the assertion reads whatever the
    // *previous* test left there. Skein invalidates at every point it changes the fleet itself; this
    // is that, for a change skein did not make.
    forget_fleet_liveness();
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(false),
        "a killed server is a stopped box, and the sweep must see it"
    );

    // A snapshot exists to rescue work, so it must not need the box's session. Taken here, with the
    // server dead — the state resize hit on the first real run, where it refused with an nsenter
    // error and left the work it was trying to save unreachable.
    let snap = snapshot_box(name, repo.store.as_str(), "test-run").expect("snapshot a dead box");
    let bundle = store.join(&snap).join("repo.bundle");
    assert!(
        bundle.exists() && bundle.metadata().map(|m| m.len()).unwrap_or(0) > 0,
        "the box's commits must be saved even with no session: {}",
        bundle.display()
    );
    assert!(
        store.join(&snap).join("agent-state.tgz").exists(),
        "and its private agent state with them"
    );
    ensure_box_session(name).expect("restart the session from the tree");
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(true),
        "the box is reachable again without a re-clone"
    );
    let after = shared_record(name).unwrap().ns_pid;
    assert_ne!(
        before, after,
        "the anchor is a new process, so the placement must name it — a stale pid addresses nothing"
    );

    // ---- one login, seeded down, and never written back up ----
    // The fleet's copy is what every box is seeded from, so a box that could write it could hand
    // every later box a credential of its choosing — and the expiry it would win on is a number
    // inside a file the box writes. So the flow is one-way here: the box keeps its re-login and the
    // fleet's copy is untouched, whatever the two files claim about themselves.
    let reconciled =
        fs::read_to_string(sandbox_home.join(".claude/.credentials.json")).unwrap_or_default();
    assert!(
        reconciled.contains("SEEDED") && !reconciled.contains("RELOGIN"),
        "a box wrote the copy every later box is seeded from: {reconciled}"
    );
    // Nothing else travelled either, and this half fails silently. Copying the file whole would
    // have handed the sandbox this box's per-repo grant and destroyed the sandbox's own —
    // surfacing much later as an MCP server asking to be authorised again.
    assert!(
        reconciled.contains("GRANT-SHARED"),
        "the sandbox's own MCP grant was destroyed by a login sync: {reconciled}"
    );
    assert!(
        !reconciled.contains("GRANT-MINE"),
        "one box's per-repo MCP grant escaped into the shared copy: {reconciled}"
    );
    // And the box was not handed the fleet's older copy over its own newer one either: it keeps
    // what it has. Losing a working login to a stale one is the failure this rule replaced, not
    // one it is allowed to reintroduce.
    let kept = fs::read_to_string(&box_cred).unwrap_or_default();
    assert!(
        kept.contains("RELOGIN"),
        "the box's own login was overwritten with the fleet's older one: {kept}"
    );

    // ---- stopping and destroying settle the gate too, and this is also the teardown ----
    // Waited out through `/proc` rather than by polling `fleet_liveness`: every extra read is a
    // chance for the refresh running behind an earlier one to land, which would hide exactly the
    // staleness under test. One act, one read.
    let anchor = shared_record(name).expect("the box is placed").ns_pid;
    stop_box(name).expect("stop the box");
    // Through `anchor_gone` rather than a loop of its own: this site had the rule and the site
    // sixty lines above did not, which is the whole of why that one raced. Two spellings of one
    // wait is two places for it to stop agreeing.
    anchor_gone(anchor);
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(false),
        "stopping a box must settle the liveness gate, or the board keeps it running"
    );

    // The same rule with a worse failure: the box is not stopped but gone, and a gate serving its
    // last good answer leaves a destroyed box on the board for anyone to click.
    destroy_box(name).expect("destroy the box");
    // **The post-condition of a destroy, asked before the gate is.** `destroy_script` ends
    // `rm -rf <root>/<name>; …; exit 0`, so a removal that failed — a straggler mount from the
    // box's namespace is the way it can — is swallowed, and the box's directory is one of the two
    // registers `fleet::live_box_names` and `place::local_liveness` both read. Without this the
    // symptom is `Some(false)` where `None` was expected, sixty characters of Option that name
    // neither the directory nor the removal. Asked with no wait: the removal is inside a script
    // this call already waited for, so a directory still here is a defect and not a delay.
    assert!(
        !Path::new(&box_root(name)).exists(),
        "the destroy left {} behind, so the box still reads as live to `live_box_names` and to \
         the sweep — the `rm -rf` in `destroy_script` failed and its `exit 0` swallowed it",
        box_root(name)
    );
    assert_eq!(
        fleet_liveness().get(name).copied(),
        None,
        "destroying a box must settle the liveness gate, or the board keeps a box that is gone"
    );
    assert!(
        shared_record(name).is_none(),
        "a destroyed box is unplaced, so nothing can be sent into what used to be its namespace"
    );
}

/// A fleet outlives the skein that made it, so restarting the server has to repair one.
///
/// The sandbox keeps whichever `box-session.sh` it was last given. Upgrade the host and the two
/// disagree: this skein passes a spec the installed launcher cannot read, the launcher exits before
/// tmux, and every reconnect enters an anchor pid from the last boot — `nsenter: cannot open
/// /proc/<pid>/ns/user`, forever, because nothing on the reconnect path ever replaced the copy that
/// could not start. Nothing else in a run observes that mismatch, which is why the repair belongs to
/// the restart.
///
/// And it must not cost a VM boot. Starting the cockpit is not a request to run the fleet, so a
/// sleeping sandbox is asked about (`sbx ls`) rather than asked *of* (`sbx exec`) — the launcher it
/// carries is repaired by `ensure_box_session` on the path that wakes it instead.
#[test]
fn a_server_restart_repairs_a_fleet_that_predates_it() {
    let _env = env_lock();
    // **The real crossing is this suite's subject**, so it says so rather than being refused:
    // `Place::spawning` turns a fleet-scope command into a panic in a test process that has
    // installed no stand-in (SKEIN-530), and a stand-in here would delete what the module note
    // above promises — a real clone, a real bwrap namespace, a real tmux server, real `nsenter`
    // re-entry. What keeps all of that inside the fixture is the `$SKEIN_FLEET_ROOT` these tests
    // pin at their own scratch tree.
    let _real = skein::place::seam::real_crossings();
    let root = scratch_named("box");
    write_fake_sbx(&root.join("bin"));
    // Bound after `root`, so every name stops pointing into the scratch tree before it is removed.
    let mut pins = env_pins();
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"));
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        fleet_memory: "26g".into(),
        ..Config::default()
    })
    .expect("configure a fleet");

    // The launcher an older skein left behind, in the one place the sandbox looks for it.
    install_launcher(FLEET).expect("install box-session.sh");
    let launcher = box_session_path();
    let stale = "#!/usr/bin/env bash\nexit 9 # an older skein's copy\n";
    fs::write(&launcher, stale).unwrap();

    // **The sleeping half of this test is gone, and the property with it** (SKEIN-576). It asserted
    // that a fleet reported `stopped` by `sbx ls` was left alone — that starting the cockpit is not
    // a request to boot a VM — and it was a host's property: only a skein OUTSIDE the sandbox can
    // observe one that is not running. Skein runs inside the fleet now, so a fleet it can heal is
    // running by definition; there is no state in which this process exists and its sandbox does
    // not. What replaced the observation is that `sbx ls` is not asked at all from in here.
    //
    // ---- the copy out there becomes this binary's copy ----
    pins.set(
        "SKEIN_LS_CMD",
        format!(r#"echo '[{{"name":"{FLEET}","status":"running"}}]'"#),
    );
    await_ls(Some(Liveness::Running));
    heal_fleet().expect("heal a running fleet");
    let now = fs::read_to_string(&launcher).unwrap();
    assert_ne!(
        now, stale,
        "a running fleet keeps the launcher it was given"
    );
    assert!(
        now.contains("apply_fleet_ceilings"),
        "the launcher installed is the embedded one, whole: {now:.120}"
    );

    // ---- and the doorway it started does not outlive the fixture ----
    //
    // **This is the producer** SKEIN-855 was looking for. `heal_fleet` reaches `ensure_fleet_door`
    // → `fleet::start_server`, which leaves a tmux server holding a loop that restarts
    // `server-doorway.py` for as long as that file exists — and nothing in this binary ever stopped
    // it. Every green `--test fleet_launch` run left one behind per fixture; the only reason they
    // were not there an hour later is that removing the fixture directory eventually took the
    // loop's own condition away with it, which is a fixture *directory* doing a teardown's job and
    // stops happening the moment a test fails and the directory is kept as evidence.
    //
    // **Presence, then absence, in that order.** An absence that was never a presence proves
    // nothing (SKEIN-833): asserted the other way round this passes on a `heal_fleet` that started
    // no server at all, which is the one outcome it must not be green about. `start_server` runs
    // `tmux new-session -d` through `own_sandbox(..).exec(..)` and returns once tmux has taken it,
    // so this is read straight out of `/proc` with nothing waited on.
    //
    // **And presence-then-absence was still not enough** (SKEIN-919). Until the teardown was
    // reordered, the absence below was green under a `kill-server` that had been replaced by
    // `list-sessions` — run, not reasoned about: 1 passed, in 0.53s. Two things made it so, and
    // both are now gone. The script removal came first, so the loop's own exit condition was
    // already false; and the `SIGKILL` sweep ran before anything was counted, so it laundered the
    // result of a kill that had done nothing. `stop_fixture` is called by hand below for exactly
    // that reason: it returns what the kill achieved, measured before either could interfere.
    //
    // **What makes each assertion fail**, run rather than reasoned about:
    //
    // * presence: nothing needed — `heal_fleet` not reaching `start_server` empties it, which is
    //   the state this suite was in before SKEIN-855.
    // * `script_was_there`: moving the `remove_file` back above the `kill-server` in
    //   `stop_fixture`. It is what keeps the next one honest.
    // * `left` empty: `kill-server` → `list-sessions`. Fails in 5.54s naming three pids — the tmux
    //   server, the supervisor shell, and the python holding 7878 — against 23.83s green for the
    //   whole binary. The `Scratch` drop that follows the panic still cleaned the fixture up
    //   through the sweep, and `node tests/ui/harness/leaks.mjs` exited 0 after it, which is the
    //   evidence that the sweep is a fallback and this assertion is about the mechanism.
    let running = fixture_processes(&root);
    assert!(
        running
            .iter()
            .any(|(_, argv)| argv.contains("server-doorway.py")),
        "`heal_fleet` came back without leaving a doorway supervisor running, so the absence \
         asserted below would be about a process that was never started. Running out of {}: {:#?}",
        root.display(),
        running
    );
    // Dropped in the order the `Scratch` doc comment requires — the pins first, so no variable is
    // left naming a directory that is already gone — and by hand rather than at the closing brace,
    // because the point is to read `/proc` on the far side of the teardown while this test can
    // still fail about it.
    let fixture = root.to_path_buf();
    drop(pins);

    // ---- the teardown, called by hand, against a fixture that is STILL ON DISK ----
    //
    // Which is the shape of the panic path — the one that keeps the directory — without needing a
    // panic to produce it, and the only shape in which the kill can be measured at all. `drop(root)`
    // below runs the same teardown a second time and then removes the directory; it is idempotent,
    // and the assertions in between are what this call is for.
    let stopped = stop_fixture(&fixture);
    // **`left` cannot be read without this** (SKEIN-919, and SKEIN-920 before it). The script is the
    // supervisor loop's own `while [ -f … ]` exit condition: gone, the loop ends itself and an empty
    // count afterwards says nothing whatever about the kill. Measured, not argued — under the order
    // this replaces, `kill-server` swapped for `list-sessions` left this whole suite green.
    assert!(
        stopped.script_was_there,
        "{} was already gone when the teardown finished counting, so the supervisor loop's exit \
         condition was FALSE for some of the wait and the count below would be empty for a \
         `kill-server` that does nothing at all. Whatever moved the removal above the kill in \
         `stop_fixture` has to be undone, not accommodated",
        fixture.join("boxes/.skein/server-doorway.py").display()
    );
    // And now the kill, and only the kill: the script is still on disk, so the loop could not have
    // ended itself, and the `SIGKILL` sweep has not run yet, so it cannot have laundered this.
    assert!(
        stopped.left.is_empty(),
        "the teardown ran against a fixture that is still on disk, with the doorway script still \
         under it — so the loop could not have ended itself — and {} process(es) outlived its \
         `kill-server` by {KILL_WINDOW:?}. Nothing but that kill can end the loop while its exit \
         condition holds, so on the path where the directory is KEPT, which is every failing test, \
         this supervisor holds the cockpit's port and restarts a python for ever (SKEIN-645): \
         {:#?}",
        stopped.left.len(),
        stopped.left
    );

    drop(root);
    let left = fixture_processes(&fixture);
    assert!(
        left.is_empty(),
        "the fixture is gone and these are still running out of it. A box whose root is deleted \
         under it is the state nothing else in this suite can observe, and the count that would \
         report it is `node tests/ui/harness/leaks.mjs`, not this suite's own result: {left:#?}"
    );
}

/// Wait until `fleet_boxes` serves what `sbx ls` is now saying about the fleet sandbox.
///
/// Changing what sbx says is not the same as skein seeing it. The answer is gated (1.5s), and once
/// it ages out the *first* caller is handed the remembered one while the refresh runs behind — that
/// is deliberate, so a wedged daemon makes the cockpit stale instead of making it stop, and it is
/// exactly why a test cannot assume its next call reflects the change it just made. Bounded, so a
/// gate that never comes round fails the assertion it was called for rather than hanging the suite.
fn await_ls(want: Option<Liveness>) {
    for _ in 0..60 {
        if fleet_boxes()
            .unwrap_or_default()
            .iter()
            .any(|b| b.name == FLEET && b.live == want)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// -------------------------------------------------------------------------------------------------
// The launcher's `sudo` calls refuse rather than wait (SKEIN-555)
// -------------------------------------------------------------------------------------------------
//
// The same defect these tests now cover on the production side was fixed on the test side first,
// and the note at the top of `a_box_lives_and_dies_inside_the_fleet_sandbox` describes what it
// looked like: a probe spelled `sudo mkdir -p /sys/fs/cgroup/skein`, with no `-n`, blocked on a
// password prompt with `cargo test`'s output captured and nothing on screen to answer. The launcher
// is worse off than the test was, because nobody is at a terminal at all when it runs.

/// The bytes `fleet::install_launcher` puts into a sandbox, read the way `src/fleet.rs` reads them.
const LAUNCHER: &str = include_str!("../src/box-session.sh");

/// Every genuine `sudo` invocation in the launcher, as `(line number, the command from `sudo` on)`.
///
/// Derived, and it has to be more than a grep. `box-session.sh` is the file where the reasoning
/// lives, so most of its `sudo`s are prose: `grep -cE 'sudo ' src/box-session.sh` answers 27, of
/// which 23 are comments about the shim and about the sandbox's own sudo, and 3 are inside heredocs
/// the shim prints to a box's owner — one of them the line `sudo apt-get install <package>` in the
/// help text, which is advice to a human and not something this script runs. Four more occurrences
/// are `sudo` as a *word* rather than as the command: `command -v sudo`, and the shim's own path
/// `"$root/bin/sudo"`. A check that could not tell those apart would be the fragile grep
/// `CONTRIBUTING.md` calls worse than no check.
///
/// So: heredoc bodies are skipped whole, whole-line and trailing comments are cut, and an
/// occurrence counts only where `sudo` stands in command position — at the start of a line, or
/// after one of the operators or keywords that begins a new command.
fn launcher_sudo_calls(script: &str) -> Vec<(usize, String)> {
    let mut calls = Vec::new();
    let mut here: Option<String> = None;
    for (i, raw) in script.lines().enumerate() {
        if let Some(term) = &here {
            if raw.trim() == term.as_str() {
                here = None;
            }
            continue;
        }
        if raw.trim_start().starts_with('#') {
            continue;
        }
        let code = code_before_comment(raw);
        for at in sudo_command_positions(&code) {
            calls.push((i + 1, code[at..].trim_end().to_string()));
        }
        here = heredoc_terminator(&code);
    }
    calls
}

/// The line with any trailing `#` comment cut off, quoting respected.
///
/// Respected because the launcher writes shell inside single quotes all day —
/// `sudo -n sh -c 'echo "+memory +pids +cpu" > "$1/cgroup.subtree_control"'` — and a `#` inside one
/// of those is text.
fn code_before_comment(line: &str) -> String {
    let (mut in_single, mut in_double) = (false, false);
    for (j, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => {
                let starts_a_word = j == 0 || matches!(line.as_bytes()[j - 1], b' ' | b'\t');
                if starts_a_word {
                    return line[..j].to_string();
                }
            }
            _ => {}
        }
    }
    line.to_string()
}

/// The terminator a line opens a heredoc with, if it opens one: `<<'SHIM'`, `<<SKEIN_MOUNTS`.
fn heredoc_terminator(code: &str) -> Option<String> {
    let at = code.find("<<")?;
    let rest = code[at + 2..].trim_start_matches('-').trim_start();
    let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let first_is_a_name_start = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    first_is_a_name_start.then_some(name)
}

/// Where in `code` the word `sudo` is the command being run, rather than a word inside one.
fn sudo_command_positions(code: &str) -> Vec<usize> {
    const OPENS_A_COMMAND: [char; 5] = [';', '&', '|', '(', ')'];
    const KEYWORDS: [&str; 8] = ["if", "then", "else", "elif", "do", "while", "until", "!"];
    let mut found = Vec::new();
    for (at, _) in code.match_indices("sudo") {
        // A command needs an argument after it, and the word has to end here: `sudo_real=` and
        // `"$root/bin/sudo"` are both rejected by this line alone.
        if !code[at + 4..].starts_with([' ', '\t']) {
            continue;
        }
        let before = code[..at].trim_end();
        let is_command = match before.chars().last() {
            None => true,
            Some(c) if OPENS_A_COMMAND.contains(&c) => true,
            // `command -v sudo` is rejected here: the word in front is `-v`, not a keyword.
            Some(_) => KEYWORDS.contains(&before.rsplit([' ', '\t']).next().unwrap_or("")),
        };
        if is_command {
            found.push(at);
        }
    }
    found
}

/// Sudo's own options, which end at the first word that is not one — everything after that belongs
/// to the command sudo is being asked to run, and `-n` there would be an argument to `mkdir`.
fn sudo_own_options(call: &str) -> Vec<&str> {
    call.split_whitespace()
        .skip(1)
        .take_while(|w| w.starts_with('-'))
        .collect()
}

/// Every `sudo` the launcher runs passes `-n`, so a sudo that wants a password refuses instead of
/// asking one nobody can answer.
///
/// What makes this fail: take the `-n` off any one of them. It is not a grep for the string — the
/// derivation above tells a call from the 26 mentions of `sudo` in this file's prose — and it
/// refuses to be green on an empty derivation, which is the failure mode of the leaked-process
/// check `CLAUDE.md` describes: a pattern that has never matched anything looks exactly like a
/// pattern that matches nothing.
#[test]
fn every_sudo_the_launcher_runs_is_non_interactive() {
    let calls = launcher_sudo_calls(LAUNCHER);
    assert!(
        !calls.is_empty(),
        "no `sudo` call was derived from box-session.sh at all — the launcher cannot have stopped \
         using sudo, so this is the derivation broken, and a check that derives nothing would \
         otherwise pass for ever"
    );
    for (line, call) in &calls {
        let opts = sudo_own_options(call);
        assert!(
            opts.contains(&"-n") || opts.contains(&"--non-interactive"),
            "box-session.sh:{line} runs sudo without -n, so on a machine whose sudo wants a \
             password this blocks on a prompt nobody is watching and the box never starts: {call}"
        );
    }

    // And the file's own count of them, in the note above `export PATH=`, is checked against the
    // derivation rather than trusted: that sentence is the reason the fixed PATH covers what it
    // covers, and it is the kind of prose that goes stale silently.
    let spelled = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
    ];
    let n = calls.len();
    let word = spelled.get(n).copied().unwrap_or("");
    assert!(
        !word.is_empty() && LAUNCHER.contains(&format!("the {word} `sudo` calls")),
        "the launcher has {n} sudo calls and the note above its `export PATH=` does not say so; \
         both have to move together, because that note is the argument for the fixed PATH"
    );
}

/// Drive the real function text with a `sudo` that wants a password, and watch it not wait.
///
/// The string check above cannot see the difference between `sudo -n mkdir` and a `-n` that landed
/// somewhere useless, so this runs `ensure_container_cgroup` — lifted verbatim out of the launcher,
/// not retyped — against a stub `sudo` that blocks on a read exactly where the real one blocks on a
/// password.
///
/// **The named change that makes it fail is `sudo -n` → `sudo` in that function, and the test makes
/// that change itself.** The second half runs the same body with the `-n`s stripped and asserts it
/// does NOT finish. So a run where the stub could not have blocked — an stdin already at EOF, a
/// stub that never got on PATH — fails on the control rather than passing on both halves, which is
/// the only way a timing assertion like this can be trusted.
#[test]
fn the_launchers_sudo_never_waits_for_a_password() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::temp("skein-sudo-it");
    let body = shell_function(LAUNCHER, "ensure_container_cgroup");
    // That the function still makes sudo calls — and deliberately NOT that they carry `-n`, which
    // is the property under test. Asserting the `-n` here made a dropped one fail this line in
    // 0.00s instead of failing on the hang, so the half of the test that actually drives a shell
    // never ran.
    assert_eq!(
        launcher_sudo_calls(&body).len(),
        2,
        "ensure_container_cgroup no longer makes the two sudo calls this test drives: {body}"
    );

    let stub_dir = scratch.path().join("bin");
    fs::create_dir_all(&stub_dir).unwrap();
    let stub = stub_dir.join("sudo");
    fs::write(
        &stub,
        // `printf` and not `echo`, because `echo -n` is the shell eating the very flag under test.
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$STUB_LOG"
if [ "$1" = -n ]; then
  # The real one refuses here, with status 1 and "sudo: a password is required". 0, so that the
  # caller carries on to its remaining calls instead of stopping at the first `|| return 0` — the
  # point is what it does NOT do, which is wait.
  exit 0
fi
: > "$STUB_BLOCKED"
read -r _password
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

    // ---- the launcher as it stands: every call refuses at once ----
    let log = scratch.path().join("calls.log");
    let blocked = scratch.path().join("blocked");
    let mut child = spawn_with_stub(&body, &stub_dir, &log, &blocked);
    let finished = wait_for(&mut child, Duration::from_secs(20));
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        finished.on_its_own(),
        "ensure_container_cgroup {finished}. What this test is about is that it does not WAIT for \
         a password, which in a real launch is a box start that never finishes. Calls so \
         far: {calls:?}"
    );
    assert!(
        !blocked.exists(),
        "a call reached the stub without -n, so the stub blocked: {calls:?}"
    );
    // Both sudo lines, for both cgroups — so the body really ran rather than falling out early.
    let seen: Vec<&str> = calls.lines().collect();
    assert_eq!(
        seen.len(),
        4,
        "expected two sudo calls for each of the two cgroups: {seen:?}"
    );
    assert!(
        seen.iter().all(|c| c.starts_with("-n ")),
        "a call reached sudo with -n somewhere other than first among sudo's own options: {seen:?}"
    );

    // ---- the control: the same body with the flag taken off must hang ----
    let log = scratch.path().join("sabotage.log");
    let blocked = scratch.path().join("sabotage-blocked");
    let without = body.replace("sudo -n ", "sudo ");
    assert_ne!(without, body, "the sabotage did not apply");
    let mut child = spawn_with_stub(&without, &stub_dir, &log, &blocked);
    // Wait for the stub to say it is blocking, so that a slow box cannot pass this by being slow.
    let reached = poll_for(|| blocked.exists(), Duration::from_secs(20));
    assert!(
        reached,
        "the stub sudo was never reached without -n, so the control proves nothing about the half \
         above: {:?}",
        fs::read_to_string(&log).unwrap_or_default()
    );
    // **The assertion that used to accuse the wrong thing** (SKEIN-901). Written as
    // `assert!(!wait_for(..))` it says "a sudo with no -n returned anyway" about a child that was
    // SIGKILLed by a sibling test's teardown just as readily as about one that really did return,
    // and those are opposite diagnoses: the first is somebody else's bug four directories away and
    // the second is this control being worthless. Naming which ending happened costs nothing — the
    // `ExitStatus` is already in hand.
    let blocking = wait_for(&mut child, Duration::from_secs(2));
    assert!(
        matches!(blocking, Ended::No),
        "the control child {blocking}. If it exited, a sudo with no -n returned anyway, this stub \
         cannot block, and the first half of this test passes for a reason that has nothing to do \
         with -n"
    );
    // Closing its stdin is the EOF the stub's `read` is waiting for, so nothing is left running.
    drop(child.stdin.take());
    let unwound = wait_for(&mut child, Duration::from_secs(20));
    assert!(
        unwound.on_its_own(),
        "the control {unwound} after its stdin closed, rather than unwinding"
    );
}

/// A top-level shell function lifted out of a script, matched at its own indentation (column 0).
fn shell_function(script: &str, name: &str) -> String {
    let open = format!("{name}() {{");
    let mut body = String::new();
    for line in script.lines() {
        if body.is_empty() && !line.starts_with(&open) {
            continue;
        }
        body.push_str(line);
        body.push('\n');
        if !body.is_empty() && line == "}" {
            return body;
        }
    }
    panic!("`{name}` is no longer a top-level function in box-session.sh");
}

/// Run a shell function body with `sudo` resolving only to the stub, and stdin a pipe nobody writes
/// to — which is the thing a password prompt waits on.
fn spawn_with_stub(body: &str, stub_dir: &Path, log: &Path, blocked: &Path) -> std::process::Child {
    // Named absolutely, because the PATH below is the stub directory alone — a `bash` looked up on
    // it would not be found either, which is how this first failed.
    let bash = ["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .expect("a bash to run the launcher's own function body with");
    Command::new(bash)
        .arg("-c")
        .arg(format!("{body}\nensure_container_cgroup\n"))
        // The stub directory ALONE: there is no path by which the machine's real sudo can be
        // reached from here, which is what makes this safe to run anywhere.
        .env("PATH", stub_dir)
        .env("STUB_LOG", log)
        .env("STUB_BLOCKED", blocked)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("bash")
}

/// How a child ended within `budget`, or that it did not — because "still running" and "killed" and
/// "returned" are three different findings and a `bool` reports two of them as one (SKEIN-901).
///
/// A child that EXITED because the thing under test let it, and a child that was KILLED by something
/// else on this box, are indistinguishable to "is it still running" — and the assertion below names
/// only the first. When the second happened here, the message sent the reader to look at `sudo -n`
/// and at box load; the cause was a SIGKILL from a sibling test's teardown four directories away,
/// and it cost about an hour of looking for a load flake that does not exist.
///
/// **The distinction is free**, which is the whole argument for making it: the `ExitStatus` is
/// already in hand, and `ExitStatusExt::signal()` is `Some(9)` for a killed child and `None` for one
/// that exited on its own. Nothing is waited for that was not already waited for.
#[derive(Debug)]
enum Ended {
    /// Still running when the budget ran out.
    No,
    /// Ran to completion on its own, with this status code.
    Exited(i32),
    /// Ended by a signal. **Something outside this test killed it**, so whatever the test was about
    /// to conclude from the ending is about that killer and not about the code under test.
    Killed(i32),
}

impl Ended {
    /// Did it end *of its own accord* — which is what every caller that wants "it returned" means,
    /// and what a bare "is it still running" quietly answers `true` to for a corpse.
    fn on_its_own(&self) -> bool {
        matches!(self, Ended::Exited(_))
    }
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ended::No => write!(f, "was still running when the budget ran out"),
            Ended::Exited(code) => write!(f, "exited on its own with status {code}"),
            Ended::Killed(signal) => write!(
                f,
                "was KILLED by signal {signal} — something outside this test ended it, and nothing \
                 here is evidence about the behaviour under test. On this box that has meant a \
                 sibling test's teardown: see `fixture_processes`, which exempts a live descendant \
                 of this process for exactly that reason"
            ),
        }
    }
}

fn wait_for(child: &mut std::process::Child, budget: Duration) -> Ended {
    use std::os::unix::process::ExitStatusExt;
    let mut status = None;
    poll_for(
        || match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                true
            }
            _ => false,
        },
        budget,
    );
    match status {
        None => Ended::No,
        // `signal()` first and not `code()` first: a killed child's `code()` is `None`, so a match
        // written the other way round reports every kill as an unknown exit status.
        Some(s) => match s.signal() {
            Some(signal) => Ended::Killed(signal),
            None => Ended::Exited(s.code().unwrap_or(-1)),
        },
    }
}

/// **A child that was killed does not read as one that returned** (SKEIN-901).
///
/// Both endings, because one alone proves nothing. Reporting every ended child as `Killed` would
/// satisfy the first half and fail the second; reporting every one as `Exited` — which is what the
/// `bool` this replaced effectively did, since it said only "not running any more" — fails the
/// first. And each child is asserted to be RUNNING before it is ended, so the ending being reported
/// on is the one this test caused rather than a spawn that never happened (SKEIN-833).
///
/// `sleep` with a bounded argument rather than an unbounded blocker, so a failure between the spawn
/// and the kill cannot leave this test's own orphan behind.
#[test]
fn a_killed_child_is_reported_as_killed_and_not_as_having_returned() {
    let mut killed = Command::new("sleep")
        .arg("400")
        .spawn()
        .expect("a child to kill");
    let running = wait_for(&mut killed, Duration::from_millis(200));
    let killed_pid = killed.id();
    killed.kill().expect("SIGKILL it by pid — never a pattern");
    let after_kill = wait_for(&mut killed, Duration::from_secs(20));

    let mut returns = Command::new("/bin/true")
        .spawn()
        .expect("a child that ends");
    let after_exit = wait_for(&mut returns, Duration::from_secs(20));

    assert!(
        matches!(running, Ended::No),
        "pid {killed_pid} {running} before anything killed it, so what is asserted below is not \
         about a kill at all"
    );
    assert!(
        matches!(after_kill, Ended::Killed(9)),
        "a child this test SIGKILLed itself came back as `{after_kill:?}`. A reader of that is \
         sent to look at the code under test for an ending that something outside it caused"
    );
    assert!(
        !after_kill.on_its_own(),
        "a killed child answers `on_its_own`, so every caller that asks whether the thing under \
         test returned is answered `yes` by a corpse: {after_kill:?}"
    );
    assert!(
        matches!(after_exit, Ended::Exited(0)) && after_exit.on_its_own(),
        "a child that ran to completion came back as `{after_exit:?}`, so the kill above is \
         reported as a kill only because nothing is ever reported as an exit"
    );
    assert!(
        after_kill.to_string().contains("KILLED by signal 9")
            && !after_kill.to_string().contains("exited"),
        "the message a failing assertion would print does not say which ending it saw: {after_kill}"
    );
}

fn poll_for(mut done: impl FnMut() -> bool, budget: Duration) -> bool {
    let until = std::time::Instant::now() + budget;
    loop {
        if done() {
            return true;
        }
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What is actually on a box's own terminal, with the wrapping taken out.
///
/// `capture-pane` reports the pane as the pty holds it, so an 80-column wrap puts a line break in
/// the middle of a sentence — collapsing the whitespace is what stops that from deciding whether an
/// assertion passes. `-J` joins what tmux itself knows is one wrapped line; the collapse covers the
/// rest.
fn pane_text(name: &str) -> String {
    let out = Command::new("tmux")
        .args([
            "-S",
            &box_sock(name),
            "capture-pane",
            "-p",
            "-J",
            "-t",
            "skein-shell",
        ])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The pane, once it has had a chance to print — bounded, and returning whatever it has either way.
///
/// `session_script` returns as soon as tmux has the session and the anchor pid can be read, which
/// is not quite the same instant as the pane's first process having written anything. Waiting on
/// the post-condition rather than sleeping a fixed amount is the same rule `anchor_gone` above is
/// written to; the text is returned unconditionally so the assertion, not this helper, is what
/// fails and says what it wanted.
fn pane_once_it_speaks(name: &str, want: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let text = pane_text(name);
        if text.contains(want) || std::time::Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A box skein cannot match to a repository is **refused**, and once allowed it **says so where a
/// person is** — on its own terminal and on the stderr of the command that started it (SKEIN-846,
/// SKEIN-836).
///
/// **Why this is one test and not four.** Every step consumes the one before it: there is no pane
/// to read until a box has started, no placement to attach to until `start_box` has recorded one,
/// and no way to prove the refusal is a refusal *of this condition* rather than of everything
/// unless a covered box comes up in the same fixture, from the same call, moments later. An absence
/// that was never a presence proves nothing.
///
/// **What would make each assertion fail — named before it was written, and each one then made to
/// fail by doing exactly this:**
///
///   * *the refusal* — deleting the `refuse_if_uncovered(name)?` line from `fleet::start_box_inner`.
///     The unmatched box then starts, and `.expect_err` finds an `Ok`.
///   * *the covered box still starting* — making `box_exposure` return `Uncovered` for everything,
///     or dropping the `uncovered_is_allowed` arm so the permission is never read. Either way the
///     second and third starts fail and the first assertion goes on passing, which is the whole
///     reason this half is here.
///   * *surface 1, the box's own terminal* — deleting the `pane_cmd=(sh -c …)` wrapper from
///     `box-session.sh`. The banner is still written, still correct, and the pane is empty: exactly
///     the state this item found, reproduced.
///   * *surface 2, the command that started it* — deleting the `printf 'SKEIN_NOTICE %s\n'` from
///     `box-session.sh` (leaving the `echo … >&2` that was there for months), or deleting the
///     `say_what_the_launcher_said` call from `fleet::ensure_box_session`. The real `skein attach`
///     run below then prints nothing about the cover, which is what it did before this.
///
/// None of those are assertions about a string being present in a file. `cockpit.rs`'s
/// `the_workshop_switch_says_what_it_grants` is the one that checks the *wording*, and it says so.
#[test]
fn an_uncovered_box_is_refused_until_it_is_allowed_and_then_says_so_where_someone_is_looking() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") || !have("git") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let root = scratch_named("cover");
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);
    let sandbox_home = sandbox_home_with_agent(&root);

    // `demo-…` matches the repo registered below by the longest-id-prefix rule `repo_for_box` uses;
    // `adrift-…` matches nothing, which is the entire difference between them. Both are cloned from
    // the same remote and started by the same call, so nothing else can explain a difference.
    let covered = "demo-cover";
    let adrift = "adrift-cover";

    let mut pins = env_pins();
    pins.set("SKEIN_WARDEN", "127.0.0.1:1");
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"))
    .set("SKEIN_RUNTIME_PACKAGES", "")
    .set("HOME", &sandbox_home)
    .set("SKEIN_LS_CMD", format!("echo '[{{\"name\":\"{FLEET}\"}}]'"));

    let store = root.join("store");
    fs::create_dir_all(&store).unwrap();
    ensure_store(&store).expect("seed the store");
    ensure_probe_in(&store).expect("seed the store's scripts");
    let repo = Repo {
        read_prs: false,
        id: "demo".into(),
        source: remote.clone(),
        store: store.to_string_lossy().into_owned(),
        agent: "claude".into(),
        plane_project: String::new(),
        sync_connection: String::new(),
        review_queue: true,
        sync_gateway_url: String::new(),
        ..Default::default()
    };
    save_repos(std::slice::from_ref(&repo)).expect("register the repo");
    let mut config = load_config();
    config.fleet_sandbox = FLEET.into();
    save_config(&config).expect("turn the fleet on");
    // The cover binds each box's own state directory back through the tmpfs it lays over their
    // parent, and bwrap refuses a bind whose source does not exist — so a fixture that skipped this
    // would fail at the namespace rather than at the thing under test.
    for name in [covered, adrift] {
        fs::create_dir_all(box_state(name)).unwrap();
    }

    // ---- 1. refused, before anything is built ----
    let refusal = start_box(
        adrift,
        &repo,
        "feat/cover",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect_err("a box skein cannot cover was started with nobody having chosen that");
    assert!(
        refusal.contains("UNCOVERED"),
        "the refusal does not name the condition it is refusing on: {refusal}"
    );
    for step in ["`skein repos`", "`skein add", "--uncovered"] {
        assert!(
            refusal.contains(step),
            "the refusal never mentions `{step}`, so it names a wall and no way over or around \
             it — which is the one thing a refusal must not do: {refusal}"
        );
    }
    assert!(
        !Path::new(&format!("{}/tree", box_root(adrift))).exists(),
        "the refusal came after the clone, so a box nobody may start now has a checkout"
    );

    // ---- 2. and a covered box, from the same call, is not ----
    start_box(
        covered,
        &repo,
        "feat/cover",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect("a box whose name matches a repository must still start");

    // ---- 3. allowed, deliberately, and then it starts ----
    // `skein start <box> --uncovered` writes exactly this, in `src/bin/skein.rs`.
    skein::fleet::allow_uncovered(adrift, true).expect("record the permission");
    start_box(
        adrift,
        &repo,
        "feat/cover",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect("the permission was recorded and the refusal still stood");
    assert!(
        shared_record(adrift).is_some(),
        "the allowed box reported success without leaving a placement"
    );

    // ---- 4. surface 1: the box's own terminal ----
    let said = pane_once_it_speaks(adrift, "UNCOVERED");
    assert!(
        said.contains(&format!("{adrift} came up UNCOVERED")),
        "nothing on the box's own terminal says it came up uncovered, so anyone who attaches or \
         opens a shell in it sees a box exactly like every other one. Pane: {said:?}"
    );
    assert!(
        said.contains("every other repo's store and work tree"),
        "the banner reached the pane with its reach edited out of it: {said:?}"
    );
    let quiet = pane_text(covered);
    assert!(
        !quiet.contains("UNCOVERED") && !quiet.contains("WORKSHOP"),
        "a covered box is told it came up uncovered, so the banner is firing on every box and \
         means nothing. Pane: {quiet:?}"
    );

    // ---- 5. surface 2: the stderr of the command a person actually ran ----
    // The real binary, not this process: the thing under test is that `skein attach` puts the
    // launcher's words in front of whoever typed it, and this process's `eprintln!` is captured by
    // the test harness where nothing can read it. The session is ended first so that attaching has
    // to relaunch — a live session is a no-op and would prove nothing.
    let anchor = shared_record(adrift).unwrap().ns_pid;
    own_sandbox(FLEET)
        .exec(
            &format!("tmux -S {} kill-server", box_sock(adrift)),
            Duration::from_secs(30),
        )
        .expect("end the session so the attach has to start one");
    anchor_gone(anchor);
    let attach = Command::new(env!("CARGO_BIN_EXE_skein"))
        .args(["attach", adrift])
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("SKEIN_HOME", root.join("skein"))
        .env("SKEIN_FLEET_ROOT", root.join("boxes"))
        .env("SKEIN_RUNTIME_PACKAGES", "")
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("HOME", &sandbox_home)
        .env("SKEIN_LS_CMD", format!("echo '[{{\"name\":\"{FLEET}\"}}]'"))
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run the real skein");
    // The attach itself has nowhere to attach to — there is no terminal on this pipe — and that is
    // not what is being read. What is being read is everything `skein` said on its way there.
    let told = String::from_utf8_lossy(&attach.stderr).to_string();
    assert!(
        told.contains("came up UNCOVERED"),
        "`skein attach {adrift}` relaunched an uncovered box and told the person nothing about it \
         — which is what every launch did while `box-session.sh` wrote this to a stderr that is \
         piped and dropped on success. Its whole stderr was: {told}"
    );
    // And the permission persisted, which is what keeps this from stranding anybody: nobody typed
    // `--uncovered` here.
    assert!(
        !told.contains("so skein has not started it"),
        "the permission did not outlive the command that gave it, so every attach of this box \
         meets a refusal it cannot answer from inside the server: {told}"
    );

    // ---- 6. and nothing this started is left running ----
    for name in [covered, adrift] {
        let anchor = shared_record(name).map(|r| r.ns_pid);
        stop_box(name).expect("stop the box");
        if let Some(anchor) = anchor {
            anchor_gone(anchor);
        }
        destroy_box(name).expect("destroy the box");
    }
}

// -------------------------------------------------------------------------------------------------
// Seeding a login and seeding "you have logged in before" are one act (SKEIN-957)
// -------------------------------------------------------------------------------------------------
//
// Every new box in the owner's fleet opened on Claude Code's login screen while holding a working
// credential, because the screen is gated on `hasCompletedOnboarding` in `~/.claude.json` and skein
// wrote that key nowhere. The launcher writes it now, next to the credential merge, and what is
// asserted below is the INVARIANT rather than one file's contents: whatever the launcher decides a
// box's login is, a box that HAS one must not be asked to onboard. A test pinned to an example pair
// of JSON blobs would stay green under a seed path that learned to forget the flag somewhere else.

/// A block of the launcher, lifted from the first line beginning `from` up to and including the
/// first line after it that is exactly `to`.
///
/// Read out of `box-session.sh` rather than copied, so a change to the launcher is a change to what
/// these tests run — a copy would keep passing against the version it was written from. It panics
/// rather than returning an empty block: a landmark that has moved must fail loudly here, not
/// quietly hand the shell nothing to run and report that nothing went wrong.
fn launcher_block(from: &str, to: &str) -> String {
    let lines: Vec<&str> = LAUNCHER.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with(from))
        .unwrap_or_else(|| {
            panic!("box-session.sh has no line beginning `{from}` any more, so this harness is not lifting what it names")
        });
    let end = lines[start + 1..]
        .iter()
        .position(|l| *l == to)
        .map(|i| start + 1 + i)
        .unwrap_or_else(|| {
            panic!("the block beginning `{from}` in box-session.sh does not end at a line `{to}`")
        });
    lines[start..=end].join("\n")
}

/// The launcher's own `login_life` — the judgement that decides whether a file is a login at all.
///
/// Asked by RUNNING the launcher's copy rather than by reading the JSON here: a second opinion
/// written in Rust would be a second spelling of the rule, which is how the credential merge and the
/// host's heal once elected opposite winners on the same five files.
fn launcher_says_there_is_a_login(credential: &Path) -> bool {
    let block = launcher_block("login_life() {", "}");
    assert!(
        block.contains("refreshTokenExpiresAt"),
        "the lifted `login_life` no longer asks about an expired refresh token, so this harness is \
         running something other than the launcher's judgement of what a login is"
    );
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\n{block}\nlogin_life '{}'",
            credential.display()
        ))
        .output()
        .expect("bash runs the launcher's login test")
        .status
        .success()
}

/// Run the launcher's onboarding block against one fixture home, exactly as a box start runs it.
fn run_onboarding_block(home: &Path) -> std::process::Output {
    let block = launcher_block("if command -v python3 >/dev/null 2>&1 && login_life", "fi");
    assert!(
        block.contains("hasCompletedOnboarding"),
        "the lifted block no longer writes the key the onboarding screen is gated on, so this \
         harness is running something that cannot answer the question it was written for"
    );
    let life = launcher_block("login_life() {", "}");
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\nhome='{}'\n{life}\n{block}",
            home.display()
        ))
        .output()
        .expect("bash runs the launcher's onboarding block")
}

/// Every box home under `fleet_root` the launcher itself would say carries a login.
fn homes_carrying_a_login(fleet_root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(fleet_root) else {
        return found;
    };
    let mut boxes: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    boxes.sort();
    for at in boxes {
        let home = at.join("home");
        if launcher_says_there_is_a_login(&home.join(".claude/.credentials.json")) {
            found.push(home);
        }
    }
    found
}

/// What `~/.claude.json` says about onboarding, for a home that has one.
fn onboarding_flag(home: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(home.join(".claude.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    parsed.get("hasCompletedOnboarding").cloned()
}

/// A box that ends up with a login is never asked to onboard — whatever its `~/.claude.json` was.
///
/// The antecedent is the launcher's own `login_life`, so the case table below says what is on disk
/// and never what the answer should be: swap a credential for a husk, or for a file whose refresh
/// token expired in 2001, and the expectation follows the launcher instead of contradicting it.
///
/// **What makes each assertion fail**, planted and watched before this was believed:
///
///   * deleting `data["hasCompletedOnboarding"] = True` from the launcher — the invariant fails for
///     every case that carries a login;
///   * replacing the read-modify-write with `data = {}` before it, i.e. writing the file from a
///     template — `a key that was there is gone` fails, naming the key;
///   * making the unparseable case write anyway (`except ValueError: data = {}`) — `left exactly as
///     it was` fails on the byte comparison;
///   * dropping `&& login_life …` from the condition, so the flag is written with no credential —
///     `a box with no login must still be asked` fails.
#[test]
fn a_box_that_has_a_login_is_never_asked_to_onboard() {
    if !have("python3") {
        return skip("the launcher writes this key with python3, and there is none here");
    }
    // A live login, a husk a logout leaves behind, and a credential whose refresh token died in
    // 2001 — the last two are files, and neither is a login.
    const LOGIN: &str = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#;
    const HUSK: &str = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":""}}"#;
    const SPENT: &str = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":1000000000000}}"#;
    // `(case, what is in .claude/.credentials.json, what is in .claude.json)`. `None` is a file
    // that is not there at all, which is what a brand-new box's home looks like.
    let cases: [(&str, Option<&str>, Option<&str>); 10] = [
        ("no .claude.json at all", Some(LOGIN), None),
        ("an empty .claude.json", Some(LOGIN), Some("")),
        (
            "a used one, with a record in it",
            Some(LOGIN),
            Some(r#"{"projects":{"/w":{"history":["a turn"]}},"userID":"u","numStartups":46}"#),
        ),
        (
            "one that already says so",
            Some(LOGIN),
            Some(r#"{"hasCompletedOnboarding":true,"userID":"u"}"#),
        ),
        (
            "one that says the opposite",
            Some(LOGIN),
            Some(r#"{"hasCompletedOnboarding":false,"userID":"u"}"#),
        ),
        ("one that is not JSON", Some(LOGIN), Some("not json at all")),
        (
            "one that is JSON but not an object",
            Some(LOGIN),
            Some("[1, 2, 3]"),
        ),
        ("a husk, not a login", Some(HUSK), Some(r#"{"userID":"u"}"#)),
        (
            "a login whose refresh token is spent",
            Some(SPENT),
            Some(r#"{"userID":"u"}"#),
        ),
        ("no credential at all", None, Some(r#"{"userID":"u"}"#)),
    ];

    // The same fixture family as every other test in this file, so the leak scan keeps deriving
    // one set of names from this binary. Nothing here starts a process; the block under test is
    // bash and a python that exits.
    let dir = scratch_named("onboard");
    let mut seen_with_a_login = 0;
    let mut seen_without = 0;
    for (n, (case, credential, before)) in cases.iter().enumerate() {
        let home = dir.join(format!("home-{n}"));
        fs::create_dir_all(home.join(".claude")).unwrap();
        if let Some(body) = credential {
            fs::write(home.join(".claude/.credentials.json"), body).unwrap();
        }
        if let Some(body) = before {
            fs::write(home.join(".claude.json"), body).unwrap();
        }

        let out = run_onboarding_block(&home);
        assert!(
            out.status.success(),
            "the launcher's onboarding block failed for `{case}`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        let after = fs::read_to_string(home.join(".claude.json")).ok();
        let has_login = launcher_says_there_is_a_login(&home.join(".claude/.credentials.json"));
        let was: Option<serde_json::Value> = before
            .filter(|b| !b.trim().is_empty())
            .map(|b| serde_json::from_str(b).unwrap_or(serde_json::Value::Null));
        let could_extend = was.as_ref().map(|v| v.is_object()).unwrap_or(true);

        if !has_login {
            seen_without += 1;
            // Skein does not fabricate "you have logged in before" for a box it handed nothing to:
            // that box genuinely has to log in, and hiding the screen it does that on would leave
            // it stranded in front of an agent that cannot answer.
            assert_eq!(
                after.as_deref(),
                *before,
                "a box with no login must still be asked to log in, and `{case}` had its \
                 ~/.claude.json written anyway"
            );
            continue;
        }
        seen_with_a_login += 1;

        if !could_extend {
            // `~/.claude.json` is Claude Code's file and holds the box's whole project record. A
            // shape skein cannot read is left alone and said out loud — the person meets one
            // onboarding prompt, which is where they are today, instead of losing the record.
            assert_eq!(
                after.as_deref(),
                *before,
                "`{case}` was rewritten from a template; a file skein cannot parse must be left \
                 exactly as it was"
            );
            assert!(
                said.contains(".claude.json"),
                "`{case}` was left alone in silence, so nobody can tell why the box still asks to \
                 onboard: {said:?}"
            );
            continue;
        }

        // THE INVARIANT.
        let text = after.expect("a home with a login must end up with a ~/.claude.json");
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("`{case}` left ~/.claude.json unreadable ({e}): {text}"));
        assert_eq!(
            parsed.get("hasCompletedOnboarding"),
            Some(&serde_json::Value::Bool(true)),
            "the launcher handed `{case}` a credential and left it needing to onboard, which is \
             the login screen on every new box: {text}"
        );
        // And it added a key rather than replacing a file: everything that was there is still
        // there, with the value it had.
        if let Some(serde_json::Value::Object(before)) = &was {
            for (key, value) in before {
                if key == "hasCompletedOnboarding" {
                    continue;
                }
                assert_eq!(
                    parsed.get(key),
                    Some(value),
                    "`{case}`: the key `{key}` was in ~/.claude.json and is not in what skein \
                     wrote back — this file carries the box's project history, and it is not \
                     skein's to replace: {text}"
                );
            }
        }
    }
    // Neither half of the table may quietly empty out: a run that saw no login proves nothing about
    // the invariant, and one that saw no husk proves nothing about the restraint beside it.
    assert!(
        seen_with_a_login >= 7 && seen_without == 3,
        "the launcher's own `login_life` read this table as {seen_with_a_login} logins and \
         {seen_without} non-logins, which is not the split these cases were written to have — \
         either a fixture has stopped being what it says it is, or the judgement moved"
    );
}

// -------------------------------------------------------------------------------------------------
// The tree skein cloned for a box is trusted in that box, and nothing else is (SKEIN-959)
// -------------------------------------------------------------------------------------------------
//
// With the login screen gone, a new box stopped next on Claude Code's workspace-trust dialog for its
// own tree. The launcher now records `projects[$tree].hasTrustDialogAccepted = true` in the box's
// `~/.claude.json`. The dialog is a security prompt — it guards against a repo's own hooks and MCP
// servers — so what is asserted is the narrow claim as well as the broad one: `$tree` is trusted,
// and no other `projects` key appears or changes, and nothing else in the file is lost.

/// Run the launcher's trust block against one fixture home and tree, as a box start runs it.
///
/// `login_life` is defined alongside, although the block does not call it today: if somebody puts
/// the block behind the login guard, the harness runs that guard as the launcher would rather than
/// failing on a missing function, and the no-login cases below say what went wrong.
fn run_trust_block(home: &Path, tree: &Path) -> std::process::Output {
    let block = launcher_block(
        "if command -v python3 >/dev/null 2>&1 && [ -d \"$tree\" ]",
        "fi",
    );
    assert!(
        block.contains("hasTrustDialogAccepted"),
        "the lifted block no longer writes the key Claude Code's trust dialog is gated on, so this \
         harness is running something that cannot answer the question it was written for"
    );
    let life = launcher_block("login_life() {", "}");
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\nhome='{}'\ntree='{}'\n{life}\n{block}",
            home.display(),
            tree.display()
        ))
        .output()
        .expect("bash runs the launcher's trust block")
}

/// What `~/.claude.json` says about trust for `path`, for a home that has one.
fn trust_of(home: &Path, path: &str) -> Option<serde_json::Value> {
    let text = fs::read_to_string(home.join(".claude.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    parsed
        .get("projects")?
        .get(path)?
        .get("hasTrustDialogAccepted")
        .cloned()
}

/// A fixture's `~/.claude.json` before the launch, given the tree's path and its parent's; `None`
/// is a file that is not there at all.
type BeforeOf = fn(&str, &str) -> Option<String>;

/// The tree skein cloned is trusted, whatever `~/.claude.json` was — and no other path is.
///
/// **What makes each assertion fail**, planted and watched before this was believed:
///
///   * deleting `entry["hasTrustDialogAccepted"] = True` from the launcher — `is not trusted`
///     fails for every case the block can extend;
///   * trusting a different path — `tree = os.path.dirname(tree)` (the parent), or `tree = "/"` —
///     `no other projects key may appear` fails, naming the key;
///   * clobbering the tree's existing entry rather than merging into it (`entry = {}` always) —
///     `the tree's own entry lost` fails, naming the field;
///   * replacing `projects` rather than adding to it (`projects = {}`) — `was there and is gone`
///     fails, naming the entry;
///   * writing over an unparseable file — `left exactly as it was` fails on the byte comparison;
///   * putting the block behind the login guard (`&& login_life …` in its condition) — the
///     no-credential cases fail `a box with no login must still have its tree trusted`.
#[test]
fn the_tree_skein_cloned_is_trusted_and_no_other_path_is() {
    if !have("python3") {
        return skip("the launcher writes this key with python3, and there is none here");
    }
    const LOGIN: &str = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#;
    let dir = scratch_named("trust");
    // The fixture's box root, tree and home, laid out as the launcher lays out a real one.
    let cases: [(&str, Option<&str>, BeforeOf); 12] = [
        ("no .claude.json at all", Some(LOGIN), |_, _| None),
        ("an empty .claude.json", Some(LOGIN), |_, _| {
            Some(String::new())
        }),
        (
            "a used one, with other projects in it — the parent and / among them",
            Some(LOGIN),
            |_, parent| {
                Some(format!(
                    r#"{{"projects":{{"/w":{{"history":["a turn"],"hasTrustDialogAccepted":false}},"{parent}":{{"hasTrustDialogAccepted":false}},"/":{{"allowedTools":[]}}}},"userID":"u","numStartups":46}}"#
                ))
            },
        ),
        (
            "one whose own entry for the tree carries a record and says not trusted",
            Some(LOGIN),
            |tree, _| {
                Some(format!(
                    r#"{{"projects":{{"{tree}":{{"hasTrustDialogAccepted":false,"allowedTools":["Bash(ls)"],"lastSessionId":"s","mcpServers":{{"m":{{"command":"x"}}}}}},"/w":{{"history":[]}}}},"userID":"u"}}"#
                ))
            },
        ),
        (
            "one that already trusts the tree",
            Some(LOGIN),
            |tree, _| {
                Some(format!(
                    r#"{{"projects":{{"{tree}":{{"hasTrustDialogAccepted":true,"lastCost":1.5}}}}}}"#
                ))
            },
        ),
        (
            "one with no projects key but a whole record otherwise",
            Some(LOGIN),
            |_, _| {
                Some(r#"{"hasCompletedOnboarding":true,"userID":"u","tipsHistory":{"t":3}}"#.into())
            },
        ),
        // The trust has nothing to do with the credential: a box with no login still gets it.
        ("no credential at all, and no .claude.json", None, |_, _| {
            None
        }),
        ("no credential at all, and a record", None, |_, parent| {
            Some(format!(
                r#"{{"projects":{{"{parent}":{{"x":1}}}},"userID":"u"}}"#
            ))
        }),
        ("one that is not JSON", Some(LOGIN), |_, _| {
            Some("not json at all".into())
        }),
        ("one that is JSON but not an object", Some(LOGIN), |_, _| {
            Some("[1, 2, 3]".into())
        }),
        (
            "one whose projects is not an object",
            Some(LOGIN),
            |_, _| Some(r#"{"projects":["/w"],"userID":"u"}"#.into()),
        ),
        (
            "one whose entry for the tree is not an object",
            Some(LOGIN),
            |tree, _| Some(format!(r#"{{"projects":{{"{tree}":"trusted?"}}}}"#)),
        ),
    ];

    let mut trusted = 0;
    let mut trusted_without_a_login = 0;
    let mut left_alone = 0;
    for (n, (case, credential, before_of)) in cases.iter().enumerate() {
        let at = dir.join(format!("box-{n}"));
        let home = at.join("home");
        let tree_dir = at.join("tree");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(&tree_dir).unwrap();
        let tree = tree_dir.display().to_string();
        let parent = at.display().to_string();
        let before = before_of(&tree, &parent);
        if let Some(body) = credential {
            fs::write(home.join(".claude/.credentials.json"), body).unwrap();
        }
        if let Some(body) = &before {
            fs::write(home.join(".claude.json"), body).unwrap();
        }

        let out = run_trust_block(&home, &tree_dir);
        assert!(
            out.status.success(),
            "the launcher's trust block failed for `{case}`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        let after = fs::read_to_string(home.join(".claude.json")).ok();
        let was: Option<serde_json::Value> = before
            .as_deref()
            .filter(|b| !b.trim().is_empty())
            .map(|b| serde_json::from_str(b).unwrap_or(serde_json::Value::Null));
        let could_extend = match &was {
            None => true,
            Some(serde_json::Value::Object(o)) => match o.get("projects") {
                None => true,
                Some(serde_json::Value::Object(p)) => p.get(&tree).is_none_or(|e| e.is_object()),
                Some(_) => false,
            },
            Some(_) => false,
        };

        if !could_extend {
            left_alone += 1;
            assert_eq!(
                after.as_deref(),
                before.as_deref(),
                "`{case}` was rewritten; a file skein cannot extend must be left exactly as it was"
            );
            assert!(
                said.contains(".claude.json") && said.contains(&tree),
                "`{case}` was left alone in silence, so nobody can tell why the box still asks to \
                 trust its tree: {said:?}"
            );
            continue;
        }

        let text = after.unwrap_or_else(|| {
            panic!(
                "`{case}`: no ~/.claude.json after the trust block ran, so the tree is not trusted \
                 — a box with no login must still have its tree trusted, and if this case has no \
                 credential the trust has been put behind the login, which is a different fact"
            )
        });
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("`{case}` left ~/.claude.json unreadable ({e}): {text}"));
        let projects = parsed
            .get("projects")
            .and_then(|p| p.as_object())
            .unwrap_or_else(|| {
                panic!("`{case}`: no `projects` object after the trust write: {text}")
            });
        let projects_before = was
            .as_ref()
            .and_then(|w| w.get("projects"))
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default();

        // NOTHING ELSE IS TRUSTED. Every key under `projects` other than the tree was there before,
        // with the value it had. Checked first, so a write aimed at the wrong path is named as that
        // and not merely as "the tree is not trusted".
        for (key, value) in projects {
            if *key == tree {
                continue;
            }
            assert_eq!(
                projects_before.get(key),
                Some(value),
                "`{case}`: no other projects key may appear or change, and `{key}` did — trust in \
                 Claude Code is inherited by every folder beneath it, and skein vouches only for \
                 the tree it cloned: {text}"
            );
        }
        for key in projects_before.keys() {
            assert!(
                projects.contains_key(key),
                "`{case}`: the projects entry `{key}` was there and is gone: {text}"
            );
        }

        // THE INVARIANT.
        assert_eq!(
            projects.get(&tree).and_then(|e| e.get("hasTrustDialogAccepted")),
            Some(&serde_json::Value::Bool(true)),
            "`{case}`: the tree skein cloned is not trusted, so the box opens on the trust dialog: \
             {text}"
        );
        trusted += 1;
        if credential.is_none() {
            trusted_without_a_login += 1;
        }

        // Merged into, never replaced: the tree's own entry keeps every field it had…
        if let Some(serde_json::Value::Object(entry)) = projects_before.get(&tree) {
            for (field, value) in entry {
                if field == "hasTrustDialogAccepted" {
                    continue;
                }
                assert_eq!(
                    projects[&tree].get(field),
                    Some(value),
                    "`{case}`: the tree's own entry lost `{field}` — the trust write replaced the \
                     entry rather than adding one key to it: {text}"
                );
            }
        }
        // …and so does the file.
        if let Some(serde_json::Value::Object(whole)) = &was {
            for (key, value) in whole {
                if key == "projects" {
                    continue;
                }
                assert_eq!(
                    parsed.get(key),
                    Some(value),
                    "`{case}`: the key `{key}` was in ~/.claude.json and is not in what skein wrote \
                     back: {text}"
                );
            }
        }
    }
    assert!(
        trusted == 8 && trusted_without_a_login == 2 && left_alone == 4,
        "this table was read as {trusted} trusted ({trusted_without_a_login} with no login) and \
         {left_alone} left alone, which is not the split its cases were written to have"
    );
}
