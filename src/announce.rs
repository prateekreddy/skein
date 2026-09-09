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
//! often is a decision about somebody's attention rather than about code. The default is the
//! quietest arrangement that still works — one box, once per crossing — and every other arrangement
//! is a different value of the same two fields.

use crate::health::{HealthCheck, Level};
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
    /// The box using the most disk. One interruption, aimed at the agent that can act on it.
    TheBiggestBox,
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
    /// **One box, and again every hour while it is still over the line.** The owner chose both,
    /// 2026-09-09, having been shown what each costs.
    ///
    /// The audience is the box holding the most disk, because it is the only agent whose action
    /// changes the number. He was told the price and took it: **the box that dies of `ENOSPC` is
    /// usually not that box**, so the one about to be hurt is not the one being warned.
    ///
    /// The hour is the answer to the failure the once-per-crossing arrangement has, which is that
    /// it says nothing to an agent starting work after the announcement — and the fleet can sit
    /// over the line all day. An hour rather than five minutes because a day over the line is then
    /// ten interruptions rather than nearly three hundred, and three hundred is how a warning stops
    /// being read. That is the failure this module exists to leave behind, so it is the one the
    /// number is chosen against.
    fn default() -> Policy {
        Policy {
            audience: Audience::TheBiggestBox,
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

/// What one tick did, in a form a caller can print and a test can assert against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub step: Step,
    /// Where it was delivered. Empty unless [`Step::Announce`].
    pub to: Vec<String>,
    /// Exactly the words a reader will see. Empty unless [`Step::Announce`].
    pub body: String,
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

/// Exactly what a reader sees.
///
/// Composed from the check rather than written here: `detail` is the diagnosis and `fix` is the way
/// out, both already computed and already tested, and a second copy of either in this file is a
/// second copy to drift. What this function adds is the two things a health row does not have to
/// say — why a *different* box's build is the one that dies, and that skein is not going to clear
/// anything itself.
pub fn compose(check: &HealthCheck) -> String {
    let mut body = format!(
        "the fleet's disk is filling up: {}\n\n  → {}\n\nOne filesystem serves every box, so a \
         build in any of them can die half way through — including one that is not taking the \
         space.",
        check.detail, check.fix
    );
    if check.destructive {
        body.push_str(
            "\n\nThe command above deletes what Docker is keeping. It is printed for you to run; \
             skein will not run it.",
        );
    }
    body.push_str("\n\nNothing has been deleted. This is a note, not a sweep.");
    body
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
        crate::health::biggest_first,
    )
}

/// [`announce_fleet_disk`] over a verdict and a ranking already in hand.
///
/// The two are arguments for the reason [`crate::health::disk_health`] splits the same way: a
/// verdict this function did not compute is a verdict a test can *choose*, which is what makes the
/// crossing — rather than the reading — the thing under test.
pub fn announce_disk(
    check: &HealthCheck,
    policy: &Policy,
    biggest: impl FnOnce() -> Vec<(String, u64)>,
) -> Result<Outcome, String> {
    let quiet = |step| Outcome {
        step,
        to: Vec::new(),
        body: String::new(),
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
            let to = audience(policy.audience, biggest)?;
            let body = compose(check);
            for one in &to {
                crate::mailbox::send_message(one, "fleet-disk", &body)?;
            }
            // **After the delivery, never before.** A record written first would mean a failed
            // send silenced the warning for the rest of that crossing — the exact shape of the
            // failure this module was written after, reintroduced one layer down.
            record(&Said {
                over: true,
                at: now.to_rfc3339(),
            })?;
            Ok(Outcome { step, to, body })
        }
    }
}

