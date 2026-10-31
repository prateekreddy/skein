//! A granted job's own cgroup, under its box's (SKEIN-1419), so skein can cap and measure that one
//! job.
//!
//! **The shape.** A box's cgroup is `<cgroups>/<box>` (`fleet::box_cgroup`), and it carries the
//! box's memory ceiling and floor. cgroup v2 lets a cgroup hand a controller to its children only
//! while it holds no processes itself, so the launcher puts the box's own processes in a leaf,
//! `<box>/box` ([`BOX_LEAF`]), and enables `cpu` and `memory` in `<box>/cgroup.subtree_control`
//! (`src/box-session.sh`, the per-box cgroup block). A granted job gets `<box>/job-<id>` beside that
//! leaf ([`job_cgroup`]). Everything the box's cgroup does for the whole box still holds, because a
//! cgroup's limits, counters and `cgroup.kill` all cover its descendants: the ceiling bounds the box
//! and its jobs together, `memory.current` and `cpu.stat` count them together, and a stop kills
//! them together.
//!
//! **Moved before it runs.** skein-server moves the wrapper's pid into the job's cgroup
//! ([`place`]) in the same tick that grants the job and before it writes `granted` into the box's
//! view, and the wrapper starts its command only once it reads `granted` (`src/plugin/bin/skein-job`).
//! The command is the wrapper's child, so it starts in the job's cgroup, and so does everything it
//! forks. Boxes share the sandbox's pid namespace (`box-session.sh` runs bwrap without
//! `--unshare-pid`), so the pid the wrapper writes in its request is the pid skein writes into
//! `cgroup.procs`.
//!
//! **Either a whole job cgroup or none.** [`place`] refuses before it makes anything when the box
//! has no leaf (a box with no cgroup delegation, or one started before this) or when its cgroup does
//! not hand on the `cpu` controller, and the job then runs in the box's own cgroup, as every job did
//! before, with the reason in its view. The limits are written before the pid is moved, so a job
//! never runs for a moment in a cgroup of its own with no limit on it.
//!
//! **Measured at the end, then removed.** [`finish`] reads `cpu.stat`'s `usage_usec` and
//! `memory.peak` from the job's cgroup and removes it. [`Rates`] turns two readings of a running job
//! into the cores it is using now.
//!
//! **Written as skein-server, through `sudo` where it must.** The box cgroups are root's, made by the
//! launcher with `sudo`, and skein-server runs in the sandbox where `sudo -n` works, as
//! `fleet::box_cgroup_kill` does. A write is tried directly first, so a cgroup tree a test owns is
//! written without `sudo`, and `sudo -n` is only asked when the direct write is refused.

use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The leaf a box's own processes live in: its agent, its shells, its tmux server.
pub const BOX_LEAF: &str = "box";

/// The box leaf's `cpu.weight`, against its own jobs. A weight only decides anything among siblings,
/// so these numbers share out the CPU that the box's cgroup gets, and never the fleet's.
///
/// **400 for the box, 100 a granted core for a job, and a job never above [`JOB_WEIGHT_MAX`].** The
/// box leaf is somebody's terminal: an agent turn and a shell, mostly idle, and slow to the person
/// the moment a build in the same box takes every core. With the leaf at 400 and a one-core job at
/// 100, the box's own processes get four fifths of the box's CPU whenever both want it, which is
/// more than an idle agent and its shells ever use, so they stay responsive. A job's weight is
/// capped below the leaf's, so no single job, however many cores it was granted, outweighs the
/// person; 300 still lets a three-core job take three times a one-core job's share beside it. The
/// cap on a job's speed is `cpu.max`, not this: a weight costs nothing while the machine is quiet.
pub const BOX_LEAF_WEIGHT: u32 = 400;

/// A job's `cpu.weight` for each core it was granted. See [`BOX_LEAF_WEIGHT`].
pub const WEIGHT_PER_CORE: f64 = 100.0;

