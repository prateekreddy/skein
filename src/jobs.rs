//! One queue for the fleet's heavy work: builds, tests, CI, WebKit runs (SKEIN-1388, phase 1).
//!
//! **Why.** CPU is the fleet's bottleneck, and each repo had built its own slots: a flock-edited
//! queue file, a CI runner with two slots, a cron that refills lanes. skein's own lanes share one
//! flock, `/var/tmp/skein-lanes-heavy.lock`, and every box has a private `/var/tmp`
//! (`src/box-session.sh`, the `vartmp` bind), so that lock lines up the lanes of one box and no
//! more. Nothing showed anybody what was waiting.
//!
//! **The shape**, as the primitives of architecture §2 name it:
//!
//! * a box asks with `skein-job run -- <argv>` (`src/plugin/bin/skein-job`), which writes one file
//!   into the box's own drop-box, `<fleet root>/.skein/jobs/requests/<box>/<id>.json`. The launcher
//!   binds that directory alone read-write into that box (the `for asking in …` loop in
//!   `src/box-session.sh`), so, as for [`crate::asks`], **the directory is the identity** and every
//!   field in the file is the box's own word. That is *requested* state: never authoritative.
//! * skein-server is the **only writer of the queue**, [`queue_path`], on the host under
//!   `~/.skein`, where no box can write. It is *declared* state with one owner: this loop.
//! * each box is shown its own jobs in `<box_state>/signals/jobs.json` ([`view_path`]), which the
//!   launcher binds read-only into the box beside `resources.json`: an *artifact*. The wrapper
//!   waits on it and runs its command when its job reads `granted`.
//!
//! **The job runs inside the box, as the wrapper's own child.** A crossing (`place::argv`'s
//! `nsenter`) enters the box's namespaces and not its cgroup, so a command skein started from
//! outside would run outside the box's memory ceiling and floor (`fleet::box_cgroup`). The wrapper
//! is a child of the box's own shell, and a child cannot leave its parent's cgroup by forking.
//!
//! **What a box's file can and cannot do.** It can add a job of its own, and mark one of its own
//! `cancelled` or `done`. It cannot place, rank or grant anything: the file is read into
//! [`Request`], which has no field for a priority, a position or a grant, so whatever a box writes
//! under those names is never read. The order is the order skein first saw the jobs in.
//!
//! **A job is granted cores, not a slot** (SKEIN-1420). It asks for some (`--cores`, at most
//! [`Limits::max_cores_per_job`]), and [`schedule`] starts it when the cores running jobs are
//! measured to use plus its ask fit the machine, the cores granted in all stay under
//! [`Limits::grant_ceiling`], and the memory free exceeds what it is expected to need. The wrapper
//! hands the grant to the job's tools as `CARGO_BUILD_JOBS`, `MAKEFLAGS=-jN` and the test-thread
//! counts. A repo alone may use every core; when another repo has a job waiting, that job takes
//! the next freed cores first. Nothing running is ever stopped by the queue.
//!
//! **A grant is freed by the kernel's word, not the box's.** A granted job holds its cores while
//! the wrapper's pid is in the box's `cgroup.procs`; a wrapper killed with `-9` writes no done
//! file, and gives them back on the next tick anyway. Every duration is measured on the host's
//! clock.
//!
//! Not yet: bumping, priority asks, the owner reordering from the cockpit, an ETA, MCP tools.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod cgroup;

/// How often the queue is looked at: fast enough that freed cores are taken within a couple of
/// seconds. Its own loop, apart from the resource signal's minute, because a job waiting a minute
/// for cores that were free is the waste this queue exists to remove.
pub const LOOK_EVERY: Duration = Duration::from_secs(2);

/// The most cores one job may ask for unless the config says otherwise.
pub const DEFAULT_MAX_CORES_PER_JOB: u32 = 4;

/// How far the cores granted may run past the machine's unless the config says otherwise: jobs
/// rarely use every core they ask for, and admission also counts what they are measured to use.
pub const DEFAULT_OVERSUBSCRIBE: f64 = 1.5;

/// The kinds of heavy work a job may say it is. Anything else is read as `build`.
pub const CLASSES: [&str; 5] = ["build", "test", "ci", "webkit", "train"];

/// How many jobs one box may have waiting or running at once. A box that writes more files than
/// this has the rest left unread until earlier ones finish: the queue is shared, and one box must
/// not be able to fill it (architecture §8.5).
pub const PER_BOX: usize = 20;

/// How long a finished job stays in the queue, for its durations.
const KEEP_FINISHED: Duration = Duration::from_secs(3600);

/// How long a finished job stays in its box's view, and its request file in the drop-box.
const SHOW_FINISHED: Duration = Duration::from_secs(600);

/// A view whose jobs have not changed is written again at least this often, so a wrapper can tell
/// skein still looking from skein gone.
const VIEW_FRESH: Duration = Duration::from_secs(20);

/// A request file larger than this is not read.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

// ───────────────────────────── where things live ─────────────────────────────

/// The queue's drop-boxes, **inside the sandbox** beside `asks` and `gitgate`: a box must be able
/// to write its own request, so they cannot be on the host.
pub fn jobs_dir() -> String {
    format!("{}/.skein/jobs", crate::fleet::fleet_root())
}

fn requests_dir() -> String {
    format!("{}/requests", jobs_dir())
}

/// One box's drop-box, spelled once so what removes it with the box is what reads it
/// (`fleet::disk`'s `box_side_state`).
pub(crate) fn box_requests_dir(box_name: &str) -> String {
    format!("{}/{box_name}", requests_dir())
}

/// The queue itself: on the **host**, where no box can write, and written by this module alone.
pub fn queue_path() -> PathBuf {
    crate::config::skein_home().join("jobs").join("queue.json")
}

/// Where one box's view of its jobs lives, given that box's state directory.
pub fn view_path(state: &Path) -> PathBuf {
    state.join("signals").join("jobs.json")
}

// ───────────────────────────── what is written ─────────────────────────────

/// What skein reads of a box's request file, and **all** of it: a priority, a position or a grant
/// written into the file has no field here and is never read.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub class: String,
    /// The command, for showing. skein never runs it: the wrapper does, inside the box.
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    /// The wrapper's pid. Its presence in the box's `cgroup.procs` is what holds a grant.
    #[serde(default)]
    pub pid: u32,
    /// `waiting`, or the box's `cancelled` or `done`. Anything else reads as `waiting`.
    #[serde(default)]
    pub state: String,
    /// The cores it asks for (`--cores`). Absent in a file from before cores, which then asks for
    /// [`default_cores`]. An ask, never a grant: [`schedule`] holds it to the per-job cap.
    #[serde(default)]
    pub cores: Option<u32>,
    /// The fewest cores it will start with (`--min`). Absent ⇒ all it asked for.
    #[serde(default)]
    pub min: Option<u32>,
}

/// One job, as the queue holds it. Every field but the box's words (`class`, `command`, `cwd`) is
/// skein's.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// The directory the request was found in, never a field of the file.
    #[serde(rename = "box")]
    pub box_name: String,
    /// The box's repo, as skein's registry says; the box's own name for a box with none.
    pub repo: String,
    pub class: String,
    pub command: String,
    pub cwd: String,
    pub pid: u32,
    /// `waiting`, `granted`, or one of the ends: `done`, `cancelled`, `gone` (the wrapper left the
    /// box's cgroup, or its request was removed).
    pub state: String,
    /// When skein first saw it, by the host's clock.
    pub queued: String,
    #[serde(default)]
    pub granted: String,
    #[serde(default)]
    pub ended: String,
    /// Why a waiting job did not start on the last tick, such as `waiting for 2 cores (1 free)`.
    #[serde(default)]
    pub reason: String,
    /// The cores it asks for, held by [`schedule`] to the per-job cap and the machine. Zero in a
    /// queue from before cores, read as [`default_cores`].
    #[serde(default)]
    pub cores: u32,
    /// The fewest it will start with, at most `cores`; `0` ⇒ all of `cores`.
    #[serde(default)]
    pub min: u32,
    /// The cores it was granted: `cores`, or fewer down to `min`. Zero until it is granted.
    #[serde(default)]
    pub granted_cores: u32,
    /// What it used, read from its own cgroup once it ended ([`cgroup::finish`]). `None` while it
    /// runs, and for a job that never had a cgroup of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<Usage>,
    /// The command in two words, for another box's view: [`short_command`] of its argv.
    #[serde(default)]
    pub short: String,
}

/// What a finished job used, from its own cgroup.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Its CPU time over the seconds it ran: the cores it used on average.
    pub avg_cores: f64,
    /// The most memory it held at once, in MiB. `None` where the kernel did not count it.
    pub peak_mem_mib: Option<u64>,
    /// The seconds it ran, from its grant to the tick that saw it end, by the host's clock.
    pub secs: i64,
}

