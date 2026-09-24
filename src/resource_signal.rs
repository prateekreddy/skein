//! Each box's resource signal file: what skein reads of one box's disk, memory, throttle rate and
//! process count, and whether anything is asked of it.
//!
//! Design: box-plugin §1 (signals S2–S6 and the compositions over them) and §2.2, slice S1
//! (SKEIN-1054). skein-server writes `<box_state>/signals/resources.json` for every box once a
//! minute. The box reads it and cannot write it: the whole state directory is bound read-only into
//! the box by `box-session.sh` (`--ro-bind "$state" "$state"`), the same bind that makes `inbox/`
//! trustworthy. So a file found there was put there by skein, and nothing a box writes is ever
//! interpolated into it. The plugin's hooks (slice S2) read this file and do nothing else.
//!
//! # Levels, and the crossing computed from them
//!
//! Every figure here is a level that is read again on the next tick, so a lost tick costs a minute
//! of delay and never a wrong state. An **ask** is the one derived thing, and it is a band: a kind
//! is either asked of this box or it is not, and a *crossing* is the band changing. It is computed
//! from the previous file and this reading, never latched from an event, which is the rule
//! `announce::step` already follows. A crossing mints a new `crossing_id`; while the band holds, the
//! id and `since` carry over and only the figures move. The plugin holds one command per id.
//!
//! **A reading that could not be taken leaves the band where it was.** The fleet's disk not
//! answering, or a box's cgroup being unreadable, is `Unknown`, and `Unknown` may drive neither a
//! new ask nor the clearing of an old one. The alternative is a flicker: one unreadable tick clears
//! the ask, the next raises it again under a new id, and the agent is held a second time for a
//! crossing it was already shown.
//!
//! # The three asks, and the one that is never made
//!
//! * **disk**: this box is in the audience `announce::cover` chooses for a fleet over its line, or
//!   this box holds more than its own allowance.
//! * **memory**: this box's own throttle rate is at least 60 a minute on two consecutive readings,
//!   **and** its anonymous memory is at least 90% of its `memory.high`. One hot reading is a build
//!   linking; two, with the box near its line, is a box that needs to stop something.
//! * **pids**: at least 75% of the box's `pids.max`.
//! * **never CPU.** `cpu.weight` fair-shares on purpose (`fleet::box_limits`' own doc): a lone box
//!   is meant to take every core and hand them back under contention, so a busy box is not a box
//!   doing anything wrong. `cores` is in the file as a level for the reader, and no ask kind exists
//!   for it.
//!
//! Nothing here stops, kills or throttles anything. The kernel's limits are the ceiling whether or
//! not an agent reads this; the file is how the agent is told first (owner's answers 1 and 3).

use crate::health::gib;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often skein looks. The design's 60 s: fast enough that a hold arrives within a minute or
/// two of the crossing, and slow enough that the throttle rate is a rate rather than noise.
pub const LOOK_EVERY: Duration = Duration::from_secs(60);

/// Throttles a minute at which a box's memory counts as hot. The same 60 the health banner calls
/// noticeable for the whole workload (`THROTTLE_NOTICEABLE` in `src/health/report.rs`), applied to
/// one box's own counter.
pub const THROTTLED_PER_MIN: f64 = 60.0;

/// Anonymous memory, as a percentage of `memory.high`, above which a hot box is asked.
pub const ANON_OF_HIGH_PCT: u64 = 90;

/// Processes, as a percentage of `pids.max`, at which a box is asked.
pub const PIDS_OF_MAX_PCT: u64 = 75;

/// The kinds an ask can be. **There is no `cpu`**, and the plugin's hook holds only for these.
pub const ASK_KINDS: [&str; 3] = ["disk", "memory", "pids"];

/// Where the file lives for one box, given that box's state directory.
pub fn signal_path(state: &Path) -> PathBuf {
    state.join("signals").join("resources.json")
}

// ------------------------------------------------------------------------------------------------
// What is measured
// ------------------------------------------------------------------------------------------------

/// One box's cgroup, read once. Every counter is since the cgroup was created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cgroup {
    /// `cpu.stat` `usage_usec`.
    pub usage_usec: u64,
    /// `memory.stat` `anon`, bytes.
    pub anon: u64,
    /// `memory.current`, bytes.
    pub current: u64,
    /// `memory.high`, bytes. `None` is `max`: no line to be near.
    pub high: Option<u64>,
    /// `memory.max`, bytes. `None` is `max`.
    pub max: Option<u64>,
    /// `pids.current`.
    pub pids: u64,
    /// `pids.max`. `None` is `max`.
    pub pids_max: Option<u64>,
    /// `memory.events.local` `high`: times THIS cgroup was throttled at its `memory.high`, not
    /// counting its children. The hierarchical `memory.events` would charge a box for a container's
    /// throttling (FLEET-9). `None` on a kernel without the file.
    pub throttled: Option<u64>,
}

/// Read one box's cgroup directory. `None` when the box has no cgroup, which is a stopped box or
/// one whose launcher could not get delegation; both are a reading that could not be taken.
pub fn read_cgroup(dir: &Path) -> Option<Cgroup> {
    let text = |file: &str| std::fs::read_to_string(dir.join(file)).ok();
    let number = |file: &str| text(file).and_then(|v| v.trim().parse::<u64>().ok());
    // `max` is the kernel's word for no limit, and it is the only non-number these files hold.
    let limit = |file: &str| number(file);
    let field = |file: &str, key: &str| {
        text(file).and_then(|t| {
            t.lines().find_map(|l| {
                let (k, v) = l.split_once(' ')?;
                (k == key).then(|| v.trim().parse::<u64>().ok()).flatten()
            })
        })
    };
    Some(Cgroup {
        current: number("memory.current")?,
        usage_usec: field("cpu.stat", "usage_usec").unwrap_or(0),
        anon: field("memory.stat", "anon").unwrap_or(0),
        high: limit("memory.high"),
        max: limit("memory.max"),
        pids: number("pids.current").unwrap_or(0),
        pids_max: limit("pids.max"),
        throttled: field("memory.events.local", "high"),
    })
}