/// The most `cpu.weight` a job gets, below [`BOX_LEAF_WEIGHT`].
pub const JOB_WEIGHT_MAX: u32 = 300;
const _: () = assert!(JOB_WEIGHT_MAX < BOX_LEAF_WEIGHT);

/// The `cpu.max` period, in microseconds: the kernel's default.
const PERIOD_USEC: u64 = 100_000;

/// Where box `box_name`'s job `id` has its cgroup.
pub fn job_cgroup(cgroups: &Path, box_name: &str, id: &str) -> PathBuf {
    cgroups.join(box_name).join(format!("job-{id}"))
}

/// The `cpu.weight` for a job granted `cores`, or one core's weight when no cores were given.
pub fn weight_for(cores: Option<f64>) -> u32 {
    let want = (cores.unwrap_or(1.0).max(0.0) * WEIGHT_PER_CORE).round() as u32;
    want.clamp(1, JOB_WEIGHT_MAX)
}

/// `cpu.max` for `cores`: that many periods' worth of CPU in each period.
pub fn cpu_max_for(cores: f64) -> String {
    // The kernel refuses a quota under 1 ms.
    let quota = ((cores.max(0.0) * PERIOD_USEC as f64).round() as u64).max(1_000);
    format!("{quota} {PERIOD_USEC}")
}

/// What a job's cgroup counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// CPU time, every process of the job together: `cpu.stat`'s `usage_usec`.
    pub cpu_usec: u64,
    /// The most memory the job held at once: `memory.peak`. `None` where the box's cgroup does not
    /// hand on the `memory` controller, or the kernel is older than `memory.peak` (5.19).
    pub memory_peak: Option<u64>,
}

impl Usage {
    /// The cores used on average over `secs` seconds of running.
    pub fn average_cores(&self, secs: i64) -> Option<f64> {
        (secs > 0).then(|| self.cpu_usec as f64 / 1e6 / secs as f64)
    }
}

/// What a cgroup directory has counted so far. `None` when there is no `cpu.stat` to read.
pub fn usage(dir: &Path) -> Option<Usage> {
    let stat = std::fs::read_to_string(dir.join("cpu.stat")).ok()?;
    let cpu_usec = stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|v| v.trim().parse().ok())?;
    let memory_peak = std::fs::read_to_string(dir.join("memory.peak"))
        .ok()
        .and_then(|v| v.trim().parse().ok());
    Some(Usage {
        cpu_usec,
        memory_peak,
    })
}

/// The cores a running job is using now, from two readings of its `usage_usec`. `None` when the
/// readings are not in order, or the counter went backwards (a cgroup made again under the same
/// name).
pub fn cores_between(
    then_usec: u64,
    then: DateTime<Utc>,
    now_usec: u64,
    now: DateTime<Utc>,
) -> Option<f64> {
    let micros = (now - then).num_microseconds()?;
    (micros > 0 && now_usec >= then_usec).then(|| (now_usec - then_usec) as f64 / micros as f64)
}

/// The live CPU rate of each running job, kept between ticks by whoever ticks: the last reading of
/// each job's cgroup, and the rate between it and the one before.
#[derive(Debug, Default, Clone)]
pub struct Rates {
    last: BTreeMap<(String, String), (u64, DateTime<Utc>)>,
}

impl Rates {
    /// Read box `box_name`'s job `id` at `now`, and return the cores it used since the last reading.
    /// `None` on the first reading, and for a job with no cgroup of its own.
    pub fn observe(
        &mut self,
        cgroups: &Path,
        box_name: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Option<f64> {
        let key = (box_name.to_string(), id.to_string());
        let Some(read) = usage(&job_cgroup(cgroups, box_name, id)) else {
            self.last.remove(&key);
            return None;
        };
        let prev = self.last.insert(key, (read.cpu_usec, now));
        prev.and_then(|(usec, at)| cores_between(usec, at, read.cpu_usec, now))
    }

    /// Stop following a job that ended.
    pub fn forget(&mut self, box_name: &str, id: &str) {
        self.last.remove(&(box_name.to_string(), id.to_string()));
    }
}

// ───────────────────────────── writing cgroupfs ─────────────────────────────

/// The three writes this module makes. [`Kernel`] in skein; a test of the logic names its own.
pub(crate) trait Cgroupfs {
    fn mkdir(&self, dir: &Path) -> io::Result<()>;
    fn write(&self, file: &Path, value: &str) -> io::Result<()>;
    fn rmdir(&self, dir: &Path) -> io::Result<()>;
}

/// The real cgroupfs: a direct write first, and `sudo -n` only when that is refused.
pub(crate) struct Kernel;

fn sudo(args: &[&str]) -> io::Result<()> {
    let out = Command::new("sudo").arg("-n").args(args).output()?;
    match out.status.success() {
        true => Ok(()),
        false => Err(io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )),
    }
}