/// Turn an [`Audience`] into the `to` values [`crate::mailbox::send_message`] takes.
///
/// `EveryBox` is one `broadcast`, which that function fans out over the registry — rather than a
/// list assembled here, because a list assembled here is a second answer to "which boxes exist"
/// and the two would disagree the first time one of them was wrong.
fn audience(
    who: Audience,
    biggest: impl FnOnce() -> Vec<(String, u64)>,
) -> Result<Vec<String>, String> {
    match who {
        Audience::EveryBox => Ok(vec!["broadcast".to_string()]),
        Audience::TheBiggestBox => match biggest().into_iter().next() {
            Some((name, _)) => Ok(vec![name]),
            // Over the line with no box holding anything is a real state — the space is somewhere
            // else entirely — and there is nobody in a box to tell about it. Not an error, and not
            // a silent success either: it is reported, and the record is not written, so the moment
            // there is a box to tell it is still a crossing.
            None => Err(
                "the fleet is over the line and no box is holding any of it, so there is nobody in \
                 a box to tell — `skein doctor` has the figures"
                    .into(),
            ),
        },
    }
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

    /// Every message skein has put in `name`'s inbox, newest last.
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
        let calm = announce_disk(&under(), &policy, ranking).expect("a quiet tick cannot fail");
        assert_eq!(calm.step, Step::Quiet(Quiet::RoomLeft));
        assert!(
            inbox(&home, "proj-s6").is_empty(),
            "a fleet with room woke an agent up: {:?}",
            inbox(&home, "proj-s6")
        );

        // The crossing. Delivered — into the inbox of the box that is taking the space, which is
        // the directory a box cannot write, so what arrives there arrived from skein.
        let crossed = announce_disk(&over(), &policy, ranking).expect("the crossing was not sent");
        assert_eq!(crossed.step, Step::Announce);
        assert_eq!(crossed.to, vec!["proj-s6".to_string()]);
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
        let again = announce_disk(&over(), &policy, ranking).expect("a quiet tick cannot fail");
        assert_eq!(again.step, Step::Quiet(Quiet::AlreadySaid));
        assert_eq!(
            inbox(&home, "proj-s6").len(),
            1,
            "the same warning was delivered twice without the fleet crossing anything"
        );

        // Back under: nobody is interrupted to be told a problem went away, but the record is
        // cleared.
        let cleared = announce_disk(&under(), &policy, ranking).expect("a quiet tick cannot fail");
        assert_eq!(cleared.step, Step::Cleared);
        assert_eq!(
            inbox(&home, "proj-s6").len(),
            1,
            "coming back under the line interrupted somebody"
        );

        // And over again is a crossing again — the assertion that makes every one above about the
        // crossing rather than about the reading.
        let twice = announce_disk(&over(), &policy, ranking).expect("the second crossing was lost");
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
        let first = announce_disk(&unknown, &policy, ranking).expect("a quiet tick cannot fail");
        assert_eq!(first.step, Step::Quiet(Quiet::NotMeasured));
        assert!(inbox(&home, "proj-s6").is_empty());

        announce_disk(&over(), &policy, ranking).expect("the crossing was not sent");
        assert_eq!(inbox(&home, "proj-s6").len(), 1);

        // The measurement fails for a tick. `Unknown` is not `Satisfied`, so it must not clear.
        let blind = announce_disk(&unknown, &policy, ranking).expect("a quiet tick cannot fail");
        assert_eq!(blind.step, Step::Quiet(Quiet::NotMeasured));
        let after = announce_disk(&over(), &policy, ranking).expect("a quiet tick cannot fail");
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

        let first = announce_disk(&over(), &policy, ranking).expect("the crossing was not sent");
        assert_eq!(first.step, Step::Announce);
        assert_eq!(inbox(&home, "proj-s6").len(), 1);

        // Still over, and nothing like an hour has passed: the second tick must be silent, or the
        // cadence is "every tick" and the hour is decoration.
        let soon = announce_disk(&over(), &policy, ranking).expect("a quiet tick cannot fail");
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

        let later = announce_disk(&over(), &policy, ranking).expect("the repeat was not sent");
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

        // The audience is the mailbox's own fan-out, not a list assembled here.
        assert_eq!(
            audience(Audience::EveryBox, ranking).unwrap(),
            vec!["broadcast".to_string()]
        );
        assert_eq!(
            audience(Audience::TheBiggestBox, ranking).unwrap(),
            vec!["proj-s6".to_string()]
        );
        // Over the line with nobody holding it is reported, not swallowed.
        assert!(audience(Audience::TheBiggestBox, Vec::new).is_err());
    }

    /// The words carry the diagnosis, the way out, and the promise that skein will not act on it.
    ///
    /// Sabotage: drop `check.fix` from [`compose`]'s format string. The second assertion fails, and
    /// with it the property that makes this an announcement rather than an alarm — §2.4's rule that
    /// a fault is never reported without the recipe that clears it.
    #[test]
    fn what_is_delivered_carries_the_fix_and_says_nothing_will_be_deleted_for_you() {
        let plain = compose(&over());
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
        let loud = compose(&prune);
        assert!(
            loud.contains("skein will not run it"),
            "a recipe that deletes was passed on without saying so: {loud}"
        );
    }
}