impl Usage {
    /// From what the job's cgroup counted over `secs` seconds of running. A job that ran under a
    /// second is averaged over one, so its average is its CPU seconds.
    pub fn from_cgroup(counted: &cgroup::Usage, secs: i64) -> Usage {
        let secs = secs.max(0);
        Usage {
            avg_cores: counted.cpu_usec as f64 / 1e6 / secs.max(1) as f64,
            peak_mem_mib: counted.memory_peak.map(|b| b >> 20),
            secs,
        }
    }

    /// For a person: `used 1.5 cores on average, at most 2.0 GB`.
    pub fn said(&self) -> String {
        let cores = format!("used {:.1} cores on average", self.avg_cores);
        match self.peak_mem_mib {
            Some(mib) => format!("{cores}, at most {:.1} GB", mib as f64 / 1024.0),
            None => cores,
        }
    }
}

/// A command in at most two words, for showing to another box: its program's name and its first
/// argument that is not a flag, such as `cargo test`. Never the whole argv, which may hold paths,
/// tokens or values another box has no business reading. Of each word only what follows its last
/// `/` is kept, an argument with an `=` is never taken (`KEY=value`, `--flag=value`), and each is
/// cut to 40 characters.
pub fn short_command(argv: &[String]) -> String {
    let word = |w: &str| -> String {
        w.rsplit('/')
            .next()
            .unwrap_or("")
            .chars()
            .take(40)
            .collect()
    };
    let Some(program) = argv.first().map(|p| word(p)).filter(|p| !p.is_empty()) else {
        return String::new();
    };
    match argv[1..]
        .iter()
        .find(|a| !a.starts_with('-') && !a.contains('='))
        .map(|a| word(a))
        .filter(|a| !a.is_empty())
    {
        Some(first) => format!("{program} {first}"),
        None => program,
    }
}

impl Job {
    fn is_active(&self) -> bool {
        self.state == "waiting" || self.state == "granted"
    }

    /// The cores a granted job holds. One granted by a skein from before cores holds the default
    /// ask, which is what every job then ran with.
    fn holds(&self) -> u32 {
        match self.granted_cores {
            0 => default_cores(self),
            n => n,
        }
    }
}

/// The cores a job asks for when its `skein-job run` named none. Two for now; the history child
/// of SKEIN-1418 derives it from what that repo's command used before, here and nowhere else.
/// The wrapper's fallback run, one skein never answered, uses the same two
/// (`src/plugin/bin/skein-job`).
pub fn default_cores(_job: &Job) -> u32 {
    2
}

/// The memory a job is expected to need before it may start, in MiB: a default per class for now,
/// until the history child of SKEIN-1418 reads that command's past peak, here and nowhere else.
pub fn memory_estimate_mib(job: &Job) -> u64 {
    match job.class.as_str() {
        "webkit" | "train" => 4096,
        // Measured: a CI run with every stage at once peaked at about 7 GB on a peer box, so 3 GB
        // started runs the memory free could not hold.
        "ci" => 4096,
        _ => 2048,
    }
}

/// The queue file.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    #[serde(default = "schema_one")]
    pub schema: u32,
    /// In the order skein first saw them, which is the order they are granted in.
    #[serde(default)]
    pub jobs: Vec<Job>,
}

fn schema_one() -> u32 {
    1
}

/// What jobs may be granted, and the machine reading it was worked out from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// The machine's cores.
    pub cores: u32,
    /// The most one job may ask for (`jobs_max_cores_per_job`).
    pub max_cores_per_job: u32,
    /// How far the cores granted may run past `cores` (`jobs_oversubscribe`).
    pub oversubscribe: f64,
    /// The memory free to start a job with, in MiB, or `None` when it could not be read, and then
    /// no job waits on memory.
    pub mem_free_mib: Option<u64>,
}

impl Limits {
    /// The most cores that may be granted at once: the cores times the oversubscription, rounded
    /// down. Never below the cores themselves.
    pub fn grant_ceiling(&self) -> u32 {
        (self.cores as f64 * self.oversubscribe.max(1.0)).floor() as u32
    }
}

/// The limits as configured, against the machine as last measured. The cores are the sandbox's
/// `nproc` and the free memory its `MemAvailable`, both from the one resource reading
/// (`fleet::fleet_resources`, `mem_total - mem_used`); the cores fall back to this process's own
/// count when the sandbox did not answer, since skein-server runs in the sandbox, and a reading
/// the sandbox has stopped refreshing gates no job on memory.
pub fn limits() -> Limits {
    let config = crate::config::load_config();
    let reading = crate::fleet::fleet_resources();
    let cores = reading
        .as_ref()
        .map(|r| r.cpus)
        .filter(|&c| c > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get() as u64)
                .unwrap_or(1)
        });
    let mem_free_mib = reading
        .filter(|r| r.mem_total > 0 && !r.stale)
        .map(|r| r.mem_total.saturating_sub(r.mem_used));
    Limits {
        cores: u32::try_from(cores).unwrap_or(u32::MAX).max(1),
        max_cores_per_job: match config.jobs_max_cores_per_job {
            0 => DEFAULT_MAX_CORES_PER_JOB,
            n => n,
        },
        oversubscribe: match config.jobs_oversubscribe {
            x if x.is_finite() && x >= 1.0 => x,
            _ => DEFAULT_OVERSUBSCRIBE,
        },
        mem_free_mib,
    }
}

/// The cores as [`schedule`] left them, for the views.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pool {
    pub cores: u32,
    /// Granted to running jobs, fleet-wide.
    pub granted: u32,
    /// What a waiting job could start with now.
    pub free: u32,
}

/// One box's view of its jobs: what the wrapper waits on and the agents panel shows.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    #[serde(default = "schema_one")]
    pub schema: u32,
    /// RFC3339, when skein wrote it.
    #[serde(default)]
    pub at: String,
    /// The machine's cores.
    #[serde(default)]
    pub cores: u32,
    /// Cores granted to running jobs, fleet-wide.
    #[serde(default)]
    pub granted: u32,
    /// Cores a waiting job could start with now.
    #[serde(default)]
    pub free: u32,
    #[serde(default)]
    pub running: usize,
    #[serde(default)]
    pub jobs: Vec<ViewJob>,
    /// Every waiting job that goes before this box's first waiting one, in the order they go, each
    /// named with its box. Empty when nothing of this box waits.
    #[serde(default)]
    pub ahead: Vec<Ahead>,
}

/// A waiting job ahead of the box's own, in its view.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ahead {
    #[serde(rename = "box")]
    pub box_name: String,
    pub id: String,
    pub class: String,
    /// Its command in two words, such as `cargo test` ([`short_command`]): never its whole argv.
    #[serde(default)]
    pub command: String,
    /// The cores it asks for.
    pub cores: u32,
}

/// One of the box's own jobs, in its view.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewJob {
    pub id: String,
    pub class: String,
    pub command: String,
    pub cwd: String,
    pub state: String,
    /// The cores it asks for.
    #[serde(default)]
    pub cores: u32,
    /// The fewest it will start with.
    #[serde(default)]
    pub min: u32,
    /// The cores it was granted; zero while it waits.
    #[serde(default)]
    pub granted_cores: u32,
    /// Its place among every waiting job in the fleet, in the order they go, from 1. `None` once
    /// it is not waiting.
    pub position: Option<usize>,
    pub reason: String,
    /// Seconds it waited, or has waited so far, by the host's clock.
    pub waited_secs: i64,
    /// Seconds it ran, once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_secs: Option<i64>,
    /// What it used, once it has ended and was measured; `reason` then says it in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<Usage>,
}

// ───────────────────────────── one tick ─────────────────────────────

/// The places one tick reads and writes. [`Places::live`] in skein-server; a test names its own.
#[derive(Debug, Clone)]
pub struct Places {
    /// `requests/`, holding one drop-box per box.
    pub requests: PathBuf,
    /// The box state root: `<states>/<box>/signals/` is where each view goes.
    pub states: PathBuf,
    pub queue: PathBuf,
    /// The cgroup parent of every box: `<cgroups>/<box>/cgroup.procs`.
    pub cgroups: PathBuf,
}