fn denied(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
}

impl Cgroupfs for Kernel {
    fn mkdir(&self, dir: &Path) -> io::Result<()> {
        match std::fs::create_dir(dir) {
            Err(e) if denied(&e) => sudo(&["mkdir", "--", &dir.to_string_lossy()]),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            r => r,
        }
    }

    fn write(&self, file: &Path, value: &str) -> io::Result<()> {
        match std::fs::write(file, format!("{value}\n")) {
            Err(e) if denied(&e) => sudo(&[
                "sh",
                "-c",
                "printf '%s\\n' \"$1\" > \"$2\"",
                "_",
                value,
                &file.to_string_lossy(),
            ]),
            r => r,
        }
    }

    fn rmdir(&self, dir: &Path) -> io::Result<()> {
        match std::fs::remove_dir(dir) {
            Err(e) if denied(&e) => sudo(&["rmdir", "--", &dir.to_string_lossy()]),
            r => r,
        }
    }
}

// ───────────────────────────── a job's cgroup ─────────────────────────────

/// Move box `box_name`'s wrapper `pid`, for job `id`, into a cgroup of the job's own, with
/// `cpu.max` for `cores` when cores are given and a `cpu.weight` for them either way. Returns the
/// job's cgroup, or why the job runs in its box's cgroup instead, in words for the job's view.
pub fn place(
    cgroups: &Path,
    box_name: &str,
    id: &str,
    pid: u32,
    cores: Option<f64>,
) -> Result<PathBuf, String> {
    place_with(&Kernel, cgroups, box_name, id, pid, cores)
}

pub(crate) fn place_with(
    fs: &dyn Cgroupfs,
    cgroups: &Path,
    box_name: &str,
    id: &str,
    pid: u32,
    cores: Option<f64>,
) -> Result<PathBuf, String> {
    let boxcg = cgroups.join(box_name);
    if !boxcg.is_dir() {
        return Err(
            "this box has no cgroup, because this sandbox does not delegate cgroups".into(),
        );
    }
    if !boxcg.join(BOX_LEAF).is_dir() {
        return Err(
            "this box was started before jobs had cgroups of their own; restarting the box gives \
             them one"
                .into(),
        );
    }
    let handed_on =
        std::fs::read_to_string(boxcg.join("cgroup.subtree_control")).unwrap_or_default();
    let has = |c: &str| handed_on.split_whitespace().any(|w| w == c);
    if !has("cpu") {
        return Err("this box's cgroup does not hand the cpu controller on to its jobs".into());
    }
    let dir = job_cgroup(cgroups, box_name, id);
    let made = |e: io::Error| format!("could not make its cgroup: {e}");
    fs.mkdir(&dir).map_err(made)?;
    let mut writes = vec![("cpu.weight", weight_for(cores).to_string())];
    if let Some(c) = cores {
        writes.push(("cpu.max", cpu_max_for(c)));
    }
    // The box's memory floor reaches its children only through theirs: with the controller handed
    // on, a child's `memory.min` of 0 is protected by none of its parent's (unless cgroupfs is mounted
    // with recursive memory protection). `max` takes all of the parent's, shared out by usage when
    // the children together want more than it, which is the box's floor covering its jobs as it did
    // when they ran in its own cgroup.
    if has("memory") {
        writes.push(("memory.min", "max".into()));
    }
    writes.push(("cgroup.procs", pid.to_string()));
    for (file, value) in writes {
        if let Err(e) = fs.write(&dir.join(file), &value) {
            let _ = fs.rmdir(&dir);
            return Err(format!("could not set {file} on its cgroup: {e}"));
        }
    }
    Ok(dir)
}

