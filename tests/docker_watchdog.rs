//! The Docker daemon coming back is a blip, not a fleet restart.
//!
//! `dockerd` runs in the fleet sandbox with pid 1 for a parent and nothing supervising it, so a
//! container that gets it killed costs a rebuild of the whole fleet to recover one process. The
//! watchdog is `src/dockerd.rs`, and this drives it.
//!
//! **It used to be Python, and this used to run the shipped file.** The watchdog lived in
//! `src/fleet-agent.py` on the correct grounds that the agent was already the long-lived in-sandbox
//! process; the agent is deleted (architecture §13a, SKEIN-521) and `skein-server` is that process
//! now, so the watchdog moved rather than going with it. Every scenario below is the one that was
//! there — a daemon that dies, another supervisor winning the race, a daemon never seen, one that
//! will not start, the shield, and the snapshot's shape — because what moved is where the code
//! lives, not what it decides.
//!
//! **Every collaborator is injected** ([`skein::dockerd::World`]), so none of this needs a daemon,
//! a `/proc` that says what the test wants, or a sleep that really sleeps. What is left is the
//! decision, which is the part worth asserting.

use skein::dockerd::{Verdict, Watch, World, BACKOFF_MAX, GRACE, OOM_SCORE, PROCESSES};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the watchdog saw and did, recorded so a test can read it back.
#[derive(Default)]
struct Seen {
    spawned: Vec<Vec<String>>,
    slept: Vec<Duration>,
    said: Vec<String>,
    shielded: Vec<u32>,
}

/// A watchdog over a scripted world.
///
/// `pids` is **one answer per `find()` call**, not a state: `look()` asks twice in a pass where the
/// daemon is missing — once at the top, once after the grace — and the second ask is what lets
/// another supervisor win. So `[9, None, 4242]` reads as "alive, then gone, then back by itself".
fn watching(pids: Vec<Option<u32>>, argv: Option<Vec<String>>) -> (Watch, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let answers = Arc::new(Mutex::new(pids.into_iter()));
    let (find, spawn, sleep, log) = (answers.clone(), seen.clone(), seen.clone(), seen.clone());
    let world = World {
        find: Box::new(move || find.lock().unwrap().next().flatten()),
        argv_of: Box::new(move |_| argv.clone()),
        spawn: Box::new(move |argv| {
            spawn.lock().unwrap().spawned.push(argv.to_vec());
            Ok(())
        }),
        now: Box::new(|| "2026-01-01T00:00:00Z".to_string()),
        sleep: Box::new(move |d| sleep.lock().unwrap().slept.push(d)),
        log: Box::new(move |line| log.lock().unwrap().said.push(line.to_string())),
        kin: Box::new(|_| Vec::new()),
        shield: Box::new(|_| true),
    };
    (Watch::over(world), seen)
}

#[test]
fn a_daemon_that_dies_is_restarted_with_the_command_line_it_had() {
    // Alive on the first look — which is when the argv is learned — then gone, and gone again after
    // the grace. The restart uses what was remembered, not a guess.
    let argv = vec![
        "/usr/bin/dockerd".to_string(),
        "--host=unix:///run/docker.sock".to_string(),
    ];
    let (mut watch, seen) = watching(vec![Some(9), None, None], Some(argv.clone()));
    assert_eq!(watch.look(), Verdict::Alive);
    assert_eq!(watch.look(), Verdict::Restarted);
    assert_eq!(watch.restarts, 1);
    assert_eq!(
        seen.lock().unwrap().spawned,
        vec![argv],
        "the daemon was not brought back with its own command line"
    );
}

#[test]
fn another_supervisor_that_wins_the_race_is_left_alone() {
    // The grace is the whole point: `PPID 1` does not say whether init spawned dockerd or merely
    // reaped it, so something unseen may be supervising it. Two dockerds is a worse failure than
    // none, so a daemon that comes back during the grace is not restarted again.
    let (mut watch, seen) = watching(
        vec![Some(9), None, Some(4242)],
        Some(vec!["/usr/bin/dockerd".to_string()]),
    );
    watch.look();
    assert_eq!(
        watch.look(),
        Verdict::Recovered,
        "the watchdog started a second daemon beside one that was already coming back"
    );
    let seen = seen.lock().unwrap();
    assert!(seen.spawned.is_empty());
    assert_eq!(watch.restarts, 0);
    // **And the race was actually given time to be won.** Asserted because the rest of this test
    // passes without it: the second `find` is consulted whether or not anything waited, so a
    // watchdog that had dropped the grace entirely still read as "recovered" here. Proved by
    // deleting the sleep — only the backoff test failed, which is one test too few for a property
    // whose whole content is that something else got a chance.
    assert_eq!(
        seen.slept,
        vec![GRACE],
        "nothing waited, so another supervisor had no window to win in"
    );
}

#[test]
fn a_watchdog_that_never_saw_the_daemon_says_so_instead_of_guessing() {
    // There is no safe default command line for dockerd, and inventing one is how a fleet ends up
    // with a daemon configured differently from the one it had — cgroup-parent included, which is
    // the setting the whole memory plan rests on.
    let (mut watch, seen) = watching(vec![None, None, None], None);
    assert_eq!(watch.look(), Verdict::Unknown);
    assert_eq!(watch.look(), Verdict::Unknown);
    let seen = seen.lock().unwrap();
    assert!(seen.spawned.is_empty(), "it guessed a command line");
    // Said exactly once — a line every five seconds would be noise about a condition that cannot
    // change on its own.
    assert_eq!(seen.said.len(), 1, "it repeated itself: {:?}", seen.said);
}

