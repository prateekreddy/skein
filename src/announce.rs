//! What skein says **without being asked**.
//!
//! Every other check in this repository answers a question somebody put. `skein doctor` runs when
//! it is typed; the health report is computed when a browser tab asks for it. That is the right
//! shape for a diagnosis and the wrong shape for a wall you are walking towards, and on 2026-09-09
//! the difference cost an afternoon: the fleet reached 88% of 118 GB,
//! [`crate::health::disk_health`] had already computed exactly that verdict and already named the
//! three biggest consumers, and the owner found out by asking what was using the disk.
//! `src/bin/skein.rs` says in the code's own voice that this check is "the only warning before a
//! build dies half way through it" — and it was a warning nothing delivered.
//!
//! **So this module measures nothing.** The verdict already exists, it is already right, and it is
//! already tested. What was missing is a mouth: something that notices the fleet has *crossed* the
//! line and tells whoever is in a position to act, before their build dies in the middle.
//!
//! # Crossing, not reading
//!
//! The distinction is the whole module, and it is easy to lose. "The disk is over the line" is a
//! reading, and a thing that acts on every reading either says nothing useful (because it only
//! looks when asked) or says it every tick for ever (because the disk stays full for hours). A
//! *crossing* is a transition, and a transition needs a memory of where the fleet was last time —
//! [`Said`], on disk, so a restart of the server does not re-announce what it already announced,
//! and so a fleet that was already over the line when skein started is still told about once.
//!
//! [`crate::stream`] draws the same distinction for the board ("transitions rather than
//! snapshots") and for the same reason; this is that idea applied to a threshold.
//!
//! # It never deletes
//!
//! The announcement is a note. It carries [`crate::health::HealthCheck::fix`] — which is a command
//! somebody can copy — and it carries [`crate::health::HealthCheck::destructive`] as a sentence
//! saying so when the recipe deletes. Nothing here runs a recipe, and nothing here removes a byte.
//! That is architecture §2.4's rule for a destructive recipe, and it is the owner's stated rule for
//! this kind of UX: say what it costs, offer the safe step, keep the dangerous command copyable.
//!
//! # What is still the owner's to choose
//!
//! [`Policy`] is deliberately small and deliberately explicit, because who gets interrupted and how
//! often is a decision about somebody's attention rather than about code. Its default is the
//! owner's own answer to both halves — the fewest boxes that can clear the line, told again every
//! hour while it is still crossed — and every other arrangement is a different value of the same
//! two fields.

use crate::health::{DiskDemand, HealthCheck, Level};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Who hears it.
///
/// Both arms are real and both are one line to select. Which one is right is a judgement about
/// attention, not about correctness: one filesystem serves every box, so *any* box's build is the
/// one that dies — but the box holding 14 GB is the only one whose action changes the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// **As many of the biggest boxes as it takes to get back under the line, and no more.**
    ///
    /// The owner's rule, 2026-09-09 (SKEIN-730), and the case it was chosen against is the one it
    /// refuses to interrupt: *a box holding 2% beside a box holding 90% is not told*. "Always the
    /// top two" would have told it, and an agent told it is a problem when it is not is how a
    /// warning stops being read — which is the failure this whole module exists to leave behind.
    ///
    /// So the count is derived rather than chosen: [`crate::health::DiskDemand::over_by`] says how
    /// much has to go, the ranking says who is holding it, and the answer is the shortest prefix of
    /// that ranking which covers the overage. A box holding 90% covers it alone; two holding 45%
    /// each do not, so both are told.
    ///
    /// **There is no cap on how many, and the reason is not obvious from the rule.** The owner's,
    /// same day: *"maybe don't restrict to 2, go till you can have more free memory. Sometimes it
    /// is possible that 1 can't free anymore because it needs all of that actively rn."* Holding
    /// disk and being able to give it back are different things — a box mid-build needs every byte
    /// of what it is holding — so a set that covers the overage *on paper* is not a set that will
    /// actually clear it, and a cap on how many are asked is a cap on how likely the fleet is to
    /// come back under. The ranking is biggest-first, so this is still the fewest boxes that could
    /// possibly cover it; it is just not truncated to a number.
    ///
    /// What keeps that from being a spiral of interruptions is [`Policy::repeat_after`]: a box that
    /// was asked and could not comply is still holding the disk an hour later, so it is still in
    /// the ranking and still named, and the fleet re-ranks around whoever *did* free something
    /// rather than accumulating a wider and wider audience within one crossing.
    TheFewestThatCoverIt,
    /// Every box in the fleet, through the mailbox's own fan-out.
    EveryBox,
}

/// When it fires and who hears it — the part that is a decision about a person, not about code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub audience: Audience,
    /// Say it again after this long while the fleet is still over the line, or never repeat.
    ///
    /// `None` is once per crossing: the fleet has to come back under and go over again before
    /// anything is said a second time. That is the arrangement that cannot become noise, and it is
    /// also the one that says nothing at all to an agent that starts work an hour after the
    /// announcement — which is why the other arrangement is a field rather than a rewrite.
    pub repeat_after: Option<Duration>,
}

impl Default for Policy {
    /// **The boxes that can clear it, and again every hour while it is still over the line.** The
    /// owner chose both, 2026-09-09, having been shown what each costs.
    ///
    /// The audience is the smallest set of the biggest boxes that covers the overage, because they
    /// are the only agents whose action changes the number. He was told the price and took it:
    /// **the box that dies of `ENOSPC` is usually not one of them**, so the one about to be hurt is
    /// not the one being warned.
    ///
    /// The hour is the answer to the failure the once-per-crossing arrangement has, which is that
    /// it says nothing to an agent starting work after the announcement — and the fleet can sit
    /// over the line all day. An hour rather than five minutes because a day over the line is then
    /// ten interruptions rather than nearly three hundred, and three hundred is how a warning stops
    /// being read. That is the failure this module exists to leave behind, so it is the one the
    /// number is chosen against.
    fn default() -> Policy {
        Policy {
            audience: Audience::TheFewestThatCoverIt,
            repeat_after: Some(Duration::from_secs(60 * 60)),
        }
    }
}

/// What skein last said about the fleet's disk, remembered on disk.
///
/// **Every field is written from scratch by [`record`]**, which is what makes the lossy read in
/// [`said`] safe — the SKEIN-359 pattern is read-modify-write, and this is not one. A field added
/// here that `record` does not set is a field dropped on every announcement, and nothing would say
/// so.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    /// Was the fleet over the line the last time skein looked and could tell?
    ///
    /// `false` is also what "skein has never looked" reads as, and the two are deliberately the
    /// same state: in both, the next over-the-line reading is the first anybody has been told, which
    /// is the case the fleet was actually in on the day this was written.
    #[serde(default)]
    pub over: bool,
    /// RFC3339, when that was written. Empty means never.
    #[serde(default)]
    pub at: String,
}

/// Why nothing was said.
///
/// A reason and not a bool, because the three are not interchangeable to anyone debugging a silence
/// — and a silence is exactly the failure this module was written after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    /// There is room, and there was room last time too.
    RoomLeft,
    /// The disk could not be measured.
    NotMeasured,
    /// Over the line, and skein has already said so.
    AlreadySaid,
}