/// Read what box `box_name`'s job `id` used, then remove its cgroup. `None` when the job had no
/// cgroup of its own. The cgroup is left where the kernel refuses to remove it, which is while a
/// process is still in it; [`sweep`] tries again once it is empty.
pub fn finish(cgroups: &Path, box_name: &str, id: &str) -> Option<Usage> {
    finish_with(&Kernel, cgroups, box_name, id)
}

pub(crate) fn finish_with(
    fs: &dyn Cgroupfs,
    cgroups: &Path,
    box_name: &str,
    id: &str,
) -> Option<Usage> {
    let dir = job_cgroup(cgroups, box_name, id);
    if !dir.is_dir() {
        return None;
    }
    let used = usage(&dir);
    let _ = fs.rmdir(&dir);
    used
}

/// Remove box `box_name`'s job cgroups that name no job in `active` and that the kernel says are
/// empty: left by a job whose processes outlived its wrapper, or by a tick that never saw it end.
/// A cgroup that does not say it is empty (`cgroup.events` `populated 0`) is not tried, so a
/// populated one costs no `sudo` every tick.
pub fn sweep(cgroups: &Path, box_name: &str, active: &dyn Fn(&str) -> bool) {
    sweep_with(&Kernel, cgroups, box_name, active)
}

pub(crate) fn sweep_with(
    fs: &dyn Cgroupfs,
    cgroups: &Path,
    box_name: &str,
    active: &dyn Fn(&str) -> bool,
) {
    let Ok(entries) = std::fs::read_dir(cgroups.join(box_name)) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix("job-"))
            .map(str::to_string)
        else {
            continue;
        };
        if active(&id) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let empty = std::fs::read_to_string(entry.path().join("cgroup.events"))
            .is_ok_and(|t| t.lines().any(|l| l.trim() == "populated 0"));
        if empty {
            let _ = fs.rmdir(&entry.path());
        }
    }
}

