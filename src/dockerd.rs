//! The Docker daemon coming back is a blip, not a fleet restart — and what the kernel says about
//! memory pressure while it runs.
//!
//! **Why skein owns this at all.** `dockerd` runs in the fleet sandbox with pid 1 for a parent and
//! nothing supervising it, so a container that gets it killed costs a rebuild of the *whole fleet*
//! to recover one process. Something long-lived inside the sandbox has to watch it.
//!
//! **Why here rather than in an agent.** It used to live in `src/fleet-agent.py`, on the correct
//! grounds that the agent was already the long-lived in-sandbox process. The agent is deleted
//! (architecture §13a, SKEIN-521) because it existed to survive a host-to-guest hop that no longer
//! happens — and `skein-server` is now the long-lived in-sandbox process, so the two jobs the agent
//! had that were *not* transport moved here rather than going with it. Neither capability is lost:
//! the watchdog is the same decisions, and [`counters`] reads the same files.
//!
//! ## The decisions, and why each is the way it is
//!
//! Every collaborator is injected ([`World`]) so the behaviour can be driven without a daemon:
//! finding it, reading its argv, spawning it, the clock, the sleep, and the shield. What is left in
//! [`Watch::look`] is the decision, which is the part worth asserting — `tests/docker_watchdog.rs`
//! drives all seven of them.
//!
//! **The argv is refreshed on every pass, not remembered once.** If something else restarts dockerd
//! with different arguments, the truth is whatever is running now.
//!
//! **A daemon that was never seen is reported, not guessed at.** There is no default command line
//! worth inventing: a wrong one starts a daemon nobody configured.
//!
//! **The grace before a restart is what lets another supervisor win.** `PPID 1` says nothing about
//! whether init spawned dockerd or merely reaped it, so something unseen may be supervising it, and
//! two dockerds is a worse failure than none.
//!
//! **The backoff doubles on success as well as on failure.** A daemon that starts and dies
//! immediately would otherwise be respawned every grace period for ever, which is the busy loop
//! this exists to avoid.

use serde_json::json;
use std::sync::Mutex;
use std::time::Duration;

/// How often a pass runs when nothing is wrong.
pub const POLL: Duration = Duration::from_secs(5);

/// How long anything else that supervises the daemon has to win the race.
pub const GRACE: Duration = Duration::from_secs(20);

/// The ceiling the backoff doubles towards.
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// What the daemon's `oom_score_adj` is lowered to. Halves its badness to the global killer.
pub const OOM_SCORE: i32 = -500;

/// The processes worth shielding: the daemon and the runtime it drives.
pub const PROCESSES: [&str; 2] = ["dockerd", "containerd"];

/// The cgroups whose pressure is worth reading, and what each one answers.
///
///   * `skein` — the whole workload's ceiling: the boxes and the containers they start.
///   * `skein/containers` — a runaway container, once it has a ceiling of its own to hit.
///   * `docker` — the daemon itself, which is uncapped, so only its kills matter here.
pub const WATCHED: [&str; 3] = ["skein", "skein/containers", "docker"];

/// What one pass did. The word a test asserts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The daemon is there. Its argv was refreshed and its shield renewed.
    Alive,
    /// It was gone and this brought it back.
    Restarted,
    /// It was gone and something else brought it back during the grace.
    Recovered,
    /// It was gone, this tried, and the spawn failed.
    Failed,
    /// It is gone and no command line was ever seen, so there is nothing to bring back.
    Unknown,
}

impl Verdict {
    /// The word, for a test and for a log line.
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Alive => "alive",
            Verdict::Restarted => "restarted",
            Verdict::Recovered => "recovered",
            Verdict::Failed => "failed",
            Verdict::Unknown => "unknown",
        }
    }
}

/// Find the running daemon, or say there is none.
pub type Find = Box<dyn FnMut() -> Option<u32> + Send>;
/// Read one pid's command line.
pub type ArgvOf = Box<dyn FnMut(u32) -> Option<Vec<String>> + Send>;
/// Start the daemon again. `Err` is a restart that did not happen.
pub type Spawn = Box<dyn FnMut(&[String]) -> Result<(), String> + Send>;
/// Every pid whose `comm` is one of these names.
pub type Kin = Box<dyn FnMut(&[&str]) -> Vec<u32> + Send>;
/// Lower one pid's `oom_score_adj`, and say whether it took.
pub type Shield = Box<dyn FnMut(u32) -> bool + Send>;

