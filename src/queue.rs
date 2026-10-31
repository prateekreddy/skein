//! One queue, many sources: everything that stopped and is waiting on you (§11).
//!
//! # Why one list
//!
//! A pull request awaiting your review and a box awaiting your answer are the same thing to the
//! person looking at them — work that stopped and is waiting on them. They are two lists today only
//! because they were built separately, and merging them in a template would leave **two orderings
//! that disagree**: whichever list you looked at first would decide what seemed most urgent.
//!
//! So the merge is here, server-side, and there is one comparator.
//!
//! # The rule: rank by what a row is waiting for, never by which subsystem produced it
//!
//! Easy to say and easy to get wrong, and there is a concrete trap in this exact merge. A box's
//! turn-state `waiting` means **waiting on you** — the agent asked something and stopped. A pull
//! request's [`crate::prq::Lane::Waiting`] means **waiting on everyone else** — you have already
//! reviewed it and it moves without you. The same word, opposite meanings, and a merge that mapped
//! them onto each other because they matched would put the thing you have finished with above the
//! thing that is asking you a question.
//!
//! [`Need`] is the ladder both are mapped onto, with the argument for each mapping written at the
//! arm rather than implied by a number.
//!
//! # Not on the board's tick
//!
//! `signal.rs` declares what a board tick spends and `tests/board_cost.rs` measures it. This is a
//! surface function, asked when a surface renders, and the pull-request half comes from
//! [`crate::prq::queue`]'s own 60-second cache — so a queue asked every two seconds still reaches
//! GitHub once a minute per repo, and asking it never adds a fork to the tick. That is why it lives
//! beside `load_views` rather than inside it.

use crate::board::BoxView;
use crate::prq::{Lane, Pr};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What a row is waiting for, which is the only thing its position depends on.
///
/// Ordered: lower needs you sooner. The numbers are the box tiers `board::load_views` already sorts
/// by, so a board and a queue cannot disagree about a box — and the pull-request mappings are
/// arguments rather than coincidences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Need {
    /// It is asking you something, or it broke. Nothing happens until you answer.
    You = 0,
    /// It finished, or it is waiting for you to look. Nothing is burning; something is owed.
    YourAttention = 1,
    /// Finished work you have not acknowledged.
    Done = 2,
    /// The machine is working. Nothing is owed.
    Machine = 3,
    /// Over, and nobody is waiting.
    Quiet = 4,
    /// Not talking, or set aside.
    Gone = 5,
}

/// What produced a row. Carried for rendering and for filtering — **never** for ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Box,
    PullRequest,
    /// A failing health check.
    ///
    /// In the **same** queue rather than a panel of its own, because a fleet whose substrate will
    /// not install is not a quiet fleet — it is a fleet that needs you, and putting that somewhere
    /// else is how a first run looks like nothing happening. §11.6 makes the same point about the
    /// first-run surface: the blocked state is the board, not a different page.
    Setup,
    /// A box's question for you, asked with `skein_ask_person` (`crate::asks`).
    ///
    /// Its own arm rather than a box row, because it is not the box's turn state: the box goes on
    /// working while it waits, and the question is a file on its own, answered on its own.
    Question,
    /// A box asking for a package to be installed for the fleet (`crate::substrate`).
    Package,
    /// A box asking to write to a repository that is not its own (`crate::gitgate`).
    GitWrite,
    /// A box asking for a bigger disk allowance (`crate::diskask`), approved or denied on the row
    /// itself: there is no panel for it, and the box-settings field is the same setting.
    Disk,
}

/// One thing waiting on you.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub source: Source,
    pub need: Need,
    /// The repo this belongs to. Empty for a box that belongs to none.
    ///
    /// Grouping by it is not decoration: at two repos it is, at eight it *is* the board.
    pub repo: String,
    /// A box's name, or `#<number>` for a pull request.
    pub name: String,
    /// One line saying what it is.
    pub headline: String,
    /// The state word a surface renders — a box's, or a lane.
    pub state: String,
    /// How long it has been in this state, in seconds, when that is knowable.
    ///
    /// The tie-break inside a rank, oldest first: the thing that has been waiting longest is the
    /// thing most likely to have been forgotten. `None` sorts last, because "we do not know how long"
    /// is not evidence of urgency.
    pub waiting_secs: Option<i64>,
    /// Where a surface sends someone who clicks it.
    pub url: String,
    /// What would clear it, when the row is something a person fixes rather than answers.
    ///
    /// Carried on the row rather than looked up, for the reason `health::unsatisfied` takes it as an
    /// argument: a fault with no way out cannot be written without noticing, and a fault whose way
    /// out is on another screen may as well not have one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
    /// Which dialog is holding a box, from `BoxView::blocked_kind`: `question` is the agent's own
    /// question on its screen, which only its terminal can answer. Empty on every other row.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blocked: String,
    /// The box's own reason for a request, in its own words. Shown escaped, as plain text, and
    /// never as the cockpit's: nothing checked it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// What a disk ask's Approve and Deny send back: the ask the row was drawn from. Absent on
    /// every other row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskRef>,
}