/// Whether `pid` is anywhere in the cgroup at `dir` or in one under it. `None` when `dir`'s own
/// `cgroup.procs` cannot be read.
pub fn pid_in_subtree(dir: &Path, pid: u32) -> Option<bool> {
    let procs = std::fs::read_to_string(dir.join("cgroup.procs")).ok()?;
    let want = pid.to_string();
    if procs.lines().any(|l| l.trim() == want) {
        return Some(true);
    }
    let found = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .any(|e| pid_in_subtree(&e.path(), pid) == Some(true));
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A cgroup tree in a scratch directory, written as plain files, where `rmdir` takes the
    /// directory and what the fixture wrote in it, as cgroupfs's own `rmdir` does.
    #[derive(Default)]
    struct Fake {
        refuse: Option<&'static str>,
        wrote: RefCell<Vec<(PathBuf, String)>>,
    }

    impl Cgroupfs for Fake {
        fn mkdir(&self, dir: &Path) -> io::Result<()> {
            std::fs::create_dir_all(dir)
        }
        fn write(&self, file: &Path, value: &str) -> io::Result<()> {
            if self.refuse.is_some_and(|r| file.ends_with(r)) {
                return Err(io::Error::other("refused by the fixture"));
            }
            self.wrote
                .borrow_mut()
                .push((file.to_path_buf(), value.to_string()));
            std::fs::write(file, format!("{value}\n"))
        }
        fn rmdir(&self, dir: &Path) -> io::Result<()> {
            std::fs::remove_dir_all(dir)
        }
    }

    /// A box `web-main` under `<tmp>/skein`, with its leaf and with `handed_on` in its
    /// `cgroup.subtree_control`.
    fn tree(handed_on: &str, leaf: bool) -> (crate::testutil::TempDir, PathBuf) {
        let dir = crate::testutil::tempdir();
        let cgroups = PathBuf::from(dir.as_ref() as &Path).join("skein");
        let boxcg = cgroups.join("web-main");
        std::fs::create_dir_all(&boxcg).unwrap();
        std::fs::write(boxcg.join("cgroup.subtree_control"), handed_on).unwrap();
        std::fs::write(boxcg.join("cgroup.procs"), "").unwrap();
        if leaf {
            std::fs::create_dir_all(boxcg.join(BOX_LEAF)).unwrap();
            std::fs::write(boxcg.join(BOX_LEAF).join("cgroup.procs"), "4242\n").unwrap();
        }
        (dir, cgroups)
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// **A granted job gets its own cgroup, its limits are set before its pid is moved, and the cap
    /// is the cores it was granted.**
    ///
    /// What would make it fail: `cpu.max` not written when cores are given, or written as anything
    /// but cores × the period; the pid written into the box's cgroup or leaf instead of the job's;
    /// the pid moved before the limits (the order is asserted, because a job moved first runs
    /// uncapped until the cap lands); the box's floor not reaching the job (`memory.min`).
    #[test]
    fn a_placed_job_is_capped_at_its_cores_before_it_is_moved() {
        let (_d, cgroups) = tree("cpu memory", true);
        let fs = Fake::default();
        let dir = place_with(&fs, &cgroups, "web-main", "j-1", 4242, Some(1.5)).unwrap();
        assert_eq!(dir, cgroups.join("web-main/job-j-1"));
        assert_eq!(read(&dir.join("cpu.max")), "150000 100000");
        assert_eq!(read(&dir.join("cpu.weight")), "150");
        assert_eq!(read(&dir.join("memory.min")), "max");
        assert_eq!(read(&dir.join("cgroup.procs")), "4242");
        let order: Vec<String> = fs
            .wrote
            .borrow()
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // The FIRST move, not the last: a pid moved early and again at the end still ran uncapped
        // in between.
        assert_eq!(
            order.iter().position(|f| f == "cgroup.procs"),
            Some(order.len() - 1),
            "the pid was moved before its limits were set: {order:?}"
        );
    }

    /// With no cores given, no cap, and one core's weight.
    #[test]
    fn a_job_with_no_cores_given_is_weighted_as_one_core_and_not_capped() {
        let (_d, cgroups) = tree("cpu", true);
        let dir = place_with(&Fake::default(), &cgroups, "web-main", "j-2", 7, None).unwrap();
        assert!(!dir.join("cpu.max").exists(), "a cap was invented");
        assert_eq!(read(&dir.join("cpu.weight")), "100");
        assert!(
            !dir.join("memory.min").exists(),
            "memory.min written where the memory controller is not handed on"
        );
    }

    /// **No job cgroup, and the reason, wherever the box cannot have one**; and nothing made.
    ///
    /// What would make it fail: placing a job in a box with no leaf (its processes are in the box's
    /// own cgroup, and cgroup v2 refuses a child controller there), or with no `cpu` handed on (a
    /// `cpu.max` that cannot be written), or leaving a half-made cgroup behind a refused write.
    #[test]
    fn a_box_that_cannot_have_job_cgroups_says_why_and_makes_none() {
        let (_d, cgroups) = tree("cpu memory", false);
        let why = place_with(&Fake::default(), &cgroups, "web-main", "j", 1, None).unwrap_err();
        assert!(why.contains("restarting the box"), "{why}");
        let why = place_with(&Fake::default(), &cgroups, "api-main", "j", 1, None).unwrap_err();
        assert!(why.contains("does not delegate"), "{why}");
        let (_d2, cgroups) = tree("memory pids", true);
        let why = place_with(&Fake::default(), &cgroups, "web-main", "j", 1, None).unwrap_err();
        assert!(why.contains("cpu controller"), "{why}");
        assert!(!job_cgroup(&cgroups, "web-main", "j").exists());

        let (_d3, cgroups) = tree("cpu", true);
        let refusing = Fake {
            refuse: Some("cgroup.procs"),
            ..Fake::default()
        };
        let why = place_with(&refusing, &cgroups, "web-main", "j", 1, Some(2.0)).unwrap_err();
        assert!(why.contains("cgroup.procs"), "{why}");
        assert!(
            !job_cgroup(&cgroups, "web-main", "j").exists(),
            "a cgroup the pid never reached was left behind"
        );
    }

    /// **What the job used is read before its cgroup is removed.**
    ///
    /// What would make it fail: the cgroup removed before it is read (the sabotage: the two lines
    /// swapped, and the usage is `None`), or `usage_usec` read from the wrong line of `cpu.stat`.
    #[test]
    fn a_finished_job_is_measured_and_then_removed() {
        let (_d, cgroups) = tree("cpu memory", true);
        let fs = Fake::default();
        let dir = place_with(&fs, &cgroups, "web-main", "j-3", 9, Some(2.0)).unwrap();
        std::fs::write(
            dir.join("cpu.stat"),
            "usage_usec 6000000\nuser_usec 5000000\nsystem_usec 1000000\n",
        )
        .unwrap();
        std::fs::write(dir.join("memory.peak"), "1073741824\n").unwrap();
        let used = finish_with(&fs, &cgroups, "web-main", "j-3").expect("no usage read");
        assert_eq!(
            used,
            Usage {
                cpu_usec: 6_000_000,
                memory_peak: Some(1 << 30)
            }
        );
        assert_eq!(used.average_cores(4), Some(1.5));
        assert!(!dir.exists(), "the job's cgroup was not removed");
        assert_eq!(finish_with(&fs, &cgroups, "web-main", "j-3"), None);
    }

    /// The sweep removes an empty job cgroup that names no running job, and nothing else.
    #[test]
    fn the_sweep_removes_only_empty_cgroups_of_jobs_that_are_not_running() {
        let (_d, cgroups) = tree("cpu", true);
        let fs = Fake::default();
        for (id, populated) in [("j-old", "0"), ("j-busy", "1"), ("j-live", "0")] {
            let dir = place_with(&fs, &cgroups, "web-main", id, 1, None).unwrap();
            std::fs::write(
                dir.join("cgroup.events"),
                format!("populated {populated}\n"),
            )
            .unwrap();
        }
        sweep_with(&fs, &cgroups, "web-main", &|id| id == "j-live");
        assert!(!job_cgroup(&cgroups, "web-main", "j-old").exists());
        assert!(job_cgroup(&cgroups, "web-main", "j-busy").exists());
        assert!(job_cgroup(&cgroups, "web-main", "j-live").exists());
        assert!(cgroups.join("web-main").join(BOX_LEAF).exists());
    }

    /// **A pid in a job's cgroup is still in its box.**
    ///
    /// What would make it fail: reading only the box's own `cgroup.procs`, which is empty once every
    /// process is in the leaf or a job.
    #[test]
    fn a_pid_in_a_job_cgroup_is_in_its_box() {
        let (_d, cgroups) = tree("cpu", true);
        place_with(&Fake::default(), &cgroups, "web-main", "j", 5150, None).unwrap();
        let boxcg = cgroups.join("web-main");
        assert_eq!(
            read(&boxcg.join("cgroup.procs")),
            "",
            "the pid was put in the box itself"
        );
        assert_eq!(pid_in_subtree(&boxcg, 5150), Some(true));
        assert_eq!(pid_in_subtree(&boxcg, 4242), Some(true), "the leaf's pid");
        assert_eq!(pid_in_subtree(&boxcg, 77), Some(false));
        assert_eq!(pid_in_subtree(&cgroups.join("nobox"), 5150), None);
    }

    #[test]
    fn weights_stay_below_the_box_and_caps_follow_the_cores() {
        assert_eq!(weight_for(None), 100);
        assert_eq!(weight_for(Some(0.5)), 50);
        assert_eq!(weight_for(Some(2.0)), 200);
        assert_eq!(weight_for(Some(16.0)), JOB_WEIGHT_MAX);
        assert_eq!(weight_for(Some(0.0)), 1);
        assert_eq!(cpu_max_for(4.0), "400000 100000");
        assert_eq!(cpu_max_for(0.0), "1000 100000");
    }

    /// **The live rate is the cores used between two readings**, and the first reading has none.
    ///
    /// What would make it fail: the rate divided by the wrong unit (seconds where the counter is
    /// microseconds), a rate given on the first reading, or a reading of a removed cgroup kept.
    #[test]
    fn the_live_rate_is_cores_between_two_readings() {
        let (_d, cgroups) = tree("cpu", true);
        let dir = place_with(&Fake::default(), &cgroups, "web-main", "j", 1, None).unwrap();
        let t0 = DateTime::parse_from_rfc3339("2026-10-31T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut rates = Rates::default();
        std::fs::write(dir.join("cpu.stat"), "usage_usec 1000000\n").unwrap();
        assert_eq!(rates.observe(&cgroups, "web-main", "j", t0), None);
        std::fs::write(dir.join("cpu.stat"), "usage_usec 7000000\n").unwrap();
        let t2 = t0 + chrono::Duration::seconds(2);
        assert_eq!(rates.observe(&cgroups, "web-main", "j", t2), Some(3.0));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(rates.observe(&cgroups, "web-main", "j", t2), None);
        assert_eq!(
            cores_between(10, t2, 5, t2 + chrono::Duration::seconds(1)),
            None
        );
    }

    // ─────────── through the scheduler's tick, on a cgroup tree written as plain files ───────────

    /// A fleet of one box, `web-main`, for [`crate::jobs::tick`]: `leaf` says whether the box has
    /// the launcher's leaf and hands on `cpu` and `memory`, as a box started by this launcher does.
    fn fleet(leaf: bool) -> (crate::testutil::TempDir, crate::jobs::Places) {
        let (dir, cgroups) = tree(if leaf { "cpu memory" } else { "" }, leaf);
        let root = PathBuf::from(dir.as_ref() as &Path);
        let places = crate::jobs::Places {
            requests: root.join("fleet/.skein/jobs/requests"),
            states: root.join("home/boxes"),
            queue: root.join("home/jobs/queue.json"),
            cgroups,
        };
        std::fs::create_dir_all(places.requests.join("web-main")).unwrap();
        std::fs::create_dir_all(places.states.join("web-main/signals")).unwrap();
        (dir, places)
    }

    fn submit(places: &crate::jobs::Places, id: &str, pid: u32, state: &str) {
        let body = serde_json::json!({"id": id, "class": "test", "argv": ["cargo", "test"],
                                      "cwd": "/w", "pid": pid, "state": state});
        std::fs::write(
            places.requests.join("web-main").join(format!("{id}.json")),
            body.to_string(),
        )
        .unwrap();
    }

    fn tick(places: &crate::jobs::Places, secs: i64) -> crate::jobs::Queue {
        let at = DateTime::parse_from_rfc3339("2026-10-31T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::seconds(secs);
        let limits = crate::jobs::Limits {
            fleet_slots: 2,
            repo_cap: 2,
        };
        crate::jobs::tick(places, limits, &|b: &str| b.to_string(), at).unwrap()
    }

    fn job<'a>(q: &'a crate::jobs::Queue, id: &str) -> &'a crate::jobs::Job {
        q.jobs.iter().find(|j| j.id == id).expect("no such job")
    }

    /// **The tick that grants a job moves its wrapper into the job's own cgroup, and the job holds
    /// its slot from there.**
    ///
    /// The wrapper's pid starts in the box's leaf, as a process the box's shell started does. After
    /// the granting tick it is in `job-<id>/cgroup.procs`, and the fixture then takes it out of the
    /// leaf as the kernel's move would: the next tick must still find it in the box.
    ///
    /// What would make it fail: no `place` in the tick's grant path (no job cgroup is made), or
    /// `pid_in_box` reading only the box's own `cgroup.procs` (the sabotage that was run: the job
    /// reads `gone` on the second tick, because its pid is only in the job's cgroup).
    #[test]
    fn the_granting_tick_moves_the_job_into_its_own_cgroup_and_it_stays_alive_there() {
        let (_d, places) = fleet(true);
        let leaf = places.cgroups.join("web-main").join(BOX_LEAF);
        std::fs::write(leaf.join("cgroup.procs"), "4242\n31337\n").unwrap();
        submit(&places, "j-a", 31337, "waiting");
        let q = tick(&places, 0);
        assert_eq!(job(&q, "j-a").state, "granted", "{q:#?}");
        assert_eq!(job(&q, "j-a").reason, "", "{q:#?}");
        let cg = job_cgroup(&places.cgroups, "web-main", "j-a");
        assert_eq!(read(&cg.join("cgroup.procs")), "31337");
        assert_eq!(read(&cg.join("cpu.weight")), "100");
        assert!(
            !cg.join("cpu.max").exists(),
            "the tick has no cores to cap at yet"
        );

        std::fs::write(leaf.join("cgroup.procs"), "4242\n").unwrap();
        let q = tick(&places, 2);
        assert_eq!(
            job(&q, "j-a").state,
            "granted",
            "a job whose wrapper is in its own cgroup read as gone: {q:#?}"
        );
    }

    /// **A job that ends is measured, and its view says what it used.**
    ///
    /// What would make it fail: no `finish` in the tick's end path (the reason stays empty), or the
    /// end path looking only at jobs that were waiting rather than running.
    #[test]
    fn a_job_that_ends_says_what_it_used() {
        let (_d, places) = fleet(true);
        let leaf = places.cgroups.join("web-main").join(BOX_LEAF);
        std::fs::write(leaf.join("cgroup.procs"), "31337\n").unwrap();
        submit(&places, "j-b", 31337, "waiting");
        tick(&places, 0);
        let cg = job_cgroup(&places.cgroups, "web-main", "j-b");
        std::fs::write(cg.join("cpu.stat"), "usage_usec 12000000\n").unwrap();
        std::fs::write(cg.join("memory.peak"), format!("{}\n", 3u64 << 29)).unwrap();
        submit(&places, "j-b", 31337, "done");
        let q = tick(&places, 4);
        let b = job(&q, "j-b");
        assert_eq!(b.state, "done");
        assert_eq!(
            b.reason, "used 3.0 cores on average, at most 1.5 GiB of memory",
            "{q:#?}"
        );
    }

    /// **A box with no cgroup delegation still runs its jobs, uncapped as before, and says so.**
    ///
    /// What would make it fail: a refused placement holding the grant back (the job waits for
    /// ever on a box that can never have a job cgroup), or the refusal kept out of the view.
    #[test]
    fn a_box_without_delegation_still_runs_its_jobs_and_says_they_are_uncapped() {
        let (_d, places) = fleet(false);
        std::fs::write(places.cgroups.join("web-main/cgroup.procs"), "31337\n").unwrap();
        submit(&places, "j-c", 31337, "waiting");
        tick(&places, 0);
        let view = crate::jobs::read_view(&places.states.join("web-main")).expect("no view");
        assert_eq!(view.jobs[0].state, "granted", "{view:#?}");
        assert!(
            view.jobs[0].reason.contains("uncapped") && view.jobs[0].reason.contains("restarting"),
            "{view:#?}"
        );
        assert!(!job_cgroup(&places.cgroups, "web-main", "j-c").exists());

        // And with no box cgroup at all, liveness falls back to /proc: this test's own pid.
        let (_d2, places) = fleet(false);
        std::fs::remove_dir_all(places.cgroups.join("web-main")).unwrap();
        submit(&places, "j-d", std::process::id(), "waiting");
        let q = tick(&places, 0);
        assert_eq!(job(&q, "j-d").state, "granted", "{q:#?}");
        assert!(
            job(&q, "j-d").reason.contains("does not delegate"),
            "{q:#?}"
        );
    }
}
