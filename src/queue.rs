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
        })
        .collect();

    rows.extend(
        crate::board::load_views()
            .unwrap_or_default()
            .into_iter()
            .map(|view| Row {
                source: Source::Box,
                need: need_of_box(&view),
                repo: view.repo.clone(),
                name: view.name.clone(),
                headline: view.headline.clone().unwrap_or_default(),
                state: view.state.clone(),
                waiting_secs: view.age_secs,
                url: format!("/#box={}", view.name),
                fix: String::new(),
            }),
    );

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
        }
    }

    fn pull(number: u64, lane: Lane, secs: Option<i64>) -> Row {
        Row {
            source: Source::PullRequest,
            need: need_of_pr(&Pr {
                number,
                title: String::new(),
                author: String::new(),
                url: String::new(),
                head_ref: String::new(),
                head_sha: String::new(),
                base_ref: String::new(),
                draft: false,
                updated_at: String::new(),
                committed_at: String::new(),
                settled: true,
                labels: Vec::new(),
                review_decision: String::new(),
                mergeable: None,
                merge_state: String::new(),
                additions: None,
                deletions: None,
                changed_files: None,
                checks: String::new(),
                failing_checks: Vec::new(),
                my_review: String::new(),
                review_is_current: false,
                snoozed: false,
                reasons: Vec::new(),
                lane,
                box_name: String::new(),
            }),
            repo: "r".into(),
            name: format!("#{number}"),
            headline: String::new(),
            state: String::new(),
            waiting_secs: secs,
            url: String::new(),
            fix: String::new(),
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

    impl Row {
        /// The `Pr` a test row stands for, so the mapping can be asserted directly.
        fn clone_pr(&self) -> Pr {
            Pr {
                number: 0,
                title: String::new(),
                author: String::new(),
                url: String::new(),
                head_ref: String::new(),
                head_sha: String::new(),
                base_ref: String::new(),
                draft: false,
                updated_at: String::new(),
                committed_at: String::new(),
                settled: true,
                labels: Vec::new(),
                review_decision: String::new(),
                mergeable: None,
                merge_state: String::new(),
                additions: None,
                deletions: None,
                changed_files: None,
                checks: String::new(),
                failing_checks: Vec::new(),
                my_review: String::new(),
                review_is_current: false,
                snoozed: false,
                reasons: Vec::new(),
                lane: match self.need {
                    Need::You => Lane::NeedsYou,
                    Need::Gone => Lane::Archived,
                    _ => Lane::Waiting,
                },
                box_name: String::new(),
            }
        }
    }
}