/// The ask a disk row was drawn from, echoed back by its buttons (`diskask::decide`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiskRef {
    pub id: String,
    pub mb: u64,
}

/// What the board is, as one value.
///
/// **Three states most tools botch** (§11.3), and they are three because they need different words,
/// not because a count happens to be zero. A dashboard that looks the same whether or not anything
/// is wrong has failed at its only job — so "nothing needs you" is a state to render, not an empty
/// list to fall through to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "standing", rename_all = "kebab-case")]
pub enum Standing {
    /// Something is not set up, and until it is, the fleet cannot do the thing you came for. First,
    /// because everything below it is downstream of this being wrong.
    SetupIncomplete { faults: usize },
    /// Work has stopped and is waiting on you.
    NeedsYou { rows: usize },
    /// Nothing is waiting. **Said, not implied.**
    Calm,
}

/// The board's standing, from the queue it would render.
///
/// Derived from the rows rather than computed beside them, so the headline and the list cannot
/// disagree — which is the failure a separate "all clear" banner invites.
pub fn standing(rows: &[Row]) -> Standing {
    let faults = rows.iter().filter(|r| r.source == Source::Setup).count();
    if faults > 0 {
        return Standing::SetupIncomplete { faults };
    }
    match rows
        .iter()
        .filter(|r| matches!(r.need, Need::You | Need::YourAttention))
        .count()
    {
        0 => Standing::Calm,
        rows => Standing::NeedsYou { rows },
    }
}

/// A box's rank, which is the tier the board already computed.
///
/// Read from `BoxView::tier` rather than re-derived, because two ladders for one box is exactly the
/// disagreement this module exists to prevent — and the board's is the one people have been reading.
fn need_of_box(view: &BoxView) -> Need {
    match view.tier {
        0 => Need::You,
        1 => Need::YourAttention,
        2 => Need::Done,
        3 => Need::Machine,
        4 => Need::Quiet,
        _ => Need::Gone,
    }
}

/// A pull request's rank, argued rather than matched.
fn need_of_pr(pr: &Pr) -> Need {
    match pr.lane {
        // Your review is wanted and you have not given one on this head. Work that stopped, waiting
        // on you — the same thing a box asking a question is, which is the whole premise of one list.
        Lane::NeedsYou => Need::You,
        // **Not `YourAttention`, despite the name.** You have decided; it moves without you now.
        // A box's `waiting` means waiting *on you* and a PR's means waiting on everyone else, and
        // mapping them together because they share a word would put what you have finished with
        // above what is asking you a question.
        Lane::Waiting => Need::Machine,
        // Not ready for review — a draft, failing checks, a conflict. Its author is still moving;
        // machine-quiet, like Waiting, because nothing here waits on you.
        Lane::NotReady => Need::Machine,
        // You set it aside for a reason skein cannot know. Still there, deliberately quiet.
        Lane::Archived => Need::Gone,
    }
}

