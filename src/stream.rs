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
/// **Not for ages any more.** It was fifteen ticks — thirty seconds — because change detection
/// ignores the clock-derived fields, so a row's displayed age would otherwise freeze until something
/// real happened to it. The client ages its own rows now, from `age_secs` and the moment it received
/// them, so that reason is gone.
///
/// What is left is a re-sync floor, and it is far rarer: a client whose applied deltas have drifted
/// from the producer — a dropped event the browser never surfaced, a bug in applying one — has no
/// way to notice on its own, and a quiet fleet gives it nothing to correct against. Ten minutes is
/// long enough to be nearly free and short enough that nobody stares at a wrong board for an
/// afternoon.
pub const FULL_EVERY: u64 = 300;

/// How often the producer turns. Here rather than at the `tokio::interval` that spends it, because
/// [`ALIVE_EVERY`] is a count of ticks and a count is meaningless without the length of one.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(2);

/// Say "still here" every this many quiet ticks — ten seconds at the cadence above.
///
/// **A different question from "what changed", and it needed its own answer.** The stream says
/// nothing when nothing moved, on purpose, and the re-sync floor above is ten minutes. So a calm
/// fleet was ten minutes of silence, and the board — which had nothing else to go on — read that as
/// a server that had died and put up "board is Ns stale — reconnecting…". A banner whose own comment
/// says it "is worth reading only when it means the server is actually gone" was firing on every
/// quiet afternoon, which is how a warning stops being read.
///
/// It was not only cosmetic. Ten minutes of a connection carrying zero bytes is a connection that
/// browsers, the OS and anything in between are entitled to drop — and each real reconnect costs a
/// fresh `load_views`, the most expensive thing skein computes.
///
/// Deliberately an EVENT and not an SSE keep-alive comment: `EventSource` does not surface comments
/// to JavaScript, so a keep-alive would have fixed the dropped connections and left the banner
/// firing. And it says more than a comment can — that the producer LOOP is still turning, where an
/// open socket only says the process has not exited.
pub const ALIVE_EVERY: u64 = 5;

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
    /// Nothing moved, and the producer is still turning.
    ///
    /// Carries no data because it answers no question about the fleet — only about the stream. A
    /// client that gets these knows its board is current; one that stops getting them knows the
    /// thing feeding it has stopped, which is the only reading of a stale board worth acting on.
    Alive,
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

/// One box changing state, as the away digest remembers it.
///
/// **State**, not any field: "your box finished while you were out" is about what it is doing, and
/// a journal that recorded every diffstat and headline would be a log rather than a digest — and
/// would be re-read as noise on every reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Moment {
    /// RFC3339, stamped by the producer. The client's clock is not consulted, because two tabs on
    /// two machines disagreeing about *when* is the whole reason this is not a client-side delta.
    pub at: String,
    pub name: String,
    pub from: String,
    pub to: String,
}

/// How many moments are kept.
///
/// A digest, not an audit log — the host-side one is §5's and this is not it. Enough for a night
/// away from a busy fleet, bounded so a month of uptime is not a memory leak with a story.
pub const REMEMBERED: usize = 512;

struct Producer {
    say: tokio::sync::broadcast::Sender<Tick>,
    /// How many ticks have been published, for [`FULL_EVERY`].
    ticks: std::sync::atomic::AtomicU64,
    /// The last full picture, so a client arriving mid-stream gets one without waiting a tick and
    /// without computing its own.
    latest: Mutex<Vec<BoxView>>,
    /// What changed state, oldest first. See [`Moment`].
    journal: Mutex<std::collections::VecDeque<Moment>>,
}

fn producer() -> &'static Producer {
    static IT: OnceLock<Producer> = OnceLock::new();
    IT.get_or_init(|| Producer {
        say: tokio::sync::broadcast::channel(BEHIND).0,
        ticks: std::sync::atomic::AtomicU64::new(1),
        latest: Mutex::new(Vec::new()),
        journal: Mutex::new(std::collections::VecDeque::new()),
    })
}

/// Publish one tick to every client. Called by the producer loop; separate so the loop's *policy*
/// (how often, where the snapshot comes from) lives in the server and the fan-out lives here.
pub fn publish(views: Vec<BoxView>) {
    let producer = producer();
    let ticks = producer
        .ticks
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    remember(&producer.journal, &producer.latest, &views);
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
        // Still not box data — but not silence either. `Alive` is the stream saying it is turning,
        // which is a different fact from anything about the fleet and is the one a board needs to
        // tell "calm" from "gone". See [`ALIVE_EVERY`].
        if ticks.is_multiple_of(ALIVE_EVERY) {
            let _ = producer.say.send(Tick::Alive);
        }
        return;
    }
    let _ = producer.say.send(tick);
}