/// Everything [`Watch`] reaches outside itself, so a test can be the outside.
pub struct World {
    /// The running daemon's pid, or `None`.
    pub find: Find,
    /// The command line that pid was started with.
    pub argv_of: ArgvOf,
    /// Start it again, detached. `Err` is a restart that did not happen.
    pub spawn: Spawn,
    /// The clock, for the restart stamp.
    pub now: Box<dyn FnMut() -> String + Send>,
    /// The grace, and the backoff.
    pub sleep: Box<dyn FnMut(Duration) + Send>,
    /// Where a line a person should read goes.
    pub log: Box<dyn FnMut(&str) + Send>,
    /// Every pid whose `comm` is one of these.
    pub kin: Kin,
    /// Lower one pid's `oom_score_adj`. `true` when it now reads what was asked.
    pub shield: Shield,
}

impl World {
    /// The real one: `/proc`, `sudo` and the wall clock.
    pub fn real() -> World {
        World {
            find: Box::new(|| daemon_pid("/proc")),
            argv_of: Box::new(|pid| daemon_argv(pid, "/proc")),
            spawn: Box::new(start_detached),
            now: Box::new(|| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            sleep: Box::new(std::thread::sleep),
            log: Box::new(|line| eprintln!("{line}")),
            kin: Box::new(|names| pids_named(names, "/proc")),
            shield: Box::new(|pid| shield(pid, OOM_SCORE, "/proc")),
        }
    }
}

/// The watchdog: watch the daemon, remember how it was started, and bring it back when nothing else
/// does.
pub struct Watch {
    world: World,
    /// The last command line seen, which is what a restart uses.
    pub argv: Option<Vec<String>>,
    /// How many times *this* watchdog has brought it back.
    pub restarts: u64,
    /// When it last did, in UTC.
    pub last_restart: String,
    /// How many of [`PROCESSES`] are shielded as of the last pass.
    pub shielded: usize,
    /// One line about the current state, for the reading below.
    pub note: String,
    backoff: Duration,
    complained: bool,
}

impl Watch {
    /// A watchdog over the given world.
    pub fn over(world: World) -> Watch {
        Watch {
            world,
            argv: None,
            restarts: 0,
            last_restart: String::new(),
            shielded: 0,
            note: "watching".into(),
            backoff: GRACE,
            complained: false,
        }
    }

    /// A watchdog over the real `/proc`.
    pub fn real() -> Watch {
        Watch::over(World::real())
    }

    /// What the reading carries. Small on purpose — this is a fact, not a diagnosis.
    pub fn snapshot(&mut self) -> serde_json::Value {
        json!({
            "pid": (self.world.find)(),
            "restarts": self.restarts,
            "last_restart": self.last_restart,
            "argv_known": self.argv.is_some(),
            "shielded": self.shielded,
            "note": self.note,
        })
    }

