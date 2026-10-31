//! What each command used when it ran before, per repo (SKEIN-1421), so a job that names no cores
//! asks for what that command really uses, and the memory gate expects what it really needs.
//!
//! **Keyed by repo and command.** The command is the job's [`super::Job::short`], its program and
//! first plain argument (`cargo test`), never its whole argv: the same command in another folder or
//! with other flags is the same work often enough, and the short form holds no path or value. The
//! repo is the one the job counts against ([`super::repo_of_box`]), so two boxes of one repo share
//! what they learn and two repos never do.
//!
//! **Written by skein-server alone**, in the tick that sees a job end (`jobs::tick`), on the host
//! beside the queue ([`super::Places::history`]), where no box can write. A box's own word reaches
//! it only as the command's two words; everything else in a run is what the job's cgroup counted.
//!
//! **Bounded.** [`RUNS_KEPT`] runs per command, the oldest dropped first, and [`COMMANDS_KEPT`]
//! commands, the one that ran longest ago dropped first; a file larger than [`MAX_BYTES`] is not
//! read, and the history starts again.
//!
//! **What it gives.** The cores to ask for: the 75th percentile of the cores the command used on
//! average, rounded up ([`History::cores`]). The memory to expect: the most it held at once in any
//! kept run, plus a margin ([`History::memory_mib`]). A command with no runs yet gives neither, and
//! the queue's own defaults hold.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// How many runs of one command are kept: enough for a percentile, few enough that a change in
/// what the command does shows within a day of runs.
pub const RUNS_KEPT: usize = 20;

/// How many commands are kept, across every repo.
pub const COMMANDS_KEPT: usize = 100;

/// A history file larger than this is not read. Twice what [`RUNS_KEPT`] × [`COMMANDS_KEPT`] runs
/// take as written, so only a file something else wrote is ever refused.
pub const MAX_BYTES: u64 = 2 << 20;

/// The least memory added to a command's past peak, in MiB: a peak is one run's, and the next may
/// hold a little more.
pub const MEMORY_MARGIN_MIB: u64 = 256;

/// One finished run of a command.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// When it ended, by the host's clock.
    pub ended: String,
    /// The cores it asked for.
    pub asked: u32,
    /// Its CPU time over the seconds it ran.
    pub avg_cores: f64,
    /// The most cores it was seen using between two of the queue's readings, when it ran long
    /// enough to be read twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_cores: Option<f64>,
    /// The most memory it held at once, in MiB, where the kernel counted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_mem_mib: Option<u64>,
    /// The seconds it ran.
    pub secs: i64,
}

/// Every run of one command in one repo, oldest first.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub repo: String,
    pub command: String,
    #[serde(default)]
    pub runs: Vec<Run>,
}

/// The history file.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default = "schema_one")]
    pub schema: u32,
    /// The command that ran longest ago first.
    #[serde(default)]
    pub commands: Vec<Command>,
}

fn schema_one() -> u32 {
    1
}

impl History {
    /// The history at `path`; empty when there is none, it does not parse, or it is larger than
    /// [`MAX_BYTES`].
    pub fn read(path: &Path) -> History {
        let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_BYTES);
        small
            .then(|| std::fs::read_to_string(path).ok())
            .flatten()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// The runs of `command` in `repo`, oldest first.
    pub fn runs(&self, repo: &str, command: &str) -> &[Run] {
        self.commands
            .iter()
            .find(|c| c.repo == repo && c.command == command)
            .map(|c| c.runs.as_slice())
            .unwrap_or(&[])
    }

    /// Add a run of `command` in `repo`, keeping [`RUNS_KEPT`] of it and [`COMMANDS_KEPT`] commands.
    /// A command with no name is not kept: a job from before the short form had none.
    pub fn record(&mut self, repo: &str, command: &str, run: Run) {
        if command.is_empty() {
            return;
        }
        let mut entry = match self
            .commands
            .iter()
            .position(|c| c.repo == repo && c.command == command)
        {
            Some(at) => self.commands.remove(at),
            None => Command {
                repo: repo.to_string(),
                command: command.to_string(),
                runs: Vec::new(),
            },
        };
        entry.runs.push(run);
        let over = entry.runs.len().saturating_sub(RUNS_KEPT);
        entry.runs.drain(..over);
        self.commands.push(entry);
        let over = self.commands.len().saturating_sub(COMMANDS_KEPT);
        self.commands.drain(..over);
    }

    /// The cores `command` in `repo` should ask for: the 75th percentile (nearest rank) of the
    /// cores its runs used on average, rounded up, at least one and at most `cap`. `None` before it
    /// has run.
    pub fn cores(&self, repo: &str, command: &str, cap: u32) -> Option<u32> {
        let mut avg: Vec<f64> = self
            .runs(repo, command)
            .iter()
            .map(|r| r.avg_cores)
            .filter(|a| a.is_finite() && *a >= 0.0)
            .collect();
        if avg.is_empty() {
            return None;
        }
        avg.sort_by(f64::total_cmp);
        let rank = (avg.len() * 3).div_ceil(4).max(1);
        let p75 = avg[rank - 1];
        Some((p75.ceil() as u32).clamp(1, cap.max(1)))
    }