/// Write down what changed state, and forget the oldest when the journal is full.
fn remember(
    journal: &Mutex<std::collections::VecDeque<Moment>>,
    latest: &Mutex<Vec<BoxView>>,
    views: &[BoxView],
) {
    let before: std::collections::HashMap<String, String> = latest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|b| (b.name.clone(), b.state.clone()))
        .collect();
    let at = chrono::Utc::now().to_rfc3339();
    let mut journal = journal.lock().unwrap_or_else(|e| e.into_inner());
    for view in views {
        // **A box with no previous state has not transitioned**, and that is the whole guard. It
        // covers two cases at once: the producer's first tick, where writing every box down would
        // make restarting skein look like a night's worth of activity, and a box that has just been
        // created, which arrived rather than moved. An earlier version had a separate `if
        // before.is_empty()` check above; it was redundant, and a sabotage pass found it by
        // removing it and watching the test still pass.
        let Some(was) = before.get(&view.name) else {
            continue;
        };
        if *was == view.state {
            continue;
        }
        journal.push_back(Moment {
            at: at.clone(),
            name: view.name.clone(),
            from: was.clone(),
            to: view.state.clone(),
        });
    }
    while journal.len() > REMEMBERED {
        journal.pop_front();
    }
}

/// What has happened since `at`, oldest first.
///
/// The **server** answers this, which is the whole design. A client-side delta computed on tab focus
/// cannot survive a reload, cannot tell a box that finished while you were away from one that
/// finished before the tab was opened, and is wrong for every second tab — three failures that all
/// look like the feature working.
pub fn since(at: &str) -> Vec<Moment> {
    let journal = producer().journal.lock().unwrap_or_else(|e| e.into_inner());
    // An unparseable or empty mark means "we do not know when you last looked", and the honest
    // answer to that is everything remembered rather than nothing — a digest that silently shows
    // nothing is indistinguishable from a quiet night.
    let Ok(mark) = chrono::DateTime::parse_from_rfc3339(at) else {
        return journal.iter().cloned().collect();
    };
    journal
        .iter()
        .filter(|m| chrono::DateTime::parse_from_rfc3339(&m.at).is_ok_and(|when| when > mark))
        .cloned()
        .collect()
}

