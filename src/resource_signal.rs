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

use crate::health::{gib, gib_up};
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

/// How far over, as a percentage of the box's share, before a disk ask holds a command (owner,
/// SKEIN-1335): 1G on a 10G share. Below it the box is still asked and told, but nothing is held.
/// A month of holds for overages of 0.2G and less (226 of 486) is what this number is chosen
/// against: a hold for a rounding error teaches an agent the hold is noise.
pub const DISK_HOLD_OVER_PCT: u64 = 10;

/// The kinds an ask can be. **There is no `cpu`**, and the plugin's hook holds only for these.
pub const ASK_KINDS: [&str; 3] = ["disk", "memory", "pids"];

/// How often a box's cleanable items are listed again. The period of the fleet's own disk walk
/// (`fleet_disk_usage`), since this is a second walk of part of the same trees.
pub const CLEAN_LIST_EVERY: Duration = Duration::from_secs(300);

/// How recent "no other box of the repo is running" must be for the plugin to remove from the
/// repo's shared cache: three ticks, so one slow or missed tick does not refuse, and a sibling
/// that started is seen within a minute and refused from then on. The plugin carries the same
/// number, in `skein-mcp`.
pub const CLEAN_SIBLINGS_FRESH: Duration = Duration::from_secs(180);

/// Where the file lives for one box, given that box's state directory.
pub fn signal_path(state: &Path) -> PathBuf {
    state.join("signals").join("resources.json")
}

/// Where one box's own spend lives, beside [`signal_path`] in the same read-only directory
/// (SKEIN-1384).
pub fn usage_path(state: &Path) -> PathBuf {
    signal_path(state).with_file_name("usage.json")
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
    /// What in this box can be removed and made again ([`clean_list`]). `None` when the box has no
    /// directory to list.
    pub clean: Option<CleanList>,
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
    /// Versions rolled out since this box started that a restart would give it (SKEIN-1372): an
    /// agent CLI or a Rust toolchain. Empty when it runs what a start would bind now. The box
    /// chooses its moment with `skein_resources`' `restart: "when-idle"`; otherwise skein restarts
    /// it once it has been idle a while (`fleet::versions::restart_onto_rollouts`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updates: Vec<Update>,
    /// What in this box can be removed and made again, for the plugin's `skein_clean` and
    /// `skein_top` (SKEIN-1356). Absent when skein has not listed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean: Option<CleanList>,
    /// This box's ask for a bigger disk allowance and the owner's answer to it (SKEIN-1425), as
    /// `diskask::for_box` reads them: the state is the host's record, never the box's file. Absent
    /// when the box has no ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_ask: Option<DiskAskView>,
}

/// A box's disk ask as its signal file says it. A contract like [`CleanList`]: fields are only
/// added.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskAskView {
    pub id: String,
    /// The allowance asked for, in MiB.
    pub mb: u64,
    /// `waiting`, `approved` or `denied`.
    pub state: String,
    /// RFC3339, when the owner answered. Empty while it waits.
    #[serde(default)]
    pub decided: String,
}

impl From<crate::diskask::Ask> for DiskAskView {
    fn from(a: crate::diskask::Ask) -> DiskAskView {
        DiskAskView {
            id: a.id,
            mb: a.mb,
            state: a.state,
            decided: a.decided,
        }
    }
}

/// One version a restart would move this box onto.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Update {
    /// `claude`, `codex`, `rust`, or `rust-repo` for the toolchain the box's repo names.
    pub component: String,
    /// What the box runs: a depot entry, or empty for the sandbox's own copy.
    pub have: String,
    /// What a restart gives it, in the same terms.
    pub new: String,
    /// Always `restart`: nothing else applies it.
    pub applies: String,
    /// Whether this box has asked to be restarted onto it once its turn ends.
    pub restart_requested: bool,
}

/// What in one box can be removed and made again, as `fleet::box_cleanables` lists it, and its
/// repo's shared download cache as `fleet::repo_cache_for_box` measures it (SKEIN-1356).
///
/// **One classifier.** The host decides what is cleanable and of what kind, here and in the box's
/// settings alike; the plugin's `skein_clean` reads this list and removes, inside the box and as
/// the box, only what is on it. It never classifies a path itself.
///
/// Every path is the fleet's, which is also where the box sees it: the launcher binds the box's
/// own directory and its repo's cache back at their own paths (`box-session.sh`).
///
/// **A contract, read by more than skein.** The plugin's `skein-mcp` reads it today and the mod
/// skein's plugin is becoming will read it too (SKEIN-1358), so a field here is only ever added,
/// never renamed or given a new meaning: `clean.at`, `box_root`, each item's `path`, `shown`,
/// `kind`, `bytes` and `live`, and the repo cache's `path`, `bytes`, `items`, `others_running`,
/// `others_unknown` and `none_running_at`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CleanList {
    /// RFC3339, when the items were listed. They are listed again every [`CLEAN_LIST_EVERY`].
    pub at: String,
    /// The box's own directory. Every path in `items` is under it.
    pub box_root: String,
    /// Biggest first.
    pub items: Vec<CleanItem>,
    /// The repo's shared download cache, when the repo has one.
    #[serde(default)]
    pub repo_cache: Option<RepoCacheList>,
}

/// One thing that can be removed: `fleet::Cleanable`, owned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanItem {
    pub path: String,
    pub shown: String,
    /// `download cache`, `build output` or `scratchpad`.
    pub kind: String,
    pub bytes: u64,
    /// A build is running in it now: cargo holds its lock (`fleet::live_builds`), read again on
    /// every tick (SKEIN-1391). It cannot be removed while that is so, and no ask counts it as
    /// clearable. Absent in a file written before it existed, which reads as not live.
    #[serde(default)]
    pub live: bool,
}

impl From<crate::fleet::Cleanable> for CleanItem {
    fn from(c: crate::fleet::Cleanable) -> CleanItem {
        CleanItem {
            path: c.path,
            shown: c.shown,
            kind: c.kind.to_string(),
            bytes: c.bytes,
            live: false,
        }
    }
}

/// A repo's shared download cache, as one box is told about it. Counted against no box: it is on
/// the store's disk, outside every box's directory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoCacheList {
    pub path: String,
    /// The sum of `items`.
    pub bytes: u64,
    pub items: Vec<CleanItem>,
    /// The repo's OTHER boxes that are running, read on every tick.
    pub others_running: usize,
    /// The repo's other boxes skein cannot tell about, which count as running.
    pub others_unknown: usize,
    /// RFC3339, the last reading at which no other box of the repo was running or unknown. The
    /// plugin removes from this cache only while that is recent ([`CLEAN_SIBLINGS_FRESH`]).
    #[serde(default)]
    pub none_running_at: Option<String>,
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
    /// disk: MiB to clear (for `fleet-every-box`, what the box holds), never more than the box's
    /// listed items can free now (SKEIN-1391). memory: throttles a minute, rounded. pids:
    /// processes running.
    pub amount: u64,
    /// disk: MiB of what the box holds that is in a build running now, which no ask counts as
    /// clearable (SKEIN-1391). Zero, and absent from the file, when none is.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub live_mb: u64,
    /// RFC3339, when this crossing began.
    pub since: String,
    /// New at each crossing, the same while the band holds. The plugin holds one command per id.
    pub crossing_id: String,
    /// The words of the held command. Empty for a disk ask not far enough over to hold one
    /// ([`DISK_HOLD_OVER_PCT`]): the box is told, and its commands run.
    pub hold: String,
    /// The SessionStart line. Every live ask has one.
    #[serde(default)]
    pub start: Option<String>,
    /// The UserPromptSubmit reminder. Every live ask has one.
    #[serde(default)]
    pub remind: Option<String>,
    /// The monitor's line on entering the band. Every live ask has one.
    #[serde(default)]
    pub enter: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

// ------------------------------------------------------------------------------------------------
// From the previous file and a reading to the next file
// ------------------------------------------------------------------------------------------------

/// A time as the file writes it: whole seconds, UTC, `Z`. Whole seconds because the plugin reads
/// it with Python's `datetime.fromisoformat`, which before 3.11 refuses the nanoseconds `chrono`
/// would otherwise print, and a stamp it cannot read makes the whole file unbelievable to it.
fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

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

/// What the box can do about a disk ask now, in MiB, from skein's list of its items (SKEIN-1391).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Room {
    /// What its agent may remove now: [`removable_now`]'s items, rounded down.
    clear: u64,
    /// What is in a build running now, rounded up.
    live: u64,
}

const MIB: u64 = 1024 * 1024;

/// [`Room`] from the box's list. `None` when skein has not listed it: then nothing is known about
/// what can be cleared, and the ask is the size it always was.
fn room(clean: Option<&CleanList>) -> Option<Room> {
    let clean = clean?;
    Some(Room {
        clear: removable_now(Some(clean))
            .iter()
            .map(|i| i.bytes)
            .sum::<u64>()
            / MIB,
        live: clean
            .items
            .iter()
            .filter(|i| i.live)
            .map(|i| i.bytes)
            .sum::<u64>()
            .div_ceil(MIB),
    })
}

/// What a disk ask asks of this box, in MiB, and what it holds: the fleet's share or the excess
/// over the allowance, or everything it holds when every box was asked for everything.
fn asked_of(d: &DiskAsk) -> (u64, u64) {
    match d {
        DiskAsk::Share(s) => (s.free, s.holds),
        DiskAsk::EveryBox(s) => (s.holds, s.holds),
        DiskAsk::Allowance { excess, holds, .. } => (*excess, *holds),
    }
}

/// What the ask comes to once it is no more than the box can clear now (owner, 2026-10-30: the ask
/// shrinks to what can actually be cleared, and a box is never held over space it cannot free).
/// The part it cannot meet is not asked of any other box: the fleet's audience is
/// `announce::cover`'s, and this changes nothing in it.
fn shrunk(d: &DiskAsk, room: Option<Room>) -> u64 {
    let (asked, _) = asked_of(d);
    room.map_or(asked, |r| asked.min(r.clear))
}

/// Whether a disk ask is far enough over to hold a command: what it asks this box to clear, once
/// [`shrunk`], is more than [`DISK_HOLD_OVER_PCT`] of the box's share.
///
/// The share is the box's allowance when it has one, for every kind of disk ask, so the fleet's
/// ask and the allowance's are held by the same rule (owner: "same rules for the fleet-share ask").
/// A box allowed the whole disk has no allowance to measure against, so its share is what it holds.
fn disk_holds(d: &DiskAsk, asked: u64, limit: Option<u64>) -> bool {
    let (_, holds) = asked_of(d);
    let share = limit.unwrap_or(holds);
    asked.saturating_mul(100) > share.saturating_mul(DISK_HOLD_OVER_PCT)
}

