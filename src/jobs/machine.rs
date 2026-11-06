//! How busy the whole machine is, for the queue's admission (SKEIN-1430).
//!
//! **Why the queue's own counts are not enough.** [`super::schedule`] counts what the jobs it
//! granted use, but most of what loads the machine never asks it: an agent's own browser run, a
//! Docker e2e run, another repo's tests. Two e2e runs and WebKit beside them once drove the load
//! to 50 and timed WebKit's tests out while the queue saw cores to spare.
//!
//! **What is read.** The kernel's `cpu` line in `/proc/stat`, the CPU time every CPU has spent in
//! each state since boot, read **in-process**: skein-server runs in the sandbox, so its `/proc` is
//! the sandbox's machine. Two readings a tick apart give the busy share of the time between them,
//! and the share times the CPUs the file lists is the cores busy. Busy is everything but `idle`
//! and `iowait`; the two guest fields are already inside `user` and `nice`, and are not counted
//! twice. One file read per tick, against `fleet::box_loads`' sandbox exec and interval.
//!
//! **Kept by whoever ticks**, as [`super::cgroup::Rates`] is: `jobs::watch_jobs` holds the
//! readings from one tick to the next, the last 15 minutes of them for the cockpit's averages.
//! The first tick, and a tick after a reading that did not parse, has no rate and gates nothing on
//! it.

use super::cgroup::Readings;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::Path;

/// The `cpu` line of `/proc/stat`, in the kernel's clock ticks, and how many CPUs the file lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Every state but idle and iowait.
    pub busy: u64,
    /// Every state.
    pub total: u64,
    /// The `cpuN` lines.
    pub cpus: u32,
}