/// Everything waiting on you, most urgent first.
///
/// The comparator is `(need, how long it has waited, name)` and nothing else. Source is not in it,
/// which is the point: two rows that need you equally sort by how long they have been ignored, not
/// by which half of skein produced them.
pub fn who_needs_you() -> Vec<Row> {
    // Setup first, and in the same list. A failing check is not a footnote beside the work — it is
    // the reason there is no work, and it carries the line that clears it.
    let mut rows: Vec<Row> = crate::health::health_report()
        .checks()
        .into_iter()
        .filter(|(_, check)| check.is_fault())
        .map(|(name, check)| Row {
            source: Source::Setup,
            need: Need::You,
            repo: String::new(),
            name: name.to_string(),
            headline: check.detail.clone(),
            state: "unsatisfied".into(),
            // A check has not been "waiting" — it is simply wrong, and inventing an age for it would
            // put it in the tie-break against boxes on a number that means nothing.
            waiting_secs: None,
            url: "/#health".into(),
            fix: check.fix.clone(),
            blocked: String::new(),
            disk: None,
            why: String::new(),
        })
        .collect();

    let views = crate::board::load_views();
    let repo_of: HashMap<String, String> = views
        .iter()
        .map(|v| (v.name.clone(), v.repo.clone()))
        .collect();
    rows.extend(views.iter().map(box_row));
    // The decision inbox: every request a box made of you, in the same list and ranked by the same
    // comparator. Each of the three reads its queue through the function its own panel reads, so
    // the inbox and the panel cannot disagree about what is waiting.
    let (asks, packages, writes, disks) =
        lock_decision_lists().get(Instant::now(), DECISIONS_FRESH, || {
            (
                crate::asks::fleet_asks(),
                crate::substrate::fleet_requests(),
                crate::gitgate::fleet_requests(),
                crate::diskask::with_figures(crate::diskask::fleet_asks()),
            )
        });
    rows.extend(decisions(asks, packages, writes, &repo_of));
    rows.extend(disk_decisions(disks, &repo_of));

    for repo in crate::repos::load_repos() {
        if !repo.review_queue {
            continue;
        }
        // `false`: the cached queue, on its own 60-second cadence. Forcing here would put a GitHub
        // round trip on whatever cadence a surface happens to render at.
        let Ok(queue) = crate::prq::queue(&repo, false) else {
            continue;
        };
        rows.extend(queue.prs.into_iter().map(|pr| {
            Row {
                source: Source::PullRequest,
                need: need_of_pr(&pr),
                repo: repo.id.clone(),
                name: format!("#{}", pr.number),
                headline: pr.title.clone(),
                state: match pr.lane {
                    Lane::NeedsYou => "needs-review",
                    Lane::Waiting => "reviewed",
                    Lane::NotReady => "not-ready",
                    Lane::Archived => "set-aside",
                }
                .to_string(),
                waiting_secs: seconds_since(&pr.updated_at),
                url: pr.url.clone(),
                fix: String::new(),
                blocked: String::new(),
                disk: None,
                why: String::new(),
            }
        }));
    }

    rows.sort_by(|a, b| {
        a.need
            .cmp(&b.need)
            // Longest-waiting first, and unknown last: "we do not know how long" is not evidence of
            // urgency, and putting it first would make every row with a missing timestamp shout.
            .then(waited(b).cmp(&waited(a)))
            .then(a.name.cmp(&b.name))
    });
    rows
}

/// How long the three request queues read for the decision inbox stay good.
///
/// Each read is an exec into the sandbox, and `/api/queue` is asked by `/v2` on every stream event
/// as well as by the board's own poll — so without this, a busy stream multiplied three execs by
/// every event. Five seconds is shorter than any panel's poll, so a request is never later on the
/// inbox than it is on its own panel; and a decision recorded through the cockpit clears it at once
/// ([`forget_decisions`]), so an answered card does not linger for the window.
pub const DECISIONS_FRESH: Duration = Duration::from_secs(5);

type DecisionLists = (
    Vec<crate::asks::Ask>,
    Vec<crate::substrate::Request>,
    Vec<crate::gitgate::Request>,
    Vec<DiskAsk>,
);

/// A disk ask with the two figures its row prints (`diskask::with_figures`).
pub(crate) type DiskAsk = (crate::diskask::Ask, Option<u64>, Option<u64>);

/// One row per disk ask the owner can be asked now: waiting, and above the allowance the box has
/// ([`crate::diskask::open_above`]). [`Need::You`], for the other requests' reason, and its row
/// is the whole sentence the owner decides on.
pub(crate) fn disk_decisions(asks: Vec<DiskAsk>, repo_of: &HashMap<String, String>) -> Vec<Row> {
    asks.into_iter()
        .filter(|(a, now, _)| crate::diskask::open_above(a, *now))
        .map(|(a, now, using)| Row {
            source: Source::Disk,
            need: Need::You,
            repo: repo_of.get(&a.box_name).cloned().unwrap_or_default(),
            headline: crate::diskask::headline(&a, now.unwrap_or_default(), using),
            state: "waiting".into(),
            waiting_secs: seconds_since(&a.asked),
            // The board, where its card carries the buttons: there is no panel to open.
            url: "/".into(),
            fix: String::new(),
            blocked: String::new(),
            why: a.why,
            disk: Some(DiskRef { id: a.id, mb: a.mb }),
            name: a.box_name,
        })
        .collect()
}