/// The box's own items its agent may remove at any time, biggest first: download caches and build
/// output (owner, 2026-10-30). Not the repo's shared cache, which counts against no box, not
/// scratchpads, which may belong to a session still running in the box, and not a build that is
/// running now (SKEIN-1391).
fn removable_now(clean: Option<&CleanList>) -> Vec<CleanItem> {
    clean
        .map(|c| {
            c.items
                .iter()
                .filter(|i| !i.live)
                .filter(|i| i.kind == "download cache" || i.kind == "build output")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
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
        at: stamp(r.at),
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
                read_at: Some(stamp(r.at)),
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
        updates: Vec::new(),
        clean: r.clean.clone(),
        disk_ask: None,
    };

    let prev_ask = |kind: &str| prev.and_then(|p| p.asks.iter().find(|a| a.kind == kind));
    // A crossing keeps its id and start while the band holds; a new one is minted on entering, and
    // on a disk ask starting or stopping holding commands, so going from far enough over to tell
    // the agent to far enough over to hold it is a crossing the plugin has not yet shown.
    let stamp = |kind: &str, fresh: Ask| -> Ask {
        match prev_ask(kind).filter(|b| b.hold.is_empty() == fresh.hold.is_empty()) {
            Some(before) => Ask {
                since: before.since.clone(),
                crossing_id: before.crossing_id.clone(),
                ..fresh
            },
            None => Ask {
                since: stamp(r.at),
                crossing_id: format!("{kind}-{}", r.at.timestamp_millis()),
                ..fresh
            },
        }
    };

    match disk_band(r) {
        Band::Asked(d) => {
            let room = room(r.clean.as_ref());
            let asked = shrunk(&d, room);
            let mut ask = words::disk(&d, &removable_now(r.clean.as_ref()), asked, room);
            if !disk_holds(&d, asked, r.disk_limit_mb) {
                // Told, not held: the plugin holds only an ask with words to hold it with.
                ask.hold = String::new();
            }
            out.asks.push(stamp("disk", ask));
        }
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

/// The file with the box's disk ask in it, and, when the box is over its own allowance by more
/// than it can clear now, the disk ask's words saying it can ask the owner for more (SKEIN-1425).
///
/// Only for the allowance's ask: when the fleet's disk is over its line, a bigger allowance frees
/// nothing. And only when what can be cleared falls short: the ask's `amount` is already
/// [`shrunk`] to what can be cleared, so it is under the excess exactly then. Pure.
pub fn with_disk_ask(mut s: Signals, ask: Option<DiskAskView>) -> Signals {
    let excess = match (s.disk.mb, s.disk.limit_mb) {
        (Some(mb), Some(limit)) => mb.saturating_sub(limit),
        _ => 0,
    };
    if let Some(a) = s
        .asks
        .iter_mut()
        .find(|a| a.kind == "disk" && a.why == "allowance")
        .filter(|a| a.amount < excess)
    {
        let line = words::ask_for_more(ask.as_ref());
        a.hold = a
            .hold
            .replace(words::HELD, &format!("{line} {}", words::HELD));
        a.start = a
            .start
            .take()
            .map(|t| t.replace(words::WHERE, &format!("{line} {}", words::WHERE)));
        a.enter = a.enter.take().map(|t| format!("{t} {line}"));
    }
    s.disk_ask = ask;
    s
}

// ------------------------------------------------------------------------------------------------
// The words
// ------------------------------------------------------------------------------------------------

/// What an agent is shown, with integers filled in and nothing else.
///
/// **The owner approved these texts on SKEIN-1055, 2026-09-24**, labelled (1a)–(6) in
/// `feature-wording.md`. Two of the owner's decisions shape them: the `skein_top` and
/// `skein_resources` sentences were left out until those tools shipped, and are in now that they
/// have (SKEIN-1059); and the closing "This one command was held so you would see this; the next
/// will run." stays. Every argument is a number skein measured, with one exception: the disk
/// ask's [`words::clean_hint`] names the box's biggest cleanable items by path (SKEIN-1356), and a
/// box names its own directories. Only a path of plain characters is named
/// ([`words::nameable`]), so no name can carry a line break or a sentence of its own into the
/// words, and only the box's own items are named, to the box's own agent.
///
/// Every live ask carries all four: a hold, a start line (4), a reminder (5) and an entering line
/// (6). The texts for the cases the first draft did not cover — (4), (5) and (6) for every disk
/// case, memory and the process count, and (1a) with fewer than two other boxes — were approved by
/// the owner on 2026-09-24, and on the same day he decided that every start line (4), not only
/// (1a)'s, ends with "`skein_resources` shows where this box stands." ([`words::WHERE`]).
mod words {
    use super::*;

    pub(super) const HELD: &str =
        "This one command was held so you would see this; the next will run.";

    /// What a box over its own allowance by more than it can clear is told it can do (SKEIN-1425),
    /// or that it has done.
    pub(super) fn ask_for_more(ask: Option<&DiskAskView>) -> String {
        const SHORT: &str = "Clearing will not bring this box under its share";
        match ask.map(|a| (a.state.as_str(), a.mb)) {
            Some(("waiting", mb)) => format!("{SHORT}; its ask for {} is with the owner.", gib(mb)),
            Some(("denied", mb)) => format!(
                "{SHORT}; the owner denied its ask for {}, so clear what it can.",
                gib(mb)
            ),
            _ => format!("{SHORT}; to ask the owner for more, call `skein_request_disk`."),
        }
    }

    /// The last sentence of every start line (4), whatever the ask (owner, 2026-09-24).
    pub(super) const WHERE: &str = "`skein_resources` shows where this box stands.";

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

    /// The end of (1a)'s second sentence: "; 2 other boxes have been asked for the rest", with one
    /// other box in the singular and no clause at all when this box was asked alone (owner,
    /// 2026-09-24): "0 other boxes have been asked for the rest" would say there is a rest.
    fn others_asked(n: usize) -> String {
        match n {
            0 => String::new(),
            1 => "; 1 other box has been asked for the rest".to_string(),
            n => format!("; {n} other boxes have been asked for the rest"),
        }
    }

    /// Whether a cleanable item's path may be named in the words: short, and nothing but the
    /// characters a path of a cache or a build directory is made of. A box chooses the names of
    /// its own directories, so anything else is left out rather than quoted.
    pub(super) fn nameable(shown: &str) -> bool {
        !shown.is_empty()
            && shown.len() <= 120
            && shown
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/~._+@-".contains(c))
    }

    /// The disk ask's pointer to `skein_clean`, with the box's biggest items that its agent may
    /// remove now (SKEIN-1356): up to three, biggest first, as `~/.cache/pip (3.2G)`, and the exact
    /// call that removes them, so the agent can act on the notice in one step.
    pub(super) fn clean_hint(biggest: &[CleanItem]) -> String {
        let named: Vec<&CleanItem> = biggest
            .iter()
            .filter(|i| nameable(&i.shown) && nameable(&i.path))
            .take(3)
            .collect();
        let sized: Vec<String> = named
            .iter()
            .map(|i| format!("{} ({})", i.shown, gib_up(i.bytes.div_ceil(1024 * 1024))))
            .collect();
        let list = match sized.as_slice() {
            [] => return CLEAN.to_string(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        };
        let paths: Vec<&str> = named.iter().map(|i| i.path.as_str()).collect();
        format!(
            "{CLEAN} The biggest {} {list}; to remove {} now, call `skein_clean` with {}.",
            if named.len() == 1 { "is" } else { "are" },
            if named.len() == 1 { "it" } else { "them" },
            serde_json::json!({ "paths": paths }),
        )
    }

    /// The sentence every disk ask's hold, start and entering line carries (SKEIN-1356).
    pub(super) const CLEAN: &str =
        "`skein_clean` lists the caches and build output this box can remove, and removes them.";

    /// The disk ask's words for asking `asked` MiB, with `room` what skein's list says the box
    /// can do about it now (SKEIN-1391). With no build running, the owner's approved words with
    /// the shrunk figure; with one, the words that say how much of the box is in it; with nothing
    /// to clear, a line that is told and never held.
    pub(super) fn disk(d: &DiskAsk, biggest: &[CleanItem], asked: u64, room: Option<Room>) -> Ask {
        let live = room.map_or(0, |r| r.live);
        if room.is_some() && asked == 0 {
            return nothing_to_clear(d, live);
        }
        let hint = clean_hint(biggest);
        let mut ask = match live {
            0 => {
                let mut ask = disk_words(&with_asked(d, asked));
                ask.amount = asked;
                ask
            }
            _ => live_words(d, asked, live),
        };
        ask.live_mb = live;
        // Before the closing sentence of the hold and the start line, which the owner fixed
        // (`HELD`, `WHERE`); at the end of the entering line.
        ask.hold = ask.hold.replace(HELD, &format!("{hint} {HELD}"));
        ask.start = ask
            .start
            .map(|s| s.replace(WHERE, &format!("{hint} {WHERE}")));
        ask.enter = ask.enter.map(|e| format!("{e} {hint}"));
        ask
    }

    /// `d` asking `asked` MiB: the fleet's share or the excess replaced. An every-box ask's words
    /// name what the box holds, not an amount, so it is unchanged.
    fn with_asked(d: &DiskAsk, asked: u64) -> DiskAsk {
        match d {
            DiskAsk::Share(s) => DiskAsk::Share(Share {
                free: asked,
                ..s.clone()
            }),
            DiskAsk::EveryBox(_) => d.clone(),
            DiskAsk::Allowance { holds, limit, .. } => DiskAsk::Allowance {
                excess: asked,
                holds: *holds,
                limit: *limit,
            },
        }
    }

    /// "This box holds 10.5G against its 10.0G share of the fleet's disk", or for the fleet's
    /// asks "The fleet's disk is 4.1G over what it can spare and this box holds 9.4G of it", with
    /// `live` MiB of it in a build running now when that is not zero.
    fn standing(d: &DiskAsk, live: u64) -> String {
        match d {
            DiskAsk::Allowance { holds, limit, .. } => match live {
                0 => format!(
                    "this box holds {} against its {} share of the fleet's disk",
                    gib_up(*holds),
                    gib(*limit)
                ),
                _ => format!(
                    "this box holds {} against its {} share ({} in a build running now)",
                    gib_up(*holds),
                    gib(*limit),
                    gib_up(live)
                ),
            },
            DiskAsk::Share(s) | DiskAsk::EveryBox(s) => match live {
                0 => format!(
                    "the fleet's disk is {} over what it can spare and this box holds {} of it",
                    gib_up(s.over_by),
                    gib(s.holds)
                ),
                _ => format!(
                    "the fleet's disk is {} over what it can spare and this box holds {} of it ({} \
                     in a build running now)",
                    gib_up(s.over_by),
                    gib(s.holds),
                    gib_up(live)
                ),
            },
        }
    }

    /// Nothing on skein's list can be cleared now (SKEIN-1391): the box is told where it stands
    /// and is never held. `amount` is zero, so nothing reads it as a figure to clear.
    fn nothing_to_clear(d: &DiskAsk, live: u64) -> Ask {
        let end = match live {
            0 => "nothing skein lists can be cleared now.",
            _ => "nothing can be cleared until it ends.",
        };
        let line = format!("skein: {}; {end}", standing(d, live));
        Ask {
            kind: "disk".into(),
            why: disk_words(d).why,
            amount: 0,
            live_mb: live,
            hold: String::new(),
            start: Some(format!("{line} {WHERE}")),
            remind: Some(line.clone()),
            enter: Some(line),
            ..Default::default()
        }
    }

    /// Some of the box is in a build running now and `asked` MiB can be cleared (SKEIN-1391): the
    /// approved shape of each ask, saying how much is in the build and that only `asked` can go.
    fn live_words(d: &DiskAsk, asked: u64, live: u64) -> Ask {
        let z = gib_up(asked);
        let y = gib_up(live);
        let (why, hold, start, remind, enter) = match d {
            DiskAsk::Allowance { holds, limit, .. } => (
                "allowance",
                format!(
                    "skein: clear {z} of storage you are not using before you continue. This box \
                     holds {} against its {} share of the fleet's disk ({y} of it in a build \
                     running now); {z} can be cleared now. {HELD}",
                    gib_up(*holds),
                    gib(*limit),
                ),
                format!(
                    "skein: before you start, this box is asked to clear {z} of storage it is not \
                     using. It holds {} against its {} share of the fleet's disk ({y} of it in a \
                     build running now); {z} can be cleared now. {WHERE}",
                    gib_up(*holds),
                    gib(*limit),
                ),
                format!(
                    "skein: still asked of this box: clear {z} of storage (it holds {}, {y} of it \
                     in a build running now).",
                    gib_up(*holds),
                ),
                format!(
                    "skein: this box holds {} against its {} share of the fleet's disk ({y} of it \
                     in a build running now); clear the {z} that can be cleared now.",
                    gib_up(*holds),
                    gib(*limit),
                ),
            ),
            DiskAsk::Share(s) | DiskAsk::EveryBox(s) => (
                if s.every_box {
                    "fleet-every-box"
                } else {
                    "fleet"
                },
                format!(
                    "skein: clear {z} of storage you are not using before you continue — build \
                     outputs, caches, and any containers or volumes you started. The fleet's disk \
                     is {} over what it can spare, and this box holds {} of it ({y} in a build \
                     running now){}; {z} can be cleared now. {HELD}",
                    gib_up(s.over_by),
                    gib(s.holds),
                    match s.every_box {
                        true => "; every box has been asked".to_string(),
                        false => others_asked(s.others),
                    },
                ),
                format!(
                    "skein: before you start, this box is asked to clear {z} of storage it is not \
                     using. The fleet's disk is {} over what it can spare and this box holds {} \
                     ({y} of it in a build running now); {z} can be cleared now. {WHERE}",
                    gib_up(s.over_by),
                    gib(s.holds),
                ),
                format!(
                    "skein: still asked of this box: clear {z} of storage (it holds {}, {y} of it \
                     in a build running now).",
                    gib(s.holds),
                ),
                format!(
                    "skein: the fleet's disk is over its line; this box is asked to clear the {z} \
                     it can clear now ({y} of it is in a build running now)."
                ),
            ),
        };
        Ask {
            kind: "disk".into(),
            why: why.into(),
            amount: asked,
            hold,
            start: Some(start),
            remind: Some(remind),
            enter: Some(enter),
            ..Default::default()
        }
    }

    /// The owner's approved words (SKEIN-1055) before [`clean_hint`] is added.
    pub(super) fn disk_words(d: &DiskAsk) -> Ask {
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
                    free = gib_up(s.free),
                    over = gib_up(s.over_by),
                    holds = gib(s.holds),
                    others = others_asked(s.others),
                ),
                // (4) as approved, with its last sentence: `skein_resources` shipped with
                // SKEIN-1059, which is when the owner said it could be restored (2026-09-24). Every
                // other start line ends with the same sentence, by the owner's decision that day.
                start: Some(format!(
                    "skein: before you start, this box is asked to clear {free} of storage it is \
                     not using. The fleet's disk is {over} over what it can spare and this box \
                     holds {holds}. {WHERE}",
                    free = gib_up(s.free),
                    over = gib_up(s.over_by),
                    holds = gib(s.holds),
                )),
                remind: Some(format!(
                    "skein: still asked of this box: clear {} of storage (it holds {}).",
                    gib_up(s.free),
                    gib(s.holds)
                )),
                enter: Some(format!(
                    "skein: the fleet's disk is over its line; this box is asked to clear {} it \
                     is not using.",
                    gib_up(s.free)
                )),
                ..Default::default()
            },
            // (1b), (4), (5), (6).
            DiskAsk::EveryBox(s) => Ask {
                kind: "disk".into(),
                why: "fleet-every-box".into(),
                amount: s.holds,
                hold: format!(
                    "skein: clear what you can of the {holds} this box holds before you continue — \
                     build outputs, caches, and any containers or volumes you started. The fleet's \
                     disk is {over} over what it can spare, and every box has been asked. {HELD}",
                    holds = gib(s.holds),
                    over = gib_up(s.over_by),
                ),
                start: Some(format!(
                    "skein: before you start, this box is asked to clear what it can of the \
                     {holds} it holds. The fleet's disk is {over} over what it can spare, and \
                     every box has been asked. {WHERE}",
                    holds = gib(s.holds),
                    over = gib_up(s.over_by),
                )),
                remind: Some(format!(
                    "skein: still asked of this box: clear what you can of the {} it holds.",
                    gib(s.holds)
                )),
                enter: Some(format!(
                    "skein: the fleet's disk is over its line; this box is asked to clear what it \
                     can of the {} it holds.",
                    gib(s.holds)
                )),
                ..Default::default()
            },
            // (1c), (4), (5), (6). Its own (6): the fleet's line is not why this box is asked.
            // What it holds is rounded up too, so a box a little over never reads as holding no more
            // than its share ("holds 10.0G against its 10.0G share").
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
                    gib_up(*excess),
                    gib_up(*holds),
                    gib(*limit),
                ),
                start: Some(format!(
                    "skein: before you start, this box is asked to clear {} of storage it is not \
                     using. It holds {} against its {} share of the fleet's disk. {WHERE}",
                    gib_up(*excess),
                    gib_up(*holds),
                    gib(*limit),
                )),
                remind: Some(format!(
                    "skein: still asked of this box: clear {} of storage (it holds {}).",
                    gib_up(*excess),
                    gib_up(*holds)
                )),
                enter: Some(format!(
                    "skein: this box holds {} against its {} share of the fleet's disk; clear {} \
                     it is not using.",
                    gib_up(*holds),
                    gib(*limit),
                    gib_up(*excess),
                )),
                ..Default::default()
            },
        }
    }

    /// (2), (4), (5) and (6).
    pub(super) fn memory(rate: f64, anon: u64, high: u64) -> Ask {
        let rate = rate.round() as u64;
        Ask {
            kind: "memory".into(),
            amount: rate,
            hold: format!(
                "skein: stop processes you no longer need before you start more. This box is being \
                 slowed for memory, {} times a minute, and holds {}; the kernel slows it above {}. \
                 `skein_top` lists this box's largest processes. {HELD}",
                count(rate),
                bytes(anon),
                bytes(high),
            ),
            start: Some(format!(
                "skein: before you start, this box is being slowed for memory ({} throttles a \
                 minute) and holds {}; stop processes you no longer need before you start more. \
                 {WHERE}",
                count(rate),
                bytes(anon),
            )),
            remind: Some(format!(
                "skein: still asked of this box: stop processes you no longer need (slowed for \
                 memory, {} throttles a minute).",
                count(rate)
            )),
            enter: Some(format!(
                "skein: this box is being slowed for memory ({} throttles a minute); stop \
                 processes you no longer need.",
                count(rate)
            )),
            ..Default::default()
        }
    }

    /// (3), (4), (5) and (6).
    pub(super) fn pids(pids: u64, max: u64) -> Ask {
        Ask {
            kind: "pids".into(),
            amount: pids,
            hold: format!(
                "skein: end processes you started and no longer need before you start more. This \
                 box is running {} processes against a limit of {}, and past that nothing in it \
                 can start a new one. `skein_top` lists the ones with no parent. {HELD}",
                count(pids),
                count(max),
            ),
            start: Some(format!(
                "skein: before you start, this box is running {} of its {} processes; end the \
                 ones you no longer need. {WHERE}",
                count(pids),
                count(max)
            )),
            remind: Some(format!(
                "skein: still asked of this box: end processes you no longer need ({} of {}).",
                count(pids),
                count(max)
            )),
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
    write_box_with(state, r, Vec::new(), None)
}

/// [`write_box`] with the versions a restart would move the box onto, and the box's disk ask.
pub fn write_box_with(
    state: &Path,
    r: &Reading,
    updates: Vec<Update>,
    disk_ask: Option<DiskAskView>,
) -> Result<Signals, String> {
    let prev = read(state);
    let mut signals = with_disk_ask(next(prev.as_ref(), r), disk_ask);
    signals.updates = updates;
    let path = signal_path(state);
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(&signals).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, dir, &bytes)?;
    Ok(signals)
}

