//! How much reading a day may cost, and who is allowed to spend it.
//!
//! Fleet-wide, per UTC day, counted at the model call and nowhere else. The whole boundary is
//! [`Trigger`]: skein's own initiative is checked and counted, a person pressing a button is
//! neither. That is the owner's rule verbatim — "Limit is only for automatic stuff, manually I can
//! invoke as many as I want" — and it is a named type rather than a bool because a call site that
//! got it backwards would either ration the person or hand the pump an unlimited allowance.

use super::summary::Summary;
use crate::prq::Pr;
use std::fs;
use std::path::PathBuf;

// ───────────────────────────── the day's analysis budget ─────────────────────────────
//
// FLEET-WIDE, per UTC day, counted HERE — at the model call — and nowhere else. One unit is one
// pull request ANALYSED: a `(number, head_sha)` that actually reached a model, whether that visit
// produced a one-liner, a brief, or a brief plus a drafted review. A cache hit is zero. The
// ceiling and its default (100) live on `Config::review_reads_per_day` — the owner's own number:
// "not more than 100 PRs a day (cache misses, actual analysis)".
//
// The client used to keep this budget (`REV_SUM_AUTO`/`revSumAuto` in index.html), and it was
// wrong twice over, in ways nobody should reintroduce:
//
// 1. **It counted REQUESTS, not model calls.** A request answered from the disk cache costs
//    nothing, so a page reload re-requested the six newest rows, took six free cache hits, and the
//    allowance was gone — rows seven onward were never read, on any reload, for ever. A limit on
//    spending has to count spending, and only the server knows whether a request reached a model.
// 2. **A button press reset it.** Every approve and set-aside reloaded the pane, and the reload
//    handed out a fresh allowance — one approve authorised six more reads (measured 6→12), thirty
//    acts a day up to 180 stage-1 calls, with no ceiling, while the reload-only reviewer got
//    nothing. A budget the client holds is a budget any client action can refill.
//
// So the ledger lives on disk beside the per-repo summary dirs, keyed by day with the per-repo
// attribution kept inside it (cheap, and it says where the money went), and every path that is
// about to spend a model call — the summary read, the drafted review, asked-for or background —
// checks the same number. One budget, not two. The file self-prunes: writing today's count drops
// every other day's key, so it never grows past one entry.
//
// **Checked and counted in ONE locked closure** — [`reserve_a_read`]. This paragraph used to argue
// the other way ("unlocked read-modify-write, deliberately … a lock here would buy precision
// nothing needs"), and the overshoot it waved away is not hypothetical: the pane's own pump sends
// three unasked requests at once, so three visits read the same under-ceiling number and each
// bought a model call. It is deleted rather than corrected because a comment that argues for a bug
// outlives the bug.

/// The one ledger, above the per-repo dirs — the budget is the fleet's, not a repo's.
pub(super) fn spend_path() -> PathBuf {
    crate::config::skein_home()
        .join("review")
        .join("reads-spent.json")
}

/// Today's key. UTC, so the budget resets at the same moment for everyone and a test can name a
/// day instead of sleeping through midnight.
pub(super) fn utc_day() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The ceiling, from settings. See `Config::review_reads_per_day` for the default and its why.
pub(super) fn reads_per_day() -> u32 {
    crate::config::load_config().review_reads_per_day
}

/// day → repo → analyses. The inner map is attribution, the budget is the day's SUM.
pub(super) type SpendLedger =
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, u32>>;

