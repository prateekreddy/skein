//! The fixture a launch runs in and how it is stopped: the scratch fleet, the processes still
//! running out of it, and the sweep that removes a dead run's directory only once nothing is
//! running from it.

use super::*;

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
pub(super) fn scratch_named(what: &str) -> Scratch {
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
pub(super) fn fixture_processes(root: &Path) -> Vec<(u32, String)> {
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
pub(super) const KILL_WINDOW: Duration = Duration::from_secs(5);

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
pub(super) struct Stopped {
    /// The doorway script — the supervisor loop's own `while [ -f … ]` exit condition — was still on
    /// disk when the wait that produced `left` finished, so for the whole of that wait nothing but
    /// the kill could have emptied it.
    pub(super) script_was_there: bool,
    /// What still ran out of the fixture when the kill's wait gave up. Empty means the kill took it.
    pub(super) left: Vec<(u32, String)>,
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
pub(super) fn stop_fixture(root: &Path) -> Stopped {
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
/// **The ghost is an ordinary child of this test, and that is deliberate.** The sweep's DELETE
/// exempts nobody, because deleting the directory out from under a process is the same harm whoever
/// owns it — so the directory is kept. Its KILL (SKEIN-1133) exempts anything that hangs off a live
/// process outside the fixture, and this ghost hangs off this test — so the ghost is left running,
/// and that is asserted too. A ghost that daemonized to `ppid=1` would be ended, which is
/// [`a_sweep_ends_the_loop_a_sigkilled_run_left_and_keeps_its_fixture`]'s half.
///
/// **And a breadcrumb keeps nothing.** A second process names the quiet directory only as `OLDPWD`
/// — where it has been, not what it uses (SKEIN-990) — and the quiet directory must still go.
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
    // And the shape the warden's `Scratch` leaves when a test panics (SKEIN-557): `-t<n>` after the
    // pid rather than `-ThreadId(n)`. Just as dead, so it must go too — a sweep that cannot read
    // past that tail would keep every one of them for ever.
    let kept_by_a_panic = root.join(format!("skein-sweep-kept-{dead}-t7"));
    fs::create_dir_all(&kept_by_a_panic).unwrap();

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
    // Standing somewhere else, with the quiet directory only as the place it `cd`ed from.
    let mut breadcrumb = Command::new("sleep")
        .arg("400")
        .current_dir(root)
        .env("OLDPWD", &quiet)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a process that has only passed through the quiet directory");
    // Polled, because until the child has `exec`ed, its `environ` is still the forked copy of this
    // process's, which carries no such variable.
    let carries = |pid: u32| {
        fs::read(format!("/proc/{pid}/environ"))
            .map(|env| {
                env.split(|b| *b == 0)
                    .any(|var| var == format!("OLDPWD={}", quiet.display()).as_bytes())
            })
            .unwrap_or(false)
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !carries(breadcrumb.id()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let breadcrumb_carries_it = carries(breadcrumb.id());

    // Every observation is taken BEFORE the ghost is ended, and every assertion AFTER — so a failing
    // assertion unwinds past nothing that had to happen. This is the `Scratch` lesson applied to a
    // process: a trailing `kill` runs exactly when the test passes.
    let haunting = common::processes_under(&haunted);
    let beside_it = common::processes_under(&quiet);
    let quiet_was_there = quiet.exists();
    let kept = common::sweep_abandoned(root);
    let haunted_survived = haunted.exists();
    let quiet_swept = !quiet.exists();
    let panic_kept_swept = !kept_by_a_panic.exists();
    let ghost_survived = matches!(ghost.try_wait(), Ok(None));
    let _ = ghost.kill();
    let _ = ghost.wait();
    let _ = breadcrumb.kill();
    let _ = breadcrumb.wait();

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
        breadcrumb_carries_it,
        "the breadcrumb process does not carry OLDPWD={}, so the quiet directory's removal below \
         says nothing about whether a breadcrumb keeps a directory",
        quiet.display()
    );
    assert!(
        ghost_survived,
        "the sweep ended pid {ghost_pid}, a child of this LIVE test that only carries the dead \
         run's path in its environment. What the sweep may end is what hangs off nothing alive \
         outside the fixture; this one hangs off the test, and a sweep that kills it will kill a \
         sibling test's child, or a shell somebody opened in a kept fixture to read it"
    );
    assert!(
        quiet_was_there && quiet_swept,
        "the control directory was {} before the sweep and {} after it. A sweep that removes \
         nothing passes every other assertion in this test",
        if quiet_was_there { "present" } else { "absent" },
        if quiet_swept { "gone" } else { "still there" }
    );
    assert!(
        panic_kept_swept,
        "{} names a dead run and was left by the sweep — it does not read a `-t<n>` thread tail",
        kept_by_a_panic.display()
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

/// **A run that was SIGKILLed leaves a supervisor loop, and the next sweep ends it and keeps the
/// fixture** (SKEIN-1133).
///
/// The owner here is a process of its own, because the case is one no `Drop` reaches: a test
/// process killed outright — a gate run killed mid-`test`, a cargo timeout. It makes its fixture
/// the way `Scratch` does, `<prefix>-<its pid>`, and starts in it what `fleet::start_server`
/// starts: a detached tmux server on a socket inside the fixture, holding a
/// `while [ -f <fixture>/.skein/server-doorway.py ]` loop that restarts its child. Then it is
/// SIGKILLed and reaped, so its pid is out of `/proc` — and the loop, being tmux's and not the
/// owner's, is still going. That is SKEIN-1019's state.
///
/// **Presence, then absence.** The loop is asserted to be running, and to have outlived its owner,
/// before the sweep is asked to do anything; otherwise an empty count afterwards would be about a
/// loop that was never there.
///
/// **What makes each assertion fail:**
///
/// * the loop still running after the sweep: `common::end_abandoned` returning without ending
///   anything, which is `sweep_abandoned` as SKEIN-900 left it. Every scan in the window finds the
///   tmux server and the loop, and the assertion names them.
/// * the fixture kept: `sweep_abandoned` removing the directory once its processes are ended.
///
/// Whatever the outcome, the loop is ended by hand after the measurements and before the
/// assertions, so a failure here does not leave the very leak it is about.
#[test]
fn a_sweep_ends_the_loop_a_sigkilled_run_left_and_keeps_its_fixture() {
    if !have("tmux") {
        return skip("this machine has no tmux, so it cannot run a fixture's supervisor loop");
    }
    let scratch = Scratch::temp("skein-sweep-owner");
    let root = scratch.path();

    // `$1` is the root; the fixture is named with the owner's own pid, `$$`, which is still its
    // pid after the `exec`. The loop's child is a thirty-second sleep standing in for the doorway,
    // which does not exit while it holds its socket — so the loop cannot end by finding its script
    // gone within the window below, and only a kill can end it there. Bounded, so a failure here
    // cannot leave it running for ever.
    let owner_script = r#"d="$1/skein-sweep-owned-$$"
mkdir -p "$d/.skein" && : > "$d/.skein/server-doorway.py" || exit 1
tmux -S "$d/server.tmux" new-session -d -s skein-server \
  "while [ -f '$d/.skein/server-doorway.py' ]; do sleep 30; sleep 0.1; done" || exit 1
echo "$d"
exec sleep 400"#;
    let mut owner = Command::new("sh")
        .args(["-c", owner_script, "sh"])
        .arg(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the fixture's owner");
    let fixture = {
        use std::io::BufRead;
        let mut line = String::new();
        let out = owner.stdout.take().expect("the owner's stdout");
        std::io::BufReader::new(out)
            .read_line(&mut line)
            .expect("the owner says where its fixture is");
        PathBuf::from(line.trim())
    };
    let sock = fixture.join("server.tmux");
    let is_loop = |argv: &str| argv.contains("while [ -f");
    let wait_for = |want_loop: bool, within: Duration| {
        let deadline = Instant::now() + within;
        loop {
            let found = common::processes_under(&fixture);
            let has_loop = found.iter().any(|(_, argv)| is_loop(argv));
            if has_loop == want_loop || Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    let before = wait_for(true, Duration::from_secs(5));
    let _ = owner.kill(); // SIGKILL: nothing of the owner's runs after this, no `Drop`, no trap
    let _ = owner.wait();
    let orphaned = wait_for(true, Duration::from_millis(0));
    let owner_gone = !Path::new(&format!("/proc/{}", owner.id())).exists();

    let kept = common::sweep_abandoned(root);
    // Bounded: the sweep's own rounds are three seconds at most, and this is well past them.
    let after = wait_for(false, Duration::from_secs(10));
    let fixture_kept = fixture.is_dir();

    // The measurements are taken; end the loop whatever they say, so a failure below is not a leak.
    let _ = fs::remove_file(fixture.join(".skein/server-doorway.py"));
    let _ = Command::new("tmux")
        .arg("-S")
        .arg(&sock)
        .arg("kill-server")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    assert!(
        before.iter().any(|(_, argv)| is_loop(argv)),
        "the owner's supervisor loop never showed up under {}, so its absence below would prove \
         nothing: {before:#?}",
        fixture.display()
    );
    assert!(
        owner_gone && orphaned.iter().any(|(_, argv)| is_loop(argv)),
        "the loop did not outlive its SIGKILLed owner (owner gone: {owner_gone}), so this is not \
         the state SKEIN-1019 was in: {orphaned:#?}"
    );
    assert!(
        !after.iter().any(|(_, argv)| is_loop(argv)) && after.is_empty(),
        "the sweep ran over a fixture whose owner was dead and {} process(es) were still running \
         out of it ten seconds later — the supervisor loop a SIGKILLed test leaves behind, \
         restarting its child for as long as nobody deletes the directory: {after:#?}",
        after.len()
    );
    assert!(
        fixture_kept && kept.iter().any(|(dir, _)| *dir == fixture),
        "the sweep removed {} — the owner chose to keep a dead run's fixture as its evidence and \
         end only what it left running (SKEIN-1133). Kept: {kept:#?}",
        fixture.display()
    );
}