/// What this tick does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Say it.
    Announce,
    /// Say nothing, and why.
    Quiet(Quiet),
    /// It came back under the line. Nothing is said — a person does not need to be interrupted to
    /// be told a problem went away — but the record is cleared, so the *next* crossing is a
    /// crossing again rather than an `AlreadySaid`.
    Cleared,
}

/// One note and the box it went to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The box — the `to` of [`crate::mailbox::send_message`].
    pub to: String,
    /// Exactly the words that box's reader will see.
    pub body: String,
}

/// What one tick did, in a form a caller can print and a test can assert against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub step: Step,
    /// What was delivered, and to whom. Empty unless [`Step::Announce`].
    ///
    /// A note per box rather than one body and a list of recipients, because **what each box is
    /// asked to free is its own figure** — two boxes told about the same crossing are asked for
    /// different amounts, and a single `body` beside a list of names could only ever carry one of
    /// them.
    pub told: Vec<Note>,
}

impl Outcome {
    /// Just the names, for a caller that is reporting who was interrupted rather than what they
    /// were told.
    pub fn to(&self) -> Vec<&str> {
        self.told.iter().map(|n| n.to.as_str()).collect()
    }
}

/// Decide, from where the fleet is now and where it was last time skein spoke.
///
/// Pure: no clock of its own, no filesystem, no fleet. Everything it needs is an argument, so the
/// sequence that matters — under, over, still over, under, over again — can be driven end to end in
/// a test on a machine with no fleet, which is every machine this suite runs on.
///
/// **`Unknown` leaves the record alone**, and that is the subtle arm. [`crate::health::Level`]'s own
/// documentation says `Unknown` may never drive a doer; here it must also never drive an *undoer*.
/// Treating "could not measure" as "there is room" would clear the record, and the next successful
/// reading would announce a crossing that never happened — turning one unreadable tick into a
/// repeat of a warning somebody has already read.
pub fn step(level: Level, said: &Said, now: DateTime<Utc>, repeat_after: Option<Duration>) -> Step {
    match level {
        Level::Unknown => Step::Quiet(Quiet::NotMeasured),
        Level::Satisfied => match said.over {
            true => Step::Cleared,
            false => Step::Quiet(Quiet::RoomLeft),
        },
        Level::Unsatisfied if !said.over => Step::Announce,
        // Over the line and already said. Repeat only if asked to, and repeat when the stamp's own
        // timestamp cannot be read: an unparseable `at` is skein's bookkeeping being wrong, and the
        // direction to be wrong in is saying it twice rather than not at all.
        Level::Unsatisfied => match repeat_after {
            Some(d) => match elapsed(&said.at, now) {
                Some(since) if since < d => Step::Quiet(Quiet::AlreadySaid),
                Some(_) | None => Step::Announce,
            },
            None => Step::Quiet(Quiet::AlreadySaid),
        },
    }
}

/// How long since `at`, or `None` when it cannot be read or is in the future.
fn elapsed(at: &str, now: DateTime<Utc>) -> Option<Duration> {
    let then = DateTime::parse_from_rfc3339(at).ok()?;
    (now - then.with_timezone(&Utc)).to_std().ok()
}

/// What one box is asked for, in figures — the half of the note that is different for every reader.
///
/// **It names an amount and never a file.** The owner's rule, 2026-09-09: *"name the amount and let
/// it choose"*. The agent in the box knows what its own build output is worth and skein does not,
/// so a suggestion from here is a guess, and a wrong guess is worse than no guess — it is the
/// sentence that gets the rest of the note dismissed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The box being asked — the `to` of [`crate::mailbox::send_message`].
    pub to: String,
    /// MiB the fleet is over the line by. **Zero says nothing is being asked of anybody**: the
    /// verdict is a fault about the image store while the boxes' filesystem has room, or the fleet
    /// is sitting exactly on the line. Somebody is still told, because a fault nobody hears is
    /// what this module was written after — they are just not asked for a number.
    pub over_by: u64,
    /// MiB this box is asked to free. Its share of the overage, in proportion to what it is
    /// holding, and **never more than it holds**.
    pub free: u64,
    /// The other boxes told about this same crossing, and what each of them is asked for. Empty
    /// when this box is the only one told, which is the common case.
    pub others: Vec<(String, u64)>,
    /// MiB still over the line once every box told has freed everything asked of it.
    ///
    /// **Non-zero means the fleet's boxes are not, between them, holding enough to clear it**, and
    /// nothing weaker: with no cap on the audience, [`cover`] runs out of ranking before it runs
    /// out of overage only when it has asked *every* box for *everything* it has. So the rest of
    /// the space is not in a box at all — it is the substrate, the image store, or something
    /// stranded — and no amount of clearing inside one will reach it.
    ///
    /// Rare, and kept because a note that let its reader believe the line will clear would be lying
    /// to the one person acting on it. When this is not zero the note never promises the line comes
    /// back under, and says what emptying every box would still leave.
    pub short_by: u64,
}

/// Exactly what a reader sees.
///
/// Composed from the check rather than written here: `detail` is the diagnosis and `fix` is the way
/// out, both already computed and already tested, and a second copy of either in this file is a
/// second copy to drift. What this function adds is the three things a health row does not have to
/// say — why a *different* box's build is the one that dies, what this particular reader is being
/// asked for, and that skein is not going to clear anything itself.
pub fn compose(check: &HealthCheck, ask: &Ask) -> String {
    let mut body = format!(
        "the fleet's disk is filling up: {}\n\n  → {}\n\nOne filesystem serves every box, so a \
         build in any of them can die half way through — including one that is not taking the \
         space.",
        check.detail, check.fix
    );
    body.push_str(&asked_of_you(ask));
    if check.destructive {
        body.push_str(
            "\n\nThe command above deletes what Docker is keeping. It is printed for you to run; \
             skein will not run it.",
        );
    }
    body.push_str("\n\nNothing has been deleted. This is a note, not a sweep.");
    body
}

