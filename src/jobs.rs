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
//! **A slot is freed by the kernel's word, not the box's.** A granted job holds its slot while the
//! wrapper's pid is in the box's `cgroup.procs`; a wrapper killed with `-9` writes no done file,
//! and leaves the slot on the next tick anyway. Every duration is measured on the host's clock.
//!
//! Not in phase 1: bumping, priority asks, the owner reordering from the cockpit, an ETA, MCP
//! tools.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the queue is looked at: fast enough that a freed slot is taken within a couple of
/// seconds. Its own loop, apart from the resource signal's minute, because a job waiting a minute
/// for a slot that was free is the waste this queue exists to remove.
pub const LOOK_EVERY: Duration = Duration::from_secs(2);

/// How many of one repo's jobs may run at once unless the config says otherwise.
pub const DEFAULT_REPO_CAP: usize = 2;

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
    /// The wrapper's pid. Its presence in the box's `cgroup.procs` is what holds a slot.
    #[serde(default)]
    pub pid: u32,
    /// `waiting`, or the box's `cancelled` or `done`. Anything else reads as `waiting`.
    #[serde(default)]
    pub state: String,
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
    /// Why a waiting job did not start on the last tick, such as `repo at cap 2/2`.
    #[serde(default)]
    pub reason: String,
}

impl Job {
    fn is_active(&self) -> bool {
        self.state == "waiting" || self.state == "granted"
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

/// How many jobs may run, fleet-wide and per repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub fleet_slots: usize,
    pub repo_cap: usize,
}

/// The fleet's slots for a sandbox with `cores` CPUs: one for every four, and never none.
pub fn fleet_slots_for(cores: u64) -> usize {
    ((cores / 4) as usize).max(1)
}

/// The limits as configured: `jobs_fleet_slots` and `jobs_repo_cap`, each `0` for the default.
/// The fleet's cores are the sandbox's `nproc`, from the resource reading
/// (`fleet::fleet_resources`), or this process's own count when the sandbox did not answer, since
/// skein-server runs in the sandbox.
pub fn limits() -> Limits {
    let config = crate::config::load_config();
    let cores = crate::fleet::fleet_resources()
        .map(|r| r.cpus)
        .filter(|&c| c > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get() as u64)
                .unwrap_or(1)
        });
    Limits {
        fleet_slots: match config.jobs_fleet_slots {
            0 => fleet_slots_for(cores),
            n => n as usize,
        },
        repo_cap: match config.jobs_repo_cap {
            0 => DEFAULT_REPO_CAP,
            n => n as usize,
        },
    }
}

/// One box's view of its jobs: what the wrapper waits on and the agents panel shows.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    #[serde(default = "schema_one")]
    pub schema: u32,
    /// RFC3339, when skein wrote it.
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub fleet_slots: usize,
    #[serde(default)]
    pub running: usize,
    #[serde(default)]
    pub repo_cap: usize,
    #[serde(default)]
    pub jobs: Vec<ViewJob>,
}

/// One of the box's own jobs, in its view.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewJob {
    pub id: String,
    pub class: String,
    pub command: String,
    pub cwd: String,
    pub state: String,
    /// Its place among every waiting job in the fleet, from 1. `None` once it is not waiting.
    pub position: Option<usize>,
    pub reason: String,
    /// Seconds it waited, or has waited so far, by the host's clock.
    pub waited_secs: i64,
    /// Seconds it ran, once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_secs: Option<i64>,
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
        if !crate::util::valid_name(&name) {
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
            // Not followed: a box can put a link in its own drop-box, and a link is not a request.
            let Ok(meta) = std::fs::symlink_metadata(file.path()) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_REQUEST_BYTES || !id_is_nameable(&id) {
                continue;
            }
            let Some(req) = std::fs::read_to_string(file.path())
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

/// Whether `pid` is in box `name`'s cgroup, by the kernel's `cgroup.procs`. A box with no cgroup
/// to read (no delegation) falls back to the pid existing at all, since boxes share the sandbox's
/// pid namespace.
pub fn pid_in_box(cgroups: &Path, name: &str, pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    match std::fs::read_to_string(cgroups.join(name).join("cgroup.procs")) {
        Ok(procs) => procs.lines().any(|l| l.trim() == pid.to_string()),
        Err(_) => Path::new("/proc").join(pid.to_string()).exists(),
    }
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
            // The pid skein first saw, not whatever the file says now: a box cannot keep a slot
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
            queue.jobs.push(Job {
                id: id.clone(),
                box_name: name.clone(),
                repo: repo_of(name),
                class,
                command,
                cwd: req.cwd.chars().take(1024).collect(),
                pid: req.pid,
                state: "waiting".into(),
                queued: stamp(now),
                ..Job::default()
            });
        }
    }
}

