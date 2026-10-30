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
use std::path::PathBuf;
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
    /// Box `name`, whose cgroups are under `cgroups`, with `slots` fleet slots.
    fn new(prefix: &str, name: &str, cgroups: PathBuf, slots: usize) -> Fleet {
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
                    fleet_slots: slots,
                    repo_cap: 2,
                };
                while !stop.load(Ordering::Relaxed) {
                    let _ =
                        skein::jobs::tick(&places, limits, &|b| b.to_string(), chrono::Utc::now());
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
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
        1,
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
/// outer job holds the box's lock and the fleet's one slot, and an inner one that queued for
/// either would wait for ever behind its own parent.
///
/// The fleet here has ONE slot and the scheduler is running, so an inner call that asked for a
/// slot would be queued behind the outer job and wait the full 30 s before giving up; the bound is
/// 5 s.
///
/// What would make it fail: the wrapper ignoring `SKEIN_JOB_ID` (the sabotage that was run: the
/// inner job files a request, reads `fleet full 1/1`, and the run is killed at 5 s), or the child
/// not being given `SKEIN_JOB_ID`.
#[cfg(target_os = "linux")]
#[test]
fn a_nested_skein_job_runs_without_waiting() {
    // No cgroup to read: liveness falls back to the pid existing, which is the real pid here.
    let fleet = Fleet::new(
        "skein-job-nested",
        "web-main",
        PathBuf::from("/nonexistent-cgroup-root"),
        1,
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
        1,
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
