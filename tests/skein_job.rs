//! The heavy-work wrapper, `skein-job` (SKEIN-1388), run as an agent runs it: the shipped bytes,
//! by `python3`, against skein's real scheduler (`skein::jobs::tick`) looping beside it as
//! skein-server's loop would, and the kernel's real `/proc` and cgroup files.
//!
//! Nothing here touches a live fleet: the drop-box, the box's state and the queue are a scratch
//! directory, and the box's heavy lock is a file of the test's own (`SKEIN_JOB_LOCK`), never the
//! `/var/tmp/skein-lanes-heavy.lock` the lanes running this suite hold.

// Every test here is Linux's, and the helpers are theirs: the wrapper reads /proc/<pid>/status
// and /proc/locks, one test reads the job's /proc/self/cgroup and the box's cgroup.procs (cgroup
// v2), and one holds the lock with util-linux `flock`. `tests/platform_gates.rs`' `GATED` list
// covers the library's tests, not this binary's, so the reasons are here.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

mod common;

use common::{skip, Scratch};
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WRAPPER: &str = include_str!("../src/plugin/bin/skein-job");

/// A fleet of one box in a scratch directory, with the scheduler ticking over it.
struct Fleet {
    dir: Scratch,
    name: String,
    places: skein::jobs::Places,
    stop: Arc<AtomicBool>,
    ticking: Option<std::thread::JoinHandle<()>>,
}