/// This box's part in the fleet's disk verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetDisk {
    /// The fleet's disk could not be measured. Leaves the disk band where it was.
    Unknown,
    /// The boxes' filesystem is under its line, or over it with nothing asked of this box.
    NotAsked { over_by: u64 },
    /// Over the line, and this box is in the audience with something to clear.
    Asked(Share),
}

/// What the fleet asks of this box when it is over its line, in MiB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// How far over the line the boxes' filesystem is.
    pub over_by: u64,
    /// What this box is asked to clear.
    pub free: u64,
    /// What this box holds.
    pub holds: u64,
    /// How many other boxes were asked in the same crossing.
    pub others: usize,
    /// Every box holding disk was asked for everything it holds and the fleet is still over.
    /// `announce::Ask::short_by` non-zero.
    pub every_box: bool,
}

/// Everything one tick knows about one box.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub at: DateTime<Utc>,
    /// MiB this box holds on the fleet's disk. `None` when the disk walk has no figure for it.
    pub disk_mb: Option<u64>,
    /// This box's allowance, in MiB, when it has one.
    pub disk_limit_mb: Option<u64>,
    pub fleet: FleetDisk,
    pub cgroup: Option<Cgroup>,
}

/// This box's share of a fleet that is over its line, from the same audience the fleet-disk note
/// is sent to. One computation, so the file and the mailbox note cannot name different boxes.
pub fn fleet_share(name: &str, demand: crate::health::DiskDemand) -> FleetDisk {
    let over_by = demand.over_by;
    if over_by == 0 {
        return FleetDisk::NotAsked { over_by };
    }
    let holds: BTreeMap<String, u64> = demand.boxes.iter().cloned().collect();
    let Ok(asked) = crate::announce::cover(demand) else {
        return FleetDisk::NotAsked { over_by };
    };
    match asked.into_iter().find(|a| a.to == name) {
        Some(a) if a.free > 0 => FleetDisk::Asked(Share {
            over_by,
            free: a.free,
            holds: holds.get(name).copied().unwrap_or(a.free),
            others: a.others.len(),
            every_box: a.short_by > 0,
        }),
        _ => FleetDisk::NotAsked { over_by },
    }
}

// ------------------------------------------------------------------------------------------------
// The file
// ------------------------------------------------------------------------------------------------