impl Places {
    pub fn live() -> Places {
        Places {
            requests: PathBuf::from(requests_dir()),
            states: PathBuf::from(crate::fleet::box_state_root()),
            queue: queue_path(),
            cgroups: PathBuf::from("/sys/fs/cgroup/skein"),
        }
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn parse_stamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn secs_between(from: &str, to: DateTime<Utc>) -> i64 {
    parse_stamp(from)
        .map(|f| (to - f).num_seconds().max(0))
        .unwrap_or(0)
}

/// A request id: a filename, so the whitelist every other queue's ids keep.
fn id_is_nameable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// Every box's requests, by box and then by id. A file whose name is not its id, that is not a
/// plain file, or that does not parse, is skipped.
pub fn read_requests(dir: &Path) -> BTreeMap<String, BTreeMap<String, Request>> {
    let mut out = BTreeMap::new();
    let Ok(boxes) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in boxes.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // The entry's own type, which is not followed: a box's drop-box is a directory the launcher
        // made, never a link.
        if !crate::util::valid_name(&name) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        let mut mine = BTreeMap::new();
        for file in files.flatten() {
            let Some(id) = file
                .file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .map(str::to_string)
            else {
                continue;
            };
            if !id_is_nameable(&id) {
                continue;
            }
            // **Opened, then checked, and only what was opened is read** (`boxfile::read_text`:
            // `O_NOFOLLOW | O_NONBLOCK`, a regular file by `fstat`, at most the cap). skein-server
            // can read what no box may, and the box writes this directory: a check of the path
            // followed by a read of the path lets the box swap the file for a link to a host file
            // in between, and a FIFO would park this loop until something wrote to it.
            let Some(req) = crate::boxfile::read_text(&file.path(), MAX_REQUEST_BYTES)
                .ok()
                .and_then(|t| serde_json::from_str::<Request>(&t).ok())
            else {
                continue;
            };
            if req.id == id {
                mine.insert(id, req);
            }
        }
        out.insert(name, mine);
    }
    out
}

/// Whether `pid` is in box `name`'s cgroup or one under it, by the kernel's `cgroup.procs`: the
/// box's own processes are in its leaf and a granted job's in a cgroup of its own
/// ([`cgroup`]). A box with no cgroup to read (no delegation) falls back to the pid existing at all,
/// since boxes share the sandbox's pid namespace.
pub fn pid_in_box(cgroups: &Path, name: &str, pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    cgroup::pid_in_subtree(&cgroups.join(name), pid)
        .unwrap_or_else(|| Path::new("/proc").join(pid.to_string()).exists())
}

/// Whether skein has asked box `name` to free memory: a `memory` ask in its resource signal,
/// `signals/resources.json`, which skein-server writes (`resource_signal`). Read as plain JSON, for
/// the one field it needs, so this module does not depend on that one and through it on the
/// whole of `fleet`'s cycle.
fn memory_asked(states: &Path, name: &str) -> bool {
    std::fs::read_to_string(states.join(name).join("signals").join("resources.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("asks").and_then(|a| a.as_array()).cloned())
        .is_some_and(|asks| asks.iter().any(|a| a.get("kind") == Some(&"memory".into())))
}

/// Bring the queue up to date with what the boxes wrote and which wrappers are still alive.
///
/// An active job ends when its box marks it `cancelled` or `done`, when its request file is gone,
/// or when its wrapper's pid has left the box's cgroup. A new request is appended, after every job
/// already queued, if its wrapper is alive and its box has fewer than [`PER_BOX`] active.
pub fn merge(
    queue: &mut Queue,
    requests: &BTreeMap<String, BTreeMap<String, Request>>,
    alive: &dyn Fn(&str, u32) -> bool,
    repo_of: &dyn Fn(&str) -> String,
    now: DateTime<Utc>,
) {
    let mut known = BTreeSet::new();
    for job in queue.jobs.iter_mut() {
        known.insert((job.box_name.clone(), job.id.clone()));
        if !job.is_active() {
            continue;
        }
        let req = requests.get(&job.box_name).and_then(|m| m.get(&job.id));
        let end = match req.map(|r| r.state.as_str()) {
            None => Some("gone"),
            Some("cancelled") => Some("cancelled"),
            Some("done") => Some("done"),
            // The pid skein first saw, not whatever the file says now: a box cannot keep a grant
            // by naming another of its live processes.
            Some(_) if !alive(&job.box_name, job.pid) => Some("gone"),
            Some(_) => None,
        };
        if let Some(end) = end {
            job.state = end.into();
            job.ended = stamp(now);
            job.reason.clear();
        }
    }
    for (name, mine) in requests {
        let mut active = queue
            .jobs
            .iter()
            .filter(|j| &j.box_name == name && j.is_active())
            .count();
        for (id, req) in mine {
            if known.contains(&(name.clone(), id.clone())) {
                continue;
            }
            if matches!(req.state.as_str(), "cancelled" | "done") || !alive(name, req.pid) {
                continue;
            }
            if active >= PER_BOX {
                break;
            }
            active += 1;
            let class = match CLASSES.contains(&req.class.as_str()) {
                true => req.class.clone(),
                false => "build".into(),
            };
            let mut command = req.argv.join(" ");
            if command.len() > 300 {
                let mut cut = 300;
                while !command.is_char_boundary(cut) {
                    cut -= 1;
                }
                command.truncate(cut);
                command.push('…');
            }
            let mut job = Job {
                id: id.clone(),
                box_name: name.clone(),
                repo: repo_of(name),
                class,
                command,
                cwd: req.cwd.chars().take(1024).collect(),
                pid: req.pid,
                state: "waiting".into(),
                queued: stamp(now),
                min: req.min.unwrap_or(0),
                short: short_command(&req.argv),
                ..Job::default()
            };
            job.cores = match req.cores {
                Some(n) if n > 0 => n,
                _ => default_cores(&job),
            };
            queue.jobs.push(job);
        }
    }
}

/// The cores of every granted job, by repo: what decides whose waiting job goes next.
fn cores_by_repo(queue: &Queue) -> BTreeMap<String, u32> {
    let mut per_repo: BTreeMap<String, u32> = BTreeMap::new();
    for j in queue.jobs.iter().filter(|j| j.state == "granted") {
        *per_repo.entry(j.repo.clone()).or_default() += j.holds();
    }
    per_repo
}

/// The order waiting jobs go in, as it stands: the job of the repo holding the fewest cores first,
/// and among equals the one skein saw first. Indices into `queue.jobs`.
fn waiting_order(queue: &Queue) -> Vec<usize> {
    let per_repo = cores_by_repo(queue);
    let mut order: Vec<usize> = (0..queue.jobs.len())
        .filter(|&i| queue.jobs[i].state == "waiting")
        .collect();
    order.sort_by_key(|&i| (per_repo.get(&queue.jobs[i].repo).copied().unwrap_or(0), i));
    order
}

fn cores_word(n: u32) -> String {
    match n {
        1 => "1 core".into(),
        n => format!("{n} cores"),
    }
}

/// MiB as GB for a person: `3` for 3072, `1.2` for 1228.
fn gb(mib: u64) -> String {
    let s = format!("{:.1}", mib as f64 / 1024.0);
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// Grant what can run.
///
/// **Which job goes next.** The waiting job of the repo that holds the fewest cores, and among
/// equals the one skein saw first; the counts move as each grant is made. So a repo alone takes
/// every core, and another repo's job that arrives goes before the busy repo's queued ones.
///
/// **Whether it starts.** All three of:
///
/// * the cores running jobs use, by `in_use` (a job it has no reading for counts at its grant),
///   plus its ask, fit the machine's cores;
/// * the cores granted in all stay within [`Limits::grant_ceiling`];
/// * the cores busy on the whole machine, by `machine_busy`, plus its ask, fit the cores: work
///   outside the queue (an agent's own browser run, another repo's tests) loads the machine too,
///   and two e2e runs beside WebKit once drove the load to 50 and timed WebKit's tests out. The
///   reading predates this pass, so what this pass grants is added to it. `None`, unread, gates
///   nothing;
/// * the memory free exceeds [`memory_estimate_mib`], less what this pass already granted against.
///
/// A job with a `min` starts with fewer than it asked, never fewer than `min`, when that is what
/// fits.
///
/// **What waits behind what.** A job that does not fit for cores holds the cores it waits for:
/// every job after it waits too, so a big ask is not passed for ever by small ones, and another
/// repo's job really does get the next freed cores. A job held by memory, or whose box is asked
/// to free memory, does not hold anything up. Every job left waiting carries its reason. Nothing
/// running is ever stopped here.
pub fn schedule(
    queue: &mut Queue,
    limits: Limits,
    memory_asked: &dyn Fn(&str) -> bool,
    in_use: &dyn Fn(&Job) -> Option<f64>,
    machine_busy: Option<f64>,
) -> Pool {
    let ask_cap = limits.max_cores_per_job.min(limits.cores).max(1);
    for job in queue.jobs.iter_mut().filter(|j| j.state == "waiting") {
        if job.cores == 0 {
            job.cores = default_cores(job);
        }
        job.cores = job.cores.clamp(1, ask_cap);
        job.min = match job.min {
            0 => job.cores,
            m => m.clamp(1, job.cores),
        };
    }
    let ceiling = limits.grant_ceiling();
    let mut granted: u32 = 0;
    let mut used: f64 = 0.0;
    for j in queue.jobs.iter().filter(|j| j.state == "granted") {
        let holds = j.holds();
        granted += holds;
        used += in_use(j)
            .filter(|u| u.is_finite() && *u >= 0.0)
            .unwrap_or(holds as f64);
    }
    let free = |granted: u32, used: f64| -> u32 {
        let by_use = (limits.cores as f64 - used).floor().max(0.0) as u32;
        by_use.min(ceiling.saturating_sub(granted))
    };
    let machine_busy = machine_busy.filter(|b| b.is_finite() && *b >= 0.0);
    // What the machine has idle once this pass's grants are added to the reading; all of it when
    // there is no reading.
    let idle = |given: u32| -> u32 {
        match machine_busy {
            Some(busy) => (limits.cores as f64 - busy - given as f64).floor().max(0.0) as u32,
            None => u32::MAX,
        }
    };
    let mut given: u32 = 0;
    let mut per_repo = cores_by_repo(queue);
    let mut mem_free = limits.mem_free_mib;
    let mut left = waiting_order(queue);
    // The repo of the first job that did not fit for cores, which holds those cores against
    // every job after it.
    let mut held_for: Option<String> = None;
    while !left.is_empty() {
        let pick = left
            .iter()
            .enumerate()
            .min_by_key(|&(_, &i)| (per_repo.get(&queue.jobs[i].repo).copied().unwrap_or(0), i))
            .map(|(at, _)| at)
            .unwrap_or(0);
        let job = &mut queue.jobs[left.remove(pick)];
        if memory_asked(&job.box_name) {
            job.reason = "this box is asked to free memory".into();
            continue;
        }
        if let Some(repo) = &held_for {
            job.reason = match *repo == job.repo {
                true => "an earlier job goes first".into(),
                false => "another repo's job goes first".into(),
            };
            continue;
        }
        let need = memory_estimate_mib(job);
        if let Some(have) = mem_free.filter(|&have| have <= need) {
            job.reason = format!("memory: needs ~{} GB, {} GB free", gb(need), gb(have));
            continue;
        }
        let fits = free(granted, used);
        if fits < job.min {
            job.reason = format!("waiting for {} ({fits} free)", cores_word(job.min));
            held_for = Some(job.repo.clone());
            continue;
        }
        if idle(given) < job.min {
            let busy = machine_busy.unwrap_or(0.0) + given as f64;
            job.reason = format!(
                "machine busy: {} of {} cores in use",
                (busy * 10.0).round() / 10.0,
                limits.cores
            );
            held_for = Some(job.repo.clone());
            continue;
        }
        let give = job.cores.min(fits).min(idle(given));
        job.state = "granted".into();
        job.granted_cores = give;
        job.reason.clear();
        granted += give;
        given += give;
        used += give as f64;
        *per_repo.entry(job.repo.clone()).or_default() += give;
        mem_free = mem_free.map(|m| m.saturating_sub(need));
    }
    Pool {
        cores: limits.cores,
        granted,
        free: free(granted, used).min(idle(given)),
    }
}

fn granted_now(queue: &mut Queue, before: &BTreeSet<(String, String)>, now: DateTime<Utc>) {
    for job in queue.jobs.iter_mut() {
        if job.state == "granted" && !before.contains(&(job.box_name.clone(), job.id.clone())) {
            job.granted = stamp(now);
        }
    }
}

/// Drop jobs that ended more than [`KEEP_FINISHED`] ago.
fn prune(queue: &mut Queue, now: DateTime<Utc>) {
    queue
        .jobs
        .retain(|j| j.is_active() || secs_between(&j.ended, now) < KEEP_FINISHED.as_secs() as i64);
}

/// Box `name`'s view of the queue.
///
/// Its own jobs, in the order skein saw them, each with its place in the order waiting jobs go
/// ([`waiting_order`]); and every waiting job ahead of its first, named with its box: the owner
/// chose named over counted (2026-10-31), so the box can tell whose work it waits behind.
pub fn view_for(queue: &Queue, name: &str, pool: Pool, now: DateTime<Utc>) -> View {
    let order = waiting_order(queue);
    let place = |i: usize| order.iter().position(|&o| o == i).map(|p| p + 1);
    let ahead = order
        .iter()
        .take_while(|&&i| queue.jobs[i].box_name != name)
        .map(|&i| Ahead {
            box_name: queue.jobs[i].box_name.clone(),
            id: queue.jobs[i].id.clone(),
            class: queue.jobs[i].class.clone(),
            command: match queue.jobs[i].short.as_str() {
                // A job queued before the short form was kept: its first two words, which is the
                // same for any argv without a space inside a word.
                "" => {
                    let words: Vec<String> = queue.jobs[i]
                        .command
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                    short_command(&words)
                }
                s => s.to_string(),
            },
            cores: queue.jobs[i].cores,
        })
        .collect::<Vec<_>>();
    let ahead = match order.iter().any(|&i| queue.jobs[i].box_name == name) {
        true => ahead,
        false => Vec::new(),
    };
    let mut jobs = Vec::new();
    for (i, job) in queue.jobs.iter().enumerate() {
        if job.box_name != name {
            continue;
        }
        if !job.is_active() && secs_between(&job.ended, now) >= SHOW_FINISHED.as_secs() as i64 {
            continue;
        }
        let waited_until = match job.granted.as_str() {
            "" if job.is_active() => now,
            "" => parse_stamp(&job.ended).unwrap_or(now),
            g => parse_stamp(g).unwrap_or(now),
        };
        jobs.push(ViewJob {
            id: job.id.clone(),
            class: job.class.clone(),
            command: job.command.clone(),
            cwd: job.cwd.clone(),
            state: job.state.clone(),
            cores: job.cores,
            min: job.min,
            granted_cores: job.granted_cores,
            position: (job.state == "waiting").then(|| place(i)).flatten(),
            reason: match job.used {
                Some(u) if !job.is_active() => u.said(),
                _ => job.reason.clone(),
            },
            waited_secs: secs_between(&job.queued, waited_until),
            ran_secs: (!job.ended.is_empty() && !job.granted.is_empty())
                .then(|| parse_stamp(&job.ended).map(|e| secs_between(&job.granted, e)))
                .flatten(),
            used: job.used,
        });
    }
    View {
        schema: 1,
        at: stamp(now),
        cores: pool.cores,
        granted: pool.granted,
        free: pool.free,
        running: queue.jobs.iter().filter(|j| j.state == "granted").count(),
        jobs,
        ahead,
    }
}

pub fn read_queue(path: &Path) -> Queue {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| Queue {
            schema: 1,
            jobs: Vec::new(),
        })
}

pub fn read_view(state: &Path) -> Option<View> {
    let raw = std::fs::read_to_string(view_path(state)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    crate::util::write_atomic(path, dir, &bytes)
}

/// One tick: read the requests, bring the queue up to date, grant what can run, write the queue
/// and every view that changed, and tidy the drop-boxes of what finished long enough ago.
///
/// `rates` is the last reading of each running job's cgroup, which the tick has no state of its
/// own to keep: the caller holds it from one tick to the next ([`watch_jobs`]), and what it gives
/// is what [`schedule`] admits on.
pub fn tick(
    places: &Places,
    limits: Limits,
    repo_of: &dyn Fn(&str) -> String,
    now: DateTime<Utc>,
    rates: &mut cgroup::Rates,
) -> Result<Queue, String> {
    let mut queue = read_queue(&places.queue);
    let requests = read_requests(&places.requests);
    let cgroups = places.cgroups.clone();
    let alive = move |name: &str, pid: u32| pid_in_box(&cgroups, name, pid);
    let granted = |q: &Queue| -> BTreeSet<(String, String)> {
        q.jobs
            .iter()
            .filter(|j| j.state == "granted")
            .map(|j| (j.box_name.clone(), j.id.clone()))
            .collect()
    };
    let was_running = granted(&queue);
    merge(&mut queue, &requests, &alive, repo_of, now);
    // A job that ended: what its cgroup counted, then the cgroup gone (SKEIN-1419). A wrapper no
    // longer in its box was killed under its command, which runs on in the job's cgroup holding
    // cores the queue just gave back: the job has ended, so that goes first.
    for job in queue.jobs.iter_mut().filter(|j| !j.is_active()) {
        if !was_running.contains(&(job.box_name.clone(), job.id.clone())) {
            continue;
        }
        rates.forget(&job.box_name, &job.id);
        let wrapper_gone = !alive(&job.box_name, job.pid);
        if let Some(used) = cgroup::finish(&places.cgroups, &job.box_name, &job.id, wrapper_gone) {
            job.used = Some(Usage::from_cgroup(&used, secs_between(&job.granted, now)));
        }
    }
    let before = granted(&queue);
    rates.retain(&|b, id| before.contains(&(b.to_string(), id.to_string())));
    // What each running job is using now, from its own cgroup, between this tick's reading and the
    // last: a job just granted, or with no cgroup of its own, has none and counts at its grant.
    let mut live: BTreeMap<(String, String), f64> = BTreeMap::new();
    for (b, id) in &before {
        if let Some(rate) = rates.observe(&places.cgroups, b, id, now) {
            live.insert((b.clone(), id.clone()), rate);
        }
    }
    let states = places.states.clone();
    // The whole machine's busy CPU is not read yet (SKEIN-1430): `None` gates nothing on it.
    let pool = schedule(
        &mut queue,
        limits,
        &|name| memory_asked(&states, name),
        &|job| live.get(&(job.box_name.clone(), job.id.clone())).copied(),
        None,
    );
    granted_now(&mut queue, &before, now);
    // A job granted now moves into a cgroup of its own, capped and weighted for the cores it was
    // granted, before its view says `granted`, which is what the wrapper waits for before it
    // starts the command (SKEIN-1419).
    for job in queue.jobs.iter_mut().filter(|j| j.state == "granted") {
        if before.contains(&(job.box_name.clone(), job.id.clone())) {
            continue;
        }
        let cores = Some(job.holds() as f64);
        if let Err(why) = cgroup::place(&places.cgroups, &job.box_name, &job.id, job.pid, cores) {
            job.reason = format!("runs in its box's cgroup, uncapped and unmeasured: {why}");
        }
    }
    let boxes_with_jobs: BTreeSet<&String> = queue.jobs.iter().map(|j| &j.box_name).collect();
    for name in boxes_with_jobs {
        let running = |id: &str| {
            queue
                .jobs
                .iter()
                .any(|j| &j.box_name == name && j.id == id && j.state == "granted")
        };
        cgroup::sweep(&places.cgroups, name, &running);
    }
    let mut granted_by_box: BTreeMap<String, u32> = BTreeMap::new();
    for j in queue.jobs.iter().filter(|j| j.state == "granted") {
        *granted_by_box.entry(j.box_name.clone()).or_default() += j.holds();
    }
    cgroup::weigh_boxes(&places.cgroups, &granted_by_box);
    prune(&mut queue, now);
    write_json(&places.queue, &queue)?;

    let mut failed = Vec::new();
    let mut boxes: BTreeSet<String> = queue.jobs.iter().map(|j| j.box_name.clone()).collect();
    boxes.extend(requests.keys().cloned());
    for name in boxes {
        let state = places.states.join(&name);
        if !state.is_dir() {
            continue;
        }
        let view = view_for(&queue, &name, pool, now);
        let prev = read_view(&state);
        let unchanged = prev.as_ref().is_some_and(|p| {
            View {
                at: view.at.clone(),
                ..p.clone()
            } == view
                && secs_between(&p.at, now) < VIEW_FRESH.as_secs() as i64
        });
        // A box with nothing to show and no view yet gets none: most boxes never queue anything.
        if unchanged || (prev.is_none() && view.jobs.is_empty()) {
            continue;
        }
        if let Err(e) = write_json(&view_path(&state), &view) {
            failed.push(format!("{name}: {e}"));
        }
    }

    // Each request file of a job that ended a while ago, and each that names no active job and
    // has sat untouched as long: the box's own file, removed by skein because the box need never
    // look at it again.
    for (name, mine) in &requests {
        for id in mine.keys() {
            let job = queue
                .jobs
                .iter()
                .find(|j| &j.box_name == name && &j.id == id);
            let stale = match job {
                Some(j) if j.is_active() => false,
                Some(j) => secs_between(&j.ended, now) >= SHOW_FINISHED.as_secs() as i64,
                None => {
                    let path = places.requests.join(name).join(format!("{id}.json"));
                    std::fs::symlink_metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|m| m.elapsed().ok())
                        .is_some_and(|age| age >= SHOW_FINISHED)
                }
            };
            if stale {
                let _ = std::fs::remove_file(places.requests.join(name).join(format!("{id}.json")));
            }
        }
    }
    match failed.is_empty() {
        true => Ok(queue),
        false => Err(failed.join("; ")),
    }
}

/// The repo a box's jobs count against: its repo's id, or the box's own name for a box in none.
pub fn repo_of_box(name: &str) -> String {
    crate::repos::repo_for_box(name)
        .map(|r| r.id)
        .unwrap_or_else(|| name.to_string())
}

/// The loop skein-server runs. Never returns. An error is printed once each time it changes, so a
/// fleet with a broken drop-box does not print a line every two seconds.
pub async fn watch_jobs() {
    let mut tick_every = tokio::time::interval(LOOK_EVERY);
    tick_every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last = String::new();
    // Each running job's last cgroup reading, carried from one tick to the next: the tick keeps
    // no state of its own, and a rate needs two readings.
    let mut rates = cgroup::Rates::default();
    loop {
        tick_every.tick().await;
        let ran = tokio::task::spawn_blocking(move || {
            let r = tick(
                &Places::live(),
                limits(),
                &repo_of_box,
                Utc::now(),
                &mut rates,
            );
            (rates, r.map(|_| ()))
        })
        .await;
        let said = match ran {
            Ok((kept, r)) => {
                rates = kept;
                match r {
                    Ok(()) => String::new(),
                    Err(e) => format!("skein: the heavy-work queue: {e}"),
                }
            }
            // A tick that panicked took the readings with it: the next starts over, and its
            // running jobs count at their grants until two readings give them a rate again.
            Err(e) => {
                rates = cgroup::Rates::default();
                format!("skein: the heavy-work queue did not run: {e}")
            }
        };
        if said != last && !said.is_empty() {
            eprintln!("{said}");
        }
        last = said;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::seconds(secs)
    }

    /// A fleet in a temporary directory: drop-boxes, box state, a queue and a fake cgroup parent,
    /// each box's `cgroup.procs` written by the test as the kernel would.
    struct Fleet {
        _dir: crate::testutil::TempDir,
        places: Places,
    }

    impl Fleet {
        fn new(boxes: &[&str]) -> Fleet {
            let dir = crate::testutil::tempdir();
            let root: &Path = dir.as_ref();
            let places = Places {
                requests: root.join("fleet/.skein/jobs/requests"),
                states: root.join("home/boxes"),
                queue: root.join("home/jobs/queue.json"),
                cgroups: root.join("cgroup/skein"),
            };
            for b in boxes {
                std::fs::create_dir_all(places.requests.join(b)).unwrap();
                std::fs::create_dir_all(places.states.join(b).join("signals")).unwrap();
                std::fs::create_dir_all(places.cgroups.join(b)).unwrap();
                std::fs::write(places.cgroups.join(b).join("cgroup.procs"), "").unwrap();
            }
            Fleet { _dir: dir, places }
        }

        /// A wrapper in box `b` with pid `pid` files `id`, with any extra fields the box chooses.
        fn submit(&self, b: &str, id: &str, pid: u32, extra: serde_json::Value) {
            let mut body = serde_json::json!({
                "id": id, "class": "test", "argv": ["cargo", "test"], "cwd": "/w", "pid": pid,
                "state": "waiting",
            });
            if let (Some(o), Some(e)) = (body.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            std::fs::write(
                self.places.requests.join(b).join(format!("{id}.json")),
                body.to_string(),
            )
            .unwrap();
            self.live(b, pid, true);
        }

        /// Put `pid` into, or take it out of, box `b`'s `cgroup.procs`.
        fn live(&self, b: &str, pid: u32, alive: bool) {
            let path = self.places.cgroups.join(b).join("cgroup.procs");
            let mut pids: Vec<String> = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(str::to_string)
                .filter(|p| p != &pid.to_string())
                .collect();
            if alive {
                pids.push(pid.to_string());
            }
            std::fs::write(&path, pids.join("\n") + "\n").unwrap();
        }

        fn tick(&self, secs: i64, limits: Limits, repos: &[(&str, &str)]) -> Queue {
            let repos: BTreeMap<String, String> = repos
                .iter()
                .map(|(b, r)| (b.to_string(), r.to_string()))
                .collect();
            let repo_of = move |b: &str| repos.get(b).cloned().unwrap_or_else(|| b.to_string());
            tick(
                &self.places,
                limits,
                &repo_of,
                at(secs),
                &mut cgroup::Rates::default(),
            )
            .unwrap()
        }

        fn view(&self, b: &str) -> View {
            read_view(&self.places.states.join(b)).expect("no view written")
        }
    }

    /// A machine of `cores` cores, the default per-job cap and oversubscription, and memory unread.
    fn machine(cores: u32) -> Limits {
        Limits {
            cores,
            max_cores_per_job: DEFAULT_MAX_CORES_PER_JOB,
            oversubscribe: DEFAULT_OVERSUBSCRIBE,
            mem_free_mib: None,
        }
    }

    fn state_of<'a>(q: &'a Queue, id: &str) -> &'a str {
        q.jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.state.as_str())
            .unwrap_or("absent")
    }

    /// **A priority, a position or a grant written in a box's file moves nothing.**
    ///
    /// The first box files first and is granted the machine's two cores. A second box files a tick
    /// later, and a third with every field a box might use to jump the line: a priority, position
    /// 0, a grant, `granted` as its state, a grant of cores. It is still third, still waiting, and
    /// its view puts it second among the waiting.
    ///
    /// What would make it fail: a `priority` field on [`Request`] that [`schedule`] or [`merge`]
    /// orders by (the sabotage that was run), or a box's `state: granted` read as a grant.
    #[test]
    fn a_priority_written_in_a_box_file_moves_nothing() {
        let f = Fleet::new(&["web-main", "api-main", "ops-main"]);
        let limits = Limits {
            oversubscribe: 1.0,
            ..machine(2)
        };
        f.submit("web-main", "j-first", 101, serde_json::json!({}));
        f.tick(0, limits, &[]);
        f.submit("ops-main", "j-second", 102, serde_json::json!({}));
        f.tick(2, limits, &[]);
        f.submit(
            "api-main",
            "j-pushy",
            103,
            serde_json::json!({"priority": -100, "position": 0, "grant": true, "granted_cores": 2,
                               "granted": "2026-10-30T11:00:00Z", "queued": "2026-01-01T00:00:00Z"}),
        );
        let q = f.tick(4, limits, &[]);
        // And once more with `granted` as its state, in case a later tick reads what an earlier
        // one did not.
        f.submit(
            "api-main",
            "j-pushy",
            103,
            serde_json::json!({"priority": -100, "position": 0, "state": "granted"}),
        );
        let q2 = f.tick(6, limits, &[]);
        for q in [&q, &q2] {
            assert_eq!(state_of(q, "j-first"), "granted", "{q:#?}");
            assert_eq!(state_of(q, "j-second"), "waiting", "{q:#?}");
            assert_eq!(state_of(q, "j-pushy"), "waiting", "{q:#?}");
            let order: Vec<&str> = q.jobs.iter().map(|j| j.id.as_str()).collect();
            assert_eq!(
                order,
                ["j-first", "j-second", "j-pushy"],
                "the box's fields moved its job"
            );
        }
        let pushy = f.view("api-main");
        assert_eq!(pushy.jobs[0].position, Some(2), "{pushy:#?}");
        assert_eq!(pushy.jobs[0].granted_cores, 0, "{pushy:#?}");
        assert_eq!(pushy.jobs[0].reason, "another repo's job goes first");
    }

    /// **A wrapper that dies frees its cores, though it never wrote that it was done.**
    ///
    /// The grant is held by the wrapper's pid in its box's `cgroup.procs`, which is the kernel's
    /// to write.
    /// The pid is asserted present and the job granted first, so the freeing is a change and not
    /// an absence that was always there. Then the pid leaves the cgroup, with the request file
    /// still saying `waiting`, and the next job is granted on the next tick.
    ///
    /// What would make it fail: [`merge`] ending a job only on the box's `done` file (the
    /// liveness arm removed, the sabotage that was run), or checking the pid the file says now
    /// rather than the one first seen.
    #[test]
    fn a_dead_wrapper_frees_its_cores() {
        let f = Fleet::new(&["web-main", "api-main"]);
        let limits = Limits {
            oversubscribe: 1.0,
            ..machine(2)
        };
        f.submit("web-main", "j-dies", 301, serde_json::json!({}));
        f.tick(0, limits, &[]);
        f.submit("api-main", "j-next", 302, serde_json::json!({}));
        let q = f.tick(2, limits, &[]);
        let procs =
            std::fs::read_to_string(f.places.cgroups.join("web-main/cgroup.procs")).unwrap();
        assert!(
            procs.lines().any(|l| l == "301"),
            "the pid was never there: {procs:?}"
        );
        assert_eq!(state_of(&q, "j-dies"), "granted");
        assert_eq!(state_of(&q, "j-next"), "waiting");

        // Killed: out of the cgroup, and the file still says waiting. A box naming another live
        // pid of its own does not keep the grant either.
        f.live("web-main", 301, false);
        f.submit("web-main", "j-dies", 399, serde_json::json!({}));
        let q = f.tick(10, limits, &[]);
        assert_eq!(state_of(&q, "j-dies"), "gone", "{q:#?}");
        assert_eq!(state_of(&q, "j-next"), "granted", "{q:#?}");
        let gone = q.jobs.iter().find(|j| j.id == "j-dies").unwrap();
        assert_eq!(
            gone.ended,
            stamp(at(10)),
            "the end was not measured on the host"
        );
        let v = f.view("web-main");
        assert_eq!(v.jobs[0].ran_secs, Some(10), "{v:#?}");
    }

    /// A box asked to free memory waits with its reason, and another box runs past it.
    #[test]
    fn a_box_asked_to_free_memory_is_skipped_with_its_reason() {
        let f = Fleet::new(&["web-main", "api-main"]);
        let signals = serde_json::json!({"asks": [{"kind": "memory", "amount": 90,
            "since": "2026-10-30T11:59:00Z", "crossing_id": "c1", "hold": ""}]});
        std::fs::write(
            f.places.states.join("web-main/signals/resources.json"),
            signals.to_string(),
        )
        .unwrap();
        assert!(
            memory_asked(&f.places.states, "web-main"),
            "the fixture's ask did not parse"
        );
        let limits = machine(4);
        f.submit("web-main", "j-hot", 401, serde_json::json!({}));
        f.submit("api-main", "j-cool", 402, serde_json::json!({}));
        let q = f.tick(0, limits, &[]);
        assert_eq!(state_of(&q, "j-hot"), "waiting");
        assert_eq!(state_of(&q, "j-cool"), "granted");
        assert_eq!(
            f.view("web-main").jobs[0].reason,
            "this box is asked to free memory"
        );
    }

    /// **A link in a box's drop-box is never read, so a host file it points at is never queued.**
    ///
    /// skein-server can read files no box can. A box that put a link to one in its drop-box, or
    /// swapped a checked file for one before the read, would have skein read the host file as its
    /// request. The host file here is a valid request with a live pid, and the control proves it:
    /// the same bytes as a plain file in another box's drop-box are read and granted.
    ///
    /// What would make it fail: opening the request without `O_NOFOLLOW` (the sabotage that was
    /// run, in `boxfile::open`: `j-link` is queued and granted), or reading by path after a check
    /// of the path, the race this replaced.
    #[test]
    fn a_link_in_a_drop_box_to_a_host_file_is_never_read_or_granted() {
        let f = Fleet::new(&["web-main", "api-main"]);
        let host = f.places.queue.parent().unwrap().join("host-file.json");
        std::fs::create_dir_all(host.parent().unwrap()).unwrap();
        let body = |id: &str| {
            serde_json::json!({"id": id, "class": "test", "argv": ["x"], "cwd": "/w",
                               "pid": 501, "state": "waiting"})
            .to_string()
        };
        std::fs::write(&host, body("j-link")).unwrap();
        std::os::unix::fs::symlink(&host, f.places.requests.join("web-main/j-link.json")).unwrap();
        f.live("web-main", 501, true);
        // The control: the same request as a plain file is read and granted.
        std::fs::write(
            f.places.requests.join("api-main/j-real.json"),
            body("j-real"),
        )
        .unwrap();
        f.live("api-main", 501, true);

        let limits = machine(8);
        let q = f.tick(0, limits, &[]);
        assert_eq!(
            state_of(&q, "j-real"),
            "granted",
            "the control was not granted, so the fixture proves nothing: {q:#?}"
        );
        assert_eq!(
            state_of(&q, "j-link"),
            "absent",
            "skein followed a box's link and queued a host file as its request: {q:#?}"
        );
        assert!(
            !read_requests(&f.places.requests)["web-main"].contains_key("j-link"),
            "the link was read"
        );
    }

    /// **A FIFO in a box's drop-box does not hold up the tick.** Opened without `O_NONBLOCK`, a FIFO
    /// with no writer parks the opener until one comes, and the tick is skein-server's only
    /// scheduler: every box would wait with it. The box's real request beside it is still granted.
    ///
    /// What would make it fail: `O_NONBLOCK` dropped from `boxfile::open` (the sabotage that was
    /// run: the tick waits on the FIFO and `within` fails at 5 s). Dropping the regular-file check
    /// alone does **not** fail it, and cannot here: with `O_NONBLOCK` a FIFO with no writer reads as
    /// empty at once, which parses as nothing. That check is what refuses a FIFO that does have a
    /// writer, and anything else that is not a file.
    #[test]
    fn a_fifo_in_a_drop_box_does_not_hang_the_tick() {
        let f = Fleet::new(&["web-main"]);
        let fifo = f.places.requests.join("web-main/j-fifo.json");
        crate::boxfile::plant_fifo(&fifo);
        f.submit("web-main", "j-ok", 601, serde_json::json!({}));
        let places = f.places.clone();
        let q = crate::boxfile::within(&fifo, "the jobs tick", move || {
            tick(
                &places,
                machine(2),
                &|b: &str| b.to_string(),
                at(0),
                &mut cgroup::Rates::default(),
            )
            .unwrap()
        });
        assert_eq!(state_of(&q, "j-ok"), "granted", "{q:#?}");
        assert_eq!(state_of(&q, "j-fifo"), "absent", "{q:#?}");
    }

    /// **One box cannot queue more than [`PER_BOX`] jobs, and another box's job still runs.**
    ///
    /// The queue is shared, and a box writes as many request files as it likes. One box files
    /// `PER_BOX + 1` jobs, each with a live wrapper; exactly `PER_BOX` are queued. The machine has
    /// exactly one job's cores more than they take, so the other box's job, filed a tick later,
    /// is granted only if the flooding box was held to its share.
    ///
    /// What would make it fail: the `active >= PER_BOX` check in [`merge`] removed (the sabotage
    /// that was run: all `PER_BOX + 1` are queued and granted, and the other box's job waits for
    /// cores), or the count taken per tick rather than over the box's active jobs.
    #[test]
    fn a_box_cannot_queue_more_than_its_share_of_jobs() {
        let f = Fleet::new(&["web-main", "api-main"]);
        // Two cores a job, and no oversubscription: room for exactly `PER_BOX + 1` jobs.
        let limits = Limits {
            oversubscribe: 1.0,
            ..machine(2 * (PER_BOX as u32 + 1))
        };
        for n in 0..=PER_BOX {
            f.submit(
                "web-main",
                &format!("j-flood-{n:02}"),
                700 + n as u32,
                serde_json::json!({}),
            );
        }
        let q = f.tick(0, limits, &[]);
        let queued = q.jobs.iter().filter(|j| j.box_name == "web-main").count();
        assert_eq!(
            queued,
            PER_BOX,
            "one box queued more than its share: {queued} of {} filed",
            PER_BOX + 1
        );

        f.submit("api-main", "j-other", 799, serde_json::json!({}));
        let q = f.tick(2, limits, &[]);
        assert_eq!(
            q.jobs.iter().filter(|j| j.box_name == "web-main").count(),
            PER_BOX,
            "a later tick let the flooding box past its share"
        );
        assert_eq!(
            state_of(&q, "j-other"),
            "granted",
            "another box's job did not get its turn: {:?}",
            q.jobs.iter().find(|j| j.id == "j-other")
        );
    }

    /// A job of `repo`, in box `<repo>-<id>`, asking for `cores`, in `state` (`granted` holds
    /// all it asked for).
    fn job(id: &str, repo: &str, cores: u32, state: &str) -> Job {
        Job {
            id: id.into(),
            box_name: format!("{repo}-{id}"),
            repo: repo.into(),
            class: "build".into(),
            state: state.into(),
            cores,
            granted_cores: if state == "granted" { cores } else { 0 },
            ..Job::default()
        }
    }

    fn find<'a>(q: &'a Queue, id: &str) -> &'a Job {
        q.jobs
            .iter()
            .find(|j| j.id == id)
            .unwrap_or_else(|| panic!("no {id} in {q:#?}"))
    }

    fn no_box_asked(_: &str) -> bool {
        false
    }

    fn unmeasured(_: &Job) -> Option<f64> {
        None
    }

    /// **A repo alone fills the whole machine.** Five two-core jobs of one repo on an eight-core
    /// machine with nothing measured: four start, eight cores granted, and the fifth waits for two
    /// cores with none free. The box's view says the same in cores.
    ///
    /// What would make it fail: any per-repo cap left in [`schedule`], such as the old one of two
    /// jobs (the sabotage that was run: `if of_repo >= 4 { continue }` on the repo's cores, and
    /// only two jobs start).
    #[test]
    fn a_lone_repo_fills_the_whole_pool() {
        let f = Fleet::new(&["web-main"]);
        for n in 1..=5 {
            f.submit(
                "web-main",
                &format!("j-{n}"),
                800 + n,
                serde_json::json!({"cores": 2}),
            );
        }
        let limits = Limits {
            oversubscribe: 1.0,
            ..machine(8)
        };
        let q = f.tick(0, limits, &[("web-main", "web")]);
        for n in 1..=4 {
            assert_eq!(state_of(&q, &format!("j-{n}")), "granted", "{q:#?}");
        }
        assert_eq!(state_of(&q, "j-5"), "waiting", "{q:#?}");
        let v = f.view("web-main");
        assert_eq!((v.cores, v.granted, v.free), (8, 8, 0), "{v:#?}");
        let last = v.jobs.iter().find(|j| j.id == "j-5").unwrap();
        assert_eq!(last.reason, "waiting for 2 cores (0 free)");
        assert!(v.ahead.is_empty(), "nothing waits ahead of j-5: {v:#?}");
        assert!(v
            .jobs
            .iter()
            .filter(|j| j.state == "granted")
            .all(|j| j.granted_cores == 2));
    }

    /// **Another repo's arriving job takes the next freed cores, ahead of the busy repo's queued
    /// ones.** Repo `web` holds all eight cores and has a job queued; then a job of `api` arrives.
    /// `api` holds nothing, so its job goes first: it waits for the cores, and `web`'s queued job,
    /// though seen earlier, is told another repo's job goes first, with that job named by its box
    /// in its view. When one `web` job ends, its two cores go to `api`, and `web`'s job still
    /// waits.
    ///
    /// What would make it fail: the order ignoring the repo's cores, first-seen alone (the
    /// sabotage that was run: `web`'s queued job goes first and the reasons swap), or a job that
    /// does not fit letting the ones after it take what it waits for.
    #[test]
    fn another_repos_job_takes_the_next_freed_cores_first() {
        let f = Fleet::new(&["web-1", "web-2", "web-3", "web-4", "web-5", "api-1"]);
        let repos = [
            ("web-1", "web"),
            ("web-2", "web"),
            ("web-3", "web"),
            ("web-4", "web"),
            ("web-5", "web"),
            ("api-1", "api"),
        ];
        let limits = Limits {
            oversubscribe: 1.0,
            ..machine(8)
        };
        for n in 1..=5u32 {
            f.submit(
                &format!("web-{n}"),
                &format!("j-w{n}"),
                900 + n,
                serde_json::json!({}),
            );
        }
        f.tick(0, limits, &repos);
        // Its argv holds a path and a value no other box should read: the view shows two words.
        f.submit(
            "api-1",
            "j-a1",
            990,
            serde_json::json!({"argv": ["/opt/tools/bin/cargo", "--locked", "nextest",
                                        "run", "--token=abc123", "/srv/private/notes"]}),
        );
        let q = f.tick(2, limits, &repos);
        assert_eq!(state_of(&q, "j-w5"), "waiting", "{q:#?}");
        assert_eq!(state_of(&q, "j-a1"), "waiting", "{q:#?}");
        assert_eq!(find(&q, "j-a1").reason, "waiting for 2 cores (0 free)");
        assert_eq!(find(&q, "j-w5").reason, "another repo's job goes first");
        let w5 = f.view("web-5");
        assert_eq!(w5.jobs[0].position, Some(2), "{w5:#?}");
        assert_eq!(
            w5.ahead,
            [Ahead {
                box_name: "api-1".into(),
                id: "j-a1".into(),
                class: "test".into(),
                command: "cargo nextest".into(),
                cores: 2,
            }],
            "the job ahead is not named with its box"
        );
        assert_eq!(f.view("api-1").jobs[0].position, Some(1));

        std::fs::write(
            f.places.requests.join("web-1/j-w1.json"),
            r#"{"id":"j-w1","pid":901,"state":"done"}"#,
        )
        .unwrap();
        let q = f.tick(4, limits, &repos);
        assert_eq!(state_of(&q, "j-w1"), "done");
        assert_eq!(
            state_of(&q, "j-a1"),
            "granted",
            "the busy repo's queued job took the freed cores: {q:#?}"
        );
        assert_eq!(state_of(&q, "j-w5"), "waiting", "{q:#?}");
    }

    /// **However little running jobs are measured to use, the cores granted stay within 1.5×.**
    /// Four cores, so a ceiling of six; three jobs hold two each and are measured at nothing. A
    /// one-core job would fit by use, and waits, with nothing free.
    ///
    /// What would make it fail: the ceiling dropped from what is free (the sabotage that was run:
    /// `by_use` alone, and the job is granted a seventh core).
    #[test]
    fn oversubscription_stays_within_the_ceiling_however_low_the_use() {
        let limits = machine(4);
        assert_eq!(limits.grant_ceiling(), 6);
        let mut q = Queue {
            schema: 1,
            jobs: vec![
                job("r1", "web", 2, "granted"),
                job("r2", "api", 2, "granted"),
                job("r3", "ops", 2, "granted"),
                job("w1", "doc", 1, "waiting"),
            ],
        };
        let pool = schedule(&mut q, limits, &no_box_asked, &|_| Some(0.0), None);
        assert_eq!(state_of(&q, "w1"), "waiting", "{q:#?}");
        assert_eq!(find(&q, "w1").reason, "waiting for 1 core (0 free)");
        assert_eq!(
            pool,
            Pool {
                cores: 4,
                granted: 6,
                free: 0
            }
        );
    }

    /// **Measured use lets in a job its grants alone would not.** Four cores; two jobs hold two
    /// each, so by their grants the machine is full. Measured at half a core each, the waiting
    /// two-core job starts, and the grants reach six, past the cores and within the ceiling. The
    /// control: with no reading, the same queue waits.
    ///
    /// What would make it fail: `in_use` ignored, every job counted at its grant (the sabotage
    /// that was run: the job waits with nothing free).
    #[test]
    fn measured_use_lets_in_a_job_its_grants_alone_would_not() {
        let queue = Queue {
            schema: 1,
            jobs: vec![
                job("r1", "web", 2, "granted"),
                job("r2", "api", 2, "granted"),
                job("w1", "ops", 2, "waiting"),
            ],
        };
        let mut unread = queue.clone();
        schedule(&mut unread, machine(4), &no_box_asked, &unmeasured, None);
        assert_eq!(
            state_of(&unread, "w1"),
            "waiting",
            "the control started it with nothing measured, so this proves nothing"
        );

        let mut q = queue;
        let pool = schedule(&mut q, machine(4), &no_box_asked, &|_| Some(0.5), None);
        assert_eq!(state_of(&q, "w1"), "granted", "{q:#?}");
        assert_eq!(find(&q, "w1").granted_cores, 2);
        assert_eq!(pool.granted, 6);
    }

    /// **A busy machine holds a job that the jobs' own accounting would let in.** Eleven cores,
    /// one job holding two and measured at one: by the jobs alone, ten cores are free, and a
    /// four-core job fits. But the machine is 9.5 cores busy with work outside the queue, so it
    /// waits and says so. The control: the same queue with the machine unread starts it. And what
    /// a pass grants counts against the reading: at six busy the first job takes four of the five
    /// idle cores, and the second then finds the machine at ten and waits.
    ///
    /// What would make it fail: `machine_busy` ignored in [`schedule`] (the sabotage that was run:
    /// `idle` always all of it, and the job starts), or the reading taken as the whole truth, with
    /// what this pass already granted not added to it.
    #[test]
    fn a_busy_machine_holds_a_job_the_jobs_alone_would_admit() {
        let queue = Queue {
            schema: 1,
            jobs: vec![
                job("r1", "web", 2, "granted"),
                job("w1", "api", 4, "waiting"),
                job("w2", "ops", 4, "waiting"),
            ],
        };
        let mut unread = queue.clone();
        schedule(
            &mut unread,
            machine(11),
            &no_box_asked,
            &|_| Some(1.0),
            None,
        );
        assert_eq!(
            state_of(&unread, "w1"),
            "granted",
            "the control did not start it, so this proves nothing: {unread:#?}"
        );

        let mut q = queue.clone();
        let pool = schedule(
            &mut q,
            machine(11),
            &no_box_asked,
            &|_| Some(1.0),
            Some(9.5),
        );
        assert_eq!(state_of(&q, "w1"), "waiting", "{q:#?}");
        assert_eq!(
            find(&q, "w1").reason,
            "machine busy: 9.5 of 11 cores in use"
        );
        assert_eq!(find(&q, "w2").reason, "another repo's job goes first");
        assert_eq!(pool.free, 1);

        // Six busy: the first takes four of the five idle; the second, asking four, then finds
        // the machine at ten and waits, though the jobs' own count says six are free.
        let mut q = queue;
        schedule(
            &mut q,
            machine(11),
            &no_box_asked,
            &|_| Some(1.0),
            Some(6.0),
        );
        assert_eq!(find(&q, "w1").granted_cores, 4, "{q:#?}");
        assert_eq!(state_of(&q, "w2"), "waiting", "{q:#?}");
        assert_eq!(find(&q, "w2").reason, "machine busy: 10 of 11 cores in use");
    }

    /// **A job that needs more memory than is free waits, with how much, and does not hold up a
    /// job that fits.** 3.5 GB free: a `ci` job, expected at 4 GB, waits; the `build` job behind
    /// it (2 GB) starts; a second `build` job then finds the 1.5 GB left and waits too.
    ///
    /// What would make it fail: the memory gate removed from [`schedule`] (the sabotage that was
    /// run: all three start), the memory a grant takes not subtracted within the pass (the last
    /// one starts), or the `ci` estimate back at 3 GB (the sabotage that was run: the `ci` job
    /// starts in 3.5 GB, which a CI run measured at about 7 GB at its peak does not fit).
    #[test]
    fn the_memory_gate_refuses_a_job_that_would_not_fit() {
        let mut ci = job("w-ci", "web", 2, "waiting");
        ci.class = "ci".into();
        let mut q = Queue {
            schema: 1,
            jobs: vec![
                ci,
                job("w-b1", "api", 2, "waiting"),
                job("w-b2", "ops", 2, "waiting"),
            ],
        };
        let limits = Limits {
            mem_free_mib: Some(3584),
            ..machine(16)
        };
        schedule(&mut q, limits, &no_box_asked, &unmeasured, None);
        assert_eq!(state_of(&q, "w-ci"), "waiting", "{q:#?}");
        assert_eq!(find(&q, "w-ci").reason, "memory: needs ~4 GB, 3.5 GB free");
        assert_eq!(state_of(&q, "w-b1"), "granted", "{q:#?}");
        assert_eq!(state_of(&q, "w-b2"), "waiting", "{q:#?}");
        assert_eq!(find(&q, "w-b2").reason, "memory: needs ~2 GB, 1.5 GB free");
    }

    /// **Another box is shown a command in two words, never its argv.**
    ///
    /// What would make it fail: the whole argv shown (a path, a token), a flag or a `KEY=value`
    /// taken as the second word, a path kept whole in either word (the sabotage that was run:
    /// `word` returning `w` itself, and `/usr/bin/cargo` is shown), or a job queued before the
    /// short form showing nothing.
    #[test]
    fn a_short_command_is_the_program_and_its_first_plain_argument() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(short_command(&argv(&["cargo", "test"])), "cargo test");
        assert_eq!(
            short_command(&argv(&[
                "/usr/bin/cargo",
                "-q",
                "--x=1",
                "test",
                "--",
                "secret"
            ])),
            "cargo test"
        );
        assert_eq!(
            short_command(&argv(&[
                "env",
                "TOKEN=abc",
                "/srv/scripts/run.sh",
                "--key",
                "k"
            ])),
            "env run.sh"
        );
        assert_eq!(short_command(&argv(&["make", "-j8"])), "make");
        assert_eq!(short_command(&argv(&[])), "");
        assert_eq!(short_command(&argv(&[&"x".repeat(60)])), "x".repeat(40));

        let mut old = job("j-old", "web", 2, "waiting");
        old.box_name = "web-old".into();
        old.command = "/usr/bin/cargo build --release".into();
        let mut mine = job("j-mine", "api", 2, "waiting");
        mine.box_name = "api-main".into();
        let q = Queue {
            schema: 1,
            jobs: vec![old, mine],
        };
        let v = view_for(&q, "api-main", Pool::default(), at(0));
        assert_eq!(v.ahead.len(), 1, "{v:#?}");
        assert_eq!(v.ahead[0].command, "cargo build");
    }

    /// **A job with `--min` starts smaller when that is what fits, and never below its min.** Four
    /// cores, three granted: a job asking four with a min of one starts with one. Behind it, with
    /// nothing free, a job with a min of two waits for two.
    ///
    /// What would make it fail: `min` ignored, a job started only with all it asked (the sabotage
    /// that was run: the first job waits for four cores), or `give` taken as `min` rather than
    /// what fits.
    #[test]
    fn a_job_with_a_min_starts_with_fewer_cores_when_that_is_what_fits() {
        let mut flexible = job("w1", "api", 4, "waiting");
        flexible.min = 1;
        let mut firm = job("w2", "ops", 3, "waiting");
        firm.min = 2;
        let mut q = Queue {
            schema: 1,
            jobs: vec![job("r1", "web", 3, "granted"), flexible, firm],
        };
        schedule(&mut q, machine(4), &no_box_asked, &unmeasured, None);
        assert_eq!(state_of(&q, "w1"), "granted", "{q:#?}");
        assert_eq!(find(&q, "w1").granted_cores, 1, "{q:#?}");
        assert_eq!(state_of(&q, "w2"), "waiting", "{q:#?}");
        assert_eq!(find(&q, "w2").reason, "waiting for 2 cores (0 free)");
    }

    /// **A request file from before cores asks for the default, an ask past the cap is held to it,
    /// and a queue from before cores still reads.** The old file has no `cores` and is granted
    /// two; a box asking for 64 is held to four; a queue file whose granted job has no core fields
    /// reads, and the job counts at the default.
    ///
    /// What would make it fail: [`default_cores`] other than two (the sabotage that was run:
    /// three, and the old file is granted three), `cores` on [`Job`] without its serde default
    /// (also run: the old queue no longer reads), or the ask not held to
    /// [`Limits::max_cores_per_job`]. (`cores` on [`Request`] is an `Option`, which serde reads as
    /// `None` when absent with or without the attribute, so dropping that one fails nothing.)
    #[test]
    fn an_old_request_without_cores_asks_for_the_default() {
        let f = Fleet::new(&["web-main", "api-main"]);
        std::fs::write(
            f.places.requests.join("web-main/j-old.json"),
            r#"{"id":"j-old","class":"build","argv":["make"],"cwd":"/w","pid":951,"state":"waiting"}"#,
        )
        .unwrap();
        f.live("web-main", 951, true);
        f.submit(
            "api-main",
            "j-greedy",
            952,
            serde_json::json!({"cores": 64}),
        );
        let q = f.tick(0, machine(16), &[]);
        let old = find(&q, "j-old");
        assert_eq!(
            (old.state.as_str(), old.cores, old.granted_cores),
            ("granted", 2, 2)
        );
        let greedy = find(&q, "j-greedy");
        assert_eq!((greedy.cores, greedy.granted_cores), (4, 4), "{greedy:#?}");

        let before: Queue = serde_json::from_str(
            r#"{"schema":1,"jobs":[{"id":"j-was","box":"web-main","repo":"web","class":"build",
                "command":"make","cwd":"/w","pid":1,"state":"granted","queued":"2026-10-30T12:00:00Z"}]}"#,
        )
        .expect("a queue from before cores no longer reads");
        assert_eq!(before.jobs[0].holds(), 2);
    }
}
