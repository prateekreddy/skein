//! The pass over every repo, and the budget it spends.
//!
//! [`sweep`] is the tick: read each repo's queue, build the facts, ask the evaluator, perform at
//! most one step per pull request. Two things bound it and both are here because they are
//! properties of the PASS rather than of any one action.
//!
//! **The reading budget.** [`READINGS_PER_SWEEP`] is 1 — a reading costs a model turn, and a sweep
//! that bought one per waiting pull request would spend the fleet's money on a queue nobody was
//! watching.
//!
//! **The clock.** A `wait:` step has no end of its own, so [`a_wait_that_will_not_end_on_its_own`]
//! reads the journal for how long this step has been the answer and stops a pull request that is
//! waiting on nothing ([`WAITING_ON_NOTHING_MS`]) or on a check that never started
//! ([`WAITING_ON_A_CHECK_MS`]). On a serial workflow that is what lets the train past a stuck
//! front instead of behind it — [`trains`] is the same rule the cockpit's panel draws.

use super::{
    carries, enabled, facts_of_in, journal, perform, read_stops, record, stop, JournalEntry,
    Outcome, Reading, Subject,
};
use crate::workflow::{Act, Chosen, Workflow};

/// How long the front of a serial train may wait on something skein cannot see running.
///
/// Twenty minutes, and the number is chosen against the two things it must not get wrong. It has
/// to be long enough that a check which is merely slow to be QUEUED is never mistaken for one that
/// is not coming — GitHub Actions starts within seconds normally, and minutes on a busy runner
/// pool — and short enough that a person watching a train notices the same day. A wait on
/// something that IS running is bounded by [`WAITING_ON_A_CHECK_MS`] instead, which is more than
/// seventy times as long, so no CI run is ever cut short by this one.
pub const WAITING_ON_NOTHING_MS: i64 = 20 * 60 * 1000;

/// How long the front of a serial train may wait on a check that HAS started and has not finished.
///
/// **Twenty-four hours, and the number is GitHub's own** rather than a guess about how slow a
/// pipeline is allowed to be. GitHub cancels a job that has been running for six hours (the
/// default `timeout-minutes: 360`), and cancels one that has sat unassigned to a runner for
/// twenty-four. A check still reported as `pending` past the longer of those has outlived every
/// bound GitHub itself applies to one, so it is not a slow run: whatever was going to report it is
/// gone, and nothing that happens on GitHub will ever move it (SKEIN-283).
///
/// That this needs to exist at all is the point. `checks: pending` was treated as proof that
/// something was in flight and therefore not bounded at all — which is true of a check that is
/// running and false of a check that has *stopped* running without saying so, and those two look
/// identical from here. A required context whose run was deleted, a self-hosted runner that went
/// away mid-job, a check GitHub is waiting on that will never be posted: each parks the front of a
/// serial train for ever, and everything behind it with it, saying "CI is running" about nothing.
/// The twenty-minute rule below cannot reach any of them, because they all say `pending`.
///
/// **The cost of getting it wrong is deliberately lopsided.** Too long, and a broken pipeline
/// parks a train until tomorrow — bad, and exactly the state this leaves it in today, for ever.
/// Too short, and somebody presses "let it run again" and the wait restarts with a fresh clock,
/// having lost one pass. There is no honest CI run this can cut short: at twenty-four hours GitHub
/// has already cancelled it.
pub const WAITING_ON_A_CHECK_MS: i64 = 24 * 60 * 60 * 1000;

/// When this pull request started waiting on THIS step, if it is still waiting on it.
///
/// The NEWEST entry is the whole answer, and that is the point: anything at all having happened
/// since — an act, a flag, a person clearing a stop, a wait on a different step — means the wait
/// that was being timed ended, and whatever is being waited on now starts its own clock. So a
/// train that is making progress can never accumulate patience across the steps it walked through.
fn waiting_since(entries: &[JournalEntry], flow: &str, step: usize) -> Option<i64> {
    entries
        .last()
        .filter(|e| e.kind == "waiting" && e.flow == flow && e.step == step)
        .map(|e| e.at_ms)
}