/// `resources.json`. Every field is written whole on every tick.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    /// RFC3339, when this was written. The plugin treats a file far older than [`LOOK_EVERY`] as
    /// skein not looking, and asks nothing on it.
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub disk: DiskLevel,
    #[serde(default)]
    pub cpu: CpuLevel,
    #[serde(default)]
    pub memory: MemoryLevel,
    #[serde(default)]
    pub pids: PidsLevel,
    /// What is asked of this box now. Empty is the ordinary state.
    #[serde(default)]
    pub asks: Vec<Ask>,
    /// For each kind NOT asked now, the monitor's line for leaving that band, with this reading's
    /// figures. The monitor prints it when it last saw that kind asked. Kinds with no line are
    /// absent.
    #[serde(default)]
    pub cleared: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskLevel {
    /// MiB this box holds.
    pub mb: Option<u64>,
    /// MiB this box is allowed, when it has an allowance.
    pub limit_mb: Option<u64>,
    /// `over`, `under` or `unknown`: the boxes' filesystem against its line.
    pub fleet: String,
    /// MiB the boxes' filesystem is over its line. Zero when it is not.
    pub fleet_over_by_mb: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuLevel {
    /// Cores in use, averaged since the previous reading. A level, never an ask.
    pub cores: Option<f64>,
    /// The counter it was worked out from, kept for the next reading.
    pub usage_usec: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryLevel {
    /// Bytes of anonymous memory: what the kernel cannot reclaim its way out of.
    pub anon: Option<u64>,
    /// Bytes of page cache and the rest.
    pub cache: Option<u64>,
    /// `memory.high`, bytes: where the kernel starts slowing this box.
    pub high: Option<u64>,
    /// `memory.max`, bytes.
    pub max: Option<u64>,
    /// `memory.events.local` `high`, the counter the rate comes from.
    pub throttled: Option<u64>,
    /// Throttles a minute since the previous reading.
    pub throttled_per_min: Option<f64>,
    /// RFC3339, when these figures were read. Not always [`Signals::at`]: a tick that cannot read
    /// the cgroup carries the last figures forward, and the next rate is worked out from when they
    /// were actually read.
    #[serde(default)]
    pub read_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PidsLevel {
    pub current: Option<u64>,
    pub max: Option<u64>,
}

/// One standing ask.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    /// One of [`ASK_KINDS`].
    pub kind: String,
    /// Which disk ask: `fleet` (a share of the fleet's overage), `fleet-every-box` (every box was
    /// asked for everything), `allowance` (over this box's own). Empty for memory and pids.
    #[serde(default)]
    pub why: String,
    /// disk: MiB to clear (for `fleet-every-box`, what the box holds). memory: throttles a minute,
    /// rounded. pids: processes running.
    pub amount: u64,
    /// RFC3339, when this crossing began.
    pub since: String,
    /// New at each crossing, the same while the band holds. The plugin holds one command per id.
    pub crossing_id: String,
    /// The words of the held command.
    pub hold: String,
    /// The SessionStart line. `None` where no approved text exists for this ask.
    #[serde(default)]
    pub start: Option<String>,
    /// The UserPromptSubmit reminder. `None` where no approved text exists for this ask.
    #[serde(default)]
    pub remind: Option<String>,
    /// The monitor's line on entering the band. `None` where no approved text exists.
    #[serde(default)]
    pub enter: Option<String>,
}

// ------------------------------------------------------------------------------------------------
// From the previous file and a reading to the next file
// ------------------------------------------------------------------------------------------------

/// How much a counter grew. A counter that went backwards was reset — the box restarted and got a
/// new cgroup — and what it reads now is everything since.
fn grew_by(then: u64, now: u64) -> u64 {
    match now < then {
        true => now,
        false => now - then,
    }
}

/// Minutes from a stamp to this reading, when it can be read and time moved forward.
fn minutes_since(then: Option<&str>, at: DateTime<Utc>) -> Option<f64> {
    let then = DateTime::parse_from_rfc3339(then?).ok()?;
    let secs = (at - then.with_timezone(&Utc)).num_milliseconds() as f64 / 1000.0;
    (secs > 0.0).then_some(secs / 60.0)
}

/// What one kind's band is this tick.
enum Band<T> {
    /// Asked, with what the words need.
    Asked(T),
    NotAsked,
    /// Could not be told. Leave the band where it was.
    Unknown,
}

/// The disk ask, as the words need it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DiskAsk {
    Share(Share),
    EveryBox(Share),
    Allowance { excess: u64, holds: u64, limit: u64 },
}

fn disk_band(r: &Reading) -> Band<DiskAsk> {
    // The fleet's line first: when the whole fleet is short, that is the ask that matters, and the
    // box's own allowance is one of its reasons rather than a second one.
    if let FleetDisk::Asked(share) = &r.fleet {
        return Band::Asked(match share.every_box {
            true => DiskAsk::EveryBox(share.clone()),
            false => DiskAsk::Share(share.clone()),
        });
    }
    let own = match (r.disk_mb, r.disk_limit_mb) {
        (Some(mb), Some(limit)) if mb > limit => Band::Asked(DiskAsk::Allowance {
            excess: mb - limit,
            holds: mb,
            limit,
        }),
        (Some(_), _) | (None, None) => Band::NotAsked,
        // An allowance and no figure to hold against it.
        (None, Some(_)) => Band::Unknown,
    };
    match (&r.fleet, own) {
        (_, Band::Asked(a)) => Band::Asked(a),
        (FleetDisk::Unknown, _) | (_, Band::Unknown) => Band::Unknown,
        _ => Band::NotAsked,
    }
}

/// Memory: hot on this reading and the one before it, and near its line.
fn memory_band(
    cg: Option<&Cgroup>,
    rate: Option<f64>,
    prev_rate: Option<f64>,
) -> Band<(f64, u64, u64)> {
    let Some(cg) = cg else {
        return Band::Unknown;
    };
    let Some(rate) = rate else {
        // No rate: the first reading this box has had, or a kernel with no local counter. Nothing
        // to say it is hot, and nothing to say it has cooled.
        return Band::Unknown;
    };
    let hot = |r: f64| r >= THROTTLED_PER_MIN;
    let near = cg
        .high
        .is_some_and(|high| cg.anon.saturating_mul(100) >= high.saturating_mul(ANON_OF_HIGH_PCT));
    match (hot(rate) && prev_rate.is_some_and(hot) && near, cg.high) {
        (true, Some(high)) => Band::Asked((rate, cg.anon, high)),
        _ => Band::NotAsked,
    }
}

fn pids_band(cg: Option<&Cgroup>) -> Band<(u64, u64)> {
    let Some(cg) = cg else {
        return Band::Unknown;
    };
    match cg.pids_max {
        Some(max) if cg.pids.saturating_mul(100) >= max.saturating_mul(PIDS_OF_MAX_PCT) => {
            Band::Asked((cg.pids, max))
        }
        _ => Band::NotAsked,
    }
}

/// The next file, from the previous one and this reading. Pure: no clock, no filesystem.
pub fn next(prev: Option<&Signals>, r: &Reading) -> Signals {
    let cg = r.cgroup.as_ref();
    // Each counter from when it was read: the file's own stamp for the CPU counter, which is not
    // carried, and the memory figures' `read_at`, which is.
    let minutes = minutes_since(prev.map(|p| p.at.as_str()), r.at);
    let memory_minutes = minutes_since(prev.and_then(|p| p.memory.read_at.as_deref()), r.at);

    let throttled_per_min = match (
        prev.and_then(|p| p.memory.throttled),
        cg.and_then(|c| c.throttled),
        memory_minutes,
    ) {
        (Some(then), Some(now), Some(m)) => Some(grew_by(then, now) as f64 / m),
        _ => None,
    };
    let cores = match (prev.and_then(|p| p.cpu.usage_usec), cg, minutes) {
        (Some(then), Some(c), Some(m)) => {
            Some(grew_by(then, c.usage_usec) as f64 / (m * 60.0 * 1_000_000.0))
        }
        _ => None,
    };
    let prev_rate = prev.and_then(|p| p.memory.throttled_per_min);

    let (fleet, fleet_over_by_mb) = match &r.fleet {
        FleetDisk::Unknown => ("unknown", 0),
        FleetDisk::NotAsked { over_by } | FleetDisk::Asked(Share { over_by, .. }) => {
            (if *over_by > 0 { "over" } else { "under" }, *over_by)
        }
    };

    let mut out = Signals {
        at: r.at.to_rfc3339(),
        disk: DiskLevel {
            mb: r.disk_mb,
            limit_mb: r.disk_limit_mb,
            fleet: fleet.to_string(),
            fleet_over_by_mb,
        },
        cpu: CpuLevel {
            cores,
            usage_usec: cg.map(|c| c.usage_usec),
        },
        memory: match cg {
            Some(c) => MemoryLevel {
                anon: Some(c.anon),
                cache: Some(c.current.saturating_sub(c.anon)),
                high: c.high,
                max: c.max,
                throttled: c.throttled,
                throttled_per_min,
                read_at: Some(r.at.to_rfc3339()),
            },
            // Carried rather than blanked: the next reading's rate is worked out from this one.
            None => prev.map(|p| p.memory.clone()).unwrap_or_default(),
        },
        pids: match cg {
            Some(c) => PidsLevel {
                current: Some(c.pids),
                max: c.pids_max,
            },
            None => prev.map(|p| p.pids.clone()).unwrap_or_default(),
        },
        asks: Vec::new(),
        cleared: BTreeMap::new(),
    };

    let prev_ask = |kind: &str| prev.and_then(|p| p.asks.iter().find(|a| a.kind == kind));
    // A crossing keeps its id and start while the band holds; a new one is minted on entering.
    let stamp = |kind: &str, fresh: Ask| -> Ask {
        match prev_ask(kind) {
            Some(before) => Ask {
                since: before.since.clone(),
                crossing_id: before.crossing_id.clone(),
                ..fresh
            },
            None => Ask {
                since: r.at.to_rfc3339(),
                crossing_id: format!("{kind}-{}", r.at.timestamp_millis()),
                ..fresh
            },
        }
    };

    match disk_band(r) {
        Band::Asked(d) => out.asks.push(stamp("disk", words::disk(&d))),
        Band::Unknown => out.asks.extend(prev_ask("disk").cloned()),
        Band::NotAsked => {
            out.cleared
                .insert("disk".into(), words::DISK_CLEARED.to_string());
        }
    }
    match memory_band(cg, throttled_per_min, prev_rate) {
        Band::Asked((rate, anon, high)) => out
            .asks
            .push(stamp("memory", words::memory(rate, anon, high))),
        Band::Unknown => out.asks.extend(prev_ask("memory").cloned()),
        Band::NotAsked => {
            out.cleared
                .insert("memory".into(), words::MEMORY_CLEARED.to_string());
        }
    }
    match pids_band(cg) {
        Band::Asked((pids, max)) => out.asks.push(stamp("pids", words::pids(pids, max))),
        Band::Unknown => out.asks.extend(prev_ask("pids").cloned()),
        Band::NotAsked => {
            if let Some(c) = cg {
                out.cleared
                    .insert("pids".into(), words::pids_cleared(c.pids));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// The words
// ------------------------------------------------------------------------------------------------

/// What an agent is shown, with integers filled in and nothing else.
///
/// **The owner approved these texts on SKEIN-1055, 2026-09-24**, labelled (1a)–(6) in
/// `feature-wording.md`. Two of the owner's decisions shape them: the `skein_top` sentence is left
/// out until the tool ships (SKEIN-1059), and the closing "This one command was held so you would
/// see this; the next will run." stays. No field a box can write reaches any of them: every
/// argument is a number skein measured.
///
/// Where the approved set has no text for a case, the field is `None` and the plugin shows
/// nothing, rather than a sentence nobody approved. The one exception is how (1a) reads when fewer
/// than two other boxes were asked; see [`others_asked`].
mod words {
    use super::*;

    const HELD: &str = "This one command was held so you would see this; the next will run.";

    /// A count as a person reads it: `8,192`.
    pub(super) fn count(n: u64) -> String {
        let digits = n.to_string();
        let mut out = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(c);
        }
        out
    }

    /// Bytes as `gib` prints MiB, so a figure here and on the health row read the same.
    fn bytes(b: u64) -> String {
        gib(b / (1024 * 1024))
    }

    /// The end of (1a)'s second sentence. The approved text shows two other boxes: "; 2 other
    /// boxes have been asked for the rest". **One and none are not in the approved set** and are
    /// flagged for sign-off: "1 other boxes" is not English, and "0 other boxes have been asked for
    /// the rest" says there is a rest to ask for when this box covers all of it.
    fn others_asked(n: usize) -> String {
        match n {
            0 => String::new(),
            1 => "; 1 other box has been asked for the rest".to_string(),
            n => format!("; {n} other boxes have been asked for the rest"),
        }
    }

    pub(super) fn disk(d: &DiskAsk) -> Ask {
        match d {
            // (1a), (4), (5), (6).
            DiskAsk::Share(s) => Ask {
                kind: "disk".into(),
                why: "fleet".into(),
                amount: s.free,
                hold: format!(
                    "skein: clear {free} of storage you are not using before you continue — build \
                     outputs, caches, and any containers or volumes you started. The fleet's disk \
                     is {over} over what it can spare, and this box holds {holds} of it{others}. \
                     {HELD}",
                    free = gib(s.free),
                    over = gib(s.over_by),
                    holds = gib(s.holds),
                    others = others_asked(s.others),
                ),
                // (4) without its last sentence, "`skein_resources` shows where this box stands.":
                // that tool ships with SKEIN-1059, the same as `skein_top`. Flagged for sign-off.
                start: Some(format!(
                    "skein: before you start, this box is asked to clear {free} of storage it is \
                     not using. The fleet's disk is {over} over what it can spare and this box \
                     holds {holds}.",
                    free = gib(s.free),
                    over = gib(s.over_by),
                    holds = gib(s.holds),
                )),
                remind: Some(format!(
                    "skein: still asked of this box: clear {} of storage (it holds {}).",
                    gib(s.free),
                    gib(s.holds)
                )),
                enter: Some(format!(
                    "skein: the fleet's disk is over its line; this box is asked to clear {} it \
                     is not using.",
                    gib(s.free)
                )),
                ..Default::default()
            },
            // (1b). No approved (4), (5) or (6) for it.
            DiskAsk::EveryBox(s) => Ask {
                kind: "disk".into(),
                why: "fleet-every-box".into(),
                amount: s.holds,
                hold: format!(
                    "skein: clear what you can of the {holds} this box holds before you continue — \
                     build outputs, caches, and any containers or volumes you started. The fleet's \
                     disk is {over} over what it can spare, and every box has been asked. {HELD}",
                    holds = gib(s.holds),
                    over = gib(s.over_by),
                ),
                ..Default::default()
            },
            // (1c) and (5). No approved (4) or (6): (6)'s disk line says the fleet is over its
            // line, which is not why this box is asked.
            DiskAsk::Allowance {
                excess,
                holds,
                limit,
            } => Ask {
                kind: "disk".into(),
                why: "allowance".into(),
                amount: *excess,
                hold: format!(
                    "skein: clear {} of storage you are not using before you continue. This box \
                     holds {} against its {} share of the fleet's disk. {HELD}",
                    gib(*excess),
                    gib(*holds),
                    gib(*limit),
                ),
                remind: Some(format!(
                    "skein: still asked of this box: clear {} of storage (it holds {}).",
                    gib(*excess),
                    gib(*holds)
                )),
                ..Default::default()
            },
        }
    }

    /// (2) without its `skein_top` sentence, and (6). No approved (4) or (5).
    pub(super) fn memory(rate: f64, anon: u64, high: u64) -> Ask {
        let rate = rate.round() as u64;
        Ask {
            kind: "memory".into(),
            amount: rate,
            hold: format!(
                "skein: stop processes you no longer need before you start more. This box is being \
                 slowed for memory, {} times a minute, and holds {}; the kernel slows it above {}. \
                 {HELD}",
                count(rate),
                bytes(anon),
                bytes(high),
            ),
            enter: Some(format!(
                "skein: this box is being slowed for memory ({} throttles a minute); stop \
                 processes you no longer need.",
                count(rate)
            )),
            ..Default::default()
        }
    }

    /// (3) without its `skein_top` sentence, and (6). No approved (4) or (5).
    pub(super) fn pids(pids: u64, max: u64) -> Ask {
        Ask {
            kind: "pids".into(),
            amount: pids,
            hold: format!(
                "skein: end processes you started and no longer need before you start more. This \
                 box is running {} processes against a limit of {}, and past that nothing in it \
                 can start a new one. {HELD}",
                count(pids),
                count(max),
            ),
            enter: Some(format!(
                "skein: this box is running {} of its {} processes; end the ones you no longer \
                 need.",
                count(pids),
                count(max)
            )),
            ..Default::default()
        }
    }

    /// (6), leaving each band.
    pub(super) const DISK_CLEARED: &str =
        "skein: the fleet's disk no longer needs anything from this box. Nothing more is asked.";
    pub(super) const MEMORY_CLEARED: &str =
        "skein: this box is no longer being slowed for memory. Nothing more is asked.";
    pub(super) fn pids_cleared(pids: u64) -> String {
        format!(
            "skein: this box is back to {} processes. Nothing more is asked.",
            count(pids)
        )
    }
}

// ------------------------------------------------------------------------------------------------
// Reading and writing the file, and the loop
// ------------------------------------------------------------------------------------------------

/// The file as it stands. Unreadable reads as absent: the next write starts the bands afresh, which
/// costs at most a repeated hold, never a missed one.
pub fn read(state: &Path) -> Option<Signals> {
    let raw = std::fs::read_to_string(signal_path(state)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Work out and write one box's file from its previous one and a reading.
pub fn write_box(state: &Path, r: &Reading) -> Result<Signals, String> {
    let prev = read(state);
    let signals = next(prev.as_ref(), r);
    let path = signal_path(state);
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(&signals).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, dir, &bytes)?;
    Ok(signals)
}

/// One tick over the whole fleet: a file for every box that has a state directory.
///
/// The state directories are the list, as they are for the owner's inbox (`mailbox::load_mailbox`):
/// a box whose file skein can put there is a box whose launcher binds it back read-only. A stopped
/// box still gets its disk figures; its cgroup is gone, so its memory and process bands stay where
/// they were until it runs again.
pub fn write_every_box() -> Result<usize, String> {
    let root = crate::fleet::box_state_root();
    let entries = std::fs::read_dir(&root).map_err(|e| format!("{root}: {e}"))?;
    // One reading of the fleet's disk for every box, so no two boxes are told about two fleets.
    let measured = crate::fleet::fleet_resources().is_some();
    let demand = crate::health::disk_demand();
    let usage = crate::fleet::fleet_disk_usage();
    let at = Utc::now();
    let mut written = 0;
    let mut failed = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        let reading = Reading {
            at,
            disk_mb: usage.get(&name).copied(),
            disk_limit_mb: crate::fleet::box_disk_limit(&name),
            fleet: match measured {
                true => fleet_share(&name, demand.clone()),
                false => FleetDisk::Unknown,
            },
            cgroup: read_cgroup(Path::new(&crate::fleet::box_cgroup(&name))),
        };
        match write_box(&entry.path(), &reading) {
            Ok(_) => written += 1,
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    match failed.is_empty() {
        true => Ok(written),
        false => Err(failed.join("; ")),
    }
}

/// The loop skein-server runs. Never returns.
///
/// Its own loop, for the reason `announce::watch_fleet_disk` gives: an ask that only exists while
/// somebody has the cockpit open is not an ask. `spawn_blocking` because the disk figures sit
/// behind a tree walk.
pub async fn watch_box_resources() {
    let mut tick = tokio::time::interval(LOOK_EVERY);
    loop {
        tick.tick().await;
        match tokio::task::spawn_blocking(write_every_box).await {
            Ok(Err(e)) => eprintln!("skein: box resource signals: {e}"),
            Err(e) => eprintln!("skein: box resource signals did not run: {e}"),
            Ok(Ok(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-24T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::minutes(minute)
    }

    /// A box at rest: nothing near any line.
    fn calm() -> Cgroup {
        Cgroup {
            usage_usec: 0,
            anon: 2 * GIB,
            current: 3 * GIB,
            high: Some(12 * GIB),
            max: Some(16 * GIB),
            pids: 300,
            pids_max: Some(8192),
            throttled: Some(0),
        }
    }

    fn reading(minute: i64, cg: Option<Cgroup>) -> Reading {
        Reading {
            at: at(minute),
            disk_mb: Some(1024),
            disk_limit_mb: None,
            fleet: FleetDisk::NotAsked { over_by: 0 },
            cgroup: cg,
        }
    }

    /// Run a sequence of readings through [`next`], each seeing the file the one before wrote.
    fn run(readings: &[Reading]) -> Vec<Signals> {
        let mut out: Vec<Signals> = Vec::new();
        for r in readings {
            let s = next(out.last(), r);
            out.push(s);
        }
        out
    }

    fn kinds(s: &Signals) -> Vec<&str> {
        s.asks.iter().map(|a| a.kind.as_str()).collect()
    }

    /// A box whose memory counter grows by `per_min` a minute from `base`, near its line.
    fn hot(minute: i64, per_min: u64) -> Cgroup {
        Cgroup {
            anon: 11 * GIB + GIB / 5,
            throttled: Some(minute as u64 * per_min),
            ..calm()
        }
    }

    /// **One hot reading does not raise the memory ask; two in a row do, and one id covers the
    /// crossing.**
    ///
    /// What would make each assertion fail:
    /// * *one reading alone asks nothing* — drop `prev_rate.is_some_and(hot)` from `memory_band`.
    /// * *the second asks* — require three readings, or compare the rate against the wrong field.
    /// * *the id holds while the band holds* — mint a new id on every tick in `stamp`.
    #[test]
    fn one_hot_reading_alone_does_not_ask_and_two_in_a_row_do() {
        let files = run(&[
            reading(0, Some(hot(0, 84))),
            reading(1, Some(hot(1, 84))),
            reading(2, Some(hot(2, 84))),
            reading(3, Some(hot(3, 84))),
        ]);
        assert!(
            files[0].asks.is_empty(),
            "the first reading has no rate at all"
        );
        assert_eq!(files[1].memory.throttled_per_min, Some(84.0));
        assert!(
            files[1].asks.is_empty(),
            "one reading of 84 a minute raised the memory ask on its own: {:?}",
            files[1].asks
        );
        assert_eq!(
            kinds(&files[2]),
            vec!["memory"],
            "two hot readings in a row did not ask"
        );
        assert_eq!(files[2].asks[0].amount, 84);
        assert_eq!(
            files[3].asks[0].crossing_id, files[2].asks[0].crossing_id,
            "the band held and the crossing id moved, so the agent would be held again"
        );
        assert_eq!(files[3].asks[0].since, files[2].asks[0].since);
    }

    /// Hot but far from its line is not asked: the throttle alone is a box doing its job.
    ///
    /// Fails if the `near` half of `memory_band` is dropped.
    #[test]
    fn a_hot_box_far_below_its_line_is_not_asked() {
        let far = |m: i64| Cgroup {
            anon: 4 * GIB,
            ..hot(m, 200)
        };
        let files = run(&[
            reading(0, Some(far(0))),
            reading(1, Some(far(1))),
            reading(2, Some(far(2))),
        ]);
        assert!(files[2].asks.is_empty(), "{:?}", files[2].asks);
    }

    /// **A missed tick does not flip a band**, in either of the two ways a tick goes missing.
    ///
    /// * The server was down for ten minutes: the rate is worked out over the ten minutes, so a box
    ///   still throttling at 84 a minute is still asked, under the same id. Fails if the rate is
    ///   divided by one minute rather than the elapsed time (840 a minute is still "hot", so that
    ///   sabotage is caught by the exact figure asserted), or if the gap resets the band.
    /// * The cgroup could not be read for one tick: the ask stands under the same id, and the tick
    ///   after reads hot again without a second crossing. Fails if `Band::Unknown` is treated as
    ///   `NotAsked`, which would clear the ask and mint a new id one tick later.
    #[test]
    fn a_missed_tick_does_not_flip_the_band() {
        let files = run(&[
            reading(0, Some(hot(0, 84))),
            reading(1, Some(hot(1, 84))),
            reading(2, Some(hot(2, 84))),
            // Ten minutes with no reading at all.
            reading(12, Some(hot(12, 84))),
            // A reading with no cgroup.
            reading(13, None),
            reading(14, Some(hot(14, 84))),
        ]);
        let id = files[2].asks[0].crossing_id.clone();
        assert_eq!(files[3].memory.throttled_per_min, Some(84.0));
        for (n, f) in files.iter().enumerate().skip(3) {
            assert_eq!(kinds(f), vec!["memory"], "tick {n} flipped the band: {f:?}");
            assert_eq!(f.asks[0].crossing_id, id, "tick {n} started a new crossing");
        }
        assert!(
            !files[4].cleared.contains_key("memory"),
            "an unreadable tick offered the monitor a line saying memory cleared"
        );
    }

    /// Cooling clears the ask and offers the monitor its leaving line; heating again is a new
    /// crossing with a new id.
    ///
    /// Fails if a cleared band keeps the old id (the second crossing would never be held), or if
    /// `cleared` is not written for a kind that is not asked.
    #[test]
    fn cooling_clears_and_heating_again_is_a_new_crossing() {
        let cool = |m: i64| Cgroup {
            throttled: Some(2 * 84 + (m as u64 - 2)),
            ..hot(m, 0)
        };
        let files = run(&[
            reading(0, Some(hot(0, 84))),
            reading(1, Some(hot(1, 84))),
            reading(2, Some(hot(2, 84))),
            reading(3, Some(cool(3))),
            reading(
                4,
                Some(Cgroup {
                    throttled: Some(2 * 84 + 1 + 84),
                    ..hot(4, 0)
                }),
            ),
            reading(
                5,
                Some(Cgroup {
                    throttled: Some(2 * 84 + 1 + 2 * 84),
                    ..hot(5, 0)
                }),
            ),
        ]);
        let first = files[2].asks[0].crossing_id.clone();
        assert!(files[3].asks.is_empty(), "{:?}", files[3].asks);
        assert_eq!(
            files[3].cleared.get("memory").map(String::as_str),
            Some("skein: this box is no longer being slowed for memory. Nothing more is asked.")
        );
        assert!(
            files[4].asks.is_empty(),
            "one hot reading after cooling asked"
        );
        assert_eq!(kinds(&files[5]), vec!["memory"]);
        assert_ne!(
            files[5].asks[0].crossing_id, first,
            "a second crossing reused the first id"
        );
    }

    /// **No CPU reading ever produces an ask**, however busy the box.
    ///
    /// Fails if any band is derived from `cores` or `usage_usec`.
    #[test]
    fn a_box_using_every_core_is_never_asked_anything() {
        // Sixty-four cores for a minute each tick.
        let busy = |m: i64| Cgroup {
            usage_usec: m as u64 * 64 * 60 * 1_000_000,
            ..calm()
        };
        let files = run(&[
            reading(0, Some(busy(0))),
            reading(1, Some(busy(1))),
            reading(2, Some(busy(2))),
        ]);
        assert_eq!(files[2].cpu.cores, Some(64.0));
        for f in &files {
            assert!(
                f.asks.is_empty(),
                "a busy CPU asked something: {:?}",
                f.asks
            );
        }
        assert!(!ASK_KINDS.contains(&"cpu"));
    }

    /// The process count asks at 75% of `pids.max`, and not below.
    ///
    /// Fails if the threshold moves, or the comparison is against the wrong limit.
    #[test]
    fn the_process_count_asks_at_three_quarters_of_its_limit() {
        let with = |pids: u64| Cgroup { pids, ..calm() };
        let files = run(&[
            reading(0, Some(with(6143))),
            reading(1, Some(with(6144))),
            reading(2, Some(with(1204))),
        ]);
        assert!(files[0].asks.is_empty(), "6,143 of 8,192 is under 75%");
        assert_eq!(kinds(&files[1]), vec!["pids"]);
        assert_eq!(
            files[1].asks[0].hold,
            "skein: end processes you started and no longer need before you start more. This box \
             is running 6,144 processes against a limit of 8,192, and past that nothing in it can \
             start a new one. This one command was held so you would see this; the next will run."
        );
        assert_eq!(
            files[2].cleared.get("pids").map(String::as_str),
            Some("skein: this box is back to 1,204 processes. Nothing more is asked.")
        );
    }

    fn share(others: usize, every_box: bool) -> Share {
        Share {
            over_by: 4198,
            free: 2355,
            holds: 9626,
            others,
            every_box,
        }
    }

    /// The disk ask comes from the fleet's audience first and from the box's own allowance
    /// second, and an unmeasured fleet leaves the band where it was.
    ///
    /// Fails if `FleetDisk::Unknown` clears the ask, or if the allowance half is dropped.
    #[test]
    fn the_disk_band_follows_the_fleet_and_then_the_allowance_and_holds_when_unmeasured() {
        let with = |minute: i64, fleet: FleetDisk, mb: u64, limit: Option<u64>| Reading {
            fleet,
            disk_mb: Some(mb),
            disk_limit_mb: limit,
            ..reading(minute, Some(calm()))
        };
        let files = run(&[
            with(0, FleetDisk::Asked(share(2, false)), 9626, None),
            with(1, FleetDisk::Unknown, 9626, None),
            with(2, FleetDisk::NotAsked { over_by: 0 }, 9626, None),
            with(3, FleetDisk::NotAsked { over_by: 0 }, 12595, Some(10240)),
        ]);
        assert_eq!(files[0].asks[0].why, "fleet");
        assert_eq!(
            files[1].asks, files[0].asks,
            "an unmeasured fleet changed the disk ask"
        );
        assert!(files[2].asks.is_empty());
        assert!(files[2].cleared.contains_key("disk"));
        assert_eq!(files[3].asks[0].why, "allowance");
        assert_eq!(
            files[3].asks[0].hold,
            "skein: clear 2.3G of storage you are not using before you continue. This box holds \
             12.3G against its 10.0G share of the fleet's disk. This one command was held so you \
             would see this; the next will run."
        );
    }

    /// The approved (1a), (1b), (2) and the lines around them, figure for figure.
    ///
    /// Fails on any change to a word of them. The texts are the owner's, from SKEIN-1055, with the
    /// `skein_top` sentences left out as the owner decided.
    #[test]
    fn the_words_are_the_approved_words_with_the_figures_filled_in() {
        let a = words::disk(&DiskAsk::Share(share(2, false)));
        assert_eq!(
            a.hold,
            "skein: clear 2.3G of storage you are not using before you continue — build outputs, \
             caches, and any containers or volumes you started. The fleet's disk is 4.1G over what \
             it can spare, and this box holds 9.4G of it; 2 other boxes have been asked for the \
             rest. This one command was held so you would see this; the next will run."
        );
        assert_eq!(
            a.remind.as_deref(),
            Some("skein: still asked of this box: clear 2.3G of storage (it holds 9.4G).")
        );
        assert_eq!(
            a.enter.as_deref(),
            Some(
                "skein: the fleet's disk is over its line; this box is asked to clear 2.3G it is \
                 not using."
            )
        );
        let b = words::disk(&DiskAsk::EveryBox(Share {
            over_by: 6144,
            ..share(3, true)
        }));
        assert_eq!(
            b.hold,
            "skein: clear what you can of the 9.4G this box holds before you continue — build \
             outputs, caches, and any containers or volumes you started. The fleet's disk is 6.0G \
             over what it can spare, and every box has been asked. This one command was held so \
             you would see this; the next will run."
        );
        assert_eq!(b.start, None, "no approved SessionStart line for (1b)");
        let m = words::memory(84.0, 11 * GIB + GIB / 5, 12 * GIB);
        assert_eq!(
            m.hold,
            "skein: stop processes you no longer need before you start more. This box is being \
             slowed for memory, 84 times a minute, and holds 11.2G; the kernel slows it above \
             12.0G. This one command was held so you would see this; the next will run."
        );
        assert_eq!(
            m.enter.as_deref(),
            Some(
                "skein: this box is being slowed for memory (84 throttles a minute); stop \
                 processes you no longer need."
            )
        );
        assert_eq!(m.start, None);
        assert_eq!(m.remind, None);
    }

    /// Nothing an agent is shown says or implies a kill (owner's answer 3).
    ///
    /// Fails if any template gains one of these words.
    #[test]
    fn no_text_says_or_implies_a_kill() {
        let mut texts: Vec<String> = Vec::new();
        for d in [
            DiskAsk::Share(share(2, false)),
            DiskAsk::EveryBox(share(0, true)),
            DiskAsk::Allowance {
                excess: 1,
                holds: 2,
                limit: 1,
            },
        ] {
            let a = words::disk(&d);
            texts.extend(
                [Some(a.hold), a.start, a.remind, a.enter]
                    .into_iter()
                    .flatten(),
            );
        }
        for a in [words::memory(84.0, GIB, GIB), words::pids(6410, 8192)] {
            texts.extend(
                [Some(a.hold), a.start, a.remind, a.enter]
                    .into_iter()
                    .flatten(),
            );
        }
        texts.extend([
            words::DISK_CLEARED.to_string(),
            words::MEMORY_CLEARED.to_string(),
            words::pids_cleared(1),
        ]);
        for t in &texts {
            let lower = t.to_lowercase();
            for word in [
                "kill",
                "oom",
                "terminat",
                "stopped for you",
                "will be stopped",
            ] {
                assert!(!lower.contains(word), "{word:?} in {t:?}");
            }
        }
    }

    /// The file is written where the plugin reads it, and the next write reads it back to carry
    /// the crossing.
    ///
    /// Fails if `write_box` does not read the previous file (the second write would mint a new id)
    /// or writes it anywhere but `signals/resources.json` under the state directory.
    #[test]
    fn the_file_is_written_under_the_boxs_state_and_carries_the_crossing() {
        let dir = crate::testutil::tempdir();
        let state: &Path = dir.as_ref();
        let pids = |n: u64| Some(Cgroup { pids: n, ..calm() });
        let first = write_box(state, &reading(0, pids(7000))).unwrap();
        let second = write_box(state, &reading(1, pids(7100))).unwrap();
        let on_disk: Signals = serde_json::from_str(
            &std::fs::read_to_string(state.join("signals/resources.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk, second);
        assert_eq!(second.asks[0].crossing_id, first.asks[0].crossing_id);
        assert_eq!(second.asks[0].amount, 7100);
    }

    /// The cgroup files are read the way the kernel writes them, `max` included.
    ///
    /// Fails if `memory.events` (hierarchical) is read in place of `memory.events.local`.
    #[test]
    fn a_cgroup_directory_is_read_as_the_kernel_writes_it() {
        let dir = crate::testutil::tempdir();
        let d: &Path = dir.as_ref();
        for (file, body) in [
            ("memory.current", "3221225472\n"),
            ("memory.stat", "anon 2147483648\nfile 1073741824\n"),
            ("memory.high", "12884901888\n"),
            ("memory.max", "max\n"),
            ("pids.current", "300\n"),
            ("pids.max", "8192\n"),
            ("cpu.stat", "usage_usec 5000\nuser_usec 4000\n"),
            ("memory.events", "low 0\nhigh 999\nmax 0\noom 0\n"),
            ("memory.events.local", "low 0\nhigh 7\nmax 0\noom 0\n"),
        ] {
            std::fs::write(d.join(file), body).unwrap();
        }
        let cg = read_cgroup(d).unwrap();
        assert_eq!(
            cg.throttled,
            Some(7),
            "read the subtree's counter, not this box's"
        );
        assert_eq!(cg.max, None);
        assert_eq!(cg.high, Some(12 * GIB));
        assert_eq!((cg.anon, cg.pids, cg.pids_max), (2 * GIB, 300, Some(8192)));
        assert_eq!(read_cgroup(&d.join("absent")), None);
    }

    /// This box's share is the one `announce::cover` computes for the fleet-disk note.
    ///
    /// Fails if the box's own figure is taken from anywhere but the ranking, or if a box outside
    /// the audience is asked.
    #[test]
    fn the_fleet_share_is_the_audience_the_note_goes_to() {
        let demand = crate::health::DiskDemand {
            over_by: 4198,
            boxes: vec![
                ("big".into(), 3000),
                ("mid".into(), 2000),
                ("tiny".into(), 10),
            ],
        };
        match fleet_share("big", demand.clone()) {
            FleetDisk::Asked(s) => {
                assert_eq!((s.holds, s.others, s.every_box), (3000, 1, false));
                assert!(s.free > 0 && s.free <= 3000);
            }
            other => panic!("the biggest box was not asked: {other:?}"),
        }
        assert_eq!(
            fleet_share("tiny", demand),
            FleetDisk::NotAsked { over_by: 4198 },
            "a box outside the audience was asked"
        );
    }
}