/// One value, read at most once per window.
///
/// The lock is held across the read, deliberately: two surfaces asking at once then share one read
/// instead of racing to make two.
struct Fresh<T> {
    held: Option<(Instant, T)>,
}

impl<T: Clone> Fresh<T> {
    const fn new() -> Self {
        Fresh { held: None }
    }

    fn get(&mut self, now: Instant, ttl: Duration, read: impl FnOnce() -> T) -> T {
        if let Some((at, value)) = &self.held {
            if now.saturating_duration_since(*at) < ttl {
                return value.clone();
            }
        }
        let value = read();
        self.held = Some((now, value.clone()));
        value
    }

    fn forget(&mut self) {
        self.held = None;
    }
}

static DECISION_LISTS: Mutex<Fresh<DecisionLists>> = Mutex::new(Fresh::new());

/// A panic in one read must not wedge the inbox for the life of the server: the value is replaced
/// whole or not at all, so a poisoned lock holds nothing half-written.
fn lock_decision_lists() -> std::sync::MutexGuard<'static, Fresh<DecisionLists>> {
    DECISION_LISTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drop the cached request queues, so the next `/api/queue` reads them again. Called by each route
/// that records a decision on one of them.
pub fn forget_decisions() {
    lock_decision_lists().forget();
}

/// A box's row, carrying which dialog holds it.
///
/// `blocked` travels so a surface can tell a box asking a question on its own screen from one that
/// merely stopped: the first is a decision, and only its terminal can take the answer.
fn box_row(view: &BoxView) -> Row {
    Row {
        source: Source::Box,
        need: need_of_box(view),
        repo: view.repo.clone(),
        name: view.name.clone(),
        headline: view.headline.clone().unwrap_or_default(),
        state: view.state.clone(),
        waiting_secs: view.age_secs,
        url: format!("/#box={}", view.name),
        fix: String::new(),
        blocked: view.blocked_kind.clone(),
        disk: None,
        why: String::new(),
    }
}

/// Every request a box has made of you that nobody has answered, one row each.
///
/// **What counts as waiting is the host's word, never the box's.** Each list arrives as its own
/// queue's reader returns it — `asks::list`, `substrate::list`, `gitgate::list` — and each of those
/// has already put the host's record over the box's file and called a request with no record
/// waiting or pending, whatever its file says. So this reads `state` as decided, and filters on
/// it, and does nothing else with it.
///
/// All three are [`Need::You`]: nothing happens on any of them until a person answers. A browser
/// fetch (`kind == "playwright"`) is left out, because it waits on skein rather than on you — the
/// package panel's badge leaves it out for the same reason.
///
/// `waiting_secs` is from `asked`, which the box wrote. It decides order only inside this rank, so
/// a box that back-dates its ask can put it first among the things waiting on you, and no higher.
pub(crate) fn decisions(
    asks: Vec<crate::asks::Ask>,
    packages: Vec<crate::substrate::Request>,
    writes: Vec<crate::gitgate::Request>,
    repo_of: &HashMap<String, String>,
) -> Vec<Row> {
    let repo = |name: &str| repo_of.get(name).cloned().unwrap_or_default();
    let mut rows = Vec::new();
    rows.extend(
        asks.into_iter()
            .filter(|a| a.state == "waiting")
            .map(|a| Row {
                source: Source::Question,
                need: Need::You,
                repo: repo(&a.box_name),
                headline: a.question,
                state: "waiting".into(),
                waiting_secs: seconds_since(&a.asked),
                url: "/#askq".into(),
                fix: String::new(),
                blocked: String::new(),
                disk: None,
                why: String::new(),
                name: a.box_name,
            }),
    );
    rows.extend(
        packages
            .into_iter()
            .filter(|r| r.is_pending() && r.kind != "playwright")
            .map(|r| Row {
                source: Source::Package,
                need: Need::You,
                repo: repo(&r.box_name),
                headline: format!("{}: {}", r.kind, r.packages.join(" ")),
                state: "pending".into(),
                waiting_secs: seconds_since(&r.asked),
                url: "/#subq".into(),
                fix: String::new(),
                blocked: String::new(),
                disk: None,
                why: r.why,
                name: r.box_name,
            }),
    );
    rows.extend(writes.into_iter().filter(|r| r.is_pending()).map(|r| Row {
        source: Source::GitWrite,
        need: Need::You,
        repo: repo(&r.box_name),
        headline: r.repo,
        state: "pending".into(),
        waiting_secs: seconds_since(&r.asked),
        url: "/#gitq".into(),
        fix: String::new(),
        blocked: String::new(),
        disk: None,
        why: r.reason,
        name: r.box_name,
    }));
    rows
}