/// Box `name`'s [`CleanList`] for this tick: its items listed again when the previous list is
/// [`CLEAN_LIST_EVERY`] old or missing, carried otherwise; and the fact about its repo's other
/// boxes read again every tick, since that is what the plugin's refusal turns on.
///
/// `repo_caches` measures each repo's shared cache once per tick, however many of its boxes are
/// listed in it. `None` when the box has no directory in the fleet.
pub(crate) fn clean_list(
    name: &str,
    prev: Option<&CleanList>,
    liveness: &std::collections::HashMap<String, bool>,
    at: DateTime<Utc>,
    repo_caches: &mut BTreeMap<String, Option<crate::fleet::RepoCache>>,
) -> Option<CleanList> {
    let fresh = prev.filter(|p| {
        minutes_since(Some(p.at.as_str()), at)
            .is_some_and(|m| m * 60.0 < CLEAN_LIST_EVERY.as_secs_f64())
    });
    let mut list = match fresh {
        Some(p) => p.clone(),
        None => {
            let items = crate::fleet::box_cleanables(name).ok()?;
            let cache = crate::fleet::repo_cache_for_box(name, liveness, repo_caches)
                .ok()
                .flatten();
            CleanList {
                at: stamp(at),
                box_root: crate::fleet::box_root(name),
                items: items.into_iter().map(CleanItem::from).collect(),
                repo_cache: cache.map(|rc| RepoCacheList {
                    path: rc.path,
                    bytes: rc.items.iter().map(|i| i.bytes).sum(),
                    items: rc.items.into_iter().map(CleanItem::from).collect(),
                    others_running: 0,
                    others_unknown: 0,
                    none_running_at: prev
                        .and_then(|p| p.repo_cache.as_ref())
                        .and_then(|c| c.none_running_at.clone()),
                }),
            }
        }
    };
    // Whether a build is running in each build directory, read every tick like the repo's other
    // boxes below: a build starts and ends between two walks of the items.
    let builds: Vec<String> = list
        .items
        .iter()
        .filter(|i| i.kind == "build output")
        .map(|i| i.path.clone())
        .collect();
    let live = crate::fleet::live_builds(&builds.iter().map(String::as_str).collect::<Vec<_>>());
    for item in list.items.iter_mut() {
        item.live = live.contains(&item.path);
    }
    if let Some(rc) = list.repo_cache.as_mut() {
        let (running, unknown) = crate::fleet::others_of_box(name, liveness);
        rc.others_running = running.len();
        rc.others_unknown = unknown.len();
        if running.is_empty() && unknown.is_empty() {
            rc.none_running_at = Some(stamp(at));
        }
    }
    Some(list)
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
    let liveness = crate::fleet::fleet_liveness();
    let mut repo_caches = BTreeMap::new();
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
            clean: clean_list(
                &name,
                read(&entry.path()).and_then(|p| p.clean).as_ref(),
                &liveness,
                at,
                &mut repo_caches,
            ),
        };
        let asked = crate::fleet::versions::restart_requested(&name);
        let updates = crate::fleet::running_record(&name)
            .map(|record| crate::fleet::versions::pending_for(&record, &name))
            .unwrap_or_default()
            .into_iter()
            .map(|p| Update {
                component: p.component,
                have: p.have,
                new: p.new,
                applies: "restart".into(),
                restart_requested: asked,
            })
            .collect();
        let disk_ask = crate::diskask::for_box(&name).map(DiskAskView::from);
        match write_box_with(&entry.path(), &reading, updates, disk_ask) {
            Ok(_) => written += 1,
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    match failed.is_empty() {
        true => Ok(written),
        false => Err(failed.join("; ")),
    }
}

