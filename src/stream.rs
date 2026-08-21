//! One producer, fanned out — and transitions rather than snapshots (§10.1).
//!
//! # What this replaces
//!
//! Every SSE client built its own interval and ran the whole fleet snapshot on the blocking pool
//! every two seconds. Five browser tabs were five snapshots a tick, each shelling out; and the
//! snapshot is the most expensive thing skein computes. **The lesson was already paid for in this
//! codebase**: check-then-act once gave every browser tab its own subprocess every tick, which is
//! why `util::Gate` exists at all. This is the same bug one layer up, and a gate cannot fix it —
//! the work was per client by construction.
//!
//! So: one producer, started when the first client arrives and stopped when the last one leaves. A
//! server nobody is watching does no work at all, which is a property the old shape could not have.
//!
//! # Transitions, not snapshots
//!
//! A full snapshot on connect, and after that only what changed. That is §10.1's design and it is
//! also what the away-digest needs — "what happened while you were gone" cannot be a client-side
//! delta computed on tab focus, because that cannot survive a reload and is wrong for the second
//! tab.
//!
//! # A slow client is told, never silently skipped
//!
//! The channel is bounded, so a client that stops reading eventually falls behind. It is **told how
//! many ticks it missed** and handed a fresh snapshot, rather than being quietly given a stream with
//! a hole in it. A hole is worse than a gap you can see: the board would look current and be wrong.

use crate::board::BoxView;
use serde::Serialize;
use std::sync::{Mutex, OnceLock};

/// How many ticks a client may fall behind before it is told it has.
///
/// Small on purpose. A client that is eight ticks behind is not reading, and holding more for it
/// costs the producer memory to serve a board that is already sixteen seconds stale — the honest
/// answer at that point is a fresh snapshot, not a backlog.
pub const BEHIND: usize = 8;

/// Send the whole picture every this many ticks, whatever changed.
///
/// Because change detection ignores the clock-derived fields (see [`transition`]), a row's displayed
/// age would otherwise stop advancing until something real happened to it — and "last seen 2m ago"
/// frozen at 2m is a worse lie than a slightly stale board. Fifteen ticks is thirty seconds.
///
/// A floor rather than the mechanism. The right answer is a client that ages its own rows, and this
/// holds the line until there is one.
pub const FULL_EVERY: u64 = 15;

/// What the stream carries.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Tick {
    /// Everything, as it is now. Sent on connect, and whenever a client has fallen behind.
    Snapshot { boxes: Vec<BoxView> },
    /// Only what moved since the last tick.
    ///
    /// `gone` is a separate list rather than an absence, because "this box is not in the update" and
    /// "this box is no longer there" are different facts and a delta that conflates them can only be
    /// applied by re-sending everything.
    Changed {
        boxes: Vec<BoxView>,
        gone: Vec<String>,
    },
}

/// What changed between two snapshots.
///
/// Compared by serialised form rather than field by field: a `BoxView` gains fields regularly, and a
/// hand-written comparison is one that stops noticing the newest one — silently, and in the
/// direction of showing a stale row. Serialising twice per tick is nothing beside computing the
/// snapshot that produced it.
///
/// **Except the fields derived from the clock**, and leaving them in made every box change on every
/// tick — transitions costing exactly what snapshots did, plus the machinery. A box's `age` is "how
/// long since it was last seen", so it moves every second whether or not anything happened to the
/// box. "Changed" has to mean *something happened*, not *time passed*, or a quiet fleet never goes
/// quiet. Found by a test that hung: the stream never stopped sending.
///
/// The cost is that a row's displayed age stops advancing between real changes, which is what
/// [`FULL_EVERY`] holds the line on. The better fix — a client ageing its own rows from `age_secs`
/// and the moment it received them — is written down rather than half-built here.
pub fn transition(before: &[BoxView], after: &[BoxView]) -> Tick {
    let substantive = |b: &BoxView| {
        serde_json::to_string(&BoxView {
            age: String::new(),
            age_secs: None,
            ..b.clone()
        })
        .unwrap_or_default()
    };
    let was: std::collections::HashMap<&str, String> = before
        .iter()
        .map(|b| (b.name.as_str(), substantive(b)))
        .collect();
    let boxes: Vec<BoxView> = after
        .iter()
        .filter(|b| was.get(b.name.as_str()) != Some(&substantive(b)))
        .cloned()
        .collect();
    let still: std::collections::HashSet<&str> = after.iter().map(|b| b.name.as_str()).collect();
    let gone: Vec<String> = before
        .iter()
        .map(|b| b.name.clone())
        .filter(|name| !still.contains(name.as_str()))
        .collect();
    Tick::Changed { boxes, gone }
}

struct Producer {
    say: tokio::sync::broadcast::Sender<Tick>,
    /// How many ticks have been published, for [`FULL_EVERY`].
    ticks: std::sync::atomic::AtomicU64,
    /// The last full picture, so a client arriving mid-stream gets one without waiting a tick and
    /// without computing its own.
    latest: Mutex<Vec<BoxView>>,
}

fn producer() -> &'static Producer {
    static IT: OnceLock<Producer> = OnceLock::new();
    IT.get_or_init(|| Producer {
        say: tokio::sync::broadcast::channel(BEHIND).0,
        ticks: std::sync::atomic::AtomicU64::new(1),
        latest: Mutex::new(Vec::new()),
    })
}