/// The paragraph that is this reader's and nobody else's: what it is being asked to clear, and the
/// fleet figure that is the reason.
///
/// **The ask leads and the reason follows**, which is the owner's steer of 2026-09-09: *"maybe we
/// can just say it has to clear unnecessary storage it is using since the fleet has only so much
/// left"*. So the first sentence is an instruction with an amount in it, and the second is why —
/// not the other way round, and with nothing in between explaining skein to its reader.
///
/// **Empty when nothing is being asked** ([`Ask::over_by`] of zero). A paragraph asking a box to
/// clear 0.0G would be worse than the silence it replaces, and the rest of the note — the verdict
/// and the recipe — is the part that still applies.
///
/// Figures through [`crate::health::gib`], which is the health row's own formatter, so the amount a
/// box is asked for and the amount the row says it is holding cannot be printed two ways.
fn asked_of_you(ask: &Ask) -> String {
    if ask.over_by == 0 {
        return String::new();
    }
    let gib = crate::health::gib;
    match ask.short_by {
        // The boxes told cover it between them, so the figure is this box's share of the overage
        // and the others are named with theirs — a reader that knows it is not being asked alone
        // clears its share rather than everything it has.
        0 => {
            let mut holders = vec!["you".to_string()];
            holders.extend(ask.others.iter().map(|(name, _)| name.clone()));
            let beside: Vec<String> = ask
                .others
                .iter()
                .enumerate()
                .map(|(nth, (name, mb))| match nth {
                    0 => format!("{name} has been asked for {}", gib(*mb)),
                    _ => format!("{name} for {}", gib(*mb)),
                })
                .collect();
            format!(
                "\n\nClear {} of storage you are not using. The fleet is {} over what it can \
                 spare and {} are holding more of it than any other box{}.",
                gib(ask.free),
                gib(ask.over_by),
                and_list(&holders),
                match beside.is_empty() {
                    true => String::new(),
                    false => format!(" — {}", and_list(&beside)),
                }
            )
        }
        // They do not — which, with no cap on the audience, means every box in the fleet has been
        // asked for everything it has and it is still not enough. The others are not enumerated
        // because "every box in it" *is* the enumeration, and the sentence never promises the line
        // comes back under, because it will not.
        short => format!(
            "\n\nClear what you can of the {} you are holding. The fleet is {} over what it can \
             spare, and emptying every box in it would still leave {} of that — the rest is not in \
             a box.",
            gib(ask.free),
            gib(ask.over_by),
            gib(short)
        ),
    }
}

/// `a`, `a and b`, `a, b and c` — a list a person reads rather than one a machine prints.
fn and_list(parts: &[String]) -> String {
    match parts.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Where the record lives — beside skein's other state, not in the volume's declared area.
///
/// It is a *recorded* fact about what skein has already said, in [`crate::stream`]'s sense: nothing
/// reconciles against it and losing it costs one repeated warning, not a wrong fleet.
fn said_path() -> PathBuf {
    crate::config::skein_home().join("disk-announced.json")
}

/// What skein last said. **An unreadable record reads as "never said"**, and that is the safe
/// direction here for the same reason it is in [`crate::stream::last_seen`]: the failure it causes
/// is one warning somebody has already read arriving a second time, and the failure of erring the
/// other way is the silence this module exists to end.
fn said() -> Said {
    std::fs::read_to_string(said_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Said>(&raw).ok())
        .unwrap_or_default()
}

/// Write the record whole. See [`Said`] for why "whole" is load-bearing.
fn record(state: &Said) -> Result<(), String> {
    let path = said_path();
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, dir, &bytes)
}

/// One tick: measure nothing new, decide whether the fleet has crossed, and if it has, say so.
///
/// This is the whole feature, and it is safe to call as often as a caller likes — the expensive
/// half is [`crate::fleet::fleet_disk_usage`], which is behind its own five-minute gate, and the
/// deciding half is a small file.
pub fn announce_fleet_disk(policy: &Policy) -> Result<Outcome, String> {
    announce_disk(
        &crate::health::disk_health(),
        policy,
        crate::health::disk_demand,
    )
}

/// How often skein LOOKS, which is never how often it speaks.
///
/// **Five minutes because that is [`crate::fleet::fleet_disk_usage`]'s own gate.** Ticking faster
/// would not read a fresher number, it would only ask more often for the cached one; the
/// announcement's own cadence is an hour and lives in [`Policy`], not here.
///
/// A `const` reached by one caller in this module rather than an argument to [`watch_fleet_disk`],
/// so there is no signature anywhere a caller could pass a different number through by accident —
/// the period is injectable exactly once, into the private [`watch_disk`] below, which nothing
/// outside this file can name.
const LOOK_EVERY: Duration = Duration::from_secs(300);

/// The loop the server runs. Never returns.
///
/// **Its own loop rather than work hung off a cockpit connection**: `crate::stream` starts its
/// producer at the first client and stops at the last — "a server nobody is watching does no work
/// at all" — and the day this exists for is the day the fleet reached 88% with nobody watching. A
/// warning that only renders when somebody has the cockpit open is not a warning.
pub async fn watch_fleet_disk() {
    watch_disk(
        LOOK_EVERY,
        Policy::default(),
        crate::health::disk_health,
        crate::health::disk_demand,
    )
    .await
}

/// [`watch_fleet_disk`] with the period, the policy and the two measurements as arguments — the
/// seam the timer is tested through.
///
/// The period is here and nowhere else: 300s is untestable by waiting, so a test that could not
/// choose it could only ever assert the loop was *written*, which is what
/// [`tests::the_server_is_what_runs_the_announcement`] does and says it cannot do more of. Private,
/// so the choosing stops at this file's edge.
///
/// **`spawn_blocking` and not a bare await**: behind [`crate::health::disk_health`] is a `du` of the
/// whole fleet root — measured at 383,606 files — and running that on a runtime thread would stall
/// every cockpit connection the server is holding.
///
/// A failed announcement is printed and the loop goes round again. There is nothing else to do with
/// it: the record is only written after a delivery, so the crossing is still a crossing on the next
/// tick, and a loop that exited here would take the warning with it.
async fn watch_disk<M, D>(every: Duration, policy: Policy, measure: M, demand: D)
where
    M: Fn() -> HealthCheck + Clone + Send + 'static,
    D: Fn() -> DiskDemand + Clone + Send + 'static,
{
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        let (policy, measure, demand) = (policy.clone(), measure.clone(), demand.clone());
        let said =
            tokio::task::spawn_blocking(move || announce_disk(&measure(), &policy, demand)).await;
        match said {
            Ok(Err(e)) => eprintln!("skein: disk announcement: {e}"),
            Err(e) => eprintln!("skein: disk announcement did not run: {e}"),
            Ok(Ok(_)) => {}
        }
    }
}

/// [`announce_fleet_disk`] over a verdict and a demand already in hand.
///
/// The two are arguments for the reason [`crate::health::disk_health`] splits the same way: a
/// verdict this function did not compute is a verdict a test can *choose*, which is what makes the
/// crossing — rather than the reading — the thing under test. The demand is the second of them, and
/// it is what lets a test put a 2% box beside a 90% one and watch which of them is left alone.
pub fn announce_disk(
    check: &HealthCheck,
    policy: &Policy,
    demand: impl FnOnce() -> DiskDemand,
) -> Result<Outcome, String> {
    let quiet = |step| Outcome {
        step,
        told: Vec::new(),
    };
    let now = Utc::now();
    let step = step(check.level, &said(), now, policy.repeat_after);
    match step {
        Step::Quiet(_) => Ok(quiet(step)),
        Step::Cleared => {
            record(&Said {
                over: false,
                at: now.to_rfc3339(),
            })?;
            Ok(quiet(step))
        }
        Step::Announce => {
            let told: Vec<Note> = audience(policy.audience, demand)?
                .into_iter()
                .map(|ask| Note {
                    body: compose(check, &ask),
                    to: ask.to,
                })
                .collect();
            for note in &told {
                crate::mailbox::send_message(&note.to, "fleet-disk", &note.body)?;
            }
            // **After the delivery, never before.** A record written first would mean a failed
            // send silenced the warning for the rest of that crossing — the exact shape of the
            // failure this module was written after, reintroduced one layer down.
            record(&Said {
                over: true,
                at: now.to_rfc3339(),
            })?;
            Ok(Outcome { step, told })
        }
    }
}