/// Each box's own spend from one reading of the fleet, [`usage_path`] in every box's state
/// directory (SKEIN-1384).
///
/// The same list of boxes as [`write_every_box`]: every directory under the state root. Each file
/// is [`crate::usage::box_spend`] for that box alone, zeros for a box with no turn in the reading,
/// so no box is told another's figure.
pub fn write_box_spend(report: &crate::usage::UsageReport) -> Result<usize, String> {
    let root = crate::fleet::box_state_root();
    let entries = std::fs::read_dir(&root).map_err(|e| format!("{root}: {e}"))?;
    let mut written = 0;
    let mut failed = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        let path = usage_path(&entry.path());
        let wrote = path
            .parent()
            .ok_or_else(|| format!("{} has no directory", path.display()))
            .and_then(|dir| {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
                let spend = crate::usage::box_spend(report, &name);
                let bytes = serde_json::to_vec_pretty(&spend).map_err(|e| e.to_string())?;
                crate::util::write_atomic(&path, dir, &bytes)
            });
        match wrote {
            Ok(()) => written += 1,
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
            clean: None,
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
             start a new one. `skein_top` lists the ones with no parent. This one command was held \
             so you would see this; the next will run."
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
             12.3G against its 10.0G share of the fleet's disk. `skein_clean` lists the caches and \
             build output this box can remove, and removes them. This one command was held so you \
             would see this; the next will run."
        );
    }

    /// **Every text of every live ask is the approved text, figure for figure**: the hold, the
    /// start line (4), the reminder (5) and the entering line (6), for each of the three disk
    /// cases, memory and the process count, and (1a) with two, one and no other boxes asked.
    ///
    /// The approvals are SKEIN-1055 (1a)–(6) and the owner's texts of 2026-09-24 for the cases the
    /// first draft had none for. Fails on any change to a word of any of them, on any of the four
    /// being `None`, and on any start line that does not end "`skein_resources` shows where this
    /// box stands." (the owner's rule for every start line, 2026-09-24).
    #[test]
    fn the_words_are_the_approved_words_with_the_figures_filled_in() {
        const HELD: &str = "This one command was held so you would see this; the next will run.";
        let four = |a: Ask| [Some(a.hold), a.start, a.remind, a.enter];
        let share_start = "skein: before you start, this box is asked to clear 2.3G of storage \
                           it is not using. The fleet's disk is 4.1G over what it can spare and \
                           this box holds 9.4G. `skein_resources` shows where this box stands.";
        let share_remind = "skein: still asked of this box: clear 2.3G of storage (it holds 9.4G).";
        let share_enter =
            "skein: the fleet's disk is over its line; this box is asked to clear 2.3G it is not \
             using.";
        let share_hold = |others: &str| {
            format!(
                "skein: clear 2.3G of storage you are not using before you continue — build \
                 outputs, caches, and any containers or volumes you started. The fleet's disk is \
                 4.1G over what it can spare, and this box holds 9.4G of it{others}. {HELD}"
            )
        };
        let cases: Vec<(&str, Ask, [String; 4])> = vec![
            (
                "(1a), two others",
                words::disk_words(&DiskAsk::Share(share(2, false))),
                [
                    share_hold("; 2 other boxes have been asked for the rest"),
                    share_start.into(),
                    share_remind.into(),
                    share_enter.into(),
                ],
            ),
            (
                "(1a), one other",
                words::disk_words(&DiskAsk::Share(share(1, false))),
                [
                    share_hold("; 1 other box has been asked for the rest"),
                    share_start.into(),
                    share_remind.into(),
                    share_enter.into(),
                ],
            ),
            (
                "(1a), alone",
                words::disk_words(&DiskAsk::Share(share(0, false))),
                [
                    share_hold(""),
                    share_start.into(),
                    share_remind.into(),
                    share_enter.into(),
                ],
            ),
            (
                "(1b)",
                words::disk_words(&DiskAsk::EveryBox(Share {
                    over_by: 6144,
                    ..share(3, true)
                })),
                [
                    format!(
                        "skein: clear what you can of the 9.4G this box holds before you continue \
                         — build outputs, caches, and any containers or volumes you started. The \
                         fleet's disk is 6.0G over what it can spare, and every box has been \
                         asked. {HELD}"
                    ),
                    "skein: before you start, this box is asked to clear what it can of the 9.4G \
                     it holds. The fleet's disk is 6.0G over what it can spare, and every box has \
                     been asked. `skein_resources` shows where this box stands."
                        .into(),
                    "skein: still asked of this box: clear what you can of the 9.4G it holds."
                        .into(),
                    "skein: the fleet's disk is over its line; this box is asked to clear what it \
                     can of the 9.4G it holds."
                        .into(),
                ],
            ),
            (
                "(1c)",
                words::disk_words(&DiskAsk::Allowance {
                    excess: 2355,
                    holds: 12595,
                    limit: 10240,
                }),
                [
                    format!(
                        "skein: clear 2.3G of storage you are not using before you continue. This \
                         box holds 12.3G against its 10.0G share of the fleet's disk. {HELD}"
                    ),
                    "skein: before you start, this box is asked to clear 2.3G of storage it is \
                     not using. It holds 12.3G against its 10.0G share of the fleet's disk. \
                     `skein_resources` shows where this box stands."
                        .into(),
                    "skein: still asked of this box: clear 2.3G of storage (it holds 12.3G)."
                        .into(),
                    "skein: this box holds 12.3G against its 10.0G share of the fleet's disk; \
                     clear 2.3G it is not using."
                        .into(),
                ],
            ),
            (
                "(2)",
                words::memory(84.0, 11 * GIB + GIB / 5, 12 * GIB),
                [
                    format!(
                        "skein: stop processes you no longer need before you start more. This box \
                         is being slowed for memory, 84 times a minute, and holds 11.2G; the \
                         kernel slows it above 12.0G. `skein_top` lists this box's largest \
                         processes. {HELD}"
                    ),
                    "skein: before you start, this box is being slowed for memory (84 throttles \
                     a minute) and holds 11.2G; stop processes you no longer need before you \
                     start more. `skein_resources` shows where this box stands."
                        .into(),
                    "skein: still asked of this box: stop processes you no longer need (slowed \
                     for memory, 84 throttles a minute)."
                        .into(),
                    "skein: this box is being slowed for memory (84 throttles a minute); stop \
                     processes you no longer need."
                        .into(),
                ],
            ),
            (
                "(3)",
                words::pids(6410, 8192),
                [
                    format!(
                        "skein: end processes you started and no longer need before you start \
                         more. This box is running 6,410 processes against a limit of 8,192, and \
                         past that nothing in it can start a new one. `skein_top` lists the ones \
                         with no parent. {HELD}"
                    ),
                    "skein: before you start, this box is running 6,410 of its 8,192 processes; \
                     end the ones you no longer need. `skein_resources` shows where this box \
                     stands."
                        .into(),
                    "skein: still asked of this box: end processes you no longer need (6,410 of \
                     8,192)."
                        .into(),
                    "skein: this box is running 6,410 of its 8,192 processes; end the ones you no \
                     longer need."
                        .into(),
                ],
            ),
        ];
        for (what, ask, want) in cases {
            let got = four(ask);
            // Stated on its own as well as inside each text above, because it is a rule about
            // every start line rather than a fact about one (owner, 2026-09-24).
            assert!(
                got[1].as_deref().is_some_and(
                    |start| start.ends_with(". `skein_resources` shows where this box stands.")
                ),
                "{what}: the start line does not end by pointing at skein_resources: {:?}",
                got[1]
            );
            for (n, (field, want)) in ["hold", "start", "remind", "enter"]
                .iter()
                .zip(want)
                .enumerate()
            {
                assert_eq!(
                    got[n].as_deref(),
                    Some(want.as_str()),
                    "{what}: the {field} text is not the approved one"
                );
            }
        }
    }

    /// **No live ask of any kind is without a start line (4).**
    ///
    /// Driven through [`next`] rather than the templates, so it covers every ask the file can
    /// actually carry: each disk case, memory and the process count. The start line is what makes
    /// SessionStart count as shown; an ask without one would hold the first command of every new
    /// session for a crossing the session could have been told about.
    ///
    /// Fails if any template's `start` goes back to `None`.
    #[test]
    fn every_live_ask_has_a_start_line() {
        let disk = |fleet: FleetDisk, mb: u64, limit: Option<u64>| Reading {
            fleet,
            disk_mb: Some(mb),
            disk_limit_mb: limit,
            ..reading(0, Some(calm()))
        };
        let mut live: Vec<Ask> = Vec::new();
        for r in [
            disk(FleetDisk::Asked(share(2, false)), 9626, None),
            disk(FleetDisk::Asked(share(0, true)), 9626, None),
            disk(FleetDisk::NotAsked { over_by: 0 }, 12595, Some(10240)),
            reading(
                0,
                Some(Cgroup {
                    pids: 7000,
                    ..calm()
                }),
            ),
        ] {
            live.extend(next(None, &r).asks);
        }
        let hot = run(&[
            reading(0, Some(hot(0, 84))),
            reading(1, Some(hot(1, 84))),
            reading(2, Some(hot(2, 84))),
        ]);
        live.extend(hot[2].asks.clone());

        let whys: Vec<String> = live
            .iter()
            .map(|a| format!("{}/{}", a.kind, a.why))
            .collect();
        assert_eq!(
            whys,
            vec![
                "disk/fleet",
                "disk/fleet-every-box",
                "disk/allowance",
                "pids/",
                "memory/"
            ],
            "the readings no longer raise one ask of each case"
        );
        for a in &live {
            assert!(
                a.start.as_deref().is_some_and(|s| !s.is_empty()),
                "a live {}/{} ask has no start line: {a:?}",
                a.kind,
                a.why
            );
        }
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
            let a = words::disk(&d, &[], asked_of(&d).0, None);
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

    /// A box `excess` MiB over a 10G allowance, with nothing asked of it by the fleet.
    fn over_allowance(minute: i64, excess: u64) -> Reading {
        Reading {
            disk_mb: Some(10240 + excess),
            disk_limit_mb: Some(10240),
            ..reading(minute, Some(calm()))
        }
    }

    /// **A disk ask holds a command only when it is over by more than a tenth of the box's share**
    /// (owner, SKEIN-1335): 1G on a 10G share. Under that the box is still asked — the ask is in
    /// the file with its start line, reminder and entering line — and its `hold` is empty, which is
    /// what the plugin reads as "tell, do not hold". The same rule for the fleet's ask, measured
    /// against the box's allowance, or against what it holds when it has none.
    ///
    /// Fails if the threshold is dropped (every ask holds again, so the 1,024 MiB case has words),
    /// if the comparison becomes `>=` (exactly 10% holds), or if the fleet's ask is left out of it.
    #[test]
    fn a_disk_ask_holds_only_when_over_by_more_than_a_tenth_of_the_share() {
        let disk = |r: &Reading| next(None, r).asks.into_iter().find(|a| a.kind == "disk");
        for (excess, holds) in [(1, false), (1024, false), (1025, true), (4096, true)] {
            let ask = disk(&over_allowance(0, excess)).expect("over its allowance and not asked");
            assert_eq!(ask.why, "allowance");
            assert!(ask.start.is_some() && ask.remind.is_some() && ask.enter.is_some());
            assert_eq!(
                !ask.hold.is_empty(),
                holds,
                "{excess} MiB over a 10G share: held {:?}",
                ask.hold
            );
        }
        let fleet = |free: u64, limit: Option<u64>| Reading {
            fleet: FleetDisk::Asked(Share {
                free,
                ..share(0, false)
            }),
            disk_mb: Some(9626),
            disk_limit_mb: limit,
            ..reading(0, Some(calm()))
        };
        for (free, limit, holds) in [
            (1024, Some(10240), false),
            (1025, Some(10240), true),
            // No allowance: a tenth of the 9,626 MiB it holds.
            (962, None, false),
            (963, None, true),
        ] {
            let ask = disk(&fleet(free, limit)).expect("the fleet's ask went missing");
            assert_eq!(ask.why, "fleet");
            assert_eq!(
                !ask.hold.is_empty(),
                holds,
                "asked for {free} MiB with allowance {limit:?}: held {:?}",
                ask.hold
            );
        }
    }

    /// **Going from told to held is a new crossing, and a band that stays held keeps its id.**
    ///
    /// The plugin holds one command per crossing id, so a box that grows from a little over to far
    /// over must get a new id, or it would never be held: its id was already "shown" by the start
    /// line. Fails if the crossing id carries over regardless of whether the ask holds.
    #[test]
    fn going_from_told_to_held_is_a_new_crossing() {
        let files = run(&[
            over_allowance(0, 200),
            over_allowance(1, 300),
            over_allowance(2, 2048),
            over_allowance(3, 2100),
        ]);
        let id = |n: usize| files[n].asks[0].crossing_id.clone();
        assert_eq!(id(0), id(1), "a told band that held minted a new crossing");
        assert_ne!(id(1), id(2), "starting to hold did not mint a new crossing");
        assert_eq!(id(2), id(3), "a held band that held minted a new crossing");
        assert!(files[1].asks[0].hold.is_empty() && !files[2].asks[0].hold.is_empty());
    }

    /// **No disk ask says `0.0G`** (SKEIN-1335: 73 asks to "clear 0.0G" in a month). An amount to
    /// clear is rounded up to the tenth, so it is never zero and clearing it is always enough.
    ///
    /// Fails if the disk words go back to `gib`, which prints 1 MiB as `0.0G`.
    #[test]
    fn no_disk_ask_says_nothing_needs_clearing() {
        assert_eq!(
            [1, 51, 1024, 1025, 2355].map(gib_up),
            ["0.1G", "0.1G", "1.0G", "1.1G", "2.3G"]
        );
        let tiny = Share {
            over_by: 1,
            free: 1,
            ..share(0, false)
        };
        for d in [
            DiskAsk::Share(tiny.clone()),
            DiskAsk::Allowance {
                excess: 1,
                holds: 10241,
                limit: 10240,
            },
        ] {
            let a = words::disk(&d, &[], asked_of(&d).0, None);
            if let DiskAsk::Allowance { .. } = d {
                assert!(
                    a.hold.contains("holds 10.1G against its 10.0G share"),
                    "{}",
                    a.hold
                );
            }
            for t in [Some(a.hold), a.start, a.remind, a.enter]
                .into_iter()
                .flatten()
            {
                assert!(!t.contains(" 0.0G"), "{t}");
                assert!(t.contains(" 0.1G"), "{t}");
            }
        }
    }

    // --------------------------------------------------------------------------------------------
    // A disk ask is no bigger than what can be cleared now (SKEIN-1391).
    // --------------------------------------------------------------------------------------------

    /// skein's list for box `web-main`, written at minute 0.
    fn listing(items: Vec<CleanItem>) -> CleanList {
        CleanList {
            at: stamp(at(0)),
            box_root: "/fleet/web-main".into(),
            items,
            repo_cache: None,
        }
    }

    /// `web-main/tree/target`, `mib` big, with a build running in it now.
    fn running_target(mib: u64) -> CleanItem {
        CleanItem {
            live: true,
            ..item("web-main/tree/target", "build output", mib)
        }
    }

    fn disk_ask(s: &Signals) -> Ask {
        s.asks
            .iter()
            .find(|a| a.kind == "disk")
            .cloned()
            .expect("a disk ask")
    }

    /// `excess` MiB over a 10G allowance, with `items` on skein's list.
    fn over_with(minute: i64, excess: u64, items: Vec<CleanItem>) -> Reading {
        Reading {
            clean: Some(listing(items)),
            ..over_allowance(minute, excess)
        }
    }

    const PIP_HINT: &str = "`skein_clean` lists the caches and build output this box can remove, \
                            and removes them. The biggest is ~/.cache/pip (1.5G); to remove it \
                            now, call `skein_clean` with \
                            {\"paths\":[\"/fleet/web-main/home/.cache/pip\"]}.";

    /// **A box over its own share by more than it can clear is told it can ask the owner for
    /// more, and then where its ask stands; one that can clear enough is not** (SKEIN-1425). The
    /// signal file carries the ask and the host's answer.
    ///
    /// 2G over a 10G share with a 1.5G pip cache: clearing falls short, so the hold, the start line
    /// and the entering line name `skein_request_disk`; with the ask waiting they say so, and once
    /// denied, that. With a 3G cache, clearing is enough and the words are unchanged.
    ///
    /// What would make it fail: `with_disk_ask` adding the sentence whatever can be cleared (the
    /// 3G case), not adding it to the hold, or not writing `disk_ask` into the file.
    #[test]
    fn a_box_that_cannot_clear_enough_is_told_it_can_ask_for_more() {
        let short = || {
            next(
                None,
                &over_with(0, 2048, vec![item("~/.cache/pip", "download cache", 1536)]),
            )
        };
        let told = with_disk_ask(short(), None);
        let ask = disk_ask(&told);
        let line = "Clearing will not bring this box under its share; to ask the owner for more, \
                    call `skein_request_disk`.";
        assert!(
            ask.hold
                .contains(&format!("{line} This one command was held")),
            "{}",
            ask.hold
        );
        assert!(ask
            .start
            .as_deref()
            .unwrap()
            .contains(&format!("{line} {}", words::WHERE)));
        assert!(
            ask.enter.as_deref().unwrap().ends_with(line),
            "{:?}",
            ask.enter
        );
        assert_eq!(told.disk_ask, None);

        let view = |state: &str| DiskAskView {
            id: "d-0000aaaa".into(),
            mb: 20 * 1024,
            state: state.into(),
            decided: String::new(),
        };
        let waiting = with_disk_ask(short(), Some(view("waiting")));
        assert!(
            disk_ask(&waiting).hold.contains(
                "Clearing will not bring this box under its share; its ask for 20.0G is with the \
                 owner."
            ),
            "{}",
            disk_ask(&waiting).hold
        );
        assert_eq!(waiting.disk_ask, Some(view("waiting")));
        let denied = with_disk_ask(short(), Some(view("denied")));
        assert!(
            disk_ask(&denied)
                .hold
                .contains("the owner denied its ask for 20.0G, so clear what it can."),
            "{}",
            disk_ask(&denied).hold
        );

        let enough = next(
            None,
            &over_with(0, 2048, vec![item("~/.cache/pip", "download cache", 3072)]),
        );
        let before = disk_ask(&enough);
        assert_eq!(
            disk_ask(&with_disk_ask(enough, None)),
            before,
            "told to ask when clearing is enough"
        );
    }

    /// **A build running now is not asked for: the ask shrinks to what can be cleared, and its
    /// words say how much is in the build** (SKEIN-1391).
    ///
    /// 2G over a 10G share, with a 3G target a build is running in and a 1.5G pip cache: the box
    /// is asked for 1.5G, held for it (more than a tenth of its share), and told that 3.0G is in a
    /// build running now. 0.5G over with a 0.2G pip cache: asked for 0.2G and told so, not held.
    ///
    /// What would make it fail: `removable_now` counting a live build as clearable (the 2G case is
    /// asked for 2.0G and the hint names the target). Watched: with `.filter(|i| !i.live)`
    /// removed, the amount read 2048 and this failed.
    #[test]
    fn a_running_build_is_not_asked_for_and_the_ask_shrinks_to_what_can_be_cleared() {
        let s = next(
            None,
            &over_with(
                0,
                2048,
                vec![
                    running_target(3072),
                    item("~/.cache/pip", "download cache", 1536),
                ],
            ),
        );
        let ask = disk_ask(&s);
        assert_eq!((ask.amount, ask.live_mb), (1536, 3072), "{ask:?}");
        assert_eq!(
            ask.hold,
            format!(
                "skein: clear 1.5G of storage you are not using before you continue. This box \
                 holds 12.0G against its 10.0G share of the fleet's disk (3.0G of it in a build \
                 running now); 1.5G can be cleared now. {PIP_HINT} This one command was held so \
                 you would see this; the next will run."
            )
        );
        assert_eq!(
            ask.start.as_deref(),
            Some(
                format!(
                    "skein: before you start, this box is asked to clear 1.5G of storage it is \
                     not using. It holds 12.0G against its 10.0G share of the fleet's disk (3.0G \
                     of it in a build running now); 1.5G can be cleared now. {PIP_HINT} \
                     `skein_resources` shows where this box stands."
                )
                .as_str()
            )
        );
        assert_eq!(
            ask.remind.as_deref(),
            Some(
                "skein: still asked of this box: clear 1.5G of storage (it holds 12.0G, 3.0G of \
                 it in a build running now)."
            )
        );
        assert_eq!(
            ask.enter.as_deref(),
            Some(
                format!(
                    "skein: this box holds 12.0G against its 10.0G share of the fleet's disk \
                     (3.0G of it in a build running now); clear the 1.5G that can be cleared now. \
                     {PIP_HINT}"
                )
                .as_str()
            )
        );

        let small = disk_ask(&next(
            None,
            &over_with(
                0,
                512,
                vec![
                    running_target(3072),
                    item("~/.cache/pip", "download cache", 204),
                ],
            ),
        ));
        assert_eq!(small.amount, 204, "{small:?}");
        assert!(
            small.hold.is_empty(),
            "0.2G of a 10G share held: {}",
            small.hold
        );
        assert!(
            small
                .start
                .as_deref()
                .is_some_and(|t| t.contains("0.2G can be cleared now")),
            "{small:?}"
        );
    }

    /// **A box is never held over space it cannot free** (SKEIN-1391): with everything it could
    /// clear in a running build, or nothing on skein's list at all, it is told where it stands and
    /// its commands run; and an ask shrunk below a tenth of its share is not held either.
    ///
    /// What would make each part fail:
    /// * *nothing to clear is never held* — two guards keep it told, and it takes both broken:
    ///   `nothing_to_clear` writing a hold, and `disk_holds` measuring the unshrunk ask. Watched:
    ///   with `nothing_to_clear`'s `hold` set to its line alone this passed, since an ask of 0
    ///   never holds; with that and the next sabotage together, the first assertion failed.
    /// * *the shrunk ask is what is measured* — `disk_holds` given the unshrunk `asked_of(&d).0`
    ///   (2G over with 0.5G clearable would hold). Watched: with exactly that, the third case
    ///   held and this failed.
    #[test]
    fn a_box_with_nothing_it_can_clear_is_told_and_never_held() {
        let only_live = disk_ask(&next(None, &over_with(0, 2048, vec![running_target(3072)])));
        assert!(only_live.hold.is_empty(), "held: {}", only_live.hold);
        assert_eq!((only_live.amount, only_live.live_mb), (0, 3072));
        let line = "skein: this box holds 12.0G against its 10.0G share (3.0G in a build running \
                    now); nothing can be cleared until it ends.";
        assert_eq!(only_live.enter.as_deref(), Some(line));
        assert_eq!(only_live.remind.as_deref(), Some(line));
        assert_eq!(only_live.start, Some(format!("{line} {}", words::WHERE)));

        // A list with nothing its agent may remove now, and nothing running.
        let none = disk_ask(&next(
            None,
            &over_with(
                0,
                2048,
                vec![item("~/.cache/skein/claude/x/old", "scratchpad", 4096)],
            ),
        ));
        assert!(none.hold.is_empty(), "held: {}", none.hold);
        assert_eq!((none.amount, none.live_mb), (0, 0));
        assert_eq!(
            none.enter.as_deref(),
            Some(
                "skein: this box holds 12.0G against its 10.0G share of the fleet's disk; nothing \
                 skein lists can be cleared now."
            )
        );

        let shrunk_small = disk_ask(&next(
            None,
            &over_with(
                0,
                2048,
                vec![
                    running_target(3072),
                    item("~/.cache/pip", "download cache", 512),
                ],
            ),
        ));
        assert_eq!(shrunk_small.amount, 512);
        assert!(
            shrunk_small.hold.is_empty(),
            "an ask of 0.5G on a 10G share held: {}",
            shrunk_small.hold
        );
        for a in [&only_live, &none, &shrunk_small] {
            assert!(a.start.is_some() && a.remind.is_some() && a.enter.is_some());
        }
    }

    /// **A box skein has not listed yet is asked as it always was**, and a listed box with no
    /// build running gets the owner's approved words with the shrunk figure (SKEIN-1391).
    ///
    /// `clean: None` means skein does not know what can be cleared, not that nothing can: the ask
    /// is its full size, and holds.
    ///
    /// What would make it fail: `room` reading a missing list as an empty one (the unlisted box is
    /// asked for nothing and never held). Watched: with `let clean = clean?;` made
    /// `let clean = clean.cloned().unwrap_or_default();` (and its uses borrowed), the first
    /// assertion read 0 and this failed.
    #[test]
    fn a_box_not_listed_yet_is_asked_in_full_and_held() {
        let unlisted = disk_ask(&next(None, &over_allowance(0, 2048)));
        assert_eq!((unlisted.amount, unlisted.live_mb), (2048, 0));
        assert_eq!(
            unlisted.hold,
            "skein: clear 2.0G of storage you are not using before you continue. This box holds \
             12.0G against its 10.0G share of the fleet's disk. `skein_clean` lists the caches \
             and build output this box can remove, and removes them. This one command was held \
             so you would see this; the next will run."
        );

        let listed = disk_ask(&next(
            None,
            &over_with(0, 2048, vec![item("~/.cache/pip", "download cache", 1536)]),
        ));
        assert_eq!((listed.amount, listed.live_mb), (1536, 0));
        assert_eq!(
            listed.hold,
            format!(
                "skein: clear 1.5G of storage you are not using before you continue. This box \
                 holds 12.0G against its 10.0G share of the fleet's disk. {PIP_HINT} This one \
                 command was held so you would see this; the next will run."
            )
        );
    }

    /// **A build ending is a new crossing**: the told ask becomes a held one under a new id, so
    /// the plugin holds one command then (SKEIN-1391); the hourly cap in `skein-resources` stands.
    ///
    /// What would make it fail: the crossing filter in `next`'s `stamp` matching whatever the
    /// hold (the id carries over and the plugin, having shown it, never holds). Watched: with
    /// `.filter(|b| b.hold.is_empty() == fresh.hold.is_empty())` made `.filter(|_| true)`, the
    /// ids were equal and this failed.
    #[test]
    fn a_build_ending_is_a_new_crossing_that_holds() {
        let ended = CleanItem {
            live: false,
            ..running_target(3072)
        };
        let files = run(&[
            over_with(0, 2048, vec![running_target(3072)]),
            over_with(1, 2048, vec![running_target(3072)]),
            over_with(2, 2048, vec![ended]),
        ]);
        let (a, b, c) = (
            disk_ask(&files[0]),
            disk_ask(&files[1]),
            disk_ask(&files[2]),
        );
        assert!(a.hold.is_empty() && b.hold.is_empty());
        assert_eq!(a.crossing_id, b.crossing_id, "a told band minted a new id");
        assert_eq!(c.amount, 2048, "{c:?}");
        assert!(!c.hold.is_empty(), "the build ended and nothing was held");
        assert_ne!(
            c.crossing_id, b.crossing_id,
            "the build ended under the told crossing's id, so it is never held"
        );
    }

    /// **A file written before `live` existed still reads**, every item as not running
    /// (SKEIN-1391), and an ask without `live_mb` reads as none.
    ///
    /// What would make it fail: `#[serde(default)]` removed from `CleanItem::live` (the whole file
    /// fails to parse, and `read` treats it as absent). Watched: with it removed, this failed.
    #[test]
    fn a_file_from_before_live_still_reads() {
        let old = serde_json::json!({
            "at": "2026-10-30T10:00:00Z",
            "asks": [{"kind": "disk", "why": "allowance", "amount": 2048,
                      "since": "2026-10-30T10:00:00Z", "crossing_id": "disk-1", "hold": "h"}],
            "clean": {"at": "2026-10-30T10:00:00Z", "box_root": "/fleet/web-main",
                      "items": [{"path": "/fleet/web-main/tree/target", "shown": "t",
                                 "kind": "build output", "bytes": 4096}]},
        });
        let s: Signals = serde_json::from_value(old).expect("an older file did not parse");
        assert!(!s.clean.unwrap().items[0].live);
        assert_eq!(s.asks[0].live_mb, 0);
    }

    /// **The fleet's asks shrink the same way** (SKEIN-1391), and the part the box cannot meet is
    /// not moved onto any other box: the share stays what `announce::cover` chose.
    ///
    /// What would make it fail: `shrunk` applied to the allowance alone, the fleet's share and
    /// every-box ask keeping their full size (2.3G and 9.4G). Watched: with `shrunk` returning
    /// `asked` for both fleet kinds, the amounts read 2355 and 9626 and this failed.
    #[test]
    fn the_fleets_asks_shrink_to_what_can_be_cleared_too() {
        let items = vec![
            running_target(3072),
            item("~/.cache/pip", "download cache", 1536),
        ];
        let fleet = |every_box: bool| Reading {
            fleet: FleetDisk::Asked(share(2, every_box)),
            disk_mb: Some(9626),
            disk_limit_mb: Some(10240),
            clean: Some(listing(items.clone())),
            ..reading(0, Some(calm()))
        };
        let share_ask = disk_ask(&next(None, &fleet(false)));
        assert_eq!(share_ask.why, "fleet");
        assert_eq!((share_ask.amount, share_ask.live_mb), (1536, 3072));
        assert_eq!(
            share_ask.hold,
            format!(
                "skein: clear 1.5G of storage you are not using before you continue — build \
                 outputs, caches, and any containers or volumes you started. The fleet's disk is \
                 4.1G over what it can spare, and this box holds 9.4G of it (3.0G in a build \
                 running now); 2 other boxes have been asked for the rest; 1.5G can be cleared \
                 now. {PIP_HINT} This one command was held so you would see this; the next will \
                 run."
            )
        );
        assert_eq!(
            share_ask.enter.as_deref(),
            Some(
                format!(
                    "skein: the fleet's disk is over its line; this box is asked to clear the \
                     1.5G it can clear now (3.0G of it is in a build running now). {PIP_HINT}"
                )
                .as_str()
            )
        );
        let every = disk_ask(&next(None, &fleet(true)));
        assert_eq!(every.why, "fleet-every-box");
        assert_eq!((every.amount, every.live_mb), (1536, 3072));
        assert!(
            every
                .hold
                .contains("; every box has been asked; 1.5G can be cleared now."),
            "{}",
            every.hold
        );
        if let FleetDisk::Asked(s) = &fleet(false).fleet {
            assert_eq!(s.free, 2355, "the fleet's share itself was changed");
        }

        let nothing = disk_ask(&next(
            None,
            &Reading {
                clean: Some(listing(vec![running_target(3072)])),
                ..fleet(false)
            },
        ));
        assert!(nothing.hold.is_empty());
        assert_eq!(
            nothing.enter.as_deref(),
            Some(
                "skein: the fleet's disk is 4.1G over what it can spare and this box holds 9.4G \
                 of it (3.0G in a build running now); nothing can be cleared until it ends."
            )
        );
    }

    // --------------------------------------------------------------------------------------------
    // The plugin's hooks, run as a box runs them: the shipped script, fed a file `write_box` wrote.
    // --------------------------------------------------------------------------------------------

    /// A box: its state directory, its home, and the plugin's script as the build carries it.
    struct Hooked {
        _dir: crate::testutil::TempDir,
        state: PathBuf,
        home: PathBuf,
        script: PathBuf,
    }

    fn hooked() -> Hooked {
        let dir = crate::testutil::tempdir();
        let root: &Path = dir.as_ref();
        let (state, home) = (root.join("state"), root.join("home"));
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        // The bytes `fleet::install_launcher` puts in `.skein/plugin`, not a copy of them.
        let body = crate::runtime::PLUGIN_FILES
            .iter()
            .find(|(rel, _)| *rel == "bin/skein-resources")
            .expect("the plugin ships no hook script")
            .1;
        let script = root.join("skein-resources");
        std::fs::write(&script, body).unwrap();
        Hooked {
            _dir: dir,
            state,
            home,
            script,
        }
    }

    impl Hooked {
        /// Run one hook or a monitor pass, and return its stdout.
        fn run(&self, args: &[&str], stdin: &str, xdg: Option<&Path>) -> String {
            use std::io::Write;
            let mut cmd = std::process::Command::new("python3");
            cmd.arg(&self.script)
                .args(args)
                .env("SKEIN_STATE", &self.state)
                .env("HOME", &self.home)
                .env_remove("XDG_STATE_HOME")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if let Some(x) = xdg {
                cmd.env("XDG_STATE_HOME", x);
            }
            let mut child = cmd.spawn().expect("python3");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "the hook failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        }

        fn tool(&self, session: &str) -> Option<String> {
            let out = self.run(
                &["pre-tool-use"],
                &format!(r#"{{"session_id":"{session}","tool_name":"Bash"}}"#),
                None,
            );
            match out.trim() {
                "" => None,
                said => {
                    let v: serde_json::Value = serde_json::from_str(said).unwrap();
                    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
                    Some(
                        v["hookSpecificOutput"]["permissionDecisionReason"]
                            .as_str()
                            .unwrap()
                            .to_string(),
                    )
                }
            }
        }

        fn context(&self, event: &str, session: &str) -> Option<String> {
            let out = self.run(
                &[event],
                &format!(r#"{{"session_id":"{session}","prompt":"go on"}}"#),
                None,
            );
            match out.trim() {
                "" => None,
                said => {
                    let v: serde_json::Value = serde_json::from_str(said).unwrap();
                    Some(
                        v["hookSpecificOutput"]["additionalContext"]
                            .as_str()
                            .unwrap()
                            .to_string(),
                    )
                }
            }
        }

        fn write(&self, secs: i64, cg: Cgroup, fleet: FleetDisk) -> Signals {
            write_box(
                &self.state,
                &Reading {
                    at: Utc::now() + chrono::Duration::seconds(secs),
                    disk_mb: Some(9626),
                    disk_limit_mb: None,
                    fleet,
                    cgroup: Some(cg),
                    clean: None,
                },
            )
            .unwrap()
        }
    }

    fn with_pids(n: u64) -> Cgroup {
        Cgroup { pids: n, ..calm() }
    }

    /// **One held call per crossing: deny, then allow, then allow, and deny again for a new id.**
    ///
    /// The hook is the shipped script and the file is one `write_box` wrote, so the words that
    /// reach the agent are the words the Rust side rendered.
    ///
    /// What would make each assertion fail:
    /// * *the second call runs* — the hook does not record the id it held for (drop the `record`
    ///   call from the PreToolUse hook), so every call is held.
    /// * *a new crossing is held again* — the hook records something other than the crossing id
    ///   (the kind, say), so a second crossing of the same kind reads as shown.
    /// * *another session is held once too* — the record is not per session.
    #[test]
    fn a_crossing_holds_one_call_and_the_next_runs() {
        let h = hooked();
        let first = h.write(0, with_pids(7000), FleetDisk::NotAsked { over_by: 0 });
        assert_eq!(kinds(&first), vec!["pids"]);

        assert_eq!(
            h.tool("s1").as_deref(),
            Some(first.asks[0].hold.as_str()),
            "the crossing did not hold the call, or held it with other words"
        );
        assert_eq!(h.tool("s1"), None, "the next call was held too");
        assert_eq!(h.tool("s1"), None, "and the one after");

        // Back under, then over again: a new crossing.
        h.write(1, with_pids(300), FleetDisk::NotAsked { over_by: 0 });
        assert_eq!(h.tool("s1"), None, "nothing is asked, so nothing is held");
        let again = h.write(2, with_pids(7100), FleetDisk::NotAsked { over_by: 0 });
        assert_ne!(again.asks[0].crossing_id, first.asks[0].crossing_id);
        assert!(
            h.tool("s1").is_some(),
            "a second crossing was not held, so it read as already shown"
        );
        assert_eq!(h.tool("s1"), None);

        assert!(
            h.tool("s2").is_some(),
            "a new session was not held for a crossing it has never been shown"
        );
    }

    /// **Several new asks hold one call together, and the next call runs** (owner-approved
    /// behaviour, 2026-09-24).
    ///
    /// Each hold text ends "the next will run", so that must be true however many asks arrive
    /// at once.
    ///
    /// What would make each assertion fail:
    /// * *one call carries both* — the hook holds for only the first unshown ask, or leaves the
    ///   others out of its reason.
    /// * *the next call runs* — the hook records only some of the ids it held for (for example
    ///   only the first), so the one left over holds the next call too.
    #[test]
    fn several_new_asks_hold_one_call_together_and_the_next_runs() {
        let h = hooked();
        let file = h.write(
            0,
            with_pids(7000),
            FleetDisk::Asked(Share {
                over_by: 4198,
                free: 2355,
                holds: 9626,
                others: 2,
                every_box: false,
            }),
        );
        assert_eq!(kinds(&file), vec!["disk", "pids"]);

        let held = h.tool("s1").expect("two new asks did not hold the call");
        for a in &file.asks {
            assert!(
                held.contains(a.hold.as_str()),
                "the held call left out the {} ask: {held}",
                a.kind
            );
        }
        assert_eq!(
            h.tool("s1"),
            None,
            "the call after a held one was held again, so \"the next will run\" was false"
        );
    }

    /// **The SessionStart line counts as shown** (owner, SKEIN-1055), and only where there is one.
    ///
    /// * A disk share ask has an approved start line: the session is told at start and its first
    ///   command runs. Fails if the SessionStart hook does not record what it printed.
    /// * An ask with no start line — which skein no longer writes (see
    ///   [`every_live_ask_has_a_start_line`]), so it is put in the file by hand here — prints
    ///   nothing at start, so the first command is held. Fails if the SessionStart hook records
    ///   ids it did not print.
    #[test]
    fn the_start_line_counts_as_shown_and_nothing_else_does() {
        let h = hooked();
        let disk = h.write(
            0,
            calm(),
            FleetDisk::Asked(Share {
                over_by: 4198,
                free: 2355,
                holds: 9626,
                others: 2,
                every_box: false,
            }),
        );
        assert_eq!(
            h.context("session-start", "s1").as_deref(),
            disk.asks[0].start.as_deref()
        );
        assert_eq!(
            h.tool("s1"),
            None,
            "a session told at start was held for the same crossing"
        );
        // Told at start is the hour's disk warning (SKEIN-1335), so the first prompt adds nothing.
        assert_eq!(h.context("prompt", "s1"), None);
        assert_eq!(
            h.context("prompt", "s2").as_deref(),
            Some("skein: still asked of this box: clear 2.3G of storage (it holds 9.4G).")
        );

        let g = hooked();
        let mut file = g.write(0, with_pids(7000), FleetDisk::NotAsked { over_by: 0 });
        file.asks[0].start = None;
        file.asks[0].remind = None;
        std::fs::write(signal_path(&g.state), serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(g.context("session-start", "s1"), None);
        assert!(
            g.tool("s1").is_some(),
            "an ask with no start line was recorded as shown at start"
        );
        assert_eq!(g.context("prompt", "s1"), None);
    }

    /// **No CPU reading ever produces a hold**, and neither does a kind the hook does not know.
    ///
    /// * A box using every core: nothing in its file asks, so nothing holds.
    /// * A file claiming a `cpu` ask: the hook holds only for disk, memory and pids. Fails if the
    ///   kind filter in the hook is dropped.
    #[test]
    fn no_cpu_reading_ever_holds_a_call() {
        let h = hooked();
        let busy = |secs: i64| Cgroup {
            usage_usec: (secs as u64 + 60) * 64 * 1_000_000,
            ..calm()
        };
        h.write(0, busy(0), FleetDisk::NotAsked { over_by: 0 });
        let file = h.write(60, busy(60), FleetDisk::NotAsked { over_by: 0 });
        assert!(file.cpu.cores.unwrap_or(0.0) > 32.0, "{:?}", file.cpu);
        assert_eq!(h.tool("s1"), None);

        let forged = Signals {
            asks: vec![Ask {
                kind: "cpu".into(),
                crossing_id: "cpu-1".into(),
                hold: "held for cpu".into(),
                ..Default::default()
            }],
            ..file
        };
        std::fs::write(signal_path(&h.state), serde_json::to_vec(&forged).unwrap()).unwrap();
        assert_eq!(h.tool("s1"), None, "a cpu ask held a call");
    }

    /// **Fail open**: a file skein stopped refreshing asks nothing, and a hold that cannot be
    /// recorded is not made.
    ///
    /// * Stale: fails if the hook's five-minute staleness check is dropped.
    /// * Unrecordable: the state home is a file, so the record cannot be written. Fails if the
    ///   hook denies before knowing the record landed, which would hold every call for ever.
    #[test]
    fn a_stale_file_or_an_unrecordable_hold_holds_nothing() {
        let h = hooked();
        h.write(-600, with_pids(7000), FleetDisk::NotAsked { over_by: 0 });
        assert_eq!(h.tool("s1"), None, "a file ten minutes old held a call");

        h.write(0, with_pids(7000), FleetDisk::NotAsked { over_by: 0 });
        let blocked = h.home.join("not-a-dir");
        std::fs::write(&blocked, "").unwrap();
        let out = h.run(
            &["pre-tool-use"],
            r#"{"session_id":"s1","tool_name":"Bash"}"#,
            Some(&blocked),
        );
        assert_eq!(out.trim(), "", "a hold it could not record was made anyway");
    }

    /// **The monitor prints one line when a band changes, either way, and nothing otherwise.**
    ///
    /// Driven one pass at a time, with the last look kept in a file, so no timing is involved.
    /// Fails if the first look prints an ask already standing (SessionStart told the session), if
    /// a band that holds prints again, or if leaving prints nothing.
    #[test]
    fn the_monitor_says_one_line_per_band_change() {
        let h = hooked();
        let keep = h.home.join("monitor.json");
        let keep = keep.to_str().unwrap();
        let pass = || h.run(&["monitor", "--passes", "1", "--state", keep], "", None);

        // A session that starts with an ask standing: SessionStart told it, so the monitor's first
        // look says nothing either.
        let g = hooked();
        g.write(0, with_pids(7000), FleetDisk::NotAsked { over_by: 0 });
        let first = g.home.join("monitor.json");
        let first = first.to_str().unwrap();
        assert_eq!(
            g.run(&["monitor", "--passes", "1", "--state", first], "", None),
            "",
            "the first look repeated an ask the session was already told at start"
        );

        h.write(0, calm(), FleetDisk::NotAsked { over_by: 0 });
        assert_eq!(pass(), "", "the first look spoke");
        assert_eq!(pass(), "", "nothing changed and it spoke");

        h.write(1, with_pids(6410), FleetDisk::NotAsked { over_by: 0 });
        assert_eq!(
            pass(),
            "skein: this box is running 6,410 of its 8,192 processes; end the ones you no longer \
             need.\n"
        );
        h.write(2, with_pids(6500), FleetDisk::NotAsked { over_by: 0 });
        assert_eq!(pass(), "", "a band that held was announced again");

        h.write(3, with_pids(1204), FleetDisk::NotAsked { over_by: 0 });
        assert_eq!(
            pass(),
            "skein: this box is back to 1,204 processes. Nothing more is asked.\n"
        );
        assert_eq!(pass(), "");
    }

    impl Hooked {
        /// Write a file for a box `excess` MiB over a 10G allowance.
        fn over(&self, secs: i64, excess: u64) -> Signals {
            write_box(
                &self.state,
                &Reading {
                    at: Utc::now() + chrono::Duration::seconds(secs),
                    disk_mb: Some(10240 + excess),
                    disk_limit_mb: Some(10240),
                    fleet: FleetDisk::NotAsked { over_by: 0 },
                    cgroup: Some(calm()),
                    clean: None,
                },
            )
            .unwrap()
        }

        /// Make this session's last disk warning and hold an hour and a second old.
        fn an_hour_passes(&self, session: &str) {
            let path = self
                .home
                .join(".local/state/skein/shown")
                .join(format!("{session}.disk"));
            let raw = std::fs::read_to_string(&path).expect("no disk record was written");
            let mut said: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let past = Utc::now().timestamp() as f64 - 3601.0;
            for key in ["warned", "held"] {
                if said.get(key).is_some() {
                    said[key] = serde_json::json!(past);
                }
            }
            std::fs::write(&path, said.to_string()).unwrap();
        }
    }

    /// **A box a little over its share is told, at most once an hour, and never held** (owner,
    /// SKEIN-1335).
    ///
    /// What would make each assertion fail:
    /// * *no call is held* — the plugin holds for an ask with no hold words, or skein writes hold
    ///   words for an overage under a tenth of the share.
    /// * *the second prompt says nothing* — the reminder is not limited to once an hour, which is
    ///   the every-prompt reminder this replaced.
    /// * *it is said again after the hour* — the limit never expires, so a box over all day is told
    ///   once and never again.
    #[test]
    fn a_little_over_is_told_hourly_and_never_held() {
        let h = hooked();
        let file = h.over(0, 200);
        assert!(file.asks[0].hold.is_empty());
        assert_eq!(h.tool("s1"), None, "a box 0.2G over its share was held");

        let told = h
            .context("prompt", "s1")
            .expect("a box over its share was not told");
        assert_eq!(told, file.asks[0].remind.clone().unwrap());
        assert_eq!(
            h.context("prompt", "s1"),
            None,
            "it was told again within the hour"
        );
        h.an_hour_passes("s1");
        assert_eq!(
            h.context("prompt", "s1"),
            Some(told),
            "an hour later and still over, it was not told again"
        );
        assert_eq!(h.tool("s1"), None);
    }

    /// **A box far over its share is held, at most once an hour, however often its figure crosses
    /// back and forth** (owner, SKEIN-1335). One transcript in the sweep was held 229 times: every
    /// build pushed the box over and every clean brought it back, and each crossing held a call.
    ///
    /// What would make each assertion fail:
    /// * *the first call is held* — the threshold suppresses holds that should be made.
    /// * *a new crossing within the hour runs* — fails if the hourly limit on disk holds is dropped
    ///   from the PreToolUse hook: the new crossing id is unshown, so it would hold.
    /// * *after the hour it is held* — the limit never expires, or the waiting crossing was recorded
    ///   as shown when it was not shown.
    /// * *the reminder waits too* — a hold counts as the hour's warning.
    #[test]
    fn far_over_is_held_at_most_hourly_however_often_it_crosses() {
        let h = hooked();
        let first = h.over(0, 2048);
        assert_eq!(h.tool("s1").as_deref(), Some(first.asks[0].hold.as_str()));
        assert_eq!(h.tool("s1"), None);
        assert_eq!(
            h.context("prompt", "s1"),
            None,
            "the reminder came straight after the hold"
        );

        h.over(1, 0);
        let again = h.over(2, 2100);
        assert_ne!(again.asks[0].crossing_id, first.asks[0].crossing_id);
        assert_eq!(
            h.tool("s1"),
            None,
            "a second crossing within the hour was held"
        );

        h.an_hour_passes("s1");
        assert!(
            h.tool("s1").is_some(),
            "still far over an hour later, and the crossing never held"
        );
        assert_eq!(h.tool("s1"), None);
    }

    /// **The monitor announces the disk band at most once an hour**, and says it cleared only to a
    /// session it last told it was asked.
    ///
    /// Fails if the monitor's hourly limit on disk is dropped (the second entering line prints), or
    /// if a cleared line is printed after an entering line that was not (a "nothing more is asked"
    /// about an ask the session was never told of).
    #[test]
    fn the_monitor_says_the_disk_band_at_most_hourly() {
        let h = hooked();
        let keep = h.home.join("monitor.json");
        let keep_s = keep.to_str().unwrap();
        let pass = || h.run(&["monitor", "--passes", "1", "--state", keep_s], "", None);

        h.over(0, 0);
        assert_eq!(pass(), "");
        let entered = h.over(1, 2048);
        assert_eq!(
            pass(),
            format!("{}\n", entered.asks[0].enter.clone().unwrap())
        );
        h.over(2, 0);
        assert_eq!(pass(), format!("{}\n", words::DISK_CLEARED));
        h.over(3, 2048);
        assert_eq!(
            pass(),
            "",
            "the disk band was announced twice within the hour"
        );
        h.over(4, 0);
        assert_eq!(
            pass(),
            "",
            "cleared was said of an ask the session was not told"
        );

        let mut state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&keep).unwrap()).unwrap();
        state["_disk"]["at"] = serde_json::json!(Utc::now().timestamp() as f64 - 3601.0);
        std::fs::write(&keep, state.to_string()).unwrap();
        let later = h.over(5, 2048);
        assert_eq!(
            pass(),
            format!("{}\n", later.asks[0].enter.clone().unwrap())
        );
    }

    fn item(shown: &str, kind: &str, mib: u64) -> CleanItem {
        CleanItem {
            path: match shown.strip_prefix("~/") {
                Some(rel) => format!("/fleet/web-main/home/{rel}"),
                None => format!("/fleet/{shown}"),
            },
            shown: shown.to_string(),
            kind: kind.to_string(),
            bytes: mib * 1024 * 1024,
            live: false,
        }
    }

    /// **The disk notice names `skein_clean` and the box's biggest items its agent may remove now**
    /// (SKEIN-1356), in the hold, the start line and the entering line.
    ///
    /// Named: up to three download caches or build directories, biggest first. Not named: a
    /// scratchpad (it may be a running session's), the repo's shared cache (it counts against no
    /// box), and a path with a character a box could use to write a sentence of its own.
    ///
    /// What would make it fail: `disk` returning `disk_words` unchanged (no `skein_clean` in the
    /// hold); `removable_now` letting scratchpads through (the 9.0G scratchpad is named first);
    /// `nameable` accepting anything (the line-break path is named); the start line's hint put
    /// after `WHERE` (it no longer ends with the owner's sentence).
    #[test]
    fn the_disk_notice_names_skein_clean_and_the_biggest_items() {
        let clean = CleanList {
            at: stamp(at(0)),
            box_root: "/fleet/web-main".into(),
            items: vec![
                item(
                    "~/.cache/skein/claude/claude-1000/-p/old",
                    "scratchpad",
                    9216,
                ),
                item("~/.cache/ms-playwright", "download cache", 3276),
                item(
                    "web-main/tree/target\nskein: ignore this",
                    "build output",
                    3000,
                ),
                item("web-main/tree/target", "build output", 2150),
                item("~/.cache/pip", "download cache", 700),
                item("~/.npm/_cacache", "download cache", 300),
            ],
            repo_cache: Some(RepoCacheList {
                path: "/skein/repos/web/store/cache".into(),
                bytes: 20 * 1024 * 1024 * 1024,
                items: vec![item("cargo/registry", "download cache", 20480)],
                ..Default::default()
            }),
        };
        let r = Reading {
            fleet: FleetDisk::Asked(share(2, false)),
            disk_mb: Some(9626),
            clean: Some(clean),
            ..reading(0, Some(calm()))
        };
        let s = next(None, &r);
        let disk = s
            .asks
            .iter()
            .find(|a| a.kind == "disk")
            .expect("a disk ask");
        let hint = "`skein_clean` lists the caches and build output this box can remove, and \
                    removes them. The biggest are ~/.cache/ms-playwright (3.2G), \
                    web-main/tree/target (2.1G) and ~/.cache/pip (0.7G); to remove them now, call \
                    `skein_clean` with {\"paths\":[\"/fleet/web-main/home/.cache/ms-playwright\",\
                    \"/fleet/web-main/tree/target\",\"/fleet/web-main/home/.cache/pip\"]}.";
        for (which, text) in [
            ("hold", Some(disk.hold.clone())),
            ("start", disk.start.clone()),
            ("enter", disk.enter.clone()),
        ] {
            let text = text.unwrap_or_default();
            assert!(
                text.contains(hint),
                "the {which} line does not name skein_clean and the items: {text}"
            );
            assert!(
                !text.contains("ignore this"),
                "a box-chosen name reached the {which} line: {text}"
            );
        }
        assert!(
            disk.start
                .as_deref()
                .is_some_and(|t| t.ends_with(words::WHERE)),
            "the start line no longer ends with the owner's sentence: {:?}",
            disk.start
        );
        assert_eq!(
            s.clean, r.clean,
            "the file does not carry the list the plugin reads"
        );

        // With nothing removable, the notice still names the tool.
        let bare = next(
            None,
            &Reading {
                clean: None,
                ..r.clone()
            },
        );
        let hold = &bare.asks.iter().find(|a| a.kind == "disk").unwrap().hold;
        assert!(
            hold.contains(words::CLEAN) && !hold.contains("The biggest"),
            "{hold}"
        );
    }

    /// A fleet with repo `web` (managed store, shared cache) and its boxes `web-main` and
    /// `web-two`. Returns the pins.
    fn repo_fleet(dir: &Path) -> crate::testutil::EnvPins {
        let home = dir.join("skein");
        let root = dir.join("fleet");
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_FLEET_ROOT", &root);
        pins.set("SKEIN_HOME", &home);
        let store = home.join("repos/web/store/data");
        std::fs::create_dir_all(&store).unwrap();
        crate::repos::save_repos(&[crate::repos::Repo {
            id: "web".into(),
            store: store.display().to_string(),
            ..Default::default()
        }])
        .unwrap();
        let put = |p: PathBuf| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, vec![7u8; 16 * 1024]).unwrap();
        };
        put(home.join("repos/web/store/cache/pip/http/x"));
        put(home.join("repos/web/store/cache/cargo/credentials.toml"));
        put(root.join("web-main/home/.cache/pip/http/x"));
        std::fs::create_dir_all(root.join("web-two/home")).unwrap();
        pins
    }

    /// **The fact the repo cache's clean turns on is read every tick, and "when it was last true"
    /// does not move while another box of the repo runs or cannot be told about** (SKEIN-1356).
    ///
    /// The items are walked at most every [`CLEAN_LIST_EVERY`]; the other boxes are asked every
    /// tick, and the box itself never counts.
    ///
    /// What would make it fail: stamping `none_running_at` whatever the other boxes are doing
    /// (the plugin would remove the cache under a running sibling); counting the box itself
    /// (`others_running` would be 1 with only `web-main` running); carrying the sibling counts with
    /// the items (the second tick would still say 1); or re-walking every tick (`at` would move).
    /// Watched: with the `if running.is_empty() && unknown.is_empty()` guard made `if true`, the
    /// first tick stamped `none_running_at` while `web-two` ran, and this failed.
    #[test]
    fn the_sibling_fact_is_read_every_tick_and_only_stamped_when_true() {
        let _lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let _pins = repo_fleet(dir.as_ref());
        let live = |pairs: &[(&str, bool)]| -> std::collections::HashMap<String, bool> {
            pairs.iter().map(|(n, b)| (n.to_string(), *b)).collect()
        };
        let mut memo = BTreeMap::new();
        let first = clean_list(
            "web-main",
            None,
            &live(&[("web-main", true), ("web-two", true)]),
            at(0),
            &mut memo,
        )
        .expect("a box with a directory is listed");
        let shown: Vec<&str> = first.items.iter().map(|i| i.shown.as_str()).collect();
        assert_eq!(shown, vec!["~/.cache/pip"]);
        let rc = first.repo_cache.clone().expect("web has a shared cache");
        let cached: Vec<&str> = rc.items.iter().map(|i| i.shown.as_str()).collect();
        assert_eq!(cached, vec!["pip"], "cargo's credentials are never listed");
        assert_eq!((rc.others_running, rc.others_unknown), (1, 0));
        assert_eq!(rc.none_running_at, None);

        let mut memo = BTreeMap::new();
        let second = clean_list(
            "web-main",
            Some(&first),
            &live(&[("web-main", true), ("web-two", false)]),
            at(1),
            &mut memo,
        )
        .unwrap();
        let rc = second.repo_cache.clone().unwrap();
        assert_eq!(
            second.at, first.at,
            "the items were walked again inside the period"
        );
        assert_eq!((rc.others_running, rc.others_unknown), (0, 0), "{rc:?}");
        assert_eq!(rc.none_running_at, Some(stamp(at(1))));

        for (n, liveness) in [
            (2, live(&[("web-main", true), ("web-two", true)])),
            (3, live(&[("web-main", true)])),
        ] {
            let later = clean_list("web-main", Some(&second), &liveness, at(n), &mut memo).unwrap();
            let rc = later.repo_cache.unwrap();
            assert_eq!(
                rc.others_running + rc.others_unknown,
                1,
                "minute {n}: {rc:?}"
            );
            assert_eq!(
                rc.none_running_at,
                Some(stamp(at(1))),
                "minute {n}: the last time no other box ran moved while one ran or was unknown"
            );
        }
        let relisted = clean_list(
            "web-main",
            Some(&second),
            &live(&[("web-two", false)]),
            at(6),
            &mut memo,
        )
        .unwrap();
        assert_eq!(
            relisted.at,
            stamp(at(6)),
            "the items were never walked again"
        );
    }

    /// One assistant turn as the CLI writes it, `input` and `output` tokens on `claude-opus-5`.
    fn turn_line(id: &str, ts: &str, input: u64, output: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": ts,
            "requestId": id,
            "message": {
                "id": id,
                "model": "claude-opus-5",
                "content": [{"type": "text", "text": "a message body"}],
                "usage": {"input_tokens": input, "output_tokens": output}
            }
        })
        .to_string()
    }

    /// A fleet of three boxes read by `usage::refresh`, today in UTC: `alpha` with a conversation
    /// and one subagent's transcript, `beta` with a conversation, `gamma` with nothing. Returns the
    /// pins and the reading.
    fn spend_fleet(dir: &Path) -> (crate::testutil::EnvPins, crate::usage::UsageReport) {
        let home = dir.join("skein");
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_HOME", &home);
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"));
        pins.set("HOME", dir.join("bare"));
        let ts = format!("{}T00:00:01Z", Utc::now().format("%Y-%m-%d"));
        let put = |rel: &str, body: String| {
            let p = home.join("boxes").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        put(
            "alpha/claude-projects/-w/s1.jsonl",
            turn_line("m1", &ts, 1000, 2000),
        );
        put(
            "alpha/claude-projects/-w/s1/subagents/agent-a.jsonl",
            turn_line("m2", &ts, 3000, 4000),
        );
        put(
            "beta/claude-projects/-w/s2.jsonl",
            turn_line("m3", &ts, 10_000, 0),
        );
        std::fs::create_dir_all(home.join("boxes/gamma")).unwrap();
        let report = crate::usage::refresh().expect("the fixture fleet reads");
        (pins, report)
    }

    fn spend_of(name: &str) -> crate::usage::BoxSpend {
        let state = PathBuf::from(crate::fleet::box_state(name));
        let body = std::fs::read_to_string(usage_path(&state)).expect("a box's spend was written");
        serde_json::from_str(&body).unwrap()
    }

    /// **A box's file carries the cockpit's figure for that box, its subagents included**
    /// (SKEIN-1384).
    ///
    /// `alpha`'s day is $0.17 at Opus 5's rates, $0.115 of it in a subagent's transcript. Fails if
    /// the file is figured from anything but the cells the cockpit sums, and if subagent
    /// transcripts are dropped. Watched: with `walk_jsonl` made to skip a `subagents` directory, the
    /// file read $0.055 and this failed.
    #[test]
    fn a_boxs_spend_file_is_the_cockpits_figure_for_it() {
        let _lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let (_pins, report) = spend_fleet(dir.as_ref());
        assert_eq!(write_box_spend(&report), Ok(3));

        let alpha = spend_of("alpha");
        let cockpit = report.boxes.iter().find(|b| b.name == "alpha").unwrap();
        let day = report.daily.last().unwrap();
        assert!(
            (alpha.today.cost - 0.17).abs() < 1e-9,
            "a subagent's spend is missing: {alpha:?}"
        );
        assert_eq!(
            alpha.today.cost, cockpit.cost,
            "not the cockpit's box figure"
        );
        assert_eq!(
            alpha.today.cost, day.by_box["alpha"],
            "not the cockpit's day figure"
        );
        assert_eq!((alpha.today.input, alpha.today.output), (4000, 6000));
        assert_eq!(alpha.week, alpha.today, "the only day is today");
        assert_eq!(alpha.at, report.read_at);
        assert_eq!(alpha.prices_as_of, crate::usage::PRICES_AS_OF);
        assert!(!alpha.unpriced);
    }

    /// **A box's file holds that box's figure and no other's** (SKEIN-1384).
    ///
    /// Fails if a box is told the fleet's day, or another box's, or a box with no turn is given
    /// anything but zeros. Watched: with `box_spend` reading the day's `cost` in place of the box's
    /// own cell, `alpha` read $0.22, the fleet's day, and this failed.
    #[test]
    fn a_boxs_spend_file_holds_only_that_box() {
        let _lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let (_pins, report) = spend_fleet(dir.as_ref());
        write_box_spend(&report).unwrap();

        let (alpha, beta, gamma) = (spend_of("alpha"), spend_of("beta"), spend_of("gamma"));
        assert!((alpha.today.cost - 0.17).abs() < 1e-9, "{alpha:?}");
        assert!((beta.today.cost - 0.05).abs() < 1e-9, "{beta:?}");
        assert!((alpha.week.cost - 0.17).abs() < 1e-9, "{alpha:?}");
        assert_eq!(alpha.today.input, 4000, "{alpha:?}");
        assert_eq!(beta.today.input, 10_000, "{beta:?}");
        assert_eq!(gamma.today, crate::usage::SpendWindow::default());
        assert_eq!(gamma.week, crate::usage::SpendWindow::default());
    }

    /// **A box that used a model skein has no price for is told so, and only that box**
    /// (SKEIN-1384).
    ///
    /// `alpha` made a turn today on `claude-mythos-5-1`, which the price table leaves out; `beta`
    /// used only a priced model; `gamma`'s unpriced turn is eight days old, outside the week. Fails
    /// if the flag is dropped on its way to the file (`alpha` reads false), if it is the fleet's
    /// rather than the box's (`beta` reads true), or if a day outside the week raises it (`gamma`
    /// reads true). Watched: with `out.unpriced |= false && tokens.unpriced` in `box_spend`, `alpha`
    /// read false and this failed.
    #[test]
    fn a_box_that_used_an_unpriced_model_is_told_so_and_no_other_box_is() {
        let _lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir: &Path = dir.as_ref();
        let home = dir.join("skein");
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_HOME", &home);
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"));
        pins.set("HOME", dir.join("bare"));
        let now = Utc::now();
        let today = format!("{}T00:00:01Z", now.format("%Y-%m-%d"));
        let old = format!(
            "{}T00:00:01Z",
            (now - chrono::Duration::days(8)).format("%Y-%m-%d")
        );
        let unpriced = |id: &str, ts: &str| {
            turn_line(id, ts, 500, 500).replace("claude-opus-5", "claude-mythos-5-1")
        };
        assert!(crate::usage::price_of("claude-mythos-5-1").is_none());
        let put = |rel: &str, body: String| {
            let p = home.join("boxes").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        put(
            "alpha/claude-projects/-w/s1.jsonl",
            [turn_line("a1", &today, 1000, 0), unpriced("a2", &today)].join("\n"),
        );
        put(
            "beta/claude-projects/-w/s2.jsonl",
            turn_line("b1", &today, 1000, 0),
        );
        put(
            "gamma/claude-projects/-w/s3.jsonl",
            [turn_line("g1", &today, 1000, 0), unpriced("g2", &old)].join("\n"),
        );
        let report = crate::usage::refresh().expect("the fixture fleet reads");
        assert_eq!(
            report
                .unpriced
                .iter()
                .map(|u| u.model.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-mythos-5-1"],
            "the fixture's unpriced model was not read as unpriced"
        );
        write_box_spend(&report).unwrap();

        let (alpha, beta, gamma) = (spend_of("alpha"), spend_of("beta"), spend_of("gamma"));
        assert!(
            alpha.unpriced,
            "alpha used an unpriced model today: {alpha:?}"
        );
        assert!(!beta.unpriced, "beta used no unpriced model: {beta:?}");
        assert!(
            !gamma.unpriced,
            "gamma's unpriced turn is outside the week: {gamma:?}"
        );
        assert!(
            (alpha.today.cost - 0.005).abs() < 1e-9,
            "the unpriced turn is priced at nothing: {alpha:?}"
        );
        assert_eq!(alpha.today.input, 1500, "its tokens are still counted");
    }
}