impl Fleet {
    /// Box `name`, whose cgroups are under `cgroups`, on a machine of `cores` cores with no
    /// oversubscription and memory unread.
    fn new(prefix: &str, name: &str, cgroups: PathBuf, cores: u32) -> Fleet {
        let dir = Scratch::temp(prefix);
        fs::write(dir.path().join("skein-job"), WRAPPER).unwrap();
        let places = skein::jobs::Places {
            requests: dir.path().join("fleet/.skein/jobs/requests"),
            states: dir.path().join("home/boxes"),
            queue: dir.path().join("home/jobs/queue.json"),
            cgroups,
        };
        fs::create_dir_all(places.requests.join(name)).unwrap();
        fs::create_dir_all(places.states.join(name).join("signals")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let ticking = {
            let (places, stop) = (places.clone(), stop.clone());
            std::thread::spawn(move || {
                let limits = skein::jobs::Limits {
                    cores,
                    max_cores_per_job: skein::jobs::DEFAULT_MAX_CORES_PER_JOB,
                    oversubscribe: 1.0,
                    mem_free_mib: None,
                };
                // Held across ticks, as skein-server's loop holds it.
                let mut rates = skein::jobs::cgroup::Rates::default();
                while !stop.load(Ordering::Relaxed) {
                    let _ = skein::jobs::tick(
                        &places,
                        limits,
                        &|b| b.to_string(),
                        chrono::Utc::now(),
                        &mut rates,
                        None,
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        };
        Fleet {
            dir,
            name: name.to_string(),
            places,
            stop,
            ticking: Some(ticking),
        }
    }

    /// `skein-job` as box `self.name` would run it, with its own lock and no inherited job.
    fn wrapper(&self) -> Command {
        let mut c = Command::new("python3");
        c.arg(self.dir.path().join("skein-job"))
            .env("SKEIN_BOX", &self.name)
            .env("SKEIN_FLEET_ROOT", self.dir.path().join("fleet"))
            // Not read by the wrapper; pinned so nothing it starts could reach the real ~/.skein.
            .env("SKEIN_HOME", self.dir.path().join("home"))
            .env("SKEIN_STATE", self.places.states.join(&self.name))
            .env("SKEIN_JOB_LOCK", self.lock())
            // Long enough that a fallback cannot pass for a grant inside any bound asserted here.
            .env("SKEIN_JOB_UNANSWERED_SECONDS", "30")
            .env_remove("SKEIN_JOB_ID")
            // A covered box unless a test says otherwise: run from the workshop box, this suite
            // would inherit its `SKEIN_BOX_PRIVILEGED=1` and hear the workshop box's reason.
            .env_remove("SKEIN_BOX_PRIVILEGED")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    /// Stop the scheduler, as a skein-server that has gone away stops answering.
    fn stop_ticking(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.ticking.take() {
            let _ = t.join();
        }
    }

    fn lock(&self) -> PathBuf {
        self.dir.path().join("heavy.lock")
    }

    fn queue(&self) -> skein::jobs::Queue {
        skein::jobs::read_queue(&self.places.queue)
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.ticking.take() {
            let _ = t.join();
        }
    }
}

/// Wait at most `bound` for `child`; kill it and say so if it is still running.
fn finish_within(
    mut child: std::process::Child,
    bound: Duration,
) -> Result<(String, String), String> {
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if started.elapsed() > bound {
            // The whole group: a wrapper stuck behind a killed `flock` is its grandchild, and
            // killing only the child would leave it waiting for ever.
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
            let _ = child.kill();
            let _ = child.wait();
            let mut err = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut err);
            return Err(format!("still running after {bound:?}; it said:\n{err}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (mut out, mut err) = (String::new(), String::new());
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    Ok((out, err))
}

/// This process's cgroup, as the kernel says: `0::/skein/<box>` in a box.
fn own_cgroup() -> String {
    fs::read_to_string("/proc/self/cgroup")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("")
        .to_string()
}

/// **A job runs in the cgroup of the box that submitted it, as the wrapper's own child.**
///
/// Real all the way: the wrapper files its job, skein's scheduler grants it only after reading the
/// wrapper's pid in the box's real `cgroup.procs`, and the command then reports, from inside
/// itself, its parent's pid, its `/proc/self/cgroup`, and whether its own pid is in that cgroup's
/// `cgroup.procs`. "The box" here is the cgroup this test runs in, which on a fleet box is
/// `/skein/<box>`, the one that carries the box's memory ceiling and floor.
///
/// What would make it fail: the wrapper handing the command to anything that is not its own child,
/// such as a detached `setsid` double fork (the sabotage that was run: the parent pid is 1 or a
/// subreaper, not the wrapper), or skein running it from outside by a crossing, which enters the
/// box's namespaces and not its cgroup. **That second shape cannot be made to fail here**: every
/// process this box can start is in this one cgroup, and no cgroup is writable from inside it, so
/// the cgroup assertions are true of any child this test can produce. They are kept because they
/// are the property, and on a host where a test can create cgroups they become the strong half.
#[cfg(target_os = "linux")]
#[test]
fn a_job_runs_in_the_submitting_boxs_cgroup() {
    let cgroup = own_cgroup();
    let Some((parent, name)) = cgroup.rsplit_once('/').filter(|(_, n)| !n.is_empty()) else {
        return skip(
            "this test runs in the root cgroup, so there is no box cgroup to run a job in, and \
             the job's cgroup was NOT checked",
        );
    };
    let procs = format!("/sys/fs/cgroup{cgroup}/cgroup.procs");
    if fs::read_to_string(&procs).is_err() {
        return skip(&format!(
            "{procs} cannot be read here, so the job's cgroup was NOT checked"
        ));
    }
    let fleet = Fleet::new(
        "skein-job-cgroup",
        name,
        PathBuf::from(format!("/sys/fs/cgroup{parent}")),
        2,
    );
    let probe = format!(
        "echo \"pid $$\"; echo \"ppid $PPID\"; cat /proc/self/cgroup; \
         grep -qx \"$$\" {procs} && echo in-procs; true",
    );
    let child = fleet
        .wrapper()
        .args(["run", "--class", "test", "--", "sh", "-c", &probe])
        .process_group(0)
        .spawn()
        .unwrap();
    let wrapper_pid = child.id();
    let (out, err) = finish_within(child, Duration::from_secs(20)).unwrap();

    assert!(
        !err.contains("has not answered") && !err.contains("could not file"),
        "the job ran without the scheduler, so this proves nothing about a granted job:\n{err}"
    );
    let line = |key: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_else(|| panic!("no {key:?} in what the job said:\n{out}\n{err}"))
            .to_string()
    };
    assert_eq!(
        line("ppid "),
        wrapper_pid.to_string(),
        "the job is not the wrapper's own child:\n{out}"
    );
    assert_eq!(
        line("0::"),
        cgroup,
        "the job ran outside the box's cgroup:\n{out}"
    );
    assert!(
        out.lines().any(|l| l == "in-procs"),
        "the job's pid is not in the box's cgroup.procs:\n{out}"
    );
    // And the scheduler saw it end, by the box's own done file or by the pid leaving the cgroup.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let q = fleet.queue();
        if q.jobs
            .iter()
            .any(|j| j.state == "done" || j.state == "gone")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the job never ended in the queue: {q:#?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// **A `skein-job` inside a job runs at once.** SYNAP-517 was the deadlock this prevents: the
/// outer job holds the box's lock and every core of the fleet, and an inner one that queued for
/// either would wait for ever behind its own parent.
///
/// The machine here has two cores, all the outer job's, and the scheduler is running, so an inner
/// call that asked for cores would be queued behind the outer job and wait the full 30 s before
/// giving up; the bound is 5 s.
///
/// What would make it fail: the wrapper ignoring `SKEIN_JOB_ID` (the sabotage that was run: the
/// inner job files a request, reads that it waits for cores, and the run is killed at 5 s), or
/// the child not being given `SKEIN_JOB_ID`.
#[cfg(target_os = "linux")]
#[test]
fn a_nested_skein_job_runs_without_waiting() {
    // No cgroup to read: liveness falls back to the pid existing, which is the real pid here.
    let fleet = Fleet::new(
        "skein-job-nested",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        2,
    );
    let inner = format!(
        "python3 {} run --class test -- echo nested-ran",
        fleet.dir.path().join("skein-job").display()
    );
    let started = Instant::now();
    let child = fleet
        .wrapper()
        .args(["run", "--class", "build", "--", "sh", "-c", &inner])
        .process_group(0)
        .spawn()
        .unwrap();
    let (out, err) = finish_within(child, Duration::from_secs(5)).unwrap_or_else(|e| {
        panic!("a nested skein-job waited behind its own parent: {e}");
    });
    assert!(out.contains("nested-ran"), "{out}\n{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        !err.contains("has not answered"),
        "the outer job was never granted, so nothing nested was tested:\n{err}"
    );
    let filed = fs::read_dir(fleet.places.requests.join("web-main"))
        .unwrap()
        .count();
    assert_eq!(filed, 1, "the inner call filed a job of its own:\n{err}");
}

/// **`flock <the box's lock> skein-job run -- …` does not wait on itself.** A lane that wraps the
/// wrapper in the raw `flock` it used before holds the lock in an ancestor of the wrapper, and a
/// wrapper that then asked for the same lock would wait for its own grandparent.
///
/// What would make it fail: the wrapper's ancestor check on the lock removed or always false (the sabotage that was
/// run: the run is killed at 5 s), or reading the lock's device in decimal where `/proc/locks`
/// writes hex.
#[cfg(target_os = "linux")]
#[test]
fn a_skein_job_under_a_raw_flock_of_the_same_lock_does_not_wait_on_itself() {
    let fleet = Fleet::new(
        "skein-job-flock",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        2,
    );
    fs::write(fleet.lock(), "").unwrap();
    let c = fleet.wrapper();
    let wrapper = c.get_args().next().unwrap().to_owned();
    let mut flocked = Command::new("flock");
    flocked.arg(fleet.lock()).arg("python3").arg(wrapper).args([
        "run",
        "--",
        "echo",
        "under-flock",
    ]);
    for (k, v) in c.get_envs() {
        match v {
            Some(v) => flocked.env(k, v),
            None => flocked.env_remove(k),
        };
    }
    let child = flocked
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let (out, err) = finish_within(child, Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("the wrapper waited on the lock its own ancestor holds: {e}"));
    assert!(out.contains("under-flock"), "{out}\n{err}");
}

/// **A granted job's tools are told its cores, except where the caller asked for fewer.**
///
/// The wrapper asks for three cores on a four-core machine and is granted three, and the
/// command prints what it was given. The caller had set `CARGO_BUILD_JOBS=8` (more than the
/// grant: replaced), `RUST_TEST_THREADS=1` (fewer: kept), `MAKEFLAGS="-k -j16"` (replaced, its
/// other flag kept), and no `NEXTEST_TEST_THREADS` (set).
///
/// What would make it fail: the wrapper not handing the grant to the child (the sabotage that
/// was run: the wrapper's environment step returning before its loop, and `CARGO_BUILD_JOBS`
/// stays 8), or a value the caller set lower overwritten by the grant.
#[cfg(target_os = "linux")]
#[test]
fn a_granted_job_is_told_its_cores_unless_the_caller_set_fewer() {
    let fleet = Fleet::new(
        "skein-job-cores",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        4,
    );
    let probe = "for v in CARGO_BUILD_JOBS RUST_TEST_THREADS NEXTEST_TEST_THREADS MAKEFLAGS; do \
                 printf '%s=%s\\n' \"$v\" \"$(printenv \"$v\")\"; done";
    let child = fleet
        .wrapper()
        .env("CARGO_BUILD_JOBS", "8")
        .env("RUST_TEST_THREADS", "1")
        .env("MAKEFLAGS", "-k -j16")
        .env_remove("NEXTEST_TEST_THREADS")
        .args(["run", "--cores", "3", "--", "sh", "-c", probe])
        .process_group(0)
        .spawn()
        .unwrap();
    let (out, err) = finish_within(child, Duration::from_secs(20)).unwrap();
    assert!(
        !err.contains("has not answered") && !err.contains("could not file"),
        "the job ran without the scheduler, so no grant was handed on:\n{err}"
    );
    let q = fleet.queue();
    assert!(
        q.jobs.iter().any(|j| j.granted_cores == 3),
        "the job was not granted three cores, so the grant was not what was tested:\n{q:#?}"
    );
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "CARGO_BUILD_JOBS=3",
            "RUST_TEST_THREADS=1",
            "NEXTEST_TEST_THREADS=3",
            "MAKEFLAGS=-k -j3",
        ],
        "{err}"
    );
}

/// **A job skein never answered is still told its cores: the `--cores` it asked for, or two.**
///
/// Twice, with nothing the caller set: once in a box whose scheduler has stopped, asking for
/// three, and once with no drop-box at all and no ask, which is run with the queue's default two.
/// Each run's stderr must say it fell back, so it is the fallback that is tested.
///
/// What would make it fail: the fallback run handing the child no count (the sabotage that was
/// run: `cores = None` in the wrapper's fallback branch, and every variable prints empty), or
/// the asked `--cores` ignored there.
#[cfg(target_os = "linux")]
#[test]
fn a_job_skein_never_answered_is_still_told_its_cores() {
    let probe = "for v in CARGO_BUILD_JOBS RUST_TEST_THREADS NEXTEST_TEST_THREADS MAKEFLAGS; do \
                 printf '%s=%s\\n' \"$v\" \"$(printenv \"$v\")\"; done";
    let mut fleet = Fleet::new(
        "skein-job-fallback",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        4,
    );
    fleet.stop_ticking();
    let run = |mut c: Command, args: &[&str]| -> (String, String) {
        for v in [
            "CARGO_BUILD_JOBS",
            "RUST_TEST_THREADS",
            "NEXTEST_TEST_THREADS",
            "MAKEFLAGS",
        ] {
            c.env_remove(v);
        }
        let child = c
            .env("SKEIN_JOB_UNANSWERED_SECONDS", "1")
            .args(args)
            .args(["--", "sh", "-c", probe])
            .process_group(0)
            .spawn()
            .unwrap();
        finish_within(child, Duration::from_secs(20)).unwrap()
    };

    let (out, err) = run(fleet.wrapper(), &["run", "--cores", "3"]);
    assert!(
        err.contains("has not answered"),
        "it did not fall back:\n{err}"
    );
    assert!(
        err.lines()
            .any(|l| l.starts_with("skein-job: used ") && !l.contains("GB")),
        "a run skein never measured did not end with the wrapper's own count:\n{err}"
    );
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        [
            "CARGO_BUILD_JOBS=3",
            "RUST_TEST_THREADS=3",
            "NEXTEST_TEST_THREADS=3",
            "MAKEFLAGS=-j3",
        ],
        "{err}"
    );

    let mut no_queue = fleet.wrapper();
    no_queue
        .env("SKEIN_FLEET_ROOT", fleet.dir.path().join("no-fleet-here"))
        .env("SKEIN_HOME", fleet.dir.path().join("home"));
    let (out, err) = run(no_queue, &["run"]);
    assert!(
        !err.contains("granted"),
        "a run with no drop-box was scheduled:\n{err}"
    );
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        [
            "CARGO_BUILD_JOBS=2",
            "RUST_TEST_THREADS=2",
            "NEXTEST_TEST_THREADS=2",
            "MAKEFLAGS=-j2",
        ],
        "{err}"
    );
}

// ─────────── a job the queue never saw says so (SKEIN-1459) ───────────

/// What the wrapper says before a job it runs without the fleet's queue.
const UNQUEUED: &str = "skein-job: not waiting for a fleet slot: ";

/// `echo ran` under the wrapper, with `edit` applied to its command first; what it printed.
fn ran_with(fleet: &Fleet, edit: impl FnOnce(&mut Command)) -> (String, String) {
    let mut c = fleet.wrapper();
    edit(&mut c);
    let child = c
        .args(["run", "--cores", "3", "--", "echo", "ran"])
        .process_group(0)
        .spawn()
        .unwrap();
    // Well inside the 30 s a filed job waits for an answer, so a run that filed one fails here.
    finish_within(child, Duration::from_secs(10)).unwrap()
}

/// **A box with no job drop-box says, before its job runs, that the job is not waiting for a fleet
/// slot, why, and what fixes it; and its last line does not say what it asked for.** That box is
/// one started before the queue shipped: the launcher makes the drop-box when a box starts.
///
/// What would make it fail: the line not said (the sabotage that was run: the `say` of the reason
/// taken out, and no line starts `not waiting for a fleet slot`), said after the command ran, or the
/// last line still reading `asked 3 cores` (the sabotage that was run: `asked` set to the cores
/// the tools were told again, as before SKEIN-1459).
#[cfg(target_os = "linux")]
#[test]
fn a_box_with_no_drop_box_says_its_job_is_not_queued_and_why() {
    let fleet = Fleet::new(
        "skein-job-no-drop",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        4,
    );
    fs::remove_dir(fleet.places.requests.join("web-main")).unwrap();
    let (out, err) = ran_with(&fleet, |_| {});
    assert_eq!(out.trim(), "ran", "{err}");
    let lines: Vec<&str> = err.lines().collect();
    let said = lines
        .iter()
        .position(|l| {
            *l == format!(
                "{UNQUEUED}this box has no job drop-box, because it started before skein's job \
                 queue; restarting the box fixes that"
            )
        })
        .unwrap_or_else(|| panic!("it did not say why the job was not queued:\n{err}"));
    let last = lines
        .iter()
        .position(|l| l.starts_with("skein-job: used ") && l.ends_with('s'))
        .unwrap_or_else(|| panic!("no last line of what it used:\n{err}"));
    assert!(said < last, "the reason came after the run:\n{err}");
    assert!(
        !err.contains("asked"),
        "a job nothing was asked of says it asked:\n{err}"
    );
}

/// **The workshop box with no job drop-box says why in its own words, not a covered box's.** The
/// launcher makes every box's drop-boxes now (SKEIN-1463), so a workshop box without one was
/// started by a launcher from before that, and "it started before skein's job queue" is not true
/// of it: it can have started a minute ago.
///
/// What would make it fail: the `SKEIN_BOX_PRIVILEGED` branch taken out of `drop_box()`, so the
/// workshop box hears the covered box's reason (the sabotage that was run).
#[cfg(target_os = "linux")]
#[test]
fn the_workshop_box_with_no_drop_box_says_its_own_reason() {
    let fleet = Fleet::new(
        "skein-job-no-drop-workshop",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        4,
    );
    fs::remove_dir(fleet.places.requests.join("web-main")).unwrap();
    let (out, err) = ran_with(&fleet, |c| {
        c.env("SKEIN_BOX_PRIVILEGED", "1");
    });
    assert_eq!(out.trim(), "ran", "{err}");
    assert!(
        err.lines().any(|l| l
            == format!(
                "{UNQUEUED}this is the workshop box, and the launcher that started it made job \
                 drop-boxes only for the other boxes; restarting the box after updating skein \
                 fixes that"
            )),
        "the workshop box did not say its own reason:\n{err}"
    );
    assert!(
        !err.contains("started before skein's job queue"),
        "the workshop box was told it started before the queue:\n{err}"
    );
    assert_eq!(err.matches(UNQUEUED).count(), 1, "not one reason:\n{err}");
}

/// One way to take something away from the wrapper's environment.
type Edit<'a> = Box<dyn FnOnce(&mut Command) + 'a>;

/// **Each other way the queue is skipped says its own reason**: no `SKEIN_BOX`, no fleet at
/// `SKEIN_FLEET_ROOT`, and no `SKEIN_STATE` to read skein's answer from. The last of these used to
/// file the job and wait 30 s for a view it could not find; the bound here is 10 s.
///
/// What would make it fail: the reasons worded alike, or one of the three checks gone (the
/// sabotage that was run: the `SKEIN_STATE` check taken out, and that run files its job and is
/// killed at 10 s waiting for an answer it cannot read).
#[cfg(target_os = "linux")]
#[test]
fn each_way_a_job_skips_the_queue_says_its_own_reason() {
    let fleet = Fleet::new(
        "skein-job-skips",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        4,
    );
    let nowhere = fleet.dir.path().join("no-fleet-here");
    let home = fleet.dir.path().join("home");
    let cases: [(&str, Edit); 3] = [
        (
            "SKEIN_BOX is not set, so skein cannot tell which box this is",
            Box::new(|c| {
                c.env_remove("SKEIN_BOX");
            }),
        ),
        (
            &format!("there is no skein fleet at {}", nowhere.display()),
            Box::new(|c| {
                c.env("SKEIN_FLEET_ROOT", &nowhere).env("SKEIN_HOME", &home);
            }),
        ),
        (
            "SKEIN_STATE is not set, so skein's answer could not be read",
            Box::new(|c| {
                c.env_remove("SKEIN_STATE");
            }),
        ),
    ];
    for (why, edit) in cases {
        let (out, err) = ran_with(&fleet, edit);
        assert_eq!(out.trim(), "ran", "{err}");
        assert!(
            err.lines().any(|l| l == format!("{UNQUEUED}{why}")),
            "not {why:?}:\n{err}"
        );
        assert_eq!(err.matches(UNQUEUED).count(), 1, "not one reason:\n{err}");
    }
    let filed = fs::read_dir(fleet.places.requests.join("web-main"))
        .unwrap()
        .count();
    assert_eq!(filed, 0, "a job that skipped the queue filed in it");
}

/// **A job the queue granted says nothing of skipping it**, and still ends saying what it asked.
///
/// What would make it fail: the line said whatever the drop-box (the sabotage that was run: the
/// `say` made unconditional, and the granted run says `not waiting for a fleet slot: None`).
#[cfg(target_os = "linux")]
#[test]
fn a_queued_job_does_not_say_it_skipped_the_queue() {
    let fleet = Fleet::new(
        "skein-job-queued",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        4,
    );
    let (out, err) = ran_with(&fleet, |_| {});
    assert_eq!(out.trim(), "ran", "{err}");
    assert!(
        err.lines()
            .any(|l| l.starts_with("skein-job: asked 3 cores, used ")),
        "the job was not granted, so this proves nothing about a queued one:\n{err}"
    );
    assert!(!err.contains(UNQUEUED), "{err}");
}

/// **A nested call stays silent, even in a box with no drop-box.** It runs on its parent's cores
/// by design (SYNAP-517), so it skipped no queue and has nothing to explain: the outer call
/// already said it if there was anything to say.
///
/// What would make it fail: the drop-box check made before the nested call's early `exec` (the
/// sabotage that was run: the `drop_box()` call and its `say` moved above it, and the nested call
/// says `not waiting for a fleet slot`).
#[cfg(target_os = "linux")]
#[test]
fn a_nested_call_says_nothing_about_the_queue() {
    let fleet = Fleet::new(
        "skein-job-nested-quiet",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        4,
    );
    fs::remove_dir(fleet.places.requests.join("web-main")).unwrap();
    let (out, err) = ran_with(&fleet, |c| {
        c.env("SKEIN_JOB_ID", "j-outer");
    });
    assert_eq!(out.trim(), "ran", "{err}");
    assert_eq!(err, "", "a nested call said something");
}

/// **A job ends with one line saying what it asked for and what it used, as skein measured it
/// from the job's own cgroup** (SKEIN-1421).
///
/// The box's cgroups are plain files in a scratch directory, laid out as the launcher lays them
/// out (a leaf, `cpu` and `memory` handed on), so skein's real tick makes the job's cgroup and
/// reads it back. The kernel's counters are what the job writes into that cgroup itself before it
/// ends: 6 CPU-seconds and 1.5 GB at most. The wrapper must print the line from skein's reading,
/// with the 1.5 GB in it, which its own count of its children cannot know.
///
/// What would make it fail: the wrapper not printing the line (the sabotage that was run: the
/// `say(said_used(…))` call taken out, and no line starts `asked 2 cores`), not waiting for
/// skein's measurement (the line has no `GB peak`: the wrapper's own count has no memory), or
/// skein's view of an ended job without `used`.
#[cfg(target_os = "linux")]
#[test]
fn a_job_ends_with_what_it_asked_for_and_what_it_used() {
    let cgroups = Scratch::temp("skein-job-used-cg");
    let boxcg = cgroups.path().join("web-main");
    fs::create_dir_all(boxcg.join("box")).unwrap();
    fs::write(boxcg.join("cgroup.subtree_control"), "cpu memory\n").unwrap();
    fs::write(boxcg.join("cgroup.procs"), "").unwrap();
    fs::write(boxcg.join("box/cgroup.procs"), "").unwrap();
    let fleet = Fleet::new(
        "skein-job-used",
        "web-main",
        cgroups.path().to_path_buf(),
        4,
    );
    // The job writes what the kernel would have counted into its own cgroup, found by its job id.
    let probe = format!(
        "cg={}/job-$SKEIN_JOB_ID; printf 'usage_usec 6000000\\n' > \"$cg/cpu.stat\" && \
         printf '%s\\n' {} > \"$cg/memory.peak\" && echo wrote",
        boxcg.display(),
        3u64 << 29
    );
    let child = fleet
        .wrapper()
        .args(["run", "--class", "test", "--", "sh", "-c", &probe])
        .process_group(0)
        .spawn()
        .unwrap();
    // In the box, as the kernel would say once its shell started it.
    fs::write(boxcg.join("box/cgroup.procs"), format!("{}\n", child.id())).unwrap();
    let (out, err) = finish_within(child, Duration::from_secs(20)).unwrap();
    assert_eq!(
        out.trim(),
        "wrote",
        "the job did not run in a cgroup of its own:\n{err}"
    );
    let line = err
        .lines()
        .find_map(|l| l.strip_prefix("skein-job: asked 2 cores, used "))
        .unwrap_or_else(|| panic!("no line of what it used:\n{err}"));
    assert!(
        line.contains(" avg") && line.contains(", 1.5 GB peak, ") && line.ends_with('s'),
        "the line is not skein's measurement: {line:?}\n{err}"
    );
}

/// A cgroup subtree of the test's own, `/sys/fs/cgroup/skein-jobcg-test-<pid>`, at the top of the
/// cgroup v2 hierarchy and never under `/sys/fs/cgroup/skein`, where a fleet's boxes are. It stands
/// in for `skein`: under it is one box, `demo`, laid out as the launcher lays a box out, its own
/// processes in the leaf `demo/box` and `cpu` and `memory` handed on to its children. Everything in
/// it is killed and removed on drop.
///
/// Made with `sudo -n`, as the launcher and skein-server make theirs; `Err` with the reason where it
/// cannot be, which is everywhere a test cannot write cgroupfs: inside a box (a user namespace in
/// which `sudo` cannot work), and on a host without passwordless `sudo`.
struct OwnCgroups {
    root: PathBuf,
}

fn sudo_sh(script: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new("sudo")
        .args(["-n", "sh", "-c", script, "_"])
        .args(args)
        .output()
        .map_err(|e| format!("sudo: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()),
    }
}

impl OwnCgroups {
    fn make() -> Result<OwnCgroups, String> {
        let top = PathBuf::from("/sys/fs/cgroup");
        let controllers = fs::read_to_string(top.join("cgroup.subtree_control"))
            .map_err(|e| format!("no cgroup v2 hierarchy at /sys/fs/cgroup ({e})"))?;
        for want in ["cpu", "memory"] {
            if !controllers.split_whitespace().any(|c| c == want) {
                return Err(format!(
                    "the cgroup hierarchy's root does not hand on `{want}` ({controllers:?})"
                ));
            }
        }
        let root = top.join(format!("skein-jobcg-test-{}", std::process::id()));
        let boxcg = root.join("demo");
        // Made before the first write, so a make that fails half way is removed on the way out.
        let made = OwnCgroups { root: root.clone() };
        sudo_sh(
            "mkdir -p \"$1/box\" && echo +cpu > \"$2/cgroup.subtree_control\" \
             && echo +memory > \"$2/cgroup.subtree_control\" \
             && echo +cpu > \"$1/cgroup.subtree_control\" \
             && echo +memory > \"$1/cgroup.subtree_control\"",
            &[&boxcg.to_string_lossy(), &root.to_string_lossy()],
        )
        .map_err(|e| format!("cannot make a cgroup of this test's own with `sudo -n`: {e}"))?;
        Ok(made)
    }

    fn leaf(&self) -> PathBuf {
        self.root.join("demo/box")
    }
}

impl Drop for OwnCgroups {
    fn drop(&mut self) {
        let root = self.root.to_string_lossy().to_string();
        let _ = sudo_sh("echo 1 > \"$1/cgroup.kill\"", &[&root]);
        for _ in 0..50 {
            let _ = sudo_sh(
                "find \"$1\" -depth -type d -exec rmdir {} + 2>/dev/null",
                &[&root],
            );
            if !self.root.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Wait at most `bound` for `ok`, saying `what` if it never holds.
fn eventually<T>(bound: Duration, what: &str, mut ok: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(v) = ok() {
            return v;
        }
        assert!(Instant::now() < deadline, "{what}, within {bound:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// **A granted job runs in a cgroup of its own under its box's, weighted and never capped; a kill
/// of the box's cgroup still ends it; and once it has ended, what it used is read back and its
/// cgroup removed** (SKEIN-1419). On real cgroups, through the real scheduler and the shipped
/// wrapper.
///
/// The wrapper starts in the box's leaf, as one a box's shell started does. The job's command
/// reports its own `/proc/self/cgroup` before it does anything else, so where it says it is is
/// where the tick put the wrapper **before** the wrapper read `granted`. The kernel's own `cpu.max`
/// for the job's cgroup is read back, and must be the uncapped `max` it makes every cgroup with
/// (SKEIN-1469). The box's cgroup is killed as `fleet::box_cgroup_kill` kills it, at the box's
/// level and not the job's, and the job's processes must be gone. The scheduler then sees the
/// wrapper gone, reads what the job used, and removes its cgroup.
///
/// What would make it fail: no placement in the tick's grant path, or one after the view says
/// `granted` (the command reports the leaf: `/…/demo/box`); any `cpu.max` written for the job;
/// a job cgroup made outside the box's (the kill misses it and `sleep` survives); the end path not
/// reading the job's cgroup or not removing it. **Not run inside a box**, which has no cgroup it
/// can write: there it skips and says so, and CI's runner, which has `sudo`, is where it runs.
#[cfg(target_os = "linux")]
#[test]
fn a_granted_job_has_its_own_cgroup_is_killed_with_its_box_and_is_measured_at_the_end() {
    let cgroups = match OwnCgroups::make() {
        Ok(c) => c,
        Err(why) => {
            return skip(&format!(
                "{why}, so a job's own cgroup was NOT checked on real cgroups"
            ))
        }
    };
    let fleet = Fleet::new("skein-job-own-cgroup", "demo", cgroups.root.clone(), 1);
    let said = fleet.dir.path().join("job-said");
    let probe = format!(
        "{{ echo \"pid $$\"; cat /proc/self/cgroup; }} > {}.tmp && mv {0}.tmp {0}; exec sleep 120",
        said.display()
    );
    // The wrapper as `fleet.wrapper()` would run it, started from a shell that first moves itself
    // into the box's leaf, so the wrapper begins where a box's own process does.
    let c = fleet.wrapper();
    let wrapper = c.get_args().next().unwrap().to_owned();
    let mut into_leaf = Command::new("sh");
    into_leaf
        .arg("-c")
        .arg(
            "echo $$ | sudo -n tee \"$1/cgroup.procs\" >/dev/null || exit 97; shift; \
             exec python3 \"$@\"",
        )
        .arg("_")
        .arg(cgroups.leaf())
        .arg(wrapper)
        .args(["run", "--class", "test", "--", "sh", "-c", &probe]);
    for (k, v) in c.get_envs() {
        match v {
            Some(v) => into_leaf.env(k, v),
            None => into_leaf.env_remove(k),
        };
    }
    let mut child = into_leaf
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();

    let report = eventually(Duration::from_secs(20), "the job never ran", || {
        fs::read_to_string(&said).ok()
    });
    let id = fleet
        .queue()
        .jobs
        .iter()
        .find(|j| j.state == "granted")
        .map(|j| j.id.clone())
        .unwrap_or_else(|| panic!("no granted job: {:#?}", fleet.queue()));
    let job_cg = skein::jobs::cgroup::job_cgroup(&cgroups.root, "demo", &id);
    let rel = job_cg.strip_prefix("/sys/fs/cgroup").unwrap();
    assert!(
        report
            .lines()
            .any(|l| l == format!("0::/{}", rel.display())),
        "the job did not start in a cgroup of its own, {}:\n{report}",
        job_cg.display()
    );
    let job_pid: u32 = report
        .lines()
        .find_map(|l| l.strip_prefix("pid "))
        .and_then(|p| p.parse().ok())
        .expect("the job said no pid");

    // (a) no cap, though the one-core machine granted one core: the kernel's own `max`.
    assert_eq!(
        fs::read_to_string(job_cg.join("cpu.max"))
            .unwrap()
            .split_whitespace()
            .next(),
        Some("max"),
        "the job's cgroup was capped"
    );
    let procs = fs::read_to_string(job_cg.join("cgroup.procs")).unwrap();
    assert!(procs.lines().any(|l| l == job_pid.to_string()), "{procs}");
    assert!(Path::new(&format!("/proc/{job_pid}")).exists());

    // (b) the box's cgroup killed, as a stop kills it.
    sudo_sh(
        "echo 1 > \"$1/cgroup.kill\"",
        &[&cgroups.root.join("demo").to_string_lossy()],
    )
    .unwrap();
    eventually(
        Duration::from_secs(5),
        "the wrapper survived its box's kill",
        || child.try_wait().unwrap(),
    );
    eventually(
        Duration::from_secs(5),
        "the job's process survived its box's kill",
        || (!Path::new(&format!("/proc/{job_pid}")).exists()).then_some(()),
    );

    // (c) measured, then removed.
    let ended = eventually(
        Duration::from_secs(10),
        "the scheduler never measured the job's end",
        || {
            fleet
                .queue()
                .jobs
                .into_iter()
                .find(|j| j.id == id && j.state == "gone" && j.used.is_some())
        },
    );
    assert!(
        !job_cg.exists(),
        "the job's cgroup was left behind: {:?}",
        ended.used
    );
}

// ─────────── the owner's stop (SKEIN-1422) ───────────

/// The job of the given state, once the scheduler shows one.
fn job_in(fleet: &Fleet, state: &str) -> skein::jobs::Job {
    eventually(
        Duration::from_secs(20),
        &format!("no job read `{state}`"),
        || fleet.queue().jobs.into_iter().find(|j| j.state == state),
    )
}

/// What a finished wrapper said and how it ended.
fn ended(child: std::process::Child, bound: Duration) -> (Option<i32>, String) {
    let mut child = child;
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if started.elapsed() > bound {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
            let _ = child.wait();
            panic!("the wrapper was still running after {bound:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut err = String::new();
    let _ = child.stderr.take().unwrap().read_to_string(&mut err);
    (status.code(), err)
}

/// **A waiting job the owner stops leaves the queue, and its wrapper says the owner stopped it and
/// exits non-zero, without running its command.**
///
/// One core. The first wrapper holds it with a `sleep`; the second waits behind it. The owner
/// stops the second, as the cockpit's press does (`jobs::ask_owner_stop`), and its wrapper must end
/// within a few ticks, exit 137, and say `stopped by you (the owner) from the cockpit` on stderr.
/// Its command, which would write a file, never ran.
///
/// What would make it fail: the wrapper not reading `stopped` in its view (the sabotage that was
/// run: its `state == "stopped"` branch removed, and it says "skein ended this job before it
/// started (stopped)" and exits 75); or the tick not ending the job (it waits on to the bound).
#[cfg(target_os = "linux")]
#[test]
fn a_waiting_job_the_owner_stops_says_so_and_never_runs() {
    let fleet = Fleet::new(
        "skein-job-stop-waiting",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        1,
    );
    let holder = fleet
        .wrapper()
        .args(["run", "--cores", "1", "--", "sleep", "60"])
        .process_group(0)
        .spawn()
        .unwrap();
    let first = job_in(&fleet, "granted");
    let ran = fleet.dir.path().join("it-ran");
    // A lock of its own: on the box's one lock it would wait behind the first before it ever
    // reached the queue.
    let waiter = fleet
        .wrapper()
        .env("SKEIN_JOB_LOCK", fleet.dir.path().join("second.lock"))
        .args(["run", "--cores", "1", "--", "touch"])
        .arg(&ran)
        .process_group(0)
        .spawn()
        .unwrap();
    let second = job_in(&fleet, "waiting");
    assert_ne!(first.id, second.id);

    skein::jobs::ask_owner_stop(&fleet.places, "web-main", &second.id).unwrap();
    let (code, err) = ended(waiter, Duration::from_secs(15));
    assert_eq!(code, Some(137), "{err}");
    assert!(
        err.contains("stopped by you (the owner) from the cockpit; the command did not run"),
        "{err}"
    );
    assert!(!ran.exists(), "the stopped job's command ran");

    skein::jobs::ask_owner_stop(&fleet.places, "web-main", &first.id).unwrap();
    let (code, err) = ended(holder, Duration::from_secs(20));
    assert_eq!(code, Some(137), "{err}");
}

/// **A running job the owner stops has its command killed, and its wrapper lives to say the owner
/// stopped it, and exits non-zero.**
///
/// A box with no cgroup of its own (the root does not exist), so the stop takes the path a box
/// without job cgroups has: every process under the shipped wrapper, which skein confirms is that
/// box's `skein-job` by its environment and command. The command reports its pid and sleeps; after
/// the stop that pid is gone, and the wrapper exits 137 saying `stopped by you (the owner) from the
/// cockpit; its processes were killed`.
///
/// What would make it fail: the stop killing nothing (the command sleeps on and the wrapper runs
/// to the bound), killing the wrapper as well (it exits by a signal and says nothing: the sabotage
/// that was run was `kill_under` taking the wrapper's pid too, and the code is `None`), or the
/// wrapper not looking for skein's mark after its command was killed (the sabotage that was run:
/// the wrapper's look at its view after a SIGKILL removed, and it exits 137 with no line saying
/// who stopped it).
#[cfg(target_os = "linux")]
#[test]
fn a_running_job_the_owner_stops_is_killed_and_its_wrapper_says_so() {
    let fleet = Fleet::new(
        "skein-job-stop-running",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        2,
    );
    let said = fleet.dir.path().join("job-said");
    let probe = format!(
        "echo $$ > {0}.tmp && mv {0}.tmp {0}; exec sleep 120",
        said.display()
    );
    let wrapper = fleet
        .wrapper()
        .args(["run", "--cores", "1", "--", "sh", "-c", &probe])
        .process_group(0)
        .spawn()
        .unwrap();
    let pid: u32 = eventually(Duration::from_secs(20), "the job never ran", || {
        fs::read_to_string(&said).ok()?.trim().parse().ok()
    });
    let job = job_in(&fleet, "granted");
    assert!(Path::new(&format!("/proc/{pid}")).exists());

    skein::jobs::ask_owner_stop(&fleet.places, "web-main", &job.id).unwrap();
    let (code, err) = ended(wrapper, Duration::from_secs(20));
    assert_eq!(code, Some(137), "{err}");
    assert!(
        err.contains("stopped by you (the owner) from the cockpit; its processes were killed"),
        "{err}"
    );
    eventually(
        Duration::from_secs(5),
        "the job's command survived the stop",
        || (!Path::new(&format!("/proc/{pid}")).exists()).then_some(()),
    );
    let q = fleet.queue();
    let stopped = q.jobs.iter().find(|j| j.id == job.id).unwrap();
    assert_eq!(stopped.state, "stopped", "{q:#?}");
}