#[test]
fn a_daemon_that_will_not_start_is_not_respawned_in_a_loop() {
    // The failure mode of a watchdog: a dockerd that dies immediately, respawned every grace period
    // for ever, on a sandbox that is already unwell. The wait grows instead.
    let mut pids = vec![Some(9)];
    pids.extend(std::iter::repeat_n(None, 12));
    let (mut watch, seen) = watching(pids, Some(vec!["/usr/bin/dockerd".to_string()]));
    for _ in 0..6 {
        watch.look();
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.spawned.len(), 5);
    assert_eq!(seen.slept[0], GRACE, "the first wait is the grace");
    assert!(
        seen.slept[seen.slept.len() - 1] > seen.slept[0],
        "the wait between attempts did not grow: {:?}",
        seen.slept
    );
    assert!(
        seen.slept[seen.slept.len() - 1] <= BACKOFF_MAX,
        "the wait grew without a ceiling: {:?}",
        seen.slept
    );
}

#[test]
fn the_daemon_is_shielded_from_the_global_killer_on_every_pass() {
    // Per process, so it has to be re-applied: a restarted dockerd is a new pid, and a shield that
    // does not survive the restart it exists for is not one. `-500` rather than `-1000`, because an
    // OOM-immune daemon on a sandbox with nothing left to kill is a wedged machine.
    let seen = Arc::new(Mutex::new(Seen::default()));
    let recording = seen.clone();
    let answers = Arc::new(Mutex::new(vec![Some(9), Some(9)].into_iter()));
    let world = World {
        find: Box::new(move || answers.lock().unwrap().next().flatten()),
        argv_of: Box::new(|_| Some(vec!["/usr/bin/dockerd".to_string()])),
        spawn: Box::new(|_| Ok(())),
        now: Box::new(String::new),
        sleep: Box::new(|_| {}),
        log: Box::new(|_| {}),
        kin: Box::new(|names| match names.contains(&"dockerd") {
            true => vec![11, 12],
            false => Vec::new(),
        }),
        shield: Box::new(move |pid| {
            recording.lock().unwrap().shielded.push(pid);
            true
        }),
    };
    let mut watch = Watch::over(world);
    watch.look();
    watch.look();
    assert_eq!(
        seen.lock().unwrap().shielded,
        vec![11, 12, 11, 12],
        "the shield is not re-applied on every pass"
    );
    assert_eq!(watch.shielded, 2);
    assert_eq!(OOM_SCORE, -500);
    let mut covers = PROCESSES.to_vec();
    covers.sort_unstable();
    assert_eq!(
        covers,
        vec!["containerd", "dockerd"],
        "the shield covers the wrong processes"
    );
}

/// Reading it back is the point: a refused write and a successful one look the same through a shell.
#[test]
fn a_shield_that_did_not_take_is_reported_as_not_taken() {
    let dir = std::env::temp_dir().join(format!("skein-shield-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("7")).unwrap();

    // A refusal has to be MADE here, not assumed of the machine.
    //
    // `shield` falls back to `sudo -n` when the direct write is refused, which is its documented job
    // — lowering oom_score_adj needs privilege. So on any host with passwordless sudo, and that is
    // every GitHub runner, root ignores the 0444 below and the write TAKES. This test then failed on
    // CI while passing in a box, where sudo is a stub that cannot escalate: green for an
    // environmental reason rather than a correct one, and red where the environment differed.
    //
    // Closing both paths explicitly makes the assertion mean the same thing everywhere, and covers
    // the branch nothing else reaches: sudo present, and refusing.
    let stub = dir.join("bin");
    std::fs::create_dir_all(&stub).unwrap();
    std::fs::write(stub.join("sudo"), "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(stub.join("sudo"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{path}", stub.display()));

    // Present and readable, but its value never changes — which is what a refused write looks like.
    let score = dir.join("7/oom_score_adj");
    std::fs::write(&score, "0").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&score, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let took = skein::dockerd::shield(7, -500, &dir.to_string_lossy());
    // And a process that is gone between the listing and the write.
    let missing = skein::dockerd::shield(99999, -500, &dir.to_string_lossy());

    std::env::set_var("PATH", path);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!took, "a shield that did not take was reported as taken");
    assert!(!missing, "a pid that is gone reported a shield");
}

#[test]
fn what_the_reading_carries_is_a_fact_rather_than_a_diagnosis() {
    let (mut watch, _) = watching(
        vec![Some(9), Some(9)],
        Some(vec!["/usr/bin/dockerd".to_string()]),
    );
    watch.look();
    let snap = watch.snapshot();
    let mut keys: Vec<&str> = snap
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "argv_known",
            "last_restart",
            "note",
            "pid",
            "restarts",
            "shielded"
        ],
        "the reading the board shows changed shape"
    );
    assert_eq!(snap["argv_known"], true);
    assert_eq!(snap["restarts"], 0);
}

/// `/proc` is read for what it is, and a pid that is not a number is not a pid.
///
/// Its own test because the Python this replaced got it for free from `str.isdigit()`, and the port
/// has to do it deliberately: `/proc` holds `self`, `net`, `meminfo` and a hundred other names
/// beside the numeric ones, and a parse that accepted them would ask `comm` of a directory that has
/// none — cheap, but it is the kind of thing that turns into a wrong pid rather than no pid.
#[test]
fn only_the_numbered_entries_of_proc_are_pids() {
    let dir = std::env::temp_dir().join(format!("skein-proc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for name in ["12", "34", "self", "meminfo"] {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("comm"), "dockerd\n").unwrap();
    }
    let found = skein::dockerd::pids_named(&["dockerd"], &dir.to_string_lossy());
    let pid = skein::dockerd::daemon_pid(&dir.to_string_lossy());
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(found, vec![12, 34], "a non-numeric entry was read as a pid");
    assert_eq!(
        pid,
        Some(12),
        "the daemon's pid is the lowest numbered match, deterministically"
    );
}