/// Grant what can run, first-come.
///
/// A waiting job is **skipped and keeps its place** when its repo is at its cap, its box has been
/// asked to free memory, or the fleet's slots are full; the job behind it is still considered, so
/// one repo at its cap does not hold up another repo's work. Each skipped job carries the reason.
pub fn schedule(queue: &mut Queue, limits: Limits, memory_asked: &dyn Fn(&str) -> bool) {
    let mut running = queue.jobs.iter().filter(|j| j.state == "granted").count();
    let mut per_repo: BTreeMap<String, usize> = BTreeMap::new();
    for j in queue.jobs.iter().filter(|j| j.state == "granted") {
        *per_repo.entry(j.repo.clone()).or_default() += 1;
    }
    for job in queue.jobs.iter_mut().filter(|j| j.state == "waiting") {
        let of_repo = per_repo.get(&job.repo).copied().unwrap_or(0);
        if of_repo >= limits.repo_cap {
            job.reason = format!("repo at cap {of_repo}/{}", limits.repo_cap);
            continue;
        }
        if memory_asked(&job.box_name) {
            job.reason = "this box is asked to free memory".into();
            continue;
        }
        if running >= limits.fleet_slots {
            job.reason = format!("fleet full {running}/{}", limits.fleet_slots);
            continue;
        }
        job.state = "granted".into();
        job.reason.clear();
        running += 1;
        *per_repo.entry(job.repo.clone()).or_default() += 1;
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
pub fn view_for(queue: &Queue, name: &str, limits: Limits, now: DateTime<Utc>) -> View {
    let mut position = 0;
    let mut jobs = Vec::new();
    for job in &queue.jobs {
        if job.state == "waiting" {
            position += 1;
        }
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
            position: (job.state == "waiting").then_some(position),
            reason: job.reason.clone(),
            waited_secs: secs_between(&job.queued, waited_until),
            ran_secs: (!job.ended.is_empty() && !job.granted.is_empty())
                .then(|| parse_stamp(&job.ended).map(|e| secs_between(&job.granted, e)))
                .flatten(),
        });
    }
    View {
        schema: 1,
        at: stamp(now),
        fleet_slots: limits.fleet_slots,
        running: queue.jobs.iter().filter(|j| j.state == "granted").count(),
        repo_cap: limits.repo_cap,
        jobs,
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
pub fn tick(
    places: &Places,
    limits: Limits,
    repo_of: &dyn Fn(&str) -> String,
    now: DateTime<Utc>,
) -> Result<Queue, String> {
    let mut queue = read_queue(&places.queue);
    let requests = read_requests(&places.requests);
    let cgroups = places.cgroups.clone();
    let alive = move |name: &str, pid: u32| pid_in_box(&cgroups, name, pid);
    merge(&mut queue, &requests, &alive, repo_of, now);
    let before: BTreeSet<(String, String)> = queue
        .jobs
        .iter()
        .filter(|j| j.state == "granted")
        .map(|j| (j.box_name.clone(), j.id.clone()))
        .collect();
    let states = places.states.clone();
    schedule(&mut queue, limits, &|name| memory_asked(&states, name));
    granted_now(&mut queue, &before, now);
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
        let view = view_for(&queue, &name, limits, now);
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
                    std::fs::metadata(&path)
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
    loop {
        tick_every.tick().await;
        let ran = tokio::task::spawn_blocking(|| {
            tick(&Places::live(), limits(), &repo_of_box, Utc::now()).map(|_| ())
        })
        .await;
        let said = match ran {
            Ok(Ok(())) => String::new(),
            Ok(Err(e)) => format!("skein: the heavy-work queue: {e}"),
            Err(e) => format!("skein: the heavy-work queue did not run: {e}"),
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
            tick(&self.places, limits, &repo_of, at(secs)).unwrap()
        }

        fn view(&self, b: &str) -> View {
            read_view(&self.places.states.join(b)).expect("no view written")
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
    /// The first box files first and is granted the one slot. A second box files a tick later with
    /// every field a box might use to jump the line: a priority, position 1, a grant, `granted` as
    /// its state. It is still second, still waiting, and its view puts it first among the waiting
    /// only because it is the only one waiting.
    ///
    /// What would make it fail: a `priority` field on [`Request`] that [`schedule`] or [`merge`]
    /// orders by (the sabotage that was run), or a box's `state: granted` read as a grant.
    #[test]
    fn a_priority_written_in_a_box_file_moves_nothing() {
        let f = Fleet::new(&["web-main", "api-main", "ops-main"]);
        let limits = Limits {
            fleet_slots: 1,
            repo_cap: 2,
        };
        f.submit("web-main", "j-first", 101, serde_json::json!({}));
        f.tick(0, limits, &[]);
        f.submit("ops-main", "j-second", 102, serde_json::json!({}));
        f.tick(2, limits, &[]);
        f.submit(
            "api-main",
            "j-pushy",
            103,
            serde_json::json!({"priority": -100, "position": 0, "grant": true,
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
        assert_eq!(pushy.jobs[0].reason, "fleet full 1/1");
    }

    /// **A repo at its cap is skipped, keeps its place, and the next repo's job runs.**
    ///
    /// Two of repo `web`'s jobs run and a third waits; behind it is a job of repo `api`. With four
    /// fleet slots, the third `web` job is skipped with its reason and the `api` job is granted
    /// past it. When a `web` job finishes, the waiting `web` job is next, ahead of an `api` job
    /// that came after it.
    ///
    /// What would make it fail: `break` in place of `continue` at the repo cap in [`schedule`]
    /// (the sabotage that was run: the `api` job waits behind the capped one), or a skipped job
    /// moved to the back of the line.
    #[test]
    fn a_repo_at_cap_is_skipped_and_the_next_repo_runs() {
        let f = Fleet::new(&["web-1", "web-2", "web-3", "api-1"]);
        let limits = Limits {
            fleet_slots: 4,
            repo_cap: 2,
        };
        let repos = [
            ("web-1", "web"),
            ("web-2", "web"),
            ("web-3", "web"),
            ("api-1", "api"),
        ];
        f.submit("web-1", "j-w1", 201, serde_json::json!({}));
        f.submit("web-2", "j-w2", 202, serde_json::json!({}));
        f.tick(0, limits, &repos);
        f.submit("web-3", "j-w3", 203, serde_json::json!({}));
        f.tick(2, limits, &repos);
        f.submit("api-1", "j-a1", 204, serde_json::json!({}));
        let q = f.tick(4, limits, &repos);
        assert_eq!(state_of(&q, "j-w1"), "granted");
        assert_eq!(state_of(&q, "j-w2"), "granted");
        assert_eq!(state_of(&q, "j-w3"), "waiting", "{q:#?}");
        assert_eq!(
            state_of(&q, "j-a1"),
            "granted",
            "a repo at its cap held up another repo's job:\n{q:#?}"
        );
        let w3 = f.view("web-3");
        assert_eq!(w3.jobs[0].position, Some(1));
        assert_eq!(w3.jobs[0].reason, "repo at cap 2/2");

        // Its place is kept: a later `api` job does not pass it once `web` has room.
        f.submit("api-1", "j-a2", 205, serde_json::json!({}));
        f.tick(6, limits, &repos);
        std::fs::write(
            f.places.requests.join("web-1/j-w1.json"),
            r#"{"id":"j-w1","pid":201,"state":"done"}"#,
        )
        .unwrap();
        let q = f.tick(8, limits, &repos);
        assert_eq!(state_of(&q, "j-w1"), "done");
        assert_eq!(state_of(&q, "j-w3"), "granted", "{q:#?}");
        assert_eq!(state_of(&q, "j-a2"), "waiting", "{q:#?}");
        assert_eq!(
            q.jobs.iter().find(|j| j.id == "j-a2").unwrap().reason,
            "fleet full 4/4"
        );
    }

    /// **A wrapper that dies frees its slot, though it never wrote that it was done.**
    ///
    /// The slot is the wrapper's pid in its box's `cgroup.procs`, which is the kernel's to write.
    /// The pid is asserted present and the job granted first, so the freeing is a change and not
    /// an absence that was always there. Then the pid leaves the cgroup, with the request file
    /// still saying `waiting`, and the next job is granted on the next tick.
    ///
    /// What would make it fail: [`merge`] ending a job only on the box's `done` file (the
    /// liveness arm removed, the sabotage that was run), or checking the pid the file says now
    /// rather than the one first seen.
    #[test]
    fn a_dead_wrapper_frees_its_slot() {
        let f = Fleet::new(&["web-main", "api-main"]);
        let limits = Limits {
            fleet_slots: 1,
            repo_cap: 2,
        };
        f.submit("web-main", "j-dies", 301, serde_json::json!({}));
        f.submit("api-main", "j-next", 302, serde_json::json!({}));
        let q = f.tick(0, limits, &[]);
        let procs =
            std::fs::read_to_string(f.places.cgroups.join("web-main/cgroup.procs")).unwrap();
        assert!(
            procs.lines().any(|l| l == "301"),
            "the pid was never there: {procs:?}"
        );
        assert_eq!(state_of(&q, "j-dies"), "granted");
        assert_eq!(state_of(&q, "j-next"), "waiting");

        // Killed: out of the cgroup, and the file still says waiting. A box naming another live
        // pid of its own does not keep the slot either.
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
        let limits = Limits {
            fleet_slots: 2,
            repo_cap: 2,
        };
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

    #[test]
    fn the_fleet_has_one_slot_for_every_four_cores_and_never_none() {
        assert_eq!(fleet_slots_for(0), 1);
        assert_eq!(fleet_slots_for(3), 1);
        assert_eq!(fleet_slots_for(11), 2);
        assert_eq!(fleet_slots_for(16), 4);
    }
}