/// The front of a serial train said `wait`. Start its clock, or stop it because the clock ran out.
///
/// **This is not a stale-state bug and the fix is not a re-read** (SKEIN-240). `prq::rollup`
/// answers `"none"` when nothing has ever run against a commit, and that reading is CORRECT and
/// CURRENT — it is the same answer whether CI is five seconds away or will never come, because
/// nothing GitHub sends distinguishes "no check yet" from "no check, ever, on this repository".
/// Asking again produces the same true answer for ever. The documented train has no step for
/// `checks:none` once its label is on, so the front falls to the catch-all `wait:` and holds the
/// line at one pass per two minutes, for ever, saying *"waiting for GitHub to catch up"* when
/// GitHub caught up long ago. `docs/pr-workflow.md` names exactly this: *"The failure mode to
/// avoid is not the stall. It is a **silent** stall."*
///
/// So the only thing that can tell those two apart is how long the waiting has gone on, and this
/// is where that is decided.
///
/// **Two ceilings, because there are two waits.** A wait with nothing behind it — no check
/// running, nothing in flight skein can point at, and a sentence promising something is going to
/// change — runs out after [`WAITING_ON_NOTHING_MS`]. A wait on a check that HAS started runs out
/// after [`WAITING_ON_A_CHECK_MS`], which is more than seventy times as long: the module note
/// above builds the whole guarded-step design around surviving *"a forty-minute CI run"*, and
/// cutting one short would be a worse bug than either of the ones this fixes.
///
/// It is one function and not two because it is one decision — *how long may this go on* — and the
/// only thing the evidence changes is the number. Written as a second mechanism beside the first,
/// the two would keep separate clocks over the same journal and disagree about when a wait began.
///
/// **`pending` earns patience, not immunity.** It used to end this function on the spot, on the
/// reading that a check which has started is something skein can expect to end. That is true of a
/// check that is running and false of one that has stopped running without saying so — a deleted
/// run, a self-hosted runner that went away mid-job, a required context nobody will ever post —
/// and from here the two are the same word. So `pending` was an unbounded park, which is the state
/// this rule exists to prevent, reachable by saying the one thing that switched the rule off
/// (SKEIN-283).
///
/// The clock does not restart when the evidence changes, and it does not need to: it can only ever
/// move the ceiling under a wait already in progress. Nothing running for nineteen minutes and
/// then a check starts, and the ceiling rises to a day — less eager, never a stop. A check pending
/// for twenty-three hours and then it vanishes, and the ceiling drops to twenty minutes it has
/// long since passed — a stop, on a pull request that has waited twenty-three hours with nothing
/// running. Neither direction can stop something that was going to resolve on its own.
///
/// **Only a serial train.** A stop is a demand for somebody's attention, and it is earned when the
/// alternative is a queue that has stopped moving. A pull request on a workflow that blocks nobody
/// is not costing anything by waiting, and stopping it would be manufacturing work.
///
/// Recoverable, in the two ways that matter: the stop names the elapsed time and the step so the
/// sentence is checkable, and clearing it puts the pull request back in line — where, if the wait
/// really was on something slow, it simply waits again with a fresh clock.
fn a_wait_that_will_not_end_on_its_own(
    repo_id: &str,
    number: u64,
    flow: &Workflow,
    chosen: &Chosen,
    facts: &crate::workflow::Facts,
    why: &str,
) -> Option<String> {
    if !flow.serial {
        return None;
    }
    // The one thing the evidence decides. `pending` is the only answer that means a check has
    // started; `passing`, `failing`, `none` and the empty string all mean nothing is in flight.
    let running = facts.checks == "pending";
    let ceiling = match running {
        true => WAITING_ON_A_CHECK_MS,
        false => WAITING_ON_NOTHING_MS,
    };
    let step = chosen.step + 1;
    let now_ms = now_ms();
    let Some(since) = waiting_since(&journal(repo_id, number), &flow.name, step) else {
        // The first pass on this step: put the clock down and say nothing. A wait is the ordinary
        // state of a train and this entry is what makes it a *timed* one.
        record(repo_id, number, &flow.name, step, "waiting", why);
        return None;
    };
    let waited = now_ms - since;
    if waited < ceiling {
        return None;
    }
    // The sentence says which of the two ceilings fell and what would have had to be true for it
    // to be wrong, because that is what makes it checkable by the person it interrupts.
    let reason = match running {
        true => format!(
            "step {step} has been waiting {hours} hours — {why:?} — and its checks have read \
             `pending` that whole time. GitHub cancels a job that has run for six hours and one \
             that has waited twenty-four for a runner, so a check still pending past both is not \
             a slow build: whatever was going to report it is gone. Look for a cancelled or \
             deleted run, or a required check nothing posts. Clearing this stop puts it back in \
             line with a fresh clock.",
            hours = waited / 3_600_000,
        ),
        false => format!(
            "step {step} has been waiting {minutes} minutes — {why:?} — and nothing is running \
             (checks: {}). Whatever was expected to start has not, so this wait will not end on \
             its own; if a label is meant to start CI here, check it is the one the repository's \
             workflow keys on. Clearing this stop puts it back in line.",
            match facts.checks.is_empty() {
                true => "none",
                false => facts.checks.as_str(),
            },
            minutes = waited / 60_000,
        ),
    };
    stop(repo_id, number, &reason);
    record(repo_id, number, &flow.name, step, "stopped", &reason);
    crate::warden_client::reported(
        &format!("pr-workflow:{}", flow.name),
        &format!("stopped waiting on #{number}"),
        &reason,
    );
    Some(reason)
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One serial workflow's train in one repo: who is in line, who is at the front, who has been
/// passed over — the answer to "what is it working on, and which step is everyone else waiting
/// behind".
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrainView {
    /// The serial workflow's name.
    pub flow: String,
    /// The one pull request that may act this pass: the oldest carrying PR without a stop. `None`
    /// when the line is empty or everyone in it is stopped — a train with nobody to move.
    pub front: Option<u64>,
    /// Every carrying pull request in train order — oldest first, lowest number — front included.
    pub line: Vec<u64>,
    /// Only this flow's carrying pull requests that are stopped, with their reasons, in train
    /// order. The passed-over, not the whole repo's stop file.
    pub stopped: Vec<crate::prq::StoppedPr>,
}

/// Every serial workflow's train in this repo, from the same ordering-and-front rule the tick
/// acts on.
///
/// `prs` is (number, carried flow name) for the non-archived carrying pull requests — the caller
/// has already decided who carries what, because that needs facts this function should not
/// re-derive. This is the ONE place the train order and the front are computed: [`sweep`] calls
/// through it before acting, so a panel drawn from it cannot disagree with what the tick then
/// does. A non-serial workflow gets no view — a train is the serial thing.
pub fn trains(repo_id: &str, prs: &[(u64, String)], flows: &[Workflow]) -> Vec<TrainView> {
    let stops = read_stops(repo_id);
    flows
        .iter()
        .filter(|flow| flow.serial)
        .map(|flow| {
            // Oldest first — lowest number, the sort key the owner chose.
            let mut line: Vec<u64> = prs
                .iter()
                .filter(|(_, name)| *name == flow.name)
                .map(|(number, _)| *number)
                .collect();
            line.sort_unstable();
            // The first one without a stop is the front; a stopped PR is passed over — the "skip
            // failures and move ahead" (docs/pr-workflow.md, "The merge train").
            let front = line
                .iter()
                .copied()
                .find(|number| !stops.contains_key(&number.to_string()));
            let stopped = line
                .iter()
                .filter_map(|number| {
                    stops
                        .get(&number.to_string())
                        .map(|why| crate::prq::StoppedPr {
                            number: *number,
                            why: why.clone(),
                        })
                })
                .collect();
            TrainView {
                flow: flow.name.clone(),
                front,
                line,
                stopped,
            }
        })
        .collect()
}

/// How many readings one pass may buy, across the whole fleet.
///
/// **One**, and the number comes from the tick rather than from a taste for caution. The pass runs
/// every 120 seconds and a reading is most of a minute, so one keeps a pass comfortably inside its
/// own interval; two could leave the next tick waiting on the last, with the merge train's
/// second-long steps queued behind a stack of model calls.
///
/// Burst control, not a budget. The budget is `Config::review_reads_per_day`, which this spends
/// from like every other reading — this only decides how fast. A queue where ten pull requests
/// come into scope at once therefore takes ten passes, twenty minutes, which for something nobody
/// is waiting at a keyboard for is the right trade.
const READINGS_PER_SWEEP: usize = 1;

/// One pass over the fleet: every repo skein manages, every pull request a workflow governs, one
/// step each.
///
/// **One step per pull request per pass, and the pass is the only thing that acts.** After an
/// action lands, what skein believes about that pull request is one action out of date — the label
/// is on but no check has been queued, so `checks:passing` is still true from the previous run. The
/// next pass re-reads GitHub, which is the only thing that can say what the action did.
///
/// **Every repo in the registry**, not only ones whose queue somebody has opened — the owner's
/// decision, and what makes this automation rather than a thing you have to remember to visit. A
/// repo with no workflow claiming anything costs one cached queue read.
///
/// Returns what it did, for the server's log. Every action is also in the host audit with its
/// authority; this is the line a person watching a terminal sees.
pub fn sweep() -> Vec<String> {
    // Nothing at all when the switch is off — not even a queue read. A feature that is switched off
    // should be invisible in every way somebody might notice, including a rate limit.
    if !enabled() {
        return Vec::new();
    }
    let flows = match crate::workflow::load() {
        Ok(flows) => flows,
        // A file with one bad step loads none of them (`workflow::from_bytes`), which is the right
        // answer and a silent one — so it is said here, where somebody watching the server sees it.
        Err(why) => {
            eprintln!("skein: no workflow is running — {why}");
            return Vec::new();
        }
    };
    if flows.is_empty() {
        return Vec::new();
    }
    let token = match crate::prq::host_token() {
        Ok(token) => token,
        Err(why) => {
            eprintln!("skein: workflows are on, and there is no GitHub token to act with — {why}");
            return Vec::new();
        }
    };

    let mut did = Vec::new();
    // Across every repo, not per repo: the thing being protected is the pass, and a pass that
    // spent a minute on repo A's reading has that minute gone whether repo B reads anything.
    let mut spent_readings = 0usize;
    for repo in crate::repos::load_repos() {
        let Ok(queue) = crate::prq::queue(&repo, false) else {
            // A queue that cannot be read is not a reason to stop the fleet's other repos. The
            // review pane reports the failure with its reason; this pass simply has nothing to
            // decide from.
            continue;
        };
        // Who carries what, decided once for the whole repo before anyone may act: a serial
        // workflow's rule below is about the *whole* train, and a decision made one pull request
        // at a time could not see past the one in hand.
        let mut rows = Vec::new();
        for pr in &queue.prs {
            // A pull request you set aside is one you said "not now" about. A workflow acting on it
            // would be overruling that with a rule, which is the opposite of what setting aside is
            // for — and the row that says "archived" would be acting.
            if matches!(pr.lane, crate::prq::Lane::Archived) {
                continue;
            }
            // The repo-aware one: this is the pass that ACTS, so it is the one that must be
            // able to see skein's own reading rather than wait on a fact it declined to look up.
            let facts = facts_of_in(&repo.id, pr, &queue.viewer, &queue.trunk);
            // `acting`, not `name` (SKEIN-279): a workflow whose own `matches` do not hold is
            // shown on the row and takes no part in this pass — no step, no stop, no clock, and
            // no place in a serial train's line, where standing at the front unable to act would
            // hold up everything behind it.
            let Some(name) = carries(&repo.id, pr.number, &facts, &flows)
                .acting()
                .map(str::to_string)
            else {
                continue;
            };
            rows.push((pr, facts, name));
        }
        // The front of each serial train: carrying pull requests oldest-first (lowest number —
        // the sort key the owner chose), and the first one without a stop is the only one that
        // may act this pass. A stopped front is passed over rather than reported — that is the
        // "skip failures and move ahead" — and everyone behind the front is simply waiting, which
        // is the ordinary state of a train and not an event (`docs/pr-workflow.md`, "The merge
        // train"). A workflow whose every carrying PR is stopped has no front, and nobody acts.
        //
        // Computed by [`trains`] — the same function the cockpit's train panel reads — so what a
        // person is shown and what the tick then does cannot be two computations that drift apart.
        let carrying: Vec<(u64, String)> = rows
            .iter()
            .map(|(pr, _, name)| (pr.number, name.clone()))
            .collect();
        let fronts: std::collections::BTreeMap<String, u64> = trains(&repo.id, &carrying, &flows)
            .into_iter()
            .filter_map(|train| train.front.map(|front| (train.flow, front)))
            .collect();
        let mut acted_in_repo = false;
        for (pr, facts, name) in &rows {
            let Some(flow) = flows.iter().find(|f| &f.name == name) else {
                continue;
            };
            // Everyone but the front of a serial train is passed over: no action, and no stop —
            // being behind the front is where a train's pull requests live, not a fault.
            if flow.serial && fronts.get(name) != Some(&pr.number) {
                continue;
            }
            let Some(chosen) = crate::workflow::next(flow, facts) else {
                continue;
            };
            // Burst control, and the only act in this pass that needs any: every other one is an
            // HTTP call taking a second, and a reading is most of a minute. Left uncapped, a repo
            // where a dozen pull requests came into scope at once would spend the pass on model
            // calls while a green, approved pull request three repos along waited behind them.
            //
            // Counted in readings SPENT, below, not in `read` steps taken: a step that answers
            // from the cache costs nothing and must not use the allowance up. Nothing is lost when
            // it bites — the pull request is unread, so the same step is chosen next pass.
            if matches!(chosen.act, Act::Read) && spent_readings >= READINGS_PER_SWEEP {
                eprintln!(
                    "skein: {} #{} is due a reading, and this pass has already spent its {} — \
                     next pass",
                    repo.id, pr.number, READINGS_PER_SWEEP
                );
                continue;
            }
            let subject = Subject {
                repo_id: &repo.id,
                slug: &queue.slug,
                number: pr.number,
                head_sha: &pr.head_sha,
                head_ref: &pr.head_ref,
                // The reviewer's half. Everything it carries is already in hand here, which is
                // why `Act::Read` is takeable from this caller and from no other.
                reading: Some(Reading {
                    repo: &repo,
                    pr,
                    viewer: &queue.viewer,
                    facts,
                }),
            };
            match perform(&subject, flow, &chosen, &token) {
                Outcome::Did(what) => {
                    if matches!(chosen.act, Act::Read) {
                        spent_readings += 1;
                    }
                    did.push(format!("{}: {what}", repo.id));
                    acted_in_repo = true;
                }
                // Waiting is the ordinary state and says nothing — but the front of a serial
                // train waiting on nothing is the line not moving, so it is timed. See
                // [`a_wait_that_will_not_end_on_its_own`], which is the only thing standing between a
                // repo whose CI label starts nothing and a train parked for ever.
                Outcome::Waited(why) => {
                    a_wait_that_will_not_end_on_its_own(
                        &repo.id, pr.number, flow, &chosen, facts, &why,
                    );
                }
                // A stop has already been written down and audited by `perform`; repeating it here
                // every pass would bury the log.
                Outcome::Stopped(_) => {}
            }
        }
        // The queue is cached for a minute, and skein has just changed the thing it describes. Left
        // alone, the next pass would decide from facts it had itself made stale — which is the one
        // input a cascade needs to merge on a check that has not run.
        if acted_in_repo {
            crate::prq::invalidate(&repo.id);
        }
    }
    did
}
#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::prwork::facts::facts_of;
    #[allow(unused_imports)]
    use crate::prwork::testkit::*;
    #[allow(unused_imports)]
    use crate::prwork::*;
    #[allow(unused_imports)]
    use crate::workflow::{Act, Chosen, Merge, MergeAs, Update, Workflow};
    #[allow(unused_imports)]
    use std::io::{Read, Write};
    #[allow(unused_imports)]
    use std::sync::{Arc, Mutex};

    /// The owner's example, walked to merged by the tick alone, with nothing open.
    ///
    /// The claim the whole feature makes: a pull request that is approved and green ends up merged
    /// without anybody pressing anything. Driven through `sweep` against a GitHub that answers from
    /// a fixture and CHANGES as skein acts on it — a label appears when skein adds one, checks go
    /// green once it is there — because a stub that answers the same thing every time cannot tell a
    /// workflow that advances from one that is stuck in a loop taking the same step.
    ///
    /// The other half of the claim is that it takes ONE step per pass. After an action lands, what
    /// skein believes is one action out of date, so a pass that kept going would decide the next
    /// step from facts it had just made stale — a label added, no check yet queued, `checks:passing`
    /// still true from the previous run, and it merges.
    #[test]
    fn the_tick_walks_a_pull_request_to_merged_one_step_per_pass() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_PR_WORKFLOWS", "on");
        env.set("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The workflow, as the owner described it.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"ship-mine","matches":["mine"],"steps":[
              {"when":["approved","no-label:ci"],"do":"add-label:ci"},
              {"when":["checks:pending"],"do":"wait:CI is running"},
              {"when":["checks:failing"],"do":"flag:CI is red"},
              {"when":["approved","mergeable","checks:passing"],"do":"merge:squash+delete"},
              {"when":["approved","not-mergeable"],"do":"update-branch:rebase"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub whose answers move as skein acts on it.
        // **Green before the label goes on**, which is the state that makes "one step per pass" a
        // property with teeth. The branch passed CI on an earlier run, so `checks:passing` is true
        // AND the label is missing — both step 1 and step 4 apply at once. A pass that kept going
        // would add the label and then merge, in the same breath, on a check run that predates it.
        let state: Arc<Mutex<(bool, String)>> = Arc::new(Mutex::new((false, "passing".into())));
        let merged = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (world, seen) = (state.clone(), merged.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let (labelled, checks) = world.lock().unwrap().clone();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    // What the repository's default branch is. A real GitHub answers this and a
                    // stub that did not used to be harmless — until a merge started requiring
                    // skein to KNOW the base it is shipping into
                    // ([`crate::workflow::instead_of_merging_off_the_trunk`]), at which point a
                    // fixture with no trunk is a fixture where nothing may merge.
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/labels") {
                    // The label lands, and this repository's CI starts on it.
                    *world.lock().unwrap() = (true, "pending".into());
                    "[]".to_string()
                } else if head.contains("/merge") {
                    seen.lock().unwrap().push(head.clone());
                    r#"{"merged":true}"#.to_string()
                } else if head.starts_with("DELETE") {
                    seen.lock().unwrap().push(head.clone());
                    "{}".to_string()
                } else if head.contains("/graphql") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{{"number":7,"title":"t","url":"u",
                          "isDraft":false,"author":{{"login":"me"}},"headRefName":"feat",
                          "headRefOid":"abc","baseRefName":"main",
                          "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                          "mergeable":"MERGEABLE",
                          "labels":{{"nodes":[{}]}},
                          "latestReviews":{{"nodes":[]}},
                          "commits":{{"nodes":[{{"commit":{{
                             "committedDate":"2026-08-23T00:00:00Z",
                             "statusCheckRollup":{{"contexts":{{"nodes":[{}]}}}}}}}}]}}}}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        match labelled {
                            true => r#"{"name":"ci"}"#,
                            false => "",
                        },
                        match checks.as_str() {
                            "pending" => r#"{"status":"IN_PROGRESS"}"#,
                            "passing" => r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#,
                            _ => "",
                        },
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // **A pull request you set aside is left alone**, even by a rule that claims it. Setting
        // aside is a person saying "not now" about this one; a workflow acting on it would overrule
        // that with a rule, and the row would say "archived" while skein merged it.
        std::fs::create_dir_all(crate::prq::review_dir("demo")).unwrap();
        std::fs::write(crate::prq::review_dir("demo").join("archived.json"), b"[7]").unwrap();
        assert!(
            sweep().is_empty(),
            "a pull request that was set aside was acted on anyway"
        );
        std::fs::write(crate::prq::review_dir("demo").join("archived.json"), b"[]").unwrap();

        // Pass one: approved, unlabelled. The label that starts CI.
        let did = sweep();
        assert_eq!(did.len(), 1, "a pass took more than one step: {did:?}");
        assert!(did[0].contains("label"), "{did:?}");
        assert!(
            merged.lock().unwrap().is_empty(),
            "it merged in the same pass that started CI — on a check that had not run"
        );

        // Pass two: CI is running. Waiting is not an action, so nothing is reported and nothing is
        // done — and above all it does not merge.
        assert!(
            sweep().is_empty(),
            "waiting for CI was reported as doing something"
        );
        assert!(merged.lock().unwrap().is_empty());

        // CI goes green.
        state.lock().unwrap().1 = "passing".into();
        let did = sweep();
        assert_eq!(did.len(), 1, "{did:?}");
        assert!(did[0].contains("merged #7"), "{did:?}");
        let calls = merged.lock().unwrap().clone();
        assert!(
            calls.iter().any(|c| c.contains("/pulls/7/merge"))
                && calls.iter().any(|c| c.contains("git/refs/heads/feat")),
            "the branch did not go with the merge: {calls:?}"
        );

        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// **A workflow that acted drops the queue the next pass would have decided from**
    /// (SKEIN-314).
    ///
    /// [`sweep`] ends a pass that did anything with `crate::prq::invalidate`, and the reason is in
    /// its own comment: the queue is cached for a minute, skein has just changed the thing that
    /// queue describes, and left alone the next pass would decide from facts it made stale itself.
    /// That is the one input a cascade needs to merge on a check that has not run.
    ///
    /// **It had no test that could fail, and could not have had one**: `queue_within` skipped the
    /// cache outright in this crate's unit tests, so deleting the `invalidate` changed nothing any
    /// test could see. [`crate::prq::CachedQueues`] switches the cache on for the length of this
    /// test, which makes the sweep's second pass a real one.
    ///
    /// Two passes over a repository whose state changes in between, exactly as it would on GitHub
    /// after the first act: pass one puts `ci-queue` on, CI then goes green, and pass two merges.
    /// With the `invalidate` deleted, pass two reads the cached queue instead — no label, no green
    /// — and adds the label a second time rather than merging, which is what this fails on.
    #[test]
    fn acting_drops_the_queue_the_next_pass_would_have_decided_from() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_PR_WORKFLOWS", "on");
        env.set("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // The same pull request twice: before its label and CI run, and after.
        let answer_for = |labels: &str, checks: &str| {
            format!(
                r#"{{"data":{{"q0":{{"nodes":[{{"number":12,"title":"t","url":"u","isDraft":false,
                  "author":{{"login":"me"}},"headRefName":"feat-12","headRefOid":"abc",
                  "baseRefName":"main","updatedAt":"2026-08-23T00:00:00Z",
                  "reviewDecision":"APPROVED","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                  "labels":{{"nodes":[{labels}]}},"latestReviews":{{"nodes":[]}},
                  "commits":{{"nodes":[{{"commit":{{
                    "committedDate":"2026-08-23T00:00:00Z",
                    "statusCheckRollup":{{"contexts":{{"nodes":[
                      {{"status":"COMPLETED","conclusion":"{checks}"}}]}}}}}}}}]}}}}]}},
                  "q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#
            )
        };
        let answer = Arc::new(Mutex::new(answer_for(r#"{"name":"ready"}"#, "SUCCESS")));
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (seen, queue) = (heard.clone(), answer.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(head.clone());
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing") && !head.contains("/pulls") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") {
                    queue.lock().unwrap().clone()
                } else {
                    r#"{"merged":true}"#.to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // The cache live, which is the whole point: without this the second sweep refetches
        // whether or not anything invalidated, and the assertion below cannot fail.
        let _cache = crate::prq::CachedQueues::live();

        let first = sweep();
        assert!(
            first.iter().any(|d| d.contains("ci-queue")),
            "the first pass did not act, so there is nothing for an invalidate to be about: \
             {first:?}"
        );

        // GitHub's state moves on, exactly as it would have: the label is on, and CI went green
        // against it. Nothing tells skein — the only thing that can is reading the queue again.
        *answer.lock().unwrap() = answer_for(r#"{"name":"ready"},{"name":"ci-queue"}"#, "SUCCESS");

        let second = sweep();
        let calls = heard.lock().unwrap().clone();
        assert!(
            calls.iter().filter(|c| c.contains("/graphql")).count() >= 2,
            "the second pass decided from the queue the first pass made stale — it never asked \
             GitHub again: {second:?} / {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.contains("/pulls/12/merge")),
            "the pull request was not merged on the second pass, so the pass acted on facts from \
             before its own act: {second:?} / {calls:?}"
        );

        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// One rate-limited refresh must not disable the merge train until somebody restarts skein.
    ///
    /// The shape (SKEIN-238), and it is the same shape twice over. `prq::queue_within` asks in the
    /// order `viewer()` [REST], `search_prs_all` [GraphQL], `trunk_of` [REST] — so a GraphQL-only
    /// limit passes the first, engages `crate::github`'s process-wide hold on the second, and the
    /// third is refused by that hold having asked GitHub nothing. `trunk_of` swallowed that
    /// refusal into `""` and REMEMBERED it, for the life of the process. Everything downstream
    /// then did exactly what it should with an unknown trunk: `base_is_trunk` none, `base:trunk`
    /// unsatisfied, `claims` false, `Carries::Nothing`, `sweep` moves on. A dead train, with no
    /// banner, no blind spot and no log line — nothing anybody could clear, because nothing said
    /// it was there.
    ///
    /// So what this asserts is RECOVERY, not correctness: skein is allowed to know nothing while
    /// GitHub is refusing it, and is not allowed to still know nothing one refresh after GitHub
    /// comes back. The fix is that only an ANSWER is remembered (`prq::trunk_of`); a failure is
    /// asked again, and during the hold that retry is refused before it is spent.
    #[test]
    fn a_rate_limited_refresh_does_not_disable_the_train_until_a_restart() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_PR_WORKFLOWS", "on");
        env.set("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
        crate::prq::forget_renames();

        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[{"when":["no-label:ci-queue"],"do":"add-label:ci-queue"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub whose GRAPHQL quota alone is spent — REST is fine, which is the live shape:
        // skein's search is where nearly all of its quota goes. `/rate_limit` stays free and
        // answers, because that is where the hold learns how long to last.
        let spent = Arc::new(Mutex::new(true));
        let out = spent.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let answer = if head.starts_with("GET /rate_limit") {
                    format!(
                        r#"{{"resources":{{"core":{{"remaining":4000,"reset":{}}},
                          "search":{{"remaining":30,"reset":{}}},
                          "graphql":{{"remaining":0,"reset":{}}}}}}}"#,
                        now + 600,
                        now + 600,
                        now + 600
                    )
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") && *out.lock().unwrap() {
                    r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#
                        .to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[{"number":5,"title":"t","url":"u",
                      "isDraft":false,"author":{"login":"me"},"headRefName":"feat-5",
                      "headRefOid":"abc","baseRefName":"main",
                      "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                      "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                      "labels":{"nodes":[]},"latestReviews":{"nodes":[]},
                      "commits":{"nodes":[{"commit":{
                        "committedDate":"2026-08-23T00:00:00Z",
                        "statusCheckRollup":{"contexts":{"nodes":[
                          {"status":"COMPLETED","conclusion":"SUCCESS"}]}}}}]}}]},
                      "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else {
                    "[]".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // The refresh that lands inside the outage. Knowing nothing here is correct.
        let repo = crate::repos::load_repos().remove(0);
        let during = crate::prq::queue(&repo, true).expect("a blind queue still answers");
        assert_eq!(
            during.trunk, "",
            "skein claimed to know the trunk during an outage that refused the lookup"
        );

        // GitHub comes back: the quota returns and the hold is released.
        *spent.lock().unwrap() = false;
        let _cleared = crate::github::HoldClear::new();

        let after = crate::prq::queue(&repo, true).expect("a healthy GitHub answers");
        assert_eq!(
            after.prs.len(),
            1,
            "the recovered queue lost its pull request"
        );
        assert_eq!(
            after.trunk, "main",
            "one rate-limited refresh disabled the merge train until a restart: the failed trunk \
             lookup was remembered as an answer, so `base:trunk` can never hold again"
        );

        // And the train claims it again — the thing the memoised failure had silently switched
        // off. Asserted through `claims`, which is the gate the whole chain narrows to.
        let flows = crate::workflow::load().unwrap();
        let facts = facts_of(&after.prs[0], &after.viewer, &after.trunk);
        assert!(
            crate::workflow::claims(&flows[0], &facts),
            "the merge train still claims nothing after GitHub came back: {facts:?}"
        );
        // …and the tick acts on it, which is what "the train is running" means to a person.
        let did = sweep();
        assert!(
            did.iter().any(|line| line.contains("ci-queue")),
            "the train claimed #5 and still did nothing: {did:?}"
        );

        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
        crate::prq::forget_renames();
    }

    /// A serial workflow acts on the front of the train, and only the front.
    ///
    /// Oldest first — lowest number, the sort key the owner chose — and a stopped front is
    /// passed over so the train moves ahead of a failure rather than parking behind it
    /// (docs/pr-workflow.md, "The merge train"). The assertion is on the wire, the file's
    /// discipline: two pull requests both due the same action, and exactly one request leaves.
    #[test]
    fn a_serial_workflow_acts_on_the_front_of_the_train_only() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_PR_WORKFLOWS", "on");
        env.set("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();

        let serial = br#"{"workflow":[{"name":"train","serial":true,"matches":["mine"],"steps":[
          {"when":["no-label:ci"],"do":"add-label:ci"}]}]}"#;
        std::fs::write(home.join("workflows.json"), serial).unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/serial.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub with two open pull requests, both mine, both unlabelled — and #9 listed FIRST,
        // so a sweep that took the queue's own order would act on the wrong one.
        let labelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = labelled.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.contains("/labels") {
                    seen.lock().unwrap().push(head.clone());
                    "[]".to_string()
                } else if head.contains("/graphql") {
                    let node = |number: u64| {
                        format!(
                            r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                              "author":{{"login":"me"}},"headRefName":"feat-{number}",
                              "headRefOid":"abc","baseRefName":"main",
                              "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                              "mergeable":"MERGEABLE","labels":{{"nodes":[]}},
                              "latestReviews":{{"nodes":[]}},
                              "commits":{{"nodes":[{{"commit":{{
                                "committedDate":"2026-08-23T00:00:00Z",
                                "statusCheckRollup":{{"contexts":{{"nodes":[]}}}}}}}}]}}}}"#
                        )
                    };
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(9),
                        node(5)
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // Pass one: both #5 and #9 are due the same step. Exactly one request leaves, and it is
        // for #5 — the oldest, not the first in the queue's own order.
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "a serial workflow acted past the front of the train: {calls:?} ({did:?})"
        );
        assert!(
            calls[0].contains("/issues/5/labels"),
            "the train did not act on its oldest pull request: {calls:?}"
        );

        // The front stops — CI failed, say. The next pass skips it and moves ahead: #9 is the
        // front now. That pass-over is the "skip failures and move ahead", and it is silent,
        // because a stopped PR's story is in the stops file, not re-announced every pass.
        stop("demo", 5, "CI is red");
        labelled.lock().unwrap().clear();
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "a stopped front did not yield to the next in line: {calls:?} ({did:?})"
        );
        assert!(
            calls[0].contains("/issues/9/labels"),
            "the train did not move ahead of its stopped front: {calls:?}"
        );

        // And the stops read back in numeric order, the shape the banner row carries.
        let stops = stops("demo");
        assert_eq!(stops.len(), 1);
        assert_eq!((stops[0].number, stops[0].why.as_str()), (5, "CI is red"));

        // The same two pull requests under a NON-serial workflow: everyone due a step acts, which
        // is today's behavior and must stay — serial is a property of a workflow, not of the sweep.
        clear("demo", 5).expect("the stop must clear");
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"train","matches":["mine"],"steps":[
              {"when":["no-label:ci"],"do":"add-label:ci"}]}]}"#,
        )
        .unwrap();
        labelled.lock().unwrap().clear();
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            2,
            "a workflow that never asked to be serial was serialized: {calls:?} ({did:?})"
        );

        crate::prq::forget_host_token();
    }

    /// A pull request whose CI never starts stops the train's clock, not the train.
    ///
    /// The bug (SKEIN-240) and, more importantly, what KIND of bug it is. It looks like the
    /// rate-limit family — a temporary condition that became permanent — and it is not one.
    /// `prq::rollup` says `"none"` when nothing has ever run against a commit, and that answer is
    /// correct, current and unchanging: nothing GitHub sends tells "no check yet" apart from "no
    /// check, ever, on this repository". There is no staler cache to drop and no re-read that
    /// helps. The documented train has no step for `checks:none` once its label is on, so the
    /// front falls to the catch-all `wait:` and holds the line at one pass per two minutes, for
    /// ever, over a sentence that says GitHub has not caught up when GitHub caught up long ago.
    ///
    /// The only thing that can tell the two apart is elapsed time, so this asserts a clock: the
    /// first pass writes the wait down, passes inside the twenty minutes change nothing, and the
    /// pass after it stops the pull request with a sentence naming the wait — at which point the
    /// serial train's existing pass-over rule moves it aside and #9, which has been in line all
    /// along, gets its turn.
    ///
    /// The back-dated journal entry is the clock: `record` stamps `now`, so the only way to reach
    /// the far side of twenty minutes in a test is to write the timeline the way it would look
    /// twenty minutes later.
    #[test]
    fn a_front_waiting_on_a_check_that_never_starts_stops_and_lets_the_train_past() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_PR_WORKFLOWS", "on");
        env.set("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, ending in the catch-all that has no clock of its own.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #5 is labelled and its label started nothing — `checks: none`, for ever. #9 is behind it
        // in the line, unlabelled, with a step of its own it has never been given a chance to take.
        let labelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = labelled.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/labels") {
                    seen.lock().unwrap().push(head.clone());
                    "[]".to_string()
                } else if head.contains("/graphql") {
                    // No `statusCheckRollup` contexts and none claimed on either: `checks: none`.
                    let node = |number: u64, labels: &str| {
                        format!(
                            r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                              "author":{{"login":"me"}},"headRefName":"feat-{number}",
                              "headRefOid":"abc","baseRefName":"main",
                              "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                              "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                              "labels":{{"nodes":[{labels}]}},"latestReviews":{{"nodes":[]}},
                              "commits":{{"nodes":[{{"commit":{{
                                "committedDate":"2026-08-23T00:00:00Z",
                                "statusCheckRollup":{{"contexts":{{"nodes":[]}}}}}}}}]}}}}"#
                        )
                    };
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(5, r#"{"name":"ci-queue"}"#),
                        node(9, "")
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // Pass one, and four more inside the twenty minutes. #5 is the front and waits; #9 is
        // behind it and is passed over. This is the reported failure, and up to here it is CORRECT
        // — a wait that has not gone on long enough to be suspicious.
        for _ in 0..5 {
            let _ = sweep();
        }
        assert!(
            labelled.lock().unwrap().is_empty(),
            "the train acted past a waiting front: {:?}",
            labelled.lock().unwrap()
        );
        assert_eq!(stopped("demo", 5), None, "a wait was stopped far too early");
        // …and the wait was written down once, not once per pass, with the step it is on.
        let waits: Vec<JournalEntry> = journal("demo", 5)
            .into_iter()
            .filter(|e| e.kind == "waiting")
            .collect();
        assert_eq!(
            waits.len(),
            1,
            "five passes wrote {} waiting entries: one is the clock being started, none is no \
             clock at all, and more than one is a journal turning into a log file",
            waits.len()
        );
        assert_eq!(waits[0].step, 4, "the wait must name the step it is on");

        // Twenty minutes pass. Written into the timeline, because `record` stamps `now`.
        let mut all = journal("demo", 5);
        let last = all.len() - 1;
        all[last].at_ms -= WAITING_ON_NOTHING_MS + 1;
        let mut file: std::collections::BTreeMap<String, Vec<JournalEntry>> =
            serde_json::from_str(&std::fs::read_to_string(journal_path("demo")).unwrap()).unwrap();
        file.insert("5".into(), all);
        std::fs::write(
            journal_path("demo"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();

        // The pass on the far side of the clock: #5 stops, and says what it waited for.
        let _ = sweep();
        let why = stopped("demo", 5).unwrap_or_else(|| {
            panic!("a front that has waited twenty minutes on a check that never started is still holding the line, silently")
        });
        assert!(
            why.contains("waiting for GitHub to catch up") && why.contains("checks: none"),
            "the stop does not say what it waited for or why the wait cannot end: {why}"
        );
        assert!(
            why.contains("minutes"),
            "the stop does not say how long it waited, so nobody can judge it: {why}"
        );

        // And the line moves: the serial pass-over rule now finds #9 at the front, and it acts.
        let _ = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert!(
            calls.iter().any(|c| c.contains("/issues/9/labels")),
            "#5 stopped and the train still did not move on to #9: {calls:?}"
        );

        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// A check that IS running is not cut short — and a check that says it is running for ever is.
    ///
    /// The other side of [`a_wait_that_will_not_end_on_its_own`], and the more dangerous one: this
    /// bound is the only thing in skein that can stop a pull request for taking too long, and a CI
    /// run is allowed to take as long as it takes. The module note above builds the whole
    /// guarded-step design around surviving *"a forty-minute CI run"*, so a train that stopped one
    /// at twenty minutes would have traded a parked train for a broken one.
    ///
    /// What `checks: pending` earns is [`WAITING_ON_A_CHECK_MS`] of patience — more than seventy
    /// times the other ceiling — and not immunity. It used to earn immunity, and the two things it
    /// cannot tell apart are a check that is running and a check that stopped running without
    /// saying so; the second parks the front of a serial train for ever, and everything behind it,
    /// saying "CI is running" about a run that no longer exists (SKEIN-283).
    ///
    /// So the assertions come in pairs. Six hours of `pending` is a long build and is left alone.
    /// Twenty-five hours of `pending` is past every bound GitHub applies to a check of its own,
    /// and stops — with a sentence that says which of the two ceilings fell.
    #[test]
    fn a_running_check_earns_patience_and_not_immunity() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flow = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,"steps":[
              {"when":[],"do":"wait:CI is running"}]}]}"#,
        )
        .unwrap()
        .remove(0);
        let chosen = Chosen {
            step: 0,
            act: Act::Wait("CI is running".into()),
        };
        let running = crate::workflow::Facts {
            checks: "pending".into(),
            ..Default::default()
        };

        // A wait of a chosen age, written into the timeline. Each pull request gets its own,
        // because a stop written by one assertion would otherwise reset the next one's clock and
        // it would pass without the rule ever being consulted.
        let waiting_for = |number: u64, flow: &str, ms: i64| {
            record("demo", number, flow, 1, "waiting", "CI is running");
            let mut all = journal("demo", number);
            let last = all.len() - 1;
            all[last].at_ms -= ms;
            let mut file: std::collections::BTreeMap<String, Vec<JournalEntry>> =
                serde_json::from_str(&std::fs::read_to_string(journal_path("demo")).unwrap())
                    .unwrap();
            file.insert(number.to_string(), all);
            std::fs::write(
                journal_path("demo"),
                serde_json::to_vec_pretty(&file).unwrap(),
            )
            .unwrap();
        };

        // Six hours of a build that is genuinely running. Eighteen times past the ceiling a wait
        // with nothing behind it gets, and it must not be touched.
        waiting_for(7, "merge-train", 6 * 60 * 60 * 1000);
        assert_eq!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                7,
                &flow,
                &chosen,
                &running,
                "CI is running"
            ),
            None,
            "a check that is still running was stopped six hours into a build that is allowed to \
             take as long as it takes"
        );
        assert_eq!(stopped("demo", 7), None, "and it wrote the stop down too");

        // And the same wait with nothing running IS bounded, from the same timeline — so what
        // separates them is the evidence and not the clock.
        let nothing = crate::workflow::Facts {
            checks: "none".into(),
            ..Default::default()
        };
        assert!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                7,
                &flow,
                &chosen,
                &nothing,
                "CI is running"
            )
            .is_some(),
            "the bound never falls at all, on any wait"
        );

        // A check that has said `pending` for twenty-five hours. GitHub cancels a job at six hours
        // of running and at twenty-four of queueing, so nothing is coming — and until SKEIN-283
        // this parked the front of the train, and everything behind it, with no bound at all.
        waiting_for(11, "merge-train", WAITING_ON_A_CHECK_MS + 60 * 60 * 1000);
        let why = a_wait_that_will_not_end_on_its_own(
            "demo",
            11,
            &flow,
            &chosen,
            &running,
            "CI is running",
        )
        .unwrap_or_else(|| {
            panic!(
                "a front whose check has read `pending` for twenty-five hours is still holding \
                 the line, silently, and nothing in skein can ever stop it"
            )
        });
        assert!(
            why.contains("25 hours") && why.contains("pending"),
            "the stop does not say which ceiling fell or for how long: {why}"
        );
        assert_eq!(
            stopped("demo", 11).as_deref(),
            Some(why.as_str()),
            "the stop was returned but never written down, so the next pass parks again"
        );

        // The line between them is the ceiling and nothing else: one hour SHORT of it, the same
        // pending check is left alone.
        waiting_for(12, "merge-train", WAITING_ON_A_CHECK_MS - 60 * 60 * 1000);
        assert_eq!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                12,
                &flow,
                &chosen,
                &running,
                "CI is running"
            ),
            None,
            "a check pending for twenty-three hours was stopped — the ceiling is not where it says"
        );

        // And a workflow that is not a train is left alone even then — on its OWN expired clock,
        // so the only thing that can spare it is being non-serial. A stop is a demand for
        // somebody's attention, earned when the alternative is a queue that has stopped moving; a
        // pull request blocking nobody is not costing anything by waiting, and stopping it would
        // be manufacturing work.
        let loose = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"loose","steps":[{"when":[],"do":"wait:CI is running"}]}]}"#,
        )
        .unwrap()
        .remove(0);
        waiting_for(8, "loose", WAITING_ON_A_CHECK_MS + 60 * 60 * 1000);
        assert_eq!(
            a_wait_that_will_not_end_on_its_own(
                "demo",
                8,
                &loose,
                &chosen,
                &nothing,
                "CI is running"
            ),
            None,
            "a workflow with no train behind it stopped a pull request for waiting, which costs a \
             person an interruption and nobody a queue"
        );
        assert_eq!(stopped("demo", 8), None, "and it wrote the stop down too");

        std::env::remove_var("SKEIN_HOME");
    }

    /// The train view names the front, the whole line, and the passed-over — and only for a
    /// workflow that is serial, because a train is the serial thing.
    ///
    /// This is the panel's read of the same rule the tick acts on ([`sweep`] calls [`trains`]
    /// too), so what it asserts is the rule itself: oldest first, the first unstopped one is the
    /// front, a stopped PR is in the line AND named with its reason.
    #[test]
    fn a_train_view_names_the_front_the_line_and_the_passed_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[
              {"name":"train","serial":true,"steps":[{"when":[],"do":"merge:squash"}]},
              {"name":"loose","steps":[{"when":[],"do":"add-label:ci"}]}]}"#,
        )
        .unwrap();
        stop("demo", 5, "CI is red");

        // Handed over scrambled, so the order below is the function's own and not the caller's.
        let prs = vec![
            (9, "train".to_string()),
            (3, "loose".to_string()),
            (5, "train".to_string()),
            (7, "train".to_string()),
        ];
        let views = trains("demo", &prs, &flows);
        assert_eq!(
            views.len(),
            1,
            "a non-serial workflow got a train view: {views:?}"
        );
        let view = &views[0];
        assert_eq!(view.flow, "train");
        assert_eq!(
            view.front,
            Some(7),
            "the front must be the oldest UNSTOPPED pull request"
        );
        assert_eq!(
            view.line,
            vec![5, 7, 9],
            "train order is oldest first, front included"
        );
        assert_eq!(view.stopped.len(), 1);
        assert_eq!(
            (view.stopped[0].number, view.stopped[0].why.as_str()),
            (5, "CI is red"),
            "the passed-over must be named with its reason"
        );

        // Everyone stopped: a train with nobody to move has no front, and still shows its line.
        stop("demo", 7, "conflicts");
        stop("demo", 9, "checks");
        let views = trains("demo", &prs, &flows);
        assert_eq!(views[0].front, None);
        assert_eq!(views[0].line, vec![5, 7, 9]);
        assert_eq!(views[0].stopped.len(), 3);

        std::env::remove_var("SKEIN_HOME");
    }
}