/// Publish one tick to every client. Called by the producer loop; separate so the loop's *policy*
/// (how often, where the snapshot comes from) lives in the server and the fan-out lives here.
pub fn publish(views: Vec<BoxView>) {
    let producer = producer();
    let ticks = producer
        .ticks
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tick = {
        let mut latest = producer.latest.lock().unwrap_or_else(|e| e.into_inner());
        let tick = match ticks.is_multiple_of(FULL_EVERY) {
            true => Tick::Snapshot {
                boxes: views.clone(),
            },
            false => transition(&latest, &views),
        };
        *latest = views;
        tick
    };
    // Nothing moved: say nothing. A stream that emits an empty update every two seconds is a stream
    // whose silence means nothing, and "nothing needs you" is a state the board has to be able to
    // render calmly.
    if matches!(&tick, Tick::Changed { boxes, gone } if boxes.is_empty() && gone.is_empty()) {
        return;
    }
    let _ = producer.say.send(tick);
}

/// Everything as the producer last saw it.
pub fn latest() -> Vec<BoxView> {
    producer()
        .latest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Subscribe: the snapshot to start from, and the transitions after it.
///
/// Both from one call and under one lock, because taking them separately drops whatever changed in
/// between — and the row that changed in that window is exactly the one somebody was waiting for.
pub fn subscribe() -> (Tick, tokio::sync::broadcast::Receiver<Tick>) {
    let producer = producer();
    let latest = producer.latest.lock().unwrap_or_else(|e| e.into_inner());
    let rest = producer.say.subscribe();
    (
        Tick::Snapshot {
            boxes: latest.clone(),
        },
        rest,
    )
}

/// How many clients are listening. The producer loop stops when this reaches zero.
pub fn listeners() -> usize {
    producer().say.receiver_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str, state: &str) -> BoxView {
        BoxView {
            name: name.into(),
            state: state.into(),
            ..Default::default()
        }
    }

    /// A transition carries what moved, and nothing else.
    #[test]
    fn only_what_changed_is_sent() {
        let before = vec![view("a", "live"), view("b", "waiting")];
        let after = vec![view("a", "live"), view("b", "done")];
        match transition(&before, &after) {
            Tick::Changed { boxes, gone } => {
                assert_eq!(boxes.len(), 1, "an unchanged row was re-sent");
                assert_eq!(boxes[0].name, "b");
                assert!(gone.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    /// "Not in the update" and "no longer there" are different facts.
    ///
    /// A delta that conflated them could only be applied by re-sending everything, which is the
    /// thing this exists to stop doing.
    #[test]
    fn a_box_that_went_away_is_named_rather_than_merely_absent() {
        let before = vec![view("a", "live"), view("b", "live")];
        let after = vec![view("a", "live")];
        match transition(&before, &after) {
            Tick::Changed { boxes, gone } => {
                assert!(
                    boxes.is_empty(),
                    "nothing changed about the box that stayed"
                );
                assert_eq!(gone, vec!["b".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        // And a new one is a change rather than a special case.
        match transition(&after, &before) {
            Tick::Changed { boxes, gone } => {
                assert_eq!(boxes.len(), 1);
                assert_eq!(boxes[0].name, "b");
                assert!(gone.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    /// **Time passing is not a change**, and this is the bug that made the first version useless.
    ///
    /// A box's `age` moves every second whether or not anything happened to it, so leaving it in the
    /// comparison made every box change on every tick — transitions costing exactly what snapshots
    /// did, plus the machinery. It surfaced as a test that hung, because the stream never stopped
    /// sending.
    #[test]
    fn a_box_that_only_got_older_did_not_change() {
        let before = vec![BoxView {
            name: "a".into(),
            state: "live".into(),
            age: "2m".into(),
            age_secs: Some(120),
            ..Default::default()
        }];
        let older = vec![BoxView {
            age: "4m".into(),
            age_secs: Some(240),
            ..before[0].clone()
        }];
        match transition(&before, &older) {
            Tick::Changed { boxes, gone } => {
                assert!(
                    boxes.is_empty() && gone.is_empty(),
                    "a quiet fleet never goes quiet: {boxes:?}"
                );
            }
            other => panic!("{other:?}"),
        }
        // And something real alongside the clock is still a change.
        let moved = vec![BoxView {
            state: "waiting".into(),
            age: "9m".into(),
            age_secs: Some(540),
            ..before[0].clone()
        }];
        match transition(&before, &moved) {
            Tick::Changed { boxes, .. } => assert_eq!(boxes.len(), 1),
            other => panic!("{other:?}"),
        }
    }

    /// A field added to `BoxView` is noticed without anyone remembering to compare it.
    ///
    /// The reason the comparison is over the serialised form: a hand-written one stops noticing the
    /// newest field silently, and in the direction of showing a stale row.
    #[test]
    fn a_change_in_any_field_counts_as_a_change() {
        let before = vec![view("a", "live")];
        let mut moved = view("a", "live");
        moved.headline = Some("it said something".into());
        match transition(&before, &[moved]) {
            Tick::Changed { boxes, .. } => assert_eq!(boxes.len(), 1),
            other => panic!("{other:?}"),
        }
        // And an identical picture is no transition at all.
        match transition(&before, &before.clone()) {
            Tick::Changed { boxes, gone } => {
                assert!(boxes.is_empty() && gone.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }
}