    /// One pass.
    pub fn look(&mut self) -> Verdict {
        if let Some(pid) = (self.world.find)() {
            // Refreshed rather than remembered once: if something else restarts dockerd with
            // different arguments, the truth is whatever is running now.
            if let Some(argv) = (self.world.argv_of)(pid) {
                self.argv = Some(argv);
            }
            self.note = "watching".into();
            self.backoff = GRACE;
            self.complained = false;
            // Every pass, not once. A restarted daemon is a new pid, and a shield that does not
            // survive the restart it exists for is not one. Cheap: two `/proc` reads that return
            // early once the value is already what was asked for.
            let kin = (self.world.kin)(&PROCESSES);
            self.shielded = kin.into_iter().filter(|p| (self.world.shield)(*p)).count();
            return Verdict::Alive;
        }

        let Some(argv) = self.argv.clone() else {
            // Nothing to bring back. Said once, because a line every five seconds is noise and the
            // condition does not change on its own.
            if !self.complained {
                self.complained = true;
                (self.world.log)(
                    "skein: dockerd is not running and skein never saw it, so it has no command \
                     line to restart it with. Rebuild or restart the fleet.",
                );
            }
            self.note = "gone, and no command line was ever seen".into();
            return Verdict::Unknown;
        };

        // The grace. Anything else that supervises this daemon gets to win, because two dockerds is
        // a worse failure than none — and a `PPID 1` says nothing about whether such a thing exists.
        (self.world.sleep)(self.backoff);
        if (self.world.find)().is_some() {
            self.note = "something else restarted it".into();
            return Verdict::Recovered;
        }

        if let Err(e) = (self.world.spawn)(&argv) {
            self.note = format!("restart failed: {e}");
            (self.world.log)(&format!("skein: could not restart dockerd: {e}"));
            self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
            return Verdict::Failed;
        }

        self.restarts += 1;
        self.last_restart = (self.world.now)();
        self.note = "restarted by skein".into();
        let said = format!(
            "skein: dockerd was gone; restarted it ({} so far)",
            self.restarts
        );
        (self.world.log)(&said);
        // Backed off even on success: a daemon that starts and dies immediately would otherwise be
        // respawned every grace period for ever, which is the busy loop this exists to avoid.
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        Verdict::Restarted
    }
}

/// The one watchdog this process runs, so the reading and the loop are the same state.
static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// What `/machine` used to answer about the daemon.
///
/// `null` until the loop has started, which is honest: a snapshot from a watchdog that has never
/// looked would be a fact nobody established.
pub fn snapshot() -> serde_json::Value {
    match WATCH.lock() {
        Ok(mut held) => match held.as_mut() {
            Some(watch) => watch.snapshot(),
            None => serde_json::Value::Null,
        },
        Err(_) => serde_json::Value::Null,
    }
}

/// Watch for as long as this process runs. Started once, by `skein-server`.
///
/// A stumble is reported and the loop continues: a watchdog that dies of one bad pass is no
/// watchdog, which is the same reasoning the daemon's own restart is built on.
pub fn watch_forever() {
    if let Ok(mut held) = WATCH.lock() {
        if held.is_none() {
            *held = Some(Watch::real());
        }
    }
    loop {
        match WATCH.lock() {
            Ok(mut held) => {
                if let Some(watch) = held.as_mut() {
                    watch.look();
                }
            }
            Err(_) => eprintln!("skein: the docker watchdog's state is poisoned"),
        }
        std::thread::sleep(POLL);
    }
}

/// The running dockerd's pid, or `None`.
///
/// Read from `/proc` rather than from `/run/docker.pid`, because the pidfile is exactly what a
/// crash leaves behind: a number naming a process that is gone, or worse, one since reused. `comm`
/// is the kernel's own answer to "what is this process", and it cannot be argued with.
pub fn daemon_pid(proc: &str) -> Option<u32> {
    pids_named(&["dockerd"], proc).into_iter().next()
}

/// Every pid whose `comm` is one of `names`.
pub fn pids_named(names: &[&str], proc: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(proc) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        // It exited between the listing and the read, which is ordinary.
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
            if names.contains(&comm.trim()) {
                found.push(pid);
            }
        }
    }
    found.sort_unstable();
    found
}

/// The command line dockerd was started with, while there is still a dockerd to ask.
///
/// This is the whole reason the watchdog polls rather than waiting to be told: `/proc/<pid>/cmdline`
/// is readable now and gone the moment it matters. Remembering it while the daemon is healthy is
/// what makes bringing it back an option — and not remembering it is a fact to report rather than a
/// reason to guess at a command line.
pub fn daemon_argv(pid: u32, proc: &str) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("{proc}/{pid}/cmdline")).ok()?;
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    (!argv.is_empty()).then_some(argv)
}