pub(super) fn spend_ledger() -> SpendLedger {
    fs::read_to_string(spend_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// How many pull requests the whole fleet has analysed on `day`.
pub(super) fn reads_spent(day: &str) -> u32 {
    spend_ledger()
        .get(day)
        .map(|repos| repos.values().sum())
        .unwrap_or(0)
}

/// The honest-absence sentence a budget-stopped row carries, shown verbatim by the pane — and it
/// INVITES the manual trigger, per the owner: "When limit is hit, surface and ask me to manually
/// trigger these." The machine-readable marker beside it is [`Summary::budget_stopped`], which
/// the pane uses to render the read button prominently.
pub(super) fn budget_spent_because(spent: u32, budget: u32) -> String {
    format!(
        "today's automatic reading budget is spent ({spent}/{budget}) — press read to analyse \
         this one now; the budget resets at midnight UTC."
    )
}

/// The ceiling's one comparison. [`over_budget`] asks it of an unlocked read and [`reserve_a_read`]
/// asks it with the lock held; a repo where those two disagreed would refuse a row on one screen
/// and charge it on the next.
pub(super) fn at_the_ceiling(spent: u32, budget: u32) -> Option<String> {
    (spent >= budget).then(|| budget_spent_because(spent, budget))
}

/// The row a budget refusal produces: [`budget_spent_because`]'s sentence, plus the
/// machine-readable [`Summary::budget_stopped`] the pane keys the prominent read button on — and
/// that button comes back [`Trigger::Asked`], un-budgeted. `computed` stays false, because saying
/// no cost nothing: the day is not charged for the refusal, and the button can ask tomorrow.
///
/// One function because the refusal has two doors — the cheap look before the GitHub reads, and
/// [`reserve_a_read`] at the model call — and a row that came back from one of them without the
/// marker would be a dead button on a page that looks exactly the same.
pub(super) fn budget_stopped_row(pr: &Pr, because: &str) -> Summary {
    let mut said = Summary::unread(pr.number, &pr.head_sha, because);
    said.budget_stopped = true;
    said
}

/// Who wants this pull request analysed. **The budget's whole boundary**, so it is a named type
/// rather than a bool a call site can get backwards.
///
/// The owner's rule, verbatim: "Limit is only for automatic stuff, manually I can invoke as many
/// as I want." The ceiling is on skein's INITIATIVE, never on the person — so an [`Asked`] visit
/// (the read button, the draft button, a `force` re-read, any user-initiated route) neither
/// checks the counter nor increments it: a manual call must never be refused for budget, and
/// must never eat the automatic allowance. Only [`Unasked`] work — the background pass, the
/// pane's pump asking for rows nobody clicked — pays from and is stopped by the day's ledger.
///
/// A future route defaults the safe way round: the HTTP layer treats a request as `Unasked`
/// unless it explicitly carries the asked marker, so forgetting the marker gates a button rather
/// than un-gating a sweep.
///
/// [`Asked`]: Trigger::Asked
/// [`Unasked`]: Trigger::Unasked
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A person pressed something. No check, no count.
    Asked,
    /// Skein's own idea. Checked against the ledger, and counted into it.
    Unasked,
}

/// Is the day's budget already spent — for UNASKED work? Asked BEFORE the GitHub reads, so a row
/// the ceiling is going to refuse costs no HTTP to refuse.
///
/// **Advisory, and deliberately so: the binding answer is [`reserve_a_read`]'s**, taken together
/// with the unit at the moment of the model call. This one can say "there is room" and the room be
/// gone by the time the diff has downloaded — the reservation then refuses, and the row carries
/// the same sentence and the same marker. It cannot go wrong the other way: a day's count only
/// goes up, and both read the same ceiling from the same settings, so an early no is never a no
/// the reservation would have overturned.
///
/// An [`Trigger::Asked`] visit is never over budget by definition: the ceiling is on skein's
/// initiative, not on the person.
pub(super) fn over_budget(trigger: Trigger, day: &str) -> Option<String> {
    if trigger == Trigger::Asked {
        return None;
    }
    at_the_ceiling(reads_spent(day), reads_per_day())
}

/// **Take one unit of the day's budget, or say why it cannot be taken** — the check and the count
/// in ONE closure, under one lock, so what the caller is told is what was true when the unit was
/// taken.
///
/// **Why one closure** (REV-6). The increment has been locked since 2026-09-03; the *check* stayed
/// where it had always been, an unlocked read one screen away, with the whole reading — a diff
/// download, a model call, seconds to minutes — in between. The pane's pump sends
/// `REV_SUM_PARALLEL = 3` unasked requests at once, so three visits read the same under-ceiling
/// number, all three passed, and the day overshot by the number in flight. `update_json_lossy`
/// holds an exclusive lock across the read, the compare and the write (`util::update_json`), so
/// exactly one of them can take the last unit.
///
/// **Where it is called is the money boundary, and it did not move**: after every free refusal —
/// the cache hit, the switched-off repo, the diff GitHub would not hand over — and immediately
/// before the model is asked. The unit is "a pull request that actually reached a model", so a
/// visit that fell over on the way there is charged nothing, and a call that reaches the model and
/// then fails is charged one. Both lanes take the same single unit: the unit is the pull request
/// analysed, not the number of things the analysis produced.
///
/// `update_json_lossy` rather than `update_json`, and the choice is argued rather than convenient:
/// a spend ledger that will not parse must not stop the day's readings, and its contents are a
/// counter that resets at midnight UTC — the same argument `attempt`'s lease makes, which is the
/// only other caller of the lossy variant. Refusing here would jam the review queue on a file
/// nobody looks at. A ledger that cannot be WRITTEN lets the reading through for the same reason,
/// and that is what the unlocked pair did too: a broken disk must not become a fleet-wide stop on
/// reading.
///
/// An [`Trigger::Asked`] visit takes nothing and is refused nothing — the owner's rule: "Limit is
/// only for automatic stuff, manually I can invoke as many as I want."
///
/// Every other day's key is dropped while the lock is held. The file is this day's tally, not a
/// history, and pruning on write is what keeps it one entry for ever.
pub(super) fn reserve_a_read(trigger: Trigger, repo_id: &str, day: &str) -> Option<String> {
    if trigger == Trigger::Asked {
        return None;
    }
    // Read outside the closure: this lock guards the ledger, and taking the settings' own reader
    // under it would put two files' locks in one order that nothing else promises to keep.
    let budget = reads_per_day();
    crate::util::update_json_lossy(&spend_path(), |all: &mut SpendLedger| {
        let spent: u32 = all.get(day).map(|repos| repos.values().sum()).unwrap_or(0);
        if let Some(because) = at_the_ceiling(spent, budget) {
            return Ok(Some(because));
        }
        all.retain(|k, _| k == day);
        *all.entry(day.to_string())
            .or_default()
            .entry(repo_id.to_string())
            .or_insert(0) += 1;
        Ok(None)
    })
    .unwrap_or(None)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::testkit::*;
    use crate::review::visit::*;
    use crate::review::{cache::*, scope::*, summary::*};

    // ─────────────────── the day's analysis budget ───────────────────

    /// The ledger's own arithmetic, no model anywhere near it: units accumulate across repos into
    /// ONE fleet-wide day (spend in one repo counts against every repo — the owner's ceiling is
    /// "100 PRs a day", not per anything), the day rolling over restores the full allowance
    /// without waiting for it, and writing a new day prunes the old one out of the file.
    #[test]
    fn the_budget_is_one_fleet_wide_ledger_that_resets_by_day_key() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Seeded through the production door — `reserve_a_read` is the only way a unit is taken
        // — and every one of these must have been granted, or the arithmetic below is measuring
        // a refusal rather than a count.
        for (repo, day) in [
            ("repo-a", "2026-08-23"),
            ("repo-a", "2026-08-23"),
            ("repo-b", "2026-08-23"),
        ] {
            assert!(
                reserve_a_read(Trigger::Unasked, repo, day).is_none(),
                "the default ceiling refused a unit three readings in"
            );
        }
        assert_eq!(
            reads_spent("2026-08-23"),
            3,
            "two repos' analyses sum into the one fleet-wide day"
        );
        // The rollover, by key — no sleeping through midnight.
        assert_eq!(
            reads_spent("2026-08-24"),
            0,
            "a new day starts with the whole allowance"
        );
        assert!(reserve_a_read(Trigger::Unasked, "repo-a", "2026-08-24").is_none());
        assert_eq!(reads_spent("2026-08-24"), 1);
        let raw = std::fs::read_to_string(spend_path()).unwrap();
        assert!(
            !raw.contains("2026-08-23"),
            "writing a new day must prune the old one — the file is a tally, not a history: {raw}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A ceiling of three is three, however many readings start at once** (REV-6).
    ///
    /// The check and the count used to be two acts with the whole reading between them:
    /// `over_budget` read the ledger, the diff downloaded, the model answered, and only then was
    /// the unit counted. The pane's pump sends `REV_SUM_PARALLEL = 3` unasked requests together,
    /// so three visits could read the same under-ceiling number, all three pass, and all three buy
    /// a model call the ceiling had one unit left for.
    ///
    /// Eight threads released from one barrier ask [`reserve_a_read`] for a unit of a budget of
    /// three. It is the real mechanism and real contention — eight OS threads on one file lock,
    /// not an assertion that a lock exists — and the property holds under EVERY interleaving,
    /// because the compare and the increment happen inside one `update_json_lossy` closure. Both
    /// halves are asserted: what the callers were TOLD (three grants) and what the ledger actually
    /// HOLDS (three units), because a reservation that granted four and recorded three would be
    /// the same money gone with a tidier file.
    ///
    /// Sabotage that makes it fail: split `reserve_a_read` back into its two acts — return
    /// `over_budget(trigger, day)` if it refuses, otherwise increment in a second, separate
    /// `update_json_lossy` — and both assertions report eight.
    #[test]
    fn a_ceiling_of_three_grants_three_units_however_many_readings_start_at_once() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"review_reads_per_day":3}"#,
        )
        .unwrap();
        let day = "2026-09-05";

        const RACERS: usize = 8;
        let start = std::sync::Barrier::new(RACERS);
        let granted = std::sync::atomic::AtomicU32::new(0);
        std::thread::scope(|threads| {
            for _ in 0..RACERS {
                threads.spawn(|| {
                    // Every thread is inside `reserve_a_read` at as near the same instant as the
                    // machine allows; without this they queue and the race never happens.
                    start.wait();
                    if reserve_a_read(Trigger::Unasked, "crowded", day).is_none() {
                        granted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                });
            }
        });

        assert_eq!(
            granted.into_inner(),
            3,
            "{RACERS} readings started together and more than the day's three were told to go \
             ahead — each of the extra ones is a model call nobody authorised"
        );
        assert_eq!(
            reads_spent(day),
            3,
            "the ledger records more than the ceiling: increments interleaved"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A cache hit spends nothing: the whole reason the counter lives on the server. The client's
    /// allowance counted requests, six cache hits ate it on every reload, and rows 7–29 were never
    /// read. Here the same request shape — summarise, unasked — answers from disk and the ledger
    /// does not move; the stub `claude` counts its invocations to prove the model was not even
    /// consulted.
    #[cfg(unix)]
    #[test]
    fn a_cache_hit_spends_none_of_the_days_budget() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        let asked = home.join("asked");
        let claude = home.join("claude-count.sh");
        std::fs::write(
            &claude,
            format!("#!/bin/sh\necho x >> {}\nprintf 'KIND: fix\\nLINE: x.\\nEXPAND: no\\nFLAGS: none\\n'\n", asked.display()),
        )
        .unwrap();
        std::fs::set_permissions(
            &claude,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &claude);

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo", "source": "https://github.com/acme/thing.git",
            "source_tree": "", "store": "",
        }))
        .unwrap();
        let pr = budget_pr(4, "abc");
        store(
            "demo",
            &Summary {
                // A fixture, and this is the honest value for one: nobody scanned a diff.
                owed_triggered: None,
                findings_block: None,

                swept: false,
                number: 4,
                head_sha: "abc".into(),
                depth: Depth::Line,
                line: "already read".into(),
                detail: String::new(),
                flags: Vec::new(),
                signals: Vec::new(),
                yours: Vec::new(),
                others: 0,
                ownership_unknown: String::new(),
                unread_because: String::new(),
                not_reread: String::new(),
                computed: true,
                budget_stopped: false,
            },
        )
        .unwrap();

        let s = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(s.line, "already read", "the cache answered");
        assert_eq!(
            reads_spent(&utc_day()),
            0,
            "a summary served from disk decremented the budget — the exact client bug, reborn on \
             the server"
        );
        assert!(
            !asked.exists(),
            "the model was consulted for an answer already on disk"
        );

        for key in ["SKEIN_HOME", "SKEIN_REVIEW_AI", "SKEIN_CLAUDE_BIN"] {
            std::env::remove_var(key);
        }
    }

    /// At the ceiling the model is NEVER reached for unasked work, and the row carries the
    /// invitation: the budget sentence with the numbers, the `budget_stopped` marker the pane
    /// keys the prominent read button on, `computed: false` (nothing was spent saying no), and
    /// no count moved. Both spending paths — the summary visit and the standalone draft — refuse
    /// through the same gate.
    #[cfg(unix)]
    #[test]
    fn at_the_ceiling_unasked_work_never_reaches_the_model_and_the_row_invites_the_button() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"review_reads_per_day":2}"#,
        )
        .unwrap();
        let day = utc_day();
        assert!(reserve_a_read(Trigger::Unasked, "elsewhere", &day).is_none());
        assert!(reserve_a_read(Trigger::Unasked, "elsewhere", &day).is_none());

        // The background pass finds a full queue and reads NOTHING.
        let read = read_waiting();
        assert!(
            read.is_empty(),
            "over budget, and the pass still read: {read:?}"
        );
        assert!(
            !asked.exists(),
            "the summariser was reached with the day's budget spent"
        );
        assert!(cached("crit", 21, "sha21").is_none());

        // The unasked per-row request — the pane's pump — gets the honest refusal, shaped for
        // the affordance.
        let pr = budget_pr(21, "sha21");
        let s = summarise(
            &crate::repos::load_repos()
                .into_iter()
                .find(|r| r.id == "crit")
                .unwrap(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(s.depth, Depth::Unread);
        assert!(
            s.budget_stopped,
            "the machine-readable marker the pane keys the button on is missing"
        );
        assert!(
            s.unread_because.contains("press read") && s.unread_because.contains("(2/2)"),
            "the refusal must invite the manual trigger, with the numbers: {}",
            s.unread_because
        );
        assert!(
            !s.computed,
            "saying 'budget spent' must not itself count as spending"
        );
        assert_eq!(reads_spent(&day), 2, "a refusal moved the counter");

        drafting_teardown();
    }

    /// The owner's boundary, both halves: "Limit is only for automatic stuff, manually I can
    /// invoke as many as I want." At 2/2 an ASKED analysis still runs — the model is reached and
    /// a real summary (and its merged draft) comes back — and the ledger stays at 2 afterwards:
    /// a manual call is never refused for budget and never eats the automatic allowance.
    #[cfg(unix)]
    #[test]
    fn an_asked_analysis_ignores_the_ceiling_and_leaves_it_untouched() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        std::env::set_var("SKEIN_FLEET_ROOT", home);
        let asked = drafting_fixture(home);
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"review_reads_per_day":2}"#,
        )
        .unwrap();
        let day = utc_day();
        assert!(reserve_a_read(Trigger::Unasked, "elsewhere", &day).is_none());
        assert!(reserve_a_read(Trigger::Unasked, "elsewhere", &day).is_none());

        let repo = crate::repos::load_repos()
            .into_iter()
            .find(|r| r.id == "crit")
            .unwrap();
        let pr = budget_pr(21, "sha21");
        let s = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            s.depth,
            Depth::Unread,
            "a manual call was refused for budget: {}",
            s.unread_because
        );
        assert!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count()
                >= 1,
            "the model was never reached for the asked call"
        );
        assert_eq!(
            reads_spent(&day),
            2,
            "the manual call ate the automatic allowance — asked work must not be counted"
        );

        drafting_teardown();
        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// One unasked analysis is exactly one unit — the merged visit produced a summary AND a
    /// drafted review, and the ledger moved by one, attributed to the repo that spent it.
    #[cfg(unix)]
    #[test]
    fn one_analysed_pull_request_is_one_unit_whatever_it_produced() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        std::env::set_var("SKEIN_FLEET_ROOT", home);
        let _asked = drafting_fixture(home);

        let _ = read_waiting();
        assert!(cached("crit", 21, "sha21").is_some());
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "summary plus drafted review is ONE analysed pull request, not two units"
        );
        // And the attribution names the repo.
        let raw = std::fs::read_to_string(spend_path()).unwrap();
        assert!(
            raw.contains("crit"),
            "the ledger lost the attribution: {raw}"
        );

        drafting_teardown();
        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }
}
