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

/// **A granted job runs in a cgroup of its own under its box's, capped at the cores it is given; a
/// kill of the box's cgroup still ends it; and once it has ended, what it used is read back and its
/// cgroup removed** (SKEIN-1419). On real cgroups, through the real scheduler and the shipped
/// wrapper.
///
/// The wrapper starts in the box's leaf, as one a box's shell started does. The job's command
/// reports its own `/proc/self/cgroup` before it does anything else, so where it says it is is
/// where the tick put the wrapper **before** the wrapper read `granted`. The cap is then placed on
/// the running job's cgroup with cores given, as the queue will once it grants cores, and read back
/// from the kernel. The box's cgroup is killed as `fleet::box_cgroup_kill` kills it, at the box's
/// level and not the job's, and the job's processes must be gone. The scheduler then sees the
/// wrapper gone, reads what the job used, and removes its cgroup.
///
/// What would make it fail: no placement in the tick's grant path, or one after the view says
/// `granted` (the command reports the leaf: `/…/demo/box`); `cpu.max` not written for given cores;
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
    let wrapper_pid = child.id();

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

    // (a) the cap, for cores given.
    skein::jobs::cgroup::place(&cgroups.root, "demo", &id, wrapper_pid, Some(1.5))
        .expect("placing the running job again, with cores");
    assert_eq!(
        fs::read_to_string(job_cg.join("cpu.max")).unwrap().trim(),
        "150000 100000"
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
                .find(|j| j.id == id && j.state == "gone" && j.reason.starts_with("used "))
        },
    );
    assert!(
        !job_cg.exists(),
        "the job's cgroup was left behind: {}",
        ended.reason
    );
}