    /// The memory `command` in `repo` is expected to need, in MiB: the most any kept run held at
    /// once, plus a quarter or [`MEMORY_MARGIN_MIB`], whichever is more. `None` while no run of it
    /// was counted.
    pub fn memory_mib(&self, repo: &str, command: &str) -> Option<u64> {
        let peak = self
            .runs(repo, command)
            .iter()
            .filter_map(|r| r.peak_mem_mib)
            .max()?;
        Some(peak + (peak / 4).max(MEMORY_MARGIN_MIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(avg: f64, mem: Option<u64>) -> Run {
        Run {
            ended: "2026-10-31T12:00:00Z".into(),
            asked: 4,
            avg_cores: avg,
            peak_cores: None,
            peak_mem_mib: mem,
            secs: 60,
        }
    }

    /// **The default is the 75th percentile of the average, rounded up and capped; the memory is
    /// the past peak plus a margin; another repo's runs count for nothing.**
    ///
    /// Four runs averaging 0.4, 1.1, 1.3 and 7.0 cores: the third of four is the 75th percentile,
    /// 1.3, which rounds up to 2. A cap of one holds it to one. The peaks are 900 and 3000 MiB, so
    /// the estimate is 3000 and a quarter, 3750; a run of 100 MiB alone gets the 256 MiB margin.
    ///
    /// What would make it fail: the highest run in place of the percentile (the sabotage that was
    /// run: the default reads 4, the cap), the mean (2.45, so 3), rounding down (1), the cap
    /// ignored, the last run's peak in place of the highest, or the lookup ignoring the repo.
    #[test]
    fn the_default_is_the_75th_percentile_rounded_up_and_the_memory_the_peak_plus_a_margin() {
        let mut h = History::default();
        for (avg, mem) in [
            (1.1, Some(3000)),
            (7.0, None),
            (0.4, Some(900)),
            (1.3, None),
        ] {
            h.record("web", "cargo test", run(avg, mem));
        }
        assert_eq!(h.cores("web", "cargo test", 4), Some(2));
        assert_eq!(h.cores("web", "cargo test", 1), Some(1));
        assert_eq!(h.memory_mib("web", "cargo test"), Some(3750));
        assert_eq!(h.cores("api", "cargo test", 4), None);
        assert_eq!(h.memory_mib("api", "cargo test"), None);

        h.record("api", "make", run(0.0, Some(100)));
        assert_eq!(h.cores("api", "make", 4), Some(1), "never fewer than one");
        assert_eq!(h.memory_mib("api", "make"), Some(356));
    }

    /// **The file stays bounded: [`RUNS_KEPT`] runs a command, [`COMMANDS_KEPT`] commands, the
    /// oldest going first; and a file over [`MAX_BYTES`] is not read.**
    ///
    /// What would make it fail: either trim missing (the sabotage that was run: the runs' `drain`
    /// taken out, and one command holds 25 runs), a trim that drops the newest, a command run again
    /// not moved to the end (so the one just used is the first dropped), or the size check gone.
    #[test]
    fn the_history_keeps_only_its_bound_and_drops_the_oldest_first() {
        let mut h = History::default();
        for i in 0..RUNS_KEPT + 5 {
            h.record("web", "cargo test", run(i as f64, None));
        }
        let kept = h.runs("web", "cargo test");
        assert_eq!(kept.len(), RUNS_KEPT);
        assert_eq!(
            kept[0].avg_cores, 5.0,
            "the oldest runs were not the ones dropped"
        );

        for i in 0..COMMANDS_KEPT {
            h.record("web", &format!("tool{i}"), run(1.0, None));
        }
        assert_eq!(h.commands.len(), COMMANDS_KEPT);
        assert!(
            h.runs("web", "cargo test").is_empty(),
            "the command that ran longest ago was kept"
        );
        h.record("web", "tool0", run(1.0, None));
        h.record("web", "fresh", run(1.0, None));
        assert!(
            !h.runs("web", "tool0").is_empty(),
            "a command just run was dropped"
        );
        assert!(h.runs("web", "tool1").is_empty());

        h.record("web", "", run(1.0, None));
        assert!(h.commands.iter().all(|c| !c.command.is_empty()));

        let dir = crate::testutil::tempdir();
        let path = (dir.as_ref() as &Path).join("history.json");
        std::fs::write(&path, serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(History::read(&path), h);
        let mut big = serde_json::to_string(&h).unwrap();
        big.push_str(&" ".repeat(MAX_BYTES as usize));
        std::fs::write(&path, big).unwrap();
        assert_eq!(History::read(&path), History::default());
    }
}