/// How long a row has waited, for comparison only. `None` becomes the smallest, so it sorts last
/// under the descending comparison above.
fn waited(row: &Row) -> i64 {
    row.waiting_secs.unwrap_or(i64::MIN)
}

fn seconds_since(rfc3339: &str) -> Option<i64> {
    let at = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    Some((chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(name: &str, tier: u8, secs: Option<i64>) -> Row {
        Row {
            source: Source::Box,
            need: need_of_box(&BoxView {
                tier,
                ..Default::default()
            }),
            repo: "r".into(),
            name: name.into(),
            headline: String::new(),
            state: String::new(),
            waiting_secs: secs,
            url: String::new(),
            fix: String::new(),
            blocked: String::new(),
            disk: None,
            why: String::new(),
        }
    }

    fn pull(number: u64, lane: Lane, secs: Option<i64>) -> Row {
        Row {
            source: Source::PullRequest,
            need: need_of_pr(&Pr {
                title: String::new(),
                author: String::new(),
                head_ref: String::new(),
                base_ref: String::new(),
                checks: String::new(),
                my_review: String::new(),
                reasons: Vec::new(),
                lane,
                ..crate::prq::blank_pr(number, "")
            }),
            repo: "r".into(),
            name: format!("#{number}"),
            headline: String::new(),
            state: String::new(),
            waiting_secs: secs,
            url: String::new(),
            fix: String::new(),
            blocked: String::new(),
            disk: None,
            why: String::new(),
        }
    }

    fn ordered(mut rows: Vec<Row>) -> Vec<String> {
        rows.sort_by(|a, b| {
            a.need
                .cmp(&b.need)
                .then(waited(b).cmp(&waited(a)))
                .then(a.name.cmp(&b.name))
        });
        rows.into_iter().map(|r| r.name).collect()
    }

    /// **The trap this module exists for.** A box's `waiting` waits on you; a pull request's
    /// `Waiting` waits on everyone else. Mapping them together because they share a word would put
    /// the thing you have finished with above the thing that is asking you a question.
    #[test]
    fn a_pull_request_you_have_reviewed_does_not_outrank_a_box_asking_you_something() {
        assert_eq!(
            need_of_pr(&pull(1, Lane::Waiting, None).clone_pr()),
            Need::Machine
        );
        let rows = ordered(vec![
            pull(7, Lane::Waiting, Some(9_999)),
            boxed("asking", 0, Some(1)),
            boxed("also-waiting", 1, Some(1)),
        ]);
        assert_eq!(
            rows,
            vec!["asking", "also-waiting", "#7"],
            "a reviewed pull request outranked a box that stopped to ask a question"
        );
    }

    /// A review you owe and a box that stopped to ask are the same need, and sort together.
    ///
    /// This is the premise of one list: if they did not, the merge would be decoration over two
    /// queues that still disagree.
    #[test]
    fn a_review_you_owe_ranks_with_a_box_that_needs_an_answer() {
        assert_eq!(
            need_of_pr(&pull(1, Lane::NeedsYou, None).clone_pr()),
            Need::You
        );
        // Equal need, so the tie-break decides — and it is how long each has waited, not which half
        // of skein produced it.
        let rows = ordered(vec![
            boxed("newer-box", 0, Some(60)),
            pull(3, Lane::NeedsYou, Some(86_400)),
            boxed("older-box", 0, Some(3_600)),
        ]);
        assert_eq!(
            rows,
            vec!["#3", "older-box", "newer-box"],
            "the longest-waiting thing was not first"
        );
    }

    /// An unknown age does not shout. It is the absence of evidence, not evidence of urgency.
    #[test]
    fn a_row_with_no_known_age_sorts_last_within_its_rank() {
        let rows = ordered(vec![
            boxed("unknown", 0, None),
            boxed("recent", 0, Some(5)),
            pull(9, Lane::NeedsYou, Some(500)),
        ]);
        assert_eq!(rows, vec!["#9", "recent", "unknown"]);
    }

    /// **"Nothing needs you" is a state, not an empty list.**
    ///
    /// A dashboard that looks the same whether or not anything is wrong has failed at its only job,
    /// and the way that happens is the calm case being the absence of the busy one. So it is a value
    /// a surface has to render on purpose.
    #[test]
    fn a_quiet_fleet_says_so_rather_than_showing_nothing() {
        assert_eq!(standing(&[]), Standing::Calm);
        // A fleet full of working boxes is calm: the machine is busy and nothing is owed.
        assert_eq!(
            standing(&[boxed("working", 3, Some(1)), boxed("quiet", 4, Some(1))]),
            Standing::Calm
        );
        assert_eq!(
            standing(&[boxed("asking", 0, Some(1)), boxed("working", 3, Some(1))]),
            Standing::NeedsYou { rows: 1 }
        );
    }

    /// Setup outranks everything, because everything below it is downstream of it being wrong.
    ///
    /// And it is in the **same** queue: a fleet whose substrate will not install is not a quiet
    /// fleet, and putting that on another page is how a first run looks like nothing happening.
    #[test]
    fn a_failing_check_is_the_headline_and_carries_its_fix() {
        let mut broken = boxed("asking", 0, Some(1));
        broken.source = Source::Setup;
        broken.fix = "skein login claude".into();
        assert_eq!(
            standing(&[broken.clone(), boxed("asking", 0, Some(1))]),
            Standing::SetupIncomplete { faults: 1 },
            "a fleet that is not set up reported as merely busy"
        );
        assert!(
            !broken.fix.is_empty(),
            "a fault whose way out is on another screen may as well not have one"
        );
    }

    /// A box's rank is the board's tier, not a second ladder.
    ///
    /// Two ladders for one box is the disagreement this whole module exists to prevent, and the
    /// board's is the one people have been reading.
    #[test]
    fn a_boxs_rank_is_the_one_the_board_already_sorts_by() {
        for (tier, need) in [
            (0, Need::You),
            (1, Need::YourAttention),
            (2, Need::Done),
            (3, Need::Machine),
            (4, Need::Quiet),
            (5, Need::Gone),
            (9, Need::Gone),
        ] {
            assert_eq!(
                need_of_box(&BoxView {
                    tier,
                    ..Default::default()
                }),
                need,
                "tier {tier}"
            );
        }
    }

    /// One question as the list script prints it: the box stamped from its directory, and every
    /// other field the box's own.
    fn listed(id: &str, extra: serde_json::Value) -> String {
        let mut ask = serde_json::json!({
            "id": id,
            "box": "web-main",
            "question": "keep the table?",
            "options": ["Keep it", "Drop it"],
            "asked": "2026-10-30T10:00:00Z",
        });
        if let serde_json::Value::Object(more) = extra {
            ask.as_object_mut().unwrap().extend(more);
        }
        serde_json::json!([ask]).to_string()
    }

    fn inbox(asks: Vec<crate::asks::Ask>) -> Vec<Row> {
        let repo_of = HashMap::from([("web-main".to_string(), "web".to_string())]);
        decisions(asks, Vec::new(), Vec::new(), &repo_of)
    }

    /// **A question waiting on you is one row, and it needs you.**
    ///
    /// What would make it fail: the question mapped to any rank but [`Need::You`] — it would sort
    /// under boxes that merely finished — or not carried into the inbox at all.
    #[test]
    fn a_waiting_question_is_one_row_that_needs_you() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);

        let rows = inbox(crate::asks::shown(crate::asks::parse_asks(&listed(
            "q-00000001",
            serde_json::Value::Null,
        ))));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (rows[0].source, rows[0].need, rows[0].name.as_str()),
            (Source::Question, Need::You, "web-main"),
            "{rows:?}"
        );
        assert_eq!(rows[0].repo, "web", "grouped under no repo: {rows:?}");
        assert_eq!(rows[0].headline, "keep the table?");
    }

    /// **An answered question leaves the inbox.**
    ///
    /// What would make it fail: `decisions` keeping a question whatever the host says of it, which
    /// would leave a card asking for an answer the box already has.
    #[test]
    fn an_answered_question_leaves_the_inbox() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);

        let asked = crate::asks::shown(crate::asks::parse_asks(&listed(
            "q-00000002",
            serde_json::Value::Null,
        )));
        crate::asks::answer(&asked[0], &crate::asks::Reply::Answer("Keep it".into()))
            .expect("answered");
        let after = crate::asks::shown(crate::asks::parse_asks(&listed(
            "q-00000002",
            serde_json::Value::Null,
        )));
        assert_eq!(after[0].state, "answered", "the host's record was not read");
        assert!(
            inbox(after).is_empty(),
            "an answered question is still in the inbox"
        );
    }

    /// **A box that writes `answered` into its own ask is still asking.**
    ///
    /// The file is the box's, every byte. What would make it fail: the inbox believing the file's
    /// `state` for a question the host never answered — a box could then hide its own question, or
    /// a stale copy could, and the person would never see it.
    #[test]
    fn a_box_that_says_its_own_question_is_answered_still_shows() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);

        let forged = listed(
            "q-00000003",
            serde_json::json!({
                "state": "answered",
                "answer": "Drop it",
                "decided": "2026-10-30T10:01:00Z",
            }),
        );
        let rows = inbox(crate::asks::shown(crate::asks::parse_asks(&forged)));
        assert_eq!(
            rows.len(),
            1,
            "a box's own file took its question out of the inbox: {rows:?}"
        );
    }

    /// **Each kind of request reaches the inbox, and only while it waits on you.**
    ///
    /// What would make it fail: one of the two request queues not read into `decisions`; a decided
    /// request kept; a browser fetch, which waits on skein, shown as waiting on you; or the box's
    /// reason dropped, which is the one thing on the request only the box can say.
    #[test]
    fn every_kind_of_request_reaches_the_inbox_while_it_waits() {
        let package = |id: &str, kind: &str, state: &str| crate::substrate::Request {
            id: id.into(),
            box_name: "web-main".into(),
            kind: kind.into(),
            packages: vec!["libnss3".into()],
            why: "the browser tests need it".into(),
            asked: "2026-10-30T09:00:00Z".into(),
            state: state.into(),
            ..Default::default()
        };
        let write = |id: &str, state: &str| crate::gitgate::Request {
            id: id.into(),
            box_name: "web-main".into(),
            repo: "acme/docs".into(),
            reason: "the docs live there".into(),
            asked: "2026-10-30T08:00:00Z".into(),
            state: state.into(),
            ..Default::default()
        };
        let rows = decisions(
            Vec::new(),
            vec![
                package("p-1", "apt", "pending"),
                package("p-2", "apt", "denied"),
                package("p-3", "playwright", "pending"),
            ],
            vec![write("w-1", "pending"), write("w-2", "granted")],
            &HashMap::new(),
        );
        let got: Vec<(Source, &str, &str)> = rows
            .iter()
            .map(|r| (r.source, r.headline.as_str(), r.why.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (Source::Package, "apt: libnss3", "the browser tests need it"),
                (Source::GitWrite, "acme/docs", "the docs live there"),
            ]
        );
        assert!(rows.iter().all(|r| r.need == Need::You), "{rows:?}");
    }

    /// **A waiting disk ask above the box's allowance is one row, in the owner's words, carrying
    /// the ask its buttons send back; a decided one, a smaller one, and one from a box allowed the
    /// whole disk are not rows.**
    ///
    /// What would make it fail: `disk_decisions` not filtering through `diskask::open_above`, the
    /// headline not `diskask::headline`'s, or the row without its `disk` reference.
    #[test]
    fn a_disk_ask_is_one_row_with_its_figures_and_its_reference() {
        let ask = |b: &str, id: &str, mb: u64, state: &str| crate::diskask::Ask {
            id: id.into(),
            box_name: b.into(),
            size: format!("{mb}m"),
            mb,
            why: "the index is 14G".into(),
            asked: "2026-10-31T10:00:00Z".into(),
            state: state.into(),
            ..Default::default()
        };
        let rows = disk_decisions(
            vec![
                (
                    ask("web-main", "d-1", 20 * 1024, "waiting"),
                    Some(10 * 1024),
                    Some(12_902),
                ),
                (
                    ask("web-two", "d-2", 20 * 1024, "approved"),
                    Some(10 * 1024),
                    None,
                ),
                (
                    ask("web-three", "d-3", 8 * 1024, "waiting"),
                    Some(10 * 1024),
                    None,
                ),
                (ask("web-four", "d-4", 20 * 1024, "waiting"), None, None),
            ],
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        let r = &rows[0];
        assert_eq!(
            (r.source, r.need, r.name.as_str()),
            (Source::Disk, Need::You, "web-main")
        );
        assert_eq!(r.headline, "asks for 20 GB disk (now 10 GB, using 12.6 GB)");
        assert_eq!(r.why, "the index is 14G");
        assert_eq!(
            r.disk,
            Some(DiskRef {
                id: "d-1".into(),
                mb: 20 * 1024
            })
        );
    }

    /// **A box asking on its own screen says so on its row**, so a surface can send the person to
    /// its terminal rather than offer a click it cannot honour.
    ///
    /// What would make it fail: `box_row` dropping `blocked_kind`, which leaves the agent's own
    /// question indistinguishable from a box that finished.
    #[test]
    fn a_box_asking_on_its_own_screen_carries_the_dialog_kind() {
        let row = box_row(&BoxView {
            name: "web-main".into(),
            tier: 0,
            blocked_kind: "question".into(),
            ..Default::default()
        });
        assert_eq!((row.need, row.blocked.as_str()), (Need::You, "question"));
    }

    /// **A browser fetch is not a decision, and an apt request beside it is.**
    ///
    /// A `playwright` request waits on skein, which fetches it without the owner (SKEIN-1341).
    /// What would make it fail: the `kind != "playwright"` filter in `decisions` dropped or
    /// misspelled, which would put a card in front of the owner that nothing they press can answer.
    #[test]
    fn a_browser_fetch_is_not_put_in_front_of_the_owner() {
        let package = |id: &str, kind: &str, packages: &str| crate::substrate::Request {
            id: id.into(),
            box_name: "web-main".into(),
            kind: kind.into(),
            packages: vec![packages.into()],
            asked: "2026-10-30T09:00:00Z".into(),
            state: "pending".into(),
            ..Default::default()
        };
        let rows = decisions(
            Vec::new(),
            vec![
                package("p-1", "playwright", "chromium@1.56.1"),
                package("p-2", "apt", "jq"),
            ],
            Vec::new(),
            &HashMap::new(),
        );
        let heads: Vec<&str> = rows.iter().map(|r| r.headline.as_str()).collect();
        assert_eq!(heads, ["apt: jq"], "{rows:?}");
    }

    /// **Two asks inside the window read the queues once; one after it, or after a decision, reads
    /// again.**
    ///
    /// What would make it fail: the cache not consulted (every `/api/queue` execs three times), a
    /// window that never ends (a new request never shows), or `forget` not clearing it (an answered
    /// card lingers after the press).
    #[test]
    fn the_request_queues_are_read_once_per_window() {
        let reads = std::cell::Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            reads.get()
        };
        let mut fresh = Fresh::new();
        let t0 = Instant::now();
        let ttl = DECISIONS_FRESH;
        assert_eq!(fresh.get(t0, ttl, read), 1);
        assert_eq!(
            fresh.get(t0 + ttl / 2, ttl, read),
            1,
            "read twice inside the window"
        );
        assert_eq!(reads.get(), 1);
        assert_eq!(
            fresh.get(t0 + ttl, ttl, read),
            2,
            "not read again once the window ended"
        );
        fresh.forget();
        assert_eq!(
            fresh.get(t0 + ttl, ttl, read),
            3,
            "a forgotten value was served"
        );
    }

    impl Row {
        /// The `Pr` a test row stands for, so the mapping can be asserted directly.
        fn clone_pr(&self) -> Pr {
            Pr {
                title: String::new(),
                author: String::new(),
                head_ref: String::new(),
                base_ref: String::new(),
                checks: String::new(),
                my_review: String::new(),
                reasons: Vec::new(),
                lane: match self.need {
                    Need::You => Lane::NeedsYou,
                    Need::Gone => Lane::Archived,
                    _ => Lane::Waiting,
                },
                ..crate::prq::blank_pr(0, "")
            }
        }
    }
}