/// Turn an [`Audience`] into the boxes [`crate::mailbox::send_message`] is given, and what each of
/// them is asked for.
///
/// `EveryBox` is one `broadcast`, which that function fans out over the registry — rather than a
/// list assembled here, because a list assembled here is a second answer to "which boxes exist"
/// and the two would disagree the first time one of them was wrong. It asks for nothing: a fan-out
/// has no "this box", and a share of the overage is meaningless to a reader who may be holding
/// none of it. **It does not read the demand at all**, so choosing it costs no measurement.
fn audience(who: Audience, demand: impl FnOnce() -> DiskDemand) -> Result<Vec<Ask>, String> {
    match who {
        Audience::EveryBox => Ok(vec![Ask {
            to: "broadcast".to_string(),
            over_by: 0,
            free: 0,
            others: Vec::new(),
            short_by: 0,
        }]),
        Audience::TheFewestThatCoverIt => cover(demand()),
    }
}

/// The shortest prefix of the ranking that covers the overage, and what to ask each box in it for.
///
/// **A fold over a list skein already computes**, which is the whole reason the owner's rule is
/// implementable as stated: [`crate::health::DiskDemand`] carries the two numbers, the boxes arrive
/// biggest-first from [`crate::health::biggest_first`], and this walks them until they add up. No
/// threshold, no cap, no similarity, no second ordering — see [`Audience::TheFewestThatCoverIt`]
/// for why a cap would be a cap on the fleet coming back under rather than on interruption.
///
/// The prefix is never empty. **An overage of zero still tells the biggest box** — that is the
/// image store being full while the boxes' filesystem has room, and nobody is asked for a number,
/// but the fault is real and somebody in a position to look has to hear it. Telling nobody there
/// would be the silence this module exists to end, reintroduced as an arithmetic edge case.
///
/// It can also run out of ranking, and that is [`Ask::short_by`]: every box asked for everything it
/// has, and the fleet still over.
fn cover(demand: DiskDemand) -> Result<Vec<Ask>, String> {
    let mut chosen: Vec<(String, u64)> = Vec::new();
    let mut held: u64 = 0;
    for (name, mb) in demand.boxes {
        held += mb;
        chosen.push((name, mb));
        if held >= demand.over_by {
            break;
        }
    }
    if chosen.is_empty() {
        // Over the line with no box holding anything is a real state — the space is somewhere
        // else entirely — and there is nobody in a box to tell about it. Not an error, and not
        // a silent success either: it is reported, and the record is not written, so the moment
        // there is a box to tell it is still a crossing.
        return Err(
            "the fleet is over the line and no box is holding any of it, so there is nobody in \
             a box to tell — `skein doctor` has the figures"
                .into(),
        );
    }
    let share = |mine: u64| match held {
        0 => 0,
        _ => mine.min(demand.over_by.saturating_mul(mine).div_ceil(held)),
    };
    Ok(chosen
        .iter()
        .enumerate()
        .map(|(me, (name, _))| Ask {
            to: name.clone(),
            over_by: demand.over_by,
            free: share(chosen[me].1),
            others: chosen
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != me)
                .map(|(_, (name, mb))| (name.clone(), share(*mb)))
                .collect(),
            // What is left over the line once every box told has done everything asked of it —
            // zero unless the loop ran out of boxes before it covered the overage.
            short_by: demand.over_by.saturating_sub(held),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::HealthCheck;

    /// A verdict of each shape, without a fleet to read one from.
    fn over() -> HealthCheck {
        HealthCheck::unsatisfied(
            "the boxes' disk is 90% full (52.9G of 58.6G)",
            "the largest boxes are proj-s6 (14.0G), example-work (10.4G) — `skein stop <box>` \
             keeps its checkout, branch and conversation",
        )
    }
    fn under() -> HealthCheck {
        HealthCheck::satisfied("the boxes' disk is 33% full (19.5G of 58.6G)")
    }
    fn ranking() -> Vec<(String, u64)> {
        vec![
            ("proj-s6".to_string(), 14_336),
            ("example-work".to_string(), 10_650),
        ]
    }

    /// The demand that goes with [`over`], in the same figures: 85% of 58.6G is 51,000 MiB and
    /// 54,140 are in use, so 3,140 MiB have to go — and proj-s6's 14,336 covers that alone, which
    /// is why every test below that is not about the audience still sees exactly one box told.
    fn demand() -> DiskDemand {
        DiskDemand {
            over_by: 3_140,
            boxes: ranking(),
        }
    }

    /// A demand of somebody's choosing, for the tests that are about who gets told.
    fn holding(over_by: u64, boxes: &[(&str, u64)]) -> impl Fn() -> DiskDemand {
        let boxes: Vec<(String, u64)> = boxes
            .iter()
            .map(|(name, mb)| ((*name).to_string(), *mb))
            .collect();
        move || DiskDemand {
            over_by,
            boxes: boxes.clone(),
        }
    }

    /// The **body** of every message skein has put in `name`'s inbox, newest last.
    ///
    /// The body rather than the file, and that is not tidiness. The envelope carries the
    /// recipient's own name, so two notes read raw differ whatever is in them — and
    /// [`two_boxes_are_told_when_one_cannot_cover_it_and_each_is_asked_for_its_own_share`] asserts
    /// that the two boxes were *not* sent the same words. Against the raw files that assertion
    /// could not fail: it passed with [`compose`] ignoring its [`Ask`] entirely, which is the one
    /// implementation it exists to reject.
    fn inbox(home: &std::path::Path, name: &str) -> Vec<String> {
        let dir = home.join("boxes").join(name).join("inbox");
        let mut files: Vec<_> = match std::fs::read_dir(&dir) {
            Ok(d) => d.flatten().map(|e| e.path()).collect(),
            Err(_) => return Vec::new(),
        };
        files.sort();
        files
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .map(
                |raw| match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(m) => m["body"].as_str().unwrap_or_default().to_string(),
                    // Unparseable is returned whole rather than swallowed: a message skein cannot
                    // write as JSON is a failure, and an empty string here would read as "no note".
                    Err(_) => raw,
                },
            )
            .collect()
    }

    /// **The crossing is delivered, and the silence below the line is a silence.**
    ///
    /// This asserts a file arriving in a box's inbox rather than a verdict being computed, which is
    /// the distinction the whole item turns on: a test that asserts "the warning is present when
    /// the disk is over the line" passes on a fleet that was already over the line when the test
    /// started, and would not have caught the day that produced this module — where the reading was
    /// right and nobody was told.
    ///
    /// The sabotage each assertion was proved against, in order:
    ///
    /// * *below the line delivers nothing* — make [`step`]'s `Level::Satisfied` arm return
    ///   `Step::Announce`.
    /// * *crossing delivers, into the biggest box's inbox, naming it* — make the `Level::Unsatisfied`
    ///   arm return `Step::Quiet(Quiet::RoomLeft)`; or make [`audience`] answer `EveryBox`'s
    ///   `broadcast` for both arms, which with no registry delivers nowhere.
    /// * *still over says nothing more* — drop the `!said.over` guard so every reading announces.
    /// * *back under, then over again, is a second crossing* — make `Step::Cleared` not call
    ///   [`record`], so the record stays `over: true` and the second crossing reads as
    ///   `AlreadySaid`.
    #[test]
    fn the_fleet_says_it_is_filling_up_when_it_crosses_and_says_nothing_below_the_line() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        // Below the line: nothing is said, and nothing is delivered.
        let calm = announce_disk(&under(), &policy, demand).expect("a quiet tick cannot fail");
        assert_eq!(calm.step, Step::Quiet(Quiet::RoomLeft));
        assert!(
            inbox(&home, "proj-s6").is_empty(),
            "a fleet with room woke an agent up: {:?}",
            inbox(&home, "proj-s6")
        );

        // The crossing. Delivered — into the inbox of the box that is taking the space, which is
        // the directory a box cannot write, so what arrives there arrived from skein.
        let crossed = announce_disk(&over(), &policy, demand).expect("the crossing was not sent");
        assert_eq!(crossed.step, Step::Announce);
        assert_eq!(crossed.to(), vec!["proj-s6"]);
        let delivered = inbox(&home, "proj-s6");
        assert_eq!(
            delivered.len(),
            1,
            "the crossing did not arrive in the box's inbox: {delivered:?}"
        );
        assert!(
            delivered[0].contains("proj-s6 (14.0G)"),
            "what was delivered does not name what is taking the space: {}",
            delivered[0]
        );
        assert!(
            delivered[0].contains("not a sweep"),
            "what was delivered does not say skein will not clear it itself: {}",
            delivered[0]
        );
        assert!(
            !delivered[0].contains("prune"),
            "a note about the boxes' disk offered a delete nobody asked for: {}",
            delivered[0]
        );

        // Still over, on the next tick. The fleet has not crossed anything, so nothing more is
        // said — this is the arm that stops the warning becoming a thing people scroll past.
        let again = announce_disk(&over(), &policy, demand).expect("a quiet tick cannot fail");
        assert_eq!(again.step, Step::Quiet(Quiet::AlreadySaid));
        assert_eq!(
            inbox(&home, "proj-s6").len(),
            1,
            "the same warning was delivered twice without the fleet crossing anything"
        );

        // Back under: nobody is interrupted to be told a problem went away, but the record is
        // cleared.
        let cleared = announce_disk(&under(), &policy, demand).expect("a quiet tick cannot fail");
        assert_eq!(cleared.step, Step::Cleared);
        assert_eq!(
            inbox(&home, "proj-s6").len(),
            1,
            "coming back under the line interrupted somebody"
        );

        // And over again is a crossing again — the assertion that makes every one above about the
        // crossing rather than about the reading.
        let twice = announce_disk(&over(), &policy, demand).expect("the second crossing was lost");
        assert_eq!(twice.step, Step::Announce);
        assert_eq!(
            inbox(&home, "proj-s6").len(),
            2,
            "the fleet went under the line and back over it and nobody was told the second time"
        );
    }

    /// A disk that could not be measured says nothing **and forgets nothing**.
    ///
    /// Sabotage, chosen to separate the two halves rather than to trip the first assertion: leave
    /// [`step`] answering `NotMeasured` and have [`announce_disk`] record `over: false` on that arm
    /// anyway. The step assertions all still pass; the last one — that the following over-the-line
    /// reading is `AlreadySaid` — fails with `Announce`, which is a warning repeated at somebody who
    /// has already read it because skein could not see the disk for two seconds.
    #[test]
    fn a_disk_that_cannot_be_measured_neither_announces_nor_clears_what_was_announced() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        let unknown = HealthCheck::unknown("no fleet sandbox is configured");
        let first = announce_disk(&unknown, &policy, demand).expect("a quiet tick cannot fail");
        assert_eq!(first.step, Step::Quiet(Quiet::NotMeasured));
        assert!(inbox(&home, "proj-s6").is_empty());

        announce_disk(&over(), &policy, demand).expect("the crossing was not sent");
        assert_eq!(inbox(&home, "proj-s6").len(), 1);

        // The measurement fails for a tick. `Unknown` is not `Satisfied`, so it must not clear.
        let blind = announce_disk(&unknown, &policy, demand).expect("a quiet tick cannot fail");
        assert_eq!(blind.step, Step::Quiet(Quiet::NotMeasured));
        let after = announce_disk(&over(), &policy, demand).expect("a quiet tick cannot fail");
        assert_eq!(
            after.step,
            Step::Quiet(Quiet::AlreadySaid),
            "one tick that could not measure the disk turned into a repeat of a warning already \
             read"
        );
        assert_eq!(inbox(&home, "proj-s6").len(), 1);
    }

    /// **The default cadence is the hour, and it is the default that is asserted.**
    ///
    /// The mechanism is covered by the test below, which drives `repeat_after` explicitly. That is
    /// not the same claim: it proves the field works, and a default of `None` would pass it while
    /// leaving a fleet that sits over the line all day silent after its first word. The owner chose
    /// the hour (2026-09-09) precisely to reach the agent who starts work after the announcement,
    /// so what has to be nailed down is what `Policy::default()` actually does.
    ///
    /// The stamp is aged rather than the clock moved: `announce_disk` reads `Utc::now()` itself, and
    /// a test that could move the clock could move it for every other test sharing this process.
    ///
    /// Sabotage: put `repeat_after: None` back in [`Policy::default`] and the second announcement
    /// never arrives — the inbox still holds one message and the step reads `AlreadySaid`.
    #[test]
    fn the_default_says_it_again_after_an_hour_over_the_line_and_not_before() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        let first = announce_disk(&over(), &policy, demand).expect("the crossing was not sent");
        assert_eq!(first.step, Step::Announce);
        assert_eq!(inbox(&home, "proj-s6").len(), 1);

        // Still over, and nothing like an hour has passed: the second tick must be silent, or the
        // cadence is "every tick" and the hour is decoration.
        let soon = announce_disk(&over(), &policy, demand).expect("a quiet tick cannot fail");
        assert_eq!(soon.step, Step::Quiet(Quiet::AlreadySaid));
        assert_eq!(
            inbox(&home, "proj-s6").len(),
            1,
            "it repeated immediately, so the hour is not being read at all"
        );

        // Age the stamp past the hour, leaving `over` exactly as it was — the fleet has not moved,
        // only the clock has.
        let aged = Said {
            over: true,
            at: (Utc::now() - chrono::Duration::minutes(61)).to_rfc3339(),
        };
        std::fs::write(
            said_path(),
            serde_json::to_string(&aged).expect("the stamp serialises"),
        )
        .expect("the stamp is writable");

        let later = announce_disk(&over(), &policy, demand).expect("the repeat was not sent");
        assert_eq!(
            later.step,
            Step::Announce,
            "an hour over the line said nothing, so an agent starting work now is told nothing"
        );
        assert_eq!(
            inbox(&home, "proj-s6").len(),
            2,
            "the step said Announce and no second message arrived"
        );
    }

    /// **Something actually runs it.** (SKEIN-734)
    ///
    /// Every other test in this file proves the mechanism: what is announced, to whom, how often,
    /// and that it stays quiet below the line. All of them passed on the day this module shipped
    /// with **no caller at all** — the loop was left for a file the lane could not edit, so skein
    /// carried a complete, tested, unreachable warning system. That is the same failure the module
    /// exists to fix, one level up: a right answer nobody is told.
    ///
    /// So this reads the server's own source, which is where the *start* has to be — the cockpit's
    /// producer stops when the last tab closes (`crate::stream`), and the day this exists for is
    /// the day nobody had a tab open.
    ///
    /// Sabotage: delete the `tokio::spawn` line from `bin/skein-server.rs` and this fails.
    ///
    /// **It used to assert `spawn_blocking` here too, and that assertion has moved into the
    /// test below** (SKEIN-738). The loop's body now lives in this module, where a test can run
    /// it: whether the announcement is handed to a blocking thread is asserted by watching which
    /// thread it runs on, which is the property, rather than by finding the word in a file. What is
    /// left here is the one claim a source read is the right tool for — that something in the
    /// server starts the loop at all — and it still cannot prove the loop *ticks*, which is what
    /// the test below is for.
    #[test]
    fn the_server_is_what_runs_the_announcement() {
        let server = include_str!("bin/skein-server.rs");
        assert!(
            server.contains("watch_fleet_disk"),
            "nothing in skein-server.rs starts the announcement loop, so the fleet fills up in \
             silence exactly as it did before this module existed"
        );
    }

    /// **The loop ticks, the crossing is delivered on a tick, and the ticks after it are silent.**
    ///
    /// This is the assertion SKEIN-734 asked for and did not get: every other test in this file
    /// calls [`announce_disk`] itself, so all of them pass on a skein whose timer never comes round
    /// — which is a fleet filling up in silence, the failure the module exists to end, one level
    /// up from where it was fixed. What is under test here is only the seam: the timer, the
    /// blocking hand-off, and the call. The deciding half it drives is already covered above.
    ///
    /// **The period is 20ms and production's is 300s**, which is the whole reason [`watch_disk`]
    /// takes one. Real time rather than a paused clock: `spawn_blocking` leaves the runtime with
    /// nothing to poll while the announcement is in flight, so an auto-advancing clock could run
    /// the ticks out from under the work they started.
    ///
    /// The runtime is built by hand rather than by `#[tokio::test]` for the reason
    /// `bin/skein-server.rs`'s `on_a_runtime` gives: [`crate::testutil::env_lock`] is a
    /// `std::sync::MutexGuard` held for the whole body, and under `#[tokio::test]` it would be held
    /// across await points — `clippy::await_holding_lock`, and a real deadlock shape.
    ///
    /// The sabotage each assertion was named against and proved by, in order:
    ///
    /// * *it looked again* — replace [`watch_disk`]'s `loop` with a single tick.
    /// * *the crossing was delivered* — drop the [`announce_disk`] call from the loop's body.
    /// * *and only once* — drop the `!said.over` guard from [`step`]'s `Level::Unsatisfied` arm, so
    ///   every over-the-line reading announces. Note what this one also proves: a fixture whose
    ///   timer only ever fired once would still deliver one message and pass.
    /// * *off the runtime's own thread* — call `announce_disk` directly instead of handing it to
    ///   `spawn_blocking`.
    /// * *against a fixture fleet* — delete the `$SKEIN_FLEET_ROOT` pin, and
    ///   [`crate::util::fleet_root`] refuses rather than answering `/boxes`, which is the owner's
    ///   live fleet.
    #[test]
    fn the_loop_looks_again_and_delivers_the_crossing_exactly_once() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;

        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        // Both, never one. `$SKEIN_FLEET_ROOT` falls back to `/boxes` outside a test process, and
        // nothing this loop touches may be the owner's real fleet.
        env.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        assert!(
            crate::util::fleet_root().starts_with(home.to_str().expect("a utf-8 fixture path")),
            "the fleet root this test resolves is not the fixture's: {}",
            crate::util::fleet_root()
        );

        // The thread the runtime itself is driven on, captured before it is driven.
        let runtime_thread = std::thread::current().id();
        let looks = Arc::new(AtomicUsize::new(0));
        let on_the_runtime_thread = Arc::new(AtomicBool::new(false));
        let measure = {
            let looks = Arc::clone(&looks);
            let on_the_runtime_thread = Arc::clone(&on_the_runtime_thread);
            move || {
                looks.fetch_add(1, Ordering::SeqCst);
                if std::thread::current().id() == runtime_thread {
                    on_the_runtime_thread.store(true, Ordering::SeqCst);
                }
                over()
            }
        };

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime for this test's body")
            .block_on(async {
                let watching = tokio::spawn(watch_disk(
                    Duration::from_millis(20),
                    Policy::default(),
                    measure,
                    demand,
                ));
                // Bounded, so a loop that never comes round fails the assertion below rather than
                // hanging the suite for ever.
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while looks.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                watching.abort();
            });

        assert!(
            looks.load(Ordering::SeqCst) >= 3,
            "the loop looked {} times in ten seconds, so the timer does not come round and \
             nothing below is a claim about a second tick",
            looks.load(Ordering::SeqCst)
        );
        assert!(
            !on_the_runtime_thread.load(Ordering::SeqCst),
            "the announcement ran on the runtime's own thread, and behind it is a du of the whole \
             fleet root — every cockpit connection the server holds would stall on it"
        );
        let delivered = inbox(&home, "proj-s6");
        assert!(
            !delivered.is_empty(),
            "the loop ticked over a fleet past the line and delivered nothing, which is the fleet \
             filling up in silence"
        );
        assert_eq!(
            delivered.len(),
            1,
            "the fleet stayed over the same line and the loop said so on every tick: {} messages",
            delivered.len()
        );
    }

    /// The two [`Policy`] fields, each doing the one thing it is there for.
    ///
    /// Sabotage for the cadence half: make [`step`] ignore `repeat_after` and always answer
    /// `AlreadySaid` while over — the `Announce` assertion after the hour fails. For the audience
    /// half: make [`audience`] answer the biggest box for both arms — the `broadcast` assertion
    /// fails, and with it the claim that the fan-out is the mailbox's and not a second list.
    #[test]
    fn the_policy_decides_who_is_told_and_how_often_and_nothing_else_does() {
        let now = Utc::now();
        let over = Said {
            over: true,
            at: (now - chrono::Duration::minutes(30)).to_rfc3339(),
        };

        // Once per crossing: still over, still quiet, however long it has been.
        assert_eq!(
            step(Level::Unsatisfied, &over, now, None),
            Step::Quiet(Quiet::AlreadySaid)
        );
        // A repeat window that has not elapsed is still quiet…
        assert_eq!(
            step(
                Level::Unsatisfied,
                &over,
                now,
                Some(Duration::from_secs(3600))
            ),
            Step::Quiet(Quiet::AlreadySaid)
        );
        // …and one that has, says it again.
        assert_eq!(
            step(
                Level::Unsatisfied,
                &over,
                now,
                Some(Duration::from_secs(600))
            ),
            Step::Announce
        );
        // A record whose timestamp cannot be read repeats rather than going silent.
        let broken = Said {
            over: true,
            at: "not a time".into(),
        };
        assert_eq!(
            step(
                Level::Unsatisfied,
                &broken,
                now,
                Some(Duration::from_secs(3600))
            ),
            Step::Announce
        );

        // The audience is the mailbox's own fan-out, not a list assembled here — and it asks a
        // broadcast for nothing, because a share of the overage means nothing to a reader who may
        // be holding none of it.
        let every = audience(Audience::EveryBox, demand).unwrap();
        assert_eq!(every.len(), 1);
        assert_eq!(every[0].to, "broadcast");
        assert_eq!((every[0].over_by, every[0].free), (0, 0));
        // 3,140 MiB have to go and proj-s6 is holding 14,336 of them, so it is asked for the
        // overage and nobody else is asked for anything.
        let biggest = audience(Audience::TheFewestThatCoverIt, demand).unwrap();
        assert_eq!(biggest.len(), 1, "{biggest:?}");
        assert_eq!(biggest[0].to, "proj-s6");
        assert_eq!((biggest[0].free, biggest[0].short_by), (3_140, 0));
        // Over the line with nobody holding it is reported, not swallowed.
        assert!(audience(Audience::TheFewestThatCoverIt, DiskDemand::default).is_err());
    }

    /// The words carry the diagnosis, the way out, and the promise that skein will not act on it.
    ///
    /// Sabotage: drop `check.fix` from [`compose`]'s format string. The second assertion fails, and
    /// with it the property that makes this an announcement rather than an alarm — §2.4's rule that
    /// a fault is never reported without the recipe that clears it.
    #[test]
    fn what_is_delivered_carries_the_fix_and_says_nothing_will_be_deleted_for_you() {
        let plain = compose(&over(), &asked(3_140));
        assert!(plain.contains("90% full"), "{plain}");
        assert!(plain.contains("`skein stop <box>`"), "{plain}");
        assert!(plain.contains("Nothing has been deleted"), "{plain}");
        assert!(
            !plain.contains("skein will not run it"),
            "a note whose recipe destroys nothing warned about a delete: {plain}"
        );

        // A destructive recipe is printed and said to be one.
        let prune = HealthCheck::unsatisfied(
            "the image store is 91% full",
            "`sbx exec fleet docker system prune -af` frees it",
        )
        .destroys();
        let loud = compose(&prune, &asked(3_140));
        assert!(
            loud.contains("skein will not run it"),
            "a recipe that deletes was passed on without saying so: {loud}"
        );
    }

    /// One box asked for the whole overage — [`demand`]'s own case, spelled out as an [`Ask`] for
    /// the tests that call [`compose`] without going through [`audience`].
    fn asked(free: u64) -> Ask {
        Ask {
            to: "proj-s6".to_string(),
            over_by: free,
            free,
            others: Vec::new(),
            short_by: 0,
        }
    }

    /// **The fewest boxes that cover it, which is usually one — and the small box beside a big one
    /// is never told** (SKEIN-730).
    ///
    /// This is the case the owner's rule was chosen *against*, so it is asserted end to end rather
    /// than over a return value: `tiny` holding 2% of what `hog` holds must never find a note in
    /// its inbox, because an agent told it is a problem when it is not is how a warning stops being
    /// read. "Always the top two" — the rule that was rejected — passes every other test in this
    /// file and fails this one.
    ///
    /// The fixture is lopsided on purpose. A two-box fleet where either box would do proves
    /// nothing: here the right answer (`hog` alone) and the wrong one (`hog` and `tiny`) differ in
    /// a directory a test can look in.
    ///
    /// The sabotage each assertion was named against and proved by, in order:
    ///
    /// * *the biggest is told, and asked for the overage* — make [`cover`] ask for what the box
    ///   holds rather than a share of the overage, and 48.8G is asked of a fleet that is 3.1G over.
    /// * *the small box is not told* — drop the `held >= demand.over_by` break from [`cover`], which
    ///   is exactly "always the top two", and `tiny` gets a note.
    /// * *and hears nothing at all* — the same sabotage; this reads the inbox rather than the
    ///   return value, so it holds even if something else in the chain decides who to deliver to.
    #[test]
    fn the_box_holding_almost_nothing_beside_one_holding_almost_everything_is_left_alone() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        // 3,140 MiB have to go. `hog` is holding 50,000 of them and `tiny` 1,200 — a fortieth of
        // what `hog` has, and fifteen times more than the fleet needs back all the same.
        let lopsided = holding(3_140, &[("hog", 50_000), ("tiny", 1_200)]);
        let crossed = announce_disk(&over(), &policy, lopsided).expect("the crossing was not sent");
        assert_eq!(crossed.to(), vec!["hog"]);

        let told = inbox(&home, "hog");
        assert_eq!(told.len(), 1, "the box holding it was not told: {told:?}");
        assert!(
            told[0].contains("Clear 3.1G of storage you are not using")
                && told[0].contains("The fleet is 3.1G over what it can spare"),
            "the note does not name what the fleet is over by and what this box should clear: {}",
            told[0]
        );
        assert!(
            inbox(&home, "tiny").is_empty(),
            "a box holding a fortieth of what the biggest holds was interrupted about a fleet its \
             whole disk could not have caused: {:?}",
            inbox(&home, "tiny")
        );
    }

    /// **Two are told when one cannot cover it, and each is asked for its own figure.**
    ///
    /// The two halves are one test because either alone would pass a wrong implementation: telling
    /// both boxes the same body is right about the audience and wrong about the ask, and asking for
    /// a share while telling only one box is the reverse.
    ///
    /// `small` is in the fixture and never told, which is what makes this a claim about *the fewest
    /// that cover it* rather than about a cap: two boxes is the answer here because 26,000 alone is
    /// short of 30,000, not because two is the most there is.
    ///
    /// The sabotage each assertion was named against and proved by, in order:
    ///
    /// * *both are told* — break out of [`cover`]'s loop after the first box however short it is,
    ///   and only `half-a` is told while the fleet stays 4,000 MiB over.
    /// * *the third is not* — the same one that fails the test above.
    /// * *each is asked for its own share* — have [`compose`] ignore its [`Ask`], or ask every box
    ///   for the whole overage: the two notes become the same words, which the last assertion
    ///   names as the failure it is.
    /// * *and each is told who else was asked* — drop `others` from [`asked_of_you`]'s covering
    ///   arm, and a box that is one of two reads a note it cannot tell from being asked alone.
    #[test]
    fn two_boxes_are_told_when_one_cannot_cover_it_and_each_is_asked_for_its_own_share() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        // 30,000 MiB have to go; the biggest box is holding 26,000, so it cannot do it alone.
        let split = holding(
            30_000,
            &[("half-a", 26_000), ("half-b", 25_000), ("small", 900)],
        );
        let crossed = announce_disk(&over(), &policy, split).expect("the crossing was not sent");
        assert_eq!(crossed.to(), vec!["half-a", "half-b"]);
        assert!(
            inbox(&home, "small").is_empty(),
            "a box holding 900 MiB was interrupted about 30,000: {:?}",
            inbox(&home, "small")
        );

        // 26,000 and 25,000 of the 51,000 they hold between them: 15,295 MiB and 14,706, which is
        // 29.9G of the 29.3G the fleet is over — the shares add up to the overage and no further.
        let (a, b) = (inbox(&home, "half-a"), inbox(&home, "half-b"));
        assert_eq!((a.len(), b.len()), (1, 1), "{a:?} {b:?}");
        assert!(
            a[0].contains("Clear 14.9G of storage you are not using")
                && a[0].contains("half-b has been asked for 14.4G"),
            "the note does not ask this box for its own share of the overage, or does not say who \
             else was asked: {}",
            a[0]
        );
        assert!(
            b[0].contains("Clear 14.4G of storage you are not using")
                && b[0].contains("half-a has been asked for 14.9G"),
            "the note does not ask this box for its own share of the overage, or does not say who \
             else was asked: {}",
            b[0]
        );
        assert_ne!(
            a[0], b[0],
            "both boxes were sent the same words, so the amount in them is not this box's own"
        );
    }

    /// **A third box is told when the first two do not cover it, and a fourth is not** (SKEIN-730).
    ///
    /// The owner removed the cap of two on 2026-09-09: *"maybe don't restrict to 2, go till you can
    /// have more free memory. Sometimes it is possible that 1 can't free anymore because it needs
    /// all of that actively rn."* So the rule is the shortest prefix that covers the overage, full
    /// stop, and the count is whatever that takes.
    ///
    /// The fixture separates the three implementations that would all pass a two-box test: a cap of
    /// two tells `a` and `b` and leaves the fleet 7,000 MiB short; the rule tells `a`, `b` and `c`;
    /// telling everybody adds `d`, which is holding 500 MiB of a 30,000 MiB problem and is the
    /// interruption the rule exists to refuse. All three answers differ in a directory this test
    /// reads.
    ///
    /// The sabotage each assertion was named against and proved by, in order:
    ///
    /// * *the third is told* — put a `.take(2)` back on [`cover`]'s ranking, which is the rule as it
    ///   shipped before this and is exactly what the owner struck out.
    /// * *the fourth is not* — drop the `held >= demand.over_by` break, and `d` is told too.
    /// * *and the three shares cover it* — the note's own figures, which only add up to the overage
    ///   if the share is taken over what the boxes told are holding between them.
    #[test]
    fn a_third_box_is_told_when_the_first_two_do_not_cover_it_and_a_fourth_is_left_alone() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        // 30,000 MiB have to go. The top two hold 23,000 between them and the top three hold
        // 33,000, so three is the fewest that can cover it.
        let three = holding(
            30_000,
            &[("a", 12_000), ("b", 11_000), ("c", 10_000), ("d", 500)],
        );
        let crossed = announce_disk(&over(), &policy, three).expect("the crossing was not sent");
        assert_eq!(crossed.to(), vec!["a", "b", "c"]);
        assert!(
            inbox(&home, "d").is_empty(),
            "a box holding 500 MiB of a 30,000 MiB overage was interrupted: {:?}",
            inbox(&home, "d")
        );

        // 12,000, 11,000 and 10,000 of the 33,000 they hold: 10,910 MiB, 10,000 and 9,091, which
        // is the overage and one MiB of rounding.
        let told = inbox(&home, "a");
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].contains("Clear 10.7G of storage you are not using")
                && told[0].contains("b has been asked for 9.8G and c for 8.9G"),
            "the three shares in the note do not add up to the 29.3G the fleet is over: {}",
            told[0]
        );
    }

    /// **When the fleet's boxes together are not holding enough, the note says so** — rather than
    /// letting its reader clear everything it has and find the fleet still over the line.
    ///
    /// With no cap on the audience this is the only way [`Ask::short_by`] can be non-zero: every
    /// box has been asked for everything it has and it is still short, so the rest of the space is
    /// not in a box at all. Rare, and the box being asked is the one person in a position to be
    /// misled by the difference.
    ///
    /// The sabotage each assertion was named against and proved by, in order:
    ///
    /// * *every box is told* — put a cap back on [`cover`]'s ranking and `c` is left out of a
    ///   problem that needs everything all three of them are holding.
    /// * *each is asked for what it holds and no more* — that is [`cover`]'s `min`; remove it and
    ///   the note asks a box holding 5.9G to clear 9.0G.
    /// * *and told what emptying the fleet would still leave* — compute `short_by` as `0`, and the
    ///   note reads as an ask that clears the line, which is the sentence this test forbids.
    #[test]
    fn boxes_that_cannot_cover_it_between_them_are_told_that_rather_than_left_to_assume_it_clears()
    {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        // 20,000 MiB have to go and every box in the fleet together is holding 13,000. All three
        // are told, and none of them can be promised the line will clear.
        let hopeless = holding(20_000, &[("a", 6_000), ("b", 4_000), ("c", 3_000)]);
        let crossed = announce_disk(&over(), &policy, hopeless).expect("the crossing was not sent");
        assert_eq!(crossed.to(), vec!["a", "b", "c"]);

        let told = inbox(&home, "a");
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].contains(
                "Clear what you can of the 5.9G you are holding. The fleet is 19.5G over what it \
                 can spare, and emptying every box in it would still leave 6.8G of that"
            ),
            "the note does not say that emptying the fleet's boxes still leaves it over the line: \
             {}",
            told[0]
        );
        assert!(
            !told[0].contains("of storage you are not using"),
            "the note asked for a share of an overage its whole fleet cannot cover, which reads as \
             an ask that clears the line: {}",
            told[0]
        );
    }

    /// **A fault with nothing for a box to free still reaches somebody, and asks them for
    /// nothing.**
    ///
    /// The verdict is `Unsatisfied` when *either* filesystem is past the line, so the image store
    /// filling up is a crossing with an overage of zero on the boxes' disk. Two ways to get that
    /// wrong: tell nobody, which is the silence this module exists to end; or ask the biggest box
    /// to free 0.0G, which is a paragraph that teaches its reader the note is machinery rather than
    /// a message.
    ///
    /// The sabotage each assertion was named against and proved by, in order:
    ///
    /// * *somebody is still told* — return an empty list from [`cover`] when the overage is zero
    ///   (the arithmetically natural reading of "the fewest that cover it"), and the announcement
    ///   errs instead of delivering.
    /// * *and asked for nothing* — drop [`asked_of_you`]'s early return, and the note tells the box
    ///   to clear 0.0G of storage because the fleet is 0.0G over what it can spare.
    /// * *the rest of the note is unchanged* — the same sabotage leaves this assertion passing,
    ///   which is why it is here: it says what must survive, not just what must go.
    #[test]
    fn a_crossing_with_nothing_for_a_box_to_free_still_tells_one_and_asks_it_for_nothing() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let policy = Policy::default();

        // The image store is full; the boxes' filesystem has room, so nothing anybody frees in a
        // box changes the number.
        let images = holding(0, &[("proj-s6", 14_336), ("example-work", 10_650)]);
        let crossed = announce_disk(&over(), &policy, images).expect("the crossing was not sent");
        assert_eq!(crossed.to(), vec!["proj-s6"]);

        let told = inbox(&home, "proj-s6");
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            !told[0].contains("what it can spare") && !told[0].contains("of storage you are not"),
            "a box was asked to clear a share of an overage of nothing: {}",
            told[0]
        );
        assert!(
            told[0].contains("90% full") && told[0].contains("Nothing has been deleted"),
            "the verdict and the promise went missing with the ask: {}",
            told[0]
        );
    }
}