/// Read `/proc/stat`'s text. `None` when it has no `cpu` line with at least the four fields every
/// kernel since 2.6 writes.
pub fn parse(text: &str) -> Option<CpuTimes> {
    let fields: Vec<u64> = text
        .lines()
        .find_map(|l| l.strip_prefix("cpu "))?
        .split_whitespace()
        .map(|f| f.parse().ok())
        .collect::<Option<Vec<u64>>>()?;
    if fields.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal guest guest_nice: the first eight are the
    // time, and the guest two are already counted in user and nice.
    let total: u64 = fields.iter().take(8).sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    let cpus = text
        .lines()
        .filter(|l| {
            // `cpu0`, `cpu1`, …: the word itself, so the total's `cpu ` is not one of them.
            l.split_whitespace()
                .next()
                .and_then(|w| w.strip_prefix("cpu"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .count() as u32;
    Some(CpuTimes {
        busy: total.saturating_sub(idle),
        total,
        cpus,
    })
}

/// The cores busy between two readings: the busy share of the time that passed, times the CPUs.
/// `None` when no time passed or a counter went backwards. `fallback_cpus` stands in when the
/// file listed none.
pub fn busy_between(then: CpuTimes, now: CpuTimes, fallback_cpus: u32) -> Option<f64> {
    let total = now.total.checked_sub(then.total).filter(|&t| t > 0)?;
    let busy = now.busy.checked_sub(then.busy)?;
    let cpus = match now.cpus {
        0 => fallback_cpus.max(1),
        n => n,
    };
    Some((busy as f64 / total as f64).min(1.0) * cpus as f64)
}

/// The whole machine's readings, kept from one tick to the next ([`super::cgroup::Readings`]).
#[derive(Debug, Default, Clone)]
pub struct MachineBusy {
    kept: Readings<CpuTimes>,
}

/// How busy the whole machine is: at the last reading, which is what the queue admits on, and on
/// average over each span of [`super::cgroup::AVERAGE_OVER`] the readings cover, for the cockpit's
/// jobs list.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Busy {
    /// The cores busy since the reading before. `None` gates nothing on it.
    pub now: Option<f64>,
    /// The cores busy on average over each covered span, keyed by its seconds.
    pub over: BTreeMap<String, f64>,
}

impl Busy {
    /// Busy `now`, with no averages: what a test of the admission hands the tick.
    pub fn now(now: Option<f64>) -> Busy {
        Busy {
            now,
            ..Busy::default()
        }
    }
}

impl MachineBusy {
    /// Take `text` as `/proc/stat` read at `at` and return the cores busy since the last reading.
    /// A reading that does not parse forgets every one before it, so the next starts over.
    pub fn observe(
        &mut self,
        text: Option<&str>,
        fallback_cpus: u32,
        at: DateTime<Utc>,
    ) -> Option<f64> {
        let Some(now) = text.and_then(parse) else {
            self.kept.clear();
            return None;
        };
        let then = self.kept.latest();
        self.kept.push(now, at);
        busy_between(then?.0, now, fallback_cpus)
    }

    /// [`MachineBusy::observe`] of the file at `path`, `/proc/stat` in skein-server, with the
    /// averages its readings now cover.
    pub fn read(&mut self, path: &Path, fallback_cpus: u32, at: DateTime<Utc>) -> Busy {
        let text = std::fs::read_to_string(path).ok();
        let now = self.observe(text.as_deref(), fallback_cpus, at);
        Busy {
            now,
            over: self.averages(fallback_cpus),
        }
    }

    /// The cores busy on average over each span of [`super::cgroup::AVERAGE_OVER`] the readings
    /// cover, keyed by its seconds.
    pub fn averages(&self, fallback_cpus: u32) -> BTreeMap<String, f64> {
        self.kept
            .over(|(then, _), (now, _)| busy_between(then, now, fallback_cpus))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(user: u64, system: u64, idle: u64, iowait: u64, guest: u64) -> String {
        format!(
            "cpu  {user} 0 {system} {idle} {iowait} 0 0 0 {guest} 0\n\
             cpu0 0 0 0 0 0 0 0 0 0 0\ncpu1 0 0 0 0 0 0 0 0 0 0\ncpu2 0 0 0 0 0 0 0 0 0 0\n\
             cpu3 0 0 0 0 0 0 0 0 0 0\nintr 1 2 3\nctxt 99\n"
        )
    }

    /// **Two readings of `/proc/stat` give the cores busy between them.**
    ///
    /// Four CPUs. Between the readings 400 ticks pass: 250 in user and system, 120 idle and 30
    /// waiting on I/O, so the machine was 62.5% busy, 2.5 cores. 50 ticks of guest time are already
    /// inside user, and counting them again would read about 2.7.
    ///
    /// What would make it fail: iowait counted as busy (the sabotage that was run: `idle` without
    /// `iowait`, and the reading is 2.8), the guest fields added to the total, the share not
    /// multiplied by the CPUs the file lists, the first reading answered with a rate, or a reading
    /// that did not parse kept as the last.
    #[test]
    fn two_readings_give_the_cores_busy_between_them() {
        let mut m = MachineBusy::default();
        let t = |secs| DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(secs);
        assert_eq!(
            m.observe(Some(&stat(1000, 500, 4000, 100, 200)), 11, t(0)),
            None
        );
        let busy = m.observe(Some(&stat(1200, 550, 4120, 130, 250)), 11, t(2));
        assert_eq!(busy, Some(2.5));
        assert_eq!(m.observe(Some("no cpu line here"), 11, t(4)), None);
        assert_eq!(
            m.observe(Some(&stat(1300, 600, 4200, 140, 250)), 11, t(6)),
            None,
            "a reading after one that did not parse has nothing to compare with"
        );
        // A file listing no CPUs is taken at the machine's count.
        let then = parse("cpu  10 0 0 10\n").unwrap();
        let now = parse("cpu  20 0 0 20\n").unwrap();
        assert_eq!(busy_between(then, now, 11), Some(5.5));
        assert_eq!(
            busy_between(now, then, 11),
            None,
            "a counter went backwards"
        );
    }

    /// **The whole machine's readings give its average over each span they cover, and leave out a
    /// span they do not.** Four CPUs, read every two seconds: all four busy for the first 30
    /// seconds, then one. At 90 s the last minute, from the reading at 30 s, is one core busy, and
    /// five minutes is not covered.
    ///
    /// What would make it fail: a minute given before 60 s of readings, the minute averaged from
    /// the first reading (it reads 2), or a span not covered given anyway.
    #[test]
    fn the_machines_readings_give_each_covered_spans_average() {
        let t = |secs| DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(secs);
        // Clock ticks of 100 a second: busy for the CPUs busy, idle for the rest.
        let at = |secs: u64| {
            let busy = 400 * secs.min(30) + 100 * secs.saturating_sub(30);
            stat(busy, 0, 400 * secs - busy, 0, 0)
        };
        let mut m = MachineBusy::default();
        for secs in (0..=58).step_by(2) {
            m.observe(Some(&at(secs)), 4, t(secs as i64));
        }
        assert!(m.averages(4).is_empty(), "{:?}", m.averages(4));
        for secs in (60..=90).step_by(2) {
            m.observe(Some(&at(secs)), 4, t(secs as i64));
        }
        let over = m.averages(4);
        assert_eq!(over.get("60").copied(), Some(1.0), "{over:?}");
        assert!(!over.contains_key("300"), "{over:?}");
    }

    /// **The real `/proc/stat` of this machine reads, twice, as a busy count between none and
    /// every CPU it lists.** The format is the kernel's, so it is read from the kernel once.
    ///
    /// What would make it fail: a parser that does not take the kernel's own line (two spaces after
    /// `cpu`, ten fields), or a CPU count that does not match the `cpuN` lines.
    #[cfg(target_os = "linux")]
    #[test]
    fn this_machines_proc_stat_reads_as_a_busy_count() {
        let text = std::fs::read_to_string("/proc/stat").unwrap();
        let first = parse(&text).expect("the kernel's /proc/stat did not parse");
        let listed = text
            .lines()
            .filter(|l| l.starts_with("cpu") && !l.starts_with("cpu "))
            .count() as u32;
        assert_eq!(first.cpus, listed);
        let mut m = MachineBusy::default();
        m.read(Path::new("/proc/stat"), 1, Utc::now());
        std::thread::sleep(std::time::Duration::from_millis(300));
        let busy = m
            .read(Path::new("/proc/stat"), 1, Utc::now())
            .now
            .expect("no rate from two readings 300 ms apart");
        assert!(
            (0.0..=listed as f64).contains(&busy),
            "{busy} cores busy of {listed}"
        );
    }
}