/// Make the global OOM killer look elsewhere first. `true` if it now reads what was asked.
///
/// Lowering `oom_score_adj` needs privilege, so this falls back to `sudo` exactly as the restart
/// does — and **reads the value back rather than trusting the write**, because the failure is
/// silent: a refused write and a successful one both return without complaint through a shell.
pub fn shield(pid: u32, score: i32, proc: &str) -> bool {
    let path = format!("{proc}/{pid}/oom_score_adj");
    let reads =
        |path: &str| -> Option<i32> { std::fs::read_to_string(path).ok()?.trim().parse().ok() };
    match reads(&path) {
        // Already shielded, by us or by whoever started it.
        Some(had) if had <= score => return true,
        // Not readable at all is a pid that has gone, not a shield that failed to take.
        None => return false,
        Some(_) => {}
    }
    if std::fs::write(&path, score.to_string()).is_err() {
        let _ = std::process::Command::new("sudo")
            .args(["-n", "sh", "-c", &format!("echo {score} > {path}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    reads(&path).is_some_and(|now| now <= score)
}

/// Spawn it detached, so it outlives this process exactly as the original did.
///
/// `sudo` only when skein is not already root — dockerd needs root, and asking for it when it is
/// already held would fail on a sandbox with no sudo rather than work.
fn start_detached(argv: &[String]) -> Result<(), String> {
    let root = effective_uid() == Some(0);
    let (program, rest): (&str, Vec<&str>) = match root {
        true => (
            argv[0].as_str(),
            argv[1..].iter().map(String::as_str).collect(),
        ),
        false => (
            "sudo",
            std::iter::once("-n")
                .chain(argv.iter().map(String::as_str))
                .collect(),
        ),
    };
    std::process::Command::new(program)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// This process's **effective** uid, read from `/proc/self/status` rather than through a C call.
///
/// The field is `Uid: <real> <effective> <saved> <fs>`, and the effective one is what decides
/// whether `sudo` is needed — which is the question, since asking for privilege already held fails
/// on a sandbox with no sudo rather than working. `None` is "could not tell", and the caller treats
/// that as not-root: reaching for `sudo` unnecessarily fails loudly, while skipping it when it was
/// needed fails silently at the daemon.
fn effective_uid() -> Option<u32> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// What the kernel says about memory pressure, read straight out of the filesystem.
///
/// **Free, and that is the property that matters.** These are file reads with no subprocess, which
/// is what lets skein ask for them without the board's tick paying for it — see
/// `tests/board_cost.rs`, which measures exactly that, and `signal::cost`, which declares it.
///
/// Everything here is a counter **since this boot**. Nothing is interpreted: a rate is
/// `fleet::pressure`'s business, because only it knows when it last looked.
pub fn counters(cgroup: &str, vmstat: &str) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for name in WATCHED {
        let base = format!("{cgroup}/{name}");
        let mut here = serde_json::Map::new();
        let Ok(events) = std::fs::read_to_string(format!("{base}/memory.events")) else {
            // A fleet without this cgroup is not a fleet with a problem to report.
            continue;
        };
        for line in events.lines() {
            if let Some((key, value)) = line.split_once(' ') {
                if let Ok(n) = value.trim().parse::<u64>() {
                    here.insert(key.to_string(), json!(n));
                }
            }
        }
        // `memory.events` is HIERARCHICAL — every field counts the whole subtree, so a container's
        // OOM appears in `skein/containers`, again in `skein`, and again in the VM-wide vmstat.
        // `memory.events.local` is the same fields for THIS cgroup alone, which is the only shape a
        // per-cgroup figure can honestly be built from (FLEET-9).
        //
        // Its absence is tolerated rather than fatal: this file is newer than `memory.events`
        // (Linux 5.2), and a kernel without it is a fleet whose subtree totals are still worth
        // reporting rather than one to drop from the reading.
        if let Ok(local) = std::fs::read_to_string(format!("{base}/memory.events.local")) {
            for line in local.lines() {
                if let Some((key, value)) = line.split_once(' ') {
                    if let Ok(n) = value.trim().parse::<u64>() {
                        here.insert(format!("local_{key}"), json!(n));
                    }
                }
            }
        }
        for field in ["memory.current", "memory.max"] {
            if let Ok(v) = std::fs::read_to_string(format!("{base}/{field}")) {
                here.insert(field.to_string(), json!(v.trim()));
            }
        }
        out.insert(name.to_string(), serde_json::Value::Object(here));
    }
    // The kernel's own tally, which is what says whether anything was killed OUTSIDE the cgroups
    // skein wrote — the case that took the daemon down and left no trace in any of them.
    if let Ok(text) = std::fs::read_to_string(vmstat) {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("oom_kill ") {
                if let Ok(n) = rest.trim().parse::<u64>() {
                    out.insert("vmstat_oom_kill".into(), json!(n));
                }
                break;
            }
        }
    }
    serde_json::Value::Object(out)
}

/// The whole reading `fleet::pressure` rates: the daemon, and what the kernel says.
///
/// The same two halves the deleted agent's `/machine` answered with, under the same two keys, so
/// `fleet::rate_between` reads it unchanged.
pub fn reading() -> serde_json::Value {
    json!({
        "docker": snapshot(),
        "pressure": counters("/sys/fs/cgroup", "/proc/vmstat"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counters are read for what the kernel writes, and the hierarchical trap is not fallen
    /// into.
    ///
    /// `memory.events` in cgroup v2 counts the whole SUBTREE, so a container's OOM lands in
    /// `skein/containers`, again in `skein`, and again in `/proc/vmstat`. Summing them reported one
    /// kill as three, and the number is what the board shows somebody deciding whether to raise a
    /// ceiling (FLEET-9). `memory.events.local` is the per-cgroup half, kept under a `local_`
    /// prefix so a reader can tell which it has.
    ///
    /// **What makes this fail**: adding the per-cgroup figures together, or dropping the vmstat
    /// tally that `fleet::rate_between` reads `killed` from — which is the one number that needs no
    /// assumption about which cgroup a process was in when the kernel took it.
    #[test]
    fn the_counters_are_read_per_cgroup_and_the_kernels_own_tally_is_kept() {
        let dir = crate::testutil::tempdir();
        for name in WATCHED {
            let base = dir.join("cgroup").join(name);
            std::fs::create_dir_all(&base).unwrap();
            std::fs::write(
                base.join("memory.events"),
                "low 0\nhigh 7\nmax 1\noom 2\noom_kill 3\n",
            )
            .unwrap();
            std::fs::write(base.join("memory.current"), "1234\n").unwrap();
        }
        // Only one of them has the newer per-cgroup file, which is the tolerance the reading needs:
        // it is Linux 5.2+, and a kernel without it still has subtree totals worth reporting.
        std::fs::write(
            dir.join("cgroup/skein/memory.events.local"),
            "high 2\noom_kill 1\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("vmstat"),
            "nr_free_pages 100\noom_kill 5\npgfault 9\n",
        )
        .unwrap();

        let got = counters(
            &dir.join("cgroup").to_string_lossy(),
            &dir.join("vmstat").to_string_lossy(),
        );

        // The three cgroups `fleet::rate_between` names, each with its own figure rather than a sum.
        assert_eq!(got["skein"]["high"], 7);
        assert_eq!(got["skein/containers"]["high"], 7);
        assert_eq!(got["docker"]["oom_kill"], 3);
        // Per-cgroup, kept apart from the subtree total under its own prefix.
        assert_eq!(got["skein"]["local_high"], 2);
        assert!(
            got["skein/containers"].get("local_high").is_none(),
            "a cgroup with no local file grew one: {got}"
        );
        // A string, because it may be the literal `max` — a number would have to invent something
        // for that, and `max` is what an uncapped cgroup actually says.
        assert_eq!(got["skein"]["memory.current"], "1234");
        // The kernel's own tally, which is what `killed` is derived from.
        assert_eq!(got["vmstat_oom_kill"], 5);
    }

    /// A fleet without these cgroups is not a fleet with a problem to report.
    #[test]
    fn a_missing_cgroup_contributes_nothing_rather_than_failing_the_reading() {
        let got = counters("/skein-no-such-cgroup-root", "/skein-no-such-vmstat");
        assert_eq!(
            got,
            serde_json::json!({}),
            "an absent cgroup tree produced something other than an empty reading: {got}"
        );
    }
}