/// When a person last acknowledged the board, on disk.
///
/// **On disk, and one of them**, because that is what the three failures of a client-side delta come
/// down to. A mark in a tab's memory dies on reload; a mark per tab makes two tabs disagree; and a
/// mark computed from "when this tab gained focus" cannot tell a box that finished while you were
/// out from one that finished before you opened it. One file answers all three.
///
/// Beside skein's other state rather than in the volume's declared area: it is a **recorded** fact
/// about a person's attention, not a declaration anybody reconciles against.
fn mark_path() -> std::path::PathBuf {
    crate::config::skein_home().join("seen.json")
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
struct Mark {
    /// RFC3339. Empty means nobody has ever acknowledged anything.
    #[serde(default)]
    at: String,
}

/// When the board was last acknowledged. Empty if never.
pub fn last_seen() -> String {
    std::fs::read_to_string(mark_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Mark>(&raw).ok())
        .map(|mark| mark.at)
        .unwrap_or_default()
}

/// Acknowledge everything up to now.
///
/// Stamped **here**, not taken from the caller: a client that supplies its own timestamp supplies
/// which moments it will never be shown, and a clock that is a minute fast silently swallows a
/// minute of them.
pub fn acknowledge() -> Result<String, String> {
    let at = chrono::Utc::now().to_rfc3339();
    let path = mark_path();
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec(&Mark { at: at.clone() }).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, dir, &bytes)?;
    Ok(at)
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

    /// The digest is about **state**, and a restart is not the whole fleet moving at once.
    /// A fleet where nothing happens still tells the board the producer is turning.
    ///
    /// The board had no way to tell a calm fleet from a dead server: `lastTickAt` advanced only on
    /// box data, the stream sends none when nothing moves, and the re-sync floor is ten minutes. So
    /// "board is Ns stale — reconnecting…" was the ordinary state of a quiet afternoon, and the
    /// reconnects behind it were real — ten minutes of zero bytes is a connection anything in the
    /// path may drop.
    #[test]
    fn a_quiet_fleet_still_says_it_is_there_and_says_nothing_about_the_fleet() {
        let (_snapshot, mut rest) = subscribe();
        let same = vec![view("a", "live")];
        // The first publish is a real change — the box was not there a moment ago — so it is the
        // baseline, not the case under test. Drained rather than tolerated, so "no box data" below
        // means exactly that.
        publish(same.clone());
        while rest.try_recv().is_ok() {}
        // Enough ticks to cross the cadence whatever the global counter started at, and few enough
        // that the channel (BEHIND) cannot lag this reader.
        for _ in 0..ALIVE_EVERY + 1 {
            publish(same.clone());
        }
        let mut heard = Vec::new();
        while let Ok(tick) = rest.try_recv() {
            heard.push(tick);
        }
        assert!(
            heard.iter().any(|t| matches!(t, Tick::Alive)),
            "a quiet fleet sent nothing at all, so a board watching it cannot tell it from a dead \
             server: {heard:?}"
        );
        // And still no box data — the silence about the FLEET is the property that made transitions
        // worth having, and a heartbeat must not quietly undo it.
        assert!(
            !heard.iter().any(
                |t| matches!(t, Tick::Changed { boxes, gone } if !boxes.is_empty() || !gone.is_empty())
            ),
            "a quiet fleet sent box data: {heard:?}"
        );
    }

    /// The heartbeat arrives sooner than the board gives up, and the two numbers live in two files.
    ///
    /// `ALIVE_EVERY` is a count of ticks here; `STALE_AFTER_S` is seconds in the page. Equal is not
    /// enough — a heartbeat due exactly when the banner fires loses the race half the time, and the
    /// failure is a banner that flickers on a healthy fleet, which is the bug this pair exists to
    /// end. Read out of the page rather than restated, because a copy here is a second place to
    /// update and the drift would just move.
    #[test]
    fn the_heartbeat_arrives_before_the_board_calls_the_server_dead() {
        let page = include_str!("web/index.html");
        let stated = page
            .lines()
            .find_map(|line| line.trim().strip_prefix("const STALE_AFTER_S = "))
            .and_then(|rest| rest.trim_end_matches(';').parse::<u64>().ok())
            .expect(
                "the page no longer declares `const STALE_AFTER_S = <n>;` — if it moved, this test \
                 has to follow it, because nothing else keeps these two numbers in step",
            );
        let beat = ALIVE_EVERY * TICK.as_secs();
        assert!(
            beat * 2 <= stated,
            "the producer says it is alive every {beat}s and the board calls it dead after \
             {stated}s. One heartbeat of margin is not margin: a tick that is late, a client that \
             is briefly busy, and the banner fires on a fleet where nothing is wrong."
        );
    }

    #[test]
    fn the_journal_remembers_a_state_change_and_not_a_restart() {
        let journal = Mutex::new(std::collections::VecDeque::new());
        let latest = Mutex::new(Vec::new());

        // Nothing to compare against yet. Calling every box a transition here would make restarting
        // skein look like a night's worth of activity.
        remember(&journal, &latest, &[view("a", "working")]);
        assert!(
            journal.lock().unwrap().is_empty(),
            "a producer's first tick reported the fleet as having just changed"
        );

        // The same rule covers a box that has just been created: it arrived, it did not move.
        *latest.lock().unwrap() = vec![view("a", "working")];
        remember(
            &journal,
            &latest,
            &[view("a", "working"), view("new", "live")],
        );
        assert!(
            journal.lock().unwrap().is_empty(),
            "a box that was created was reported as having changed state"
        );

        *latest.lock().unwrap() = vec![view("a", "working"), view("b", "live")];
        remember(&journal, &latest, &[view("a", "done"), view("b", "live")]);
        let seen: Vec<Moment> = journal.lock().unwrap().iter().cloned().collect();
        assert_eq!(seen.len(), 1, "an unchanged box was written down: {seen:?}");
        assert_eq!(
            (
                seen[0].name.as_str(),
                seen[0].from.as_str(),
                seen[0].to.as_str()
            ),
            ("a", "working", "done")
        );

        // Bounded: a month of uptime is not a memory leak with a story.
        for n in 0..(REMEMBERED + 50) {
            *latest.lock().unwrap() = vec![view("a", &format!("s{n}"))];
            remember(&journal, &latest, &[view("a", &format!("s{}", n + 1))]);
        }
        assert_eq!(journal.lock().unwrap().len(), REMEMBERED);
    }

    /// An unknown mark shows everything rather than nothing.
    ///
    /// The two are not interchangeable: a digest that silently shows nothing is indistinguishable
    /// from a quiet night, and only one of them is true.
    #[test]
    fn a_mark_that_cannot_be_read_shows_the_night_rather_than_hiding_it() {
        let journal = Mutex::new(std::collections::VecDeque::new());
        let latest = Mutex::new(vec![view("a", "working")]);
        remember(&journal, &latest, &[view("a", "done")]);
        let all: Vec<Moment> = journal.lock().unwrap().iter().cloned().collect();
        assert_eq!(all.len(), 1);

        let filtered = |mark: &str| match chrono::DateTime::parse_from_rfc3339(mark) {
            Err(_) => all.clone(),
            Ok(m) => all
                .iter()
                .filter(|x| chrono::DateTime::parse_from_rfc3339(&x.at).is_ok_and(|w| w > m))
                .cloned()
                .collect(),
        };
        assert_eq!(filtered("").len(), 1, "an empty mark hid the night");
        assert_eq!(filtered("not a time").len(), 1);
        // And a mark from after everything shows nothing, which is the honest empty.
        let later = (chrono::Utc::now() + chrono::Duration::seconds(5)).to_rfc3339();
        assert_eq!(filtered(&later).len(), 0);
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
