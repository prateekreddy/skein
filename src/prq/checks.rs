//! What the check rollup says, and which checks are behind it.
//!
//! [`CheckVerdict`] is the single place "failing" is defined, shared by [`rollup`] (the word on
//! the row) and [`failing_contexts`] (the names under it) — two copies of that classification is a
//! row that says "failing" while naming nothing.

use super::node::{check_rollup, contexts, CheckContext, PrNode};
use super::*;

/// One context's verdict. The single place "failing" is defined, shared by [`rollup`] (the word
/// on the row) and [`failing_contexts`] (the names under it) — two copies of this classification
/// is a row that says "failing" while naming nothing, or names a check its own dot calls green.
enum CheckVerdict {
    Failing,
    Pending,
    Passing,
}

fn verdict(c: &CheckContext) -> CheckVerdict {
    // A CheckRun carries `status`/`conclusion`; a classic StatusContext carries only `state`,
    // whose values (SUCCESS, FAILURE, ERROR, PENDING…) overlap enough to share the match.
    let status = c.status.as_deref().unwrap_or("");
    let conclusion = c.conclusion.as_deref().or(c.state.as_deref()).unwrap_or("");
    match conclusion {
        "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "ERROR" => {
            CheckVerdict::Failing
        }
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => CheckVerdict::Passing,
        // A conclusion this code does not know on a COMPLETED run is treated as failing — the
        // over-report direction — where an incomplete run is merely pending.
        _ if status == "COMPLETED" => CheckVerdict::Failing,
        _ => CheckVerdict::Pending,
    }
}

/// Reduce `statusCheckRollup` to four words.
///
/// Any failure anywhere is failing; otherwise any incomplete run is pending. Failing wins over
/// pending because a red check is information you act on now, and a queue that showed "pending"
/// for a PR with a broken build would be hiding the useful half.
///
/// **Two sources, and the more cautious of them wins** (SKEIN-232). The contexts array is a page of
/// a hundred; GitHub's own `state` is its verdict over all of them however many there are. Read
/// from the page alone, a pull request whose 101st context is red reads "passing" — and
/// `docs/pr-workflow.md`'s merge train acts on `checks:passing` by merging and deleting the branch,
/// so that is not a wrong dot on a row, it is a merge performed on a guarantee that was never
/// checked. So a red in either source is "failing", and where they disagree in the other direction
/// — GitHub says green while a context this page holds has not finished — the answer is "pending".
/// Both of those are the over-report direction this function already takes for a conclusion it does
/// not recognise (see [`verdict`]): more of your attention, and never a merge on a check nobody read.
///
/// `state` absent is a real case, not a nuisance: an answer from before this field was asked for,
/// and every fixture written against the old shape. Then the page is all there is — and if the page
/// was TRUNCATED (`statusCheckRollupTotal` past its length) a walk that found nothing wrong has not
/// earned "passing", so it says "pending" and [`queue_within`] adds the blind spot that says why.
pub(super) fn rollup(item: &PrNode) -> String {
    let checks = contexts(item);
    let state = check_rollup(item)
        .map(|r| r.state.as_str())
        .filter(|s| !s.is_empty());
    // No contexts and none claimed: nothing has ever run against this commit. Unchanged, and it is
    // why `total` may not simply default to zero — an absent `totalCount` means "not said".
    if checks.is_empty() && rollup_total(item).unwrap_or(0) == 0 {
        return "none".into();
    }
    let mut pending = false;
    for c in checks {
        match verdict(c) {
            CheckVerdict::Failing => return "failing".into(),
            CheckVerdict::Pending => pending = true,
            CheckVerdict::Passing => {}
        }
    }
    match state {
        // GitHub's own words for a red commit. `EXPECTED` is a context somebody promised and has
        // not sent, which is pending by every reading.
        Some("FAILURE" | "ERROR") => "failing".into(),
        Some("SUCCESS") if !pending => "passing".into(),
        // `PENDING`, `EXPECTED`, a green rollup over a context this page has not seen finish, or a
        // word this code does not know — none of which is a check somebody may merge on.
        Some(_) => "pending".into(),
        None if pending || truncated_rollup(item) => "pending".into(),
        None => "passing".into(),
    }
}

/// Did the answer carry GitHub's own rollup verdict at all? The one state [`rollup`] cannot decide
/// from either source, and the queue says so rather than letting its "pending" pass for CI running.
pub(super) fn rollup_state_missing(item: &PrNode) -> bool {
    !check_rollup(item).is_some_and(|r| !r.state.is_empty())
}

/// How many contexts GitHub says the rollup has, where it said — see [`PR_FRAGMENT`].
pub(super) fn rollup_total(item: &PrNode) -> Option<usize> {
    check_rollup(item)
        .and_then(|r| r.contexts.total_count)
        .map(|n| n as usize)
}

/// Did the check rollup have more contexts than the answer carried?
///
/// The comparison is against what actually arrived rather than against [`SEARCH_PAGE`]'s sibling
/// hundred, so it stays true if the page size ever moves.
pub(super) fn truncated_rollup(item: &PrNode) -> bool {
    let read = contexts(item).len();
    rollup_total(item).is_some_and(|total| total > read)
}

/// WHICH contexts are behind a red rollup — name and detail link, first [`FAILING_CHECKS_SHOWN`]
/// in rollup order, deduplicated by name (SKEIN-153).
///
/// Deduplicated because re-runs of one check arrive as repeated contexts, and a row that says
/// "build, build, build" answers the question worse than one that says "build". A failing context
/// GitHub gave no name for is skipped rather than shown blank: the one-word `checks` verdict
/// still says "failing", so nothing is hidden — there is just no name to show for it.
///
/// The same holds for the red that lives past the hundredth context: [`rollup`] says "failing" on
/// GitHub's verdict, and this returns nothing to name it by, because the name is in the part of the
/// list nobody read. An empty list under a red verdict is that, and it is the right way round — a
/// verdict with no names sends you to GitHub; names with no verdict would have sent you nowhere.
pub(super) fn failing_contexts(item: &PrNode) -> Vec<FailedCheck> {
    let mut out: Vec<FailedCheck> = Vec::new();
    for c in contexts(item) {
        if !matches!(verdict(c), CheckVerdict::Failing) {
            continue;
        }
        // A CheckRun names itself `name` and links `detailsUrl`; a StatusContext is named by its
        // `context` and links `targetUrl`. Same fields the query asks for, per branch.
        let Some(name) = c
            .name
            .as_deref()
            .or(c.context.as_deref())
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        if out.iter().any(|f| f.name == name) {
            continue;
        }
        let url = c
            .details_url
            .as_deref()
            .or(c.target_url.as_deref())
            .unwrap_or("")
            .to_string();
        out.push(FailedCheck {
            name: name.to_string(),
            url,
        });
        if out.len() == FAILING_CHECKS_SHOWN {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prq::fixtures::{
        batched_github, batched_repo, graphql_requests, item, node, node_of,
    };
    use crate::prq::node::build_pr;
    use crate::prq::search::{PR_FRAGMENT, SEARCH_PAGE};

    /// A pull-request node carrying nothing but a check rollup, for the readers that only look at
    /// one. GitHub hangs the rollup off `commits(last: 1)`, which is why a fixture about checks has
    /// a commit in it: the nesting IS the thing under test for [`rollup`]'s two sources.
    fn checks_of(contexts: &str) -> String {
        format!(
            r#"{{"commits":{{"nodes":[{{"commit":{{"statusCheckRollup":{{"contexts":{{"nodes":[{contexts}]}}}}}}}}]}}}}"#
        )
    }

    #[test]
    fn a_red_check_beats_a_pending_one() {
        let v = node(&checks_of(
            r#"{"status":"COMPLETED","conclusion":"FAILURE"},{"status":"IN_PROGRESS"}"#,
        ));
        assert_eq!(rollup(&v), "failing");
    }

    #[test]
    fn checks_vocabulary_matches_ship() {
        assert_eq!(rollup(&node(r#"{}"#)), "none");
        assert_eq!(rollup(&node(&checks_of(""))), "none");
        assert_eq!(
            rollup(&node(&checks_of(
                r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#
            ))),
            "passing"
        );
        assert_eq!(
            rollup(&node(&checks_of(r#"{"status":"IN_PROGRESS"}"#))),
            "pending"
        );
        assert_eq!(
            rollup(&node(&checks_of(r#"{"state":"SUCCESS"}"#))),
            "passing"
        );
    }

    #[test]
    fn a_completed_check_with_an_unknown_conclusion_is_failing_not_passing() {
        // Unknown must not read as green: a check state skein does not recognise is exactly the
        // case where it should defer to you rather than clear the PR.
        let v = node(&checks_of(r#"{"status":"COMPLETED","conclusion":"WEIRD"}"#));
        assert_eq!(rollup(&v), "failing");
    }

    /// SKEIN-153: a red row says WHICH check failed, not just that something did. The one-word
    /// `checks` stays for lanes and sorting; the names and links are what turn "failing" from a
    /// dot into an answer. Both branches of the context union must survive the parse — a CheckRun
    /// names itself `name`/`detailsUrl`, a classic StatusContext `context`/`targetUrl` — which is
    /// why every field on [`CheckContext`] is an `Option` rather than a defaulted string.
    #[test]
    fn a_red_row_names_the_checks_that_failed_with_their_links() {
        let node = serde_json::json!({
            "number": 8, "title": "t", "url": "u", "isDraft": false,
            "author": {"login": "someone"}, "headRefName": "f", "headRefOid": "a",
            "baseRefName": "main", "updatedAt": "2026-08-23T12:00:00Z",
            "latestReviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
                {"name": "build", "detailsUrl": "https://ci/build/1",
                 "status": "COMPLETED", "conclusion": "FAILURE"},
                {"name": "lint", "detailsUrl": "https://ci/lint/1",
                 "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"context": "deploy/preview", "targetUrl": "https://status/preview",
                 "state": "FAILURE"},
            ]}}}}]},
        });
        let pr = build_pr(
            &node_of(&node),
            8,
            "me",
            &Reason::Reviewer,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(pr.checks, "failing", "the one-word verdict is unchanged");
        assert_eq!(
            pr.failing_checks,
            vec![
                FailedCheck {
                    name: "build".into(),
                    url: "https://ci/build/1".into()
                },
                FailedCheck {
                    name: "deploy/preview".into(),
                    url: "https://status/preview".into()
                },
            ],
            "the failing contexts by name and link, in rollup order — the green one is not news"
        );

        // And the fields are actually ASKED for: everything above works on a node handed to it,
        // so without this the names would be read from a reply GitHub was never told to include.
        assert!(
            PR_FRAGMENT.contains("... on CheckRun { name detailsUrl status conclusion }"),
            "the CheckRun name/link is read but never requested: {}",
            *PR_FRAGMENT
        );
        assert!(
            PR_FRAGMENT.contains("... on StatusContext { context targetUrl state }"),
            "the StatusContext name/link is read but never requested: {}",
            *PR_FRAGMENT
        );
    }

    /// The cap and the dedupe: re-runs of one check arrive as repeated contexts, and fifty red
    /// checks are one broken pipeline — the row names the first [`FAILING_CHECKS_SHOWN`] distinct
    /// ones and stops. Absence stays absent throughout: a green rollup names nothing, a missing
    /// link renders as no link, and a queue remembered before the field existed still parses.
    #[test]
    fn failing_check_names_are_deduplicated_capped_and_absent_when_green() {
        // Seven failing contexts, but "build" three times (re-runs) and one nameless: five slots,
        // taken in order by the distinct named ones.
        let red = node(&checks_of(
            r#"{"name":"build","detailsUrl":"https://ci/1","status":"COMPLETED","conclusion":"FAILURE"},
               {"name":"build","detailsUrl":"https://ci/2","status":"COMPLETED","conclusion":"FAILURE"},
               {"status":"COMPLETED","conclusion":"FAILURE"},
               {"name":"unit","status":"COMPLETED","conclusion":"FAILURE"},
               {"name":"e2e","status":"COMPLETED","conclusion":"TIMED_OUT"},
               {"context":"style","state":"ERROR"},
               {"name":"build","detailsUrl":"https://ci/3","status":"COMPLETED","conclusion":"FAILURE"},
               {"name":"docs","status":"COMPLETED","conclusion":"CANCELLED"},
               {"name":"pack","status":"COMPLETED","conclusion":"FAILURE"},
               {"name":"sixth","status":"COMPLETED","conclusion":"FAILURE"}"#,
        ));
        let named = failing_contexts(&red);
        assert_eq!(named.len(), FAILING_CHECKS_SHOWN, "capped, not the log");
        let names: Vec<&str> = named.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["build", "unit", "e2e", "style", "docs"],
            "distinct names in rollup order — the nameless one is skipped, the word already says failing"
        );
        assert_eq!(
            named[0].url, "https://ci/1",
            "a re-run does not steal the first run's link"
        );
        assert_eq!(named[1].url, "", "a rollup with no link stays linkless");

        let green = node(&checks_of(
            r#"{"name":"build","status":"COMPLETED","conclusion":"SUCCESS"}"#,
        ));
        assert!(
            failing_contexts(&green).is_empty(),
            "nothing failed, nothing to name"
        );

        // A queue remembered on disk by an older skein has neither new field, and must not stop
        // parsing over it — that failure mode turns a new field into an empty pane.
        let old: Pr = serde_json::from_value(serde_json::json!({
            "number": 7, "title": "t", "author": "a", "url": "u",
            "head_ref": "f", "head_sha": "s", "base_ref": "main",
            "draft": false, "updated_at": "", "committed_at": "",
            "checks": "failing", "my_review": "none", "review_is_current": false,
            "reasons": [], "lane": "needs-you", "box_name": "b",
        }))
        .expect("a remembered queue from before these fields must stay readable");
        assert!(old.failing_checks.is_empty());
        assert!(!old.snoozed);
    }

    /// A check rollup as GitHub sends one: `state` beside a page of `contexts`.
    ///
    /// `state: None` is an answer from before SKEIN-232 asked for the field, which is what every
    /// fixture written against the old shape is — those must keep reading exactly as they did.
    fn rollup_node(state: Option<&str>, total: usize, contexts: &[&str]) -> serde_json::Value {
        let state = match state {
            Some(s) => format!(r#""state":"{s}","#),
            None => String::new(),
        };
        item(&format!(
            r#"{{"number":7,"title":"t","url":"u","isDraft":false,
                "headRefName":"feat","headRefOid":"abc","baseRefName":"main",
                "updatedAt":"2026-08-25T00:00:00Z","author":{{"login":"me"}},
                "latestReviews":{{"nodes":[]}},
                "commits":{{"nodes":[{{"commit":{{"committedDate":"2026-08-25T00:00:00Z",
                  "statusCheckRollup":{{{state}"contexts":{{"totalCount":{total},"nodes":[{}]}}}}
                }}}}]}}}}"#,
            contexts.join(",")
        ))
    }

    /// A hundred green contexts: the page GitHub fills, and the whole of what skein used to see.
    fn a_full_page_of_green() -> Vec<&'static str> {
        vec![r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#; SEARCH_PAGE]
    }

    /// **A red check past the hundredth context must not read as green** (SKEIN-232).
    ///
    /// The queue used to compute the verdict itself, from a `contexts(first: 100)` page, and never
    /// asked GitHub for its own answer over all of them. A matrix build (`os × rust-version ×
    /// feature`) reaches three digits routinely, so the 101st context being red was invisible.
    ///
    /// The consequence is asserted here rather than described, because it is not a wrong dot on a
    /// row: `docs/pr-workflow.md`'s merge train fires on `checks:passing` — the same string, off the
    /// same field ([`crate::prwork`] builds `Facts::checks` from `Pr::checks`) — and its act is
    /// `merge:squash+delete`. A green verdict computed from a page nobody could see all of is a
    /// merged pull request whose CI failed, with the journal recording a clean merge.
    #[test]
    fn a_red_check_past_the_hundredth_context_does_not_read_as_passing() {
        let flat = node_of(&rollup_node(Some("FAILURE"), 143, &a_full_page_of_green()));

        assert_eq!(
            rollup(&flat),
            "failing",
            "every context skein can see is green and GitHub says the commit is red — the red is \
             in the 43 it never read, and GitHub's own verdict is the only thing that knows"
        );

        // And the act that verdict guards. The owner's train, as `docs/pr-workflow.md` writes it.
        let train = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"train","serial":true,"matches":["mine"],"steps":[
                 {"when":["label:ci-queue","checks:passing","mergeable","current"],
                  "do":"merge:squash+delete"}]}]}"#,
        )
        .expect("the merge train must be expressible");
        let facts = crate::workflow::Facts {
            approved: true,
            labels: vec!["ci-queue".into()],
            checks: rollup(&flat),
            mergeable: Some(true),
            behind: Some(false),
            base_is_trunk: Some(true),
            mine: true,
            // `ci-queue` is ALL of this pull request's labels, said rather than defaulted:
            // `Facts::default()` is skein having looked nothing up, and on that a `no-label:` may
            // not hold (SKEIN-373). This fixture is about the check rollup, so it must not be
            // silently exercising a truncated label list as well.
            labels_whole: true,
            ..Default::default()
        };
        assert_eq!(
            crate::workflow::next(&train[0], &facts),
            None,
            "the merge train took a step on a pull request whose CI failed — this is the merge, \
             and the branch deletion, that the wrong verdict authorises"
        );

        // The other direction, which is what stops this fix being "say pending and never merge":
        // a rollup GitHub calls green, whose contexts are green, still merges.
        let green = node_of(&rollup_node(Some("SUCCESS"), 143, &a_full_page_of_green()));
        assert_eq!(rollup(&green), "passing");
        let facts = crate::workflow::Facts {
            checks: rollup(&green),
            ..facts
        };
        assert!(
            matches!(
                crate::workflow::next(&train[0], &facts),
                Some(crate::workflow::Chosen {
                    act: crate::workflow::Act::Merge(_),
                    ..
                })
            ),
            "a genuinely green pull request must still merge, or the fix has broken the train \
             instead of the bug"
        );
    }

    /// The verdict is the more cautious of the two sources, and an old answer still reads as it did.
    ///
    /// Each line here is a case the single-source walk got wrong or must keep getting right — the
    /// last two are the fixtures from before `state` was asked for, which have no `state` at all
    /// and must be untouched by any of this.
    #[test]
    fn a_check_verdict_never_out_ranks_the_source_that_saw_more() {
        let red = r#"{"status":"COMPLETED","conclusion":"FAILURE"}"#;
        let running = r#"{"status":"IN_PROGRESS"}"#;
        let green = r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#;
        let verdict = |state, total, contexts: &[&str]| {
            rollup(&node_of(&rollup_node(state, total, contexts)))
        };

        // GitHub's word for a red commit, whichever word it uses, over a page that looks fine.
        assert_eq!(
            verdict(Some("FAILURE"), 143, &a_full_page_of_green()),
            "failing"
        );
        assert_eq!(
            verdict(Some("ERROR"), 143, &a_full_page_of_green()),
            "failing"
        );
        // Still running, over a page that has all finished.
        assert_eq!(
            verdict(Some("PENDING"), 143, &a_full_page_of_green()),
            "pending"
        );
        assert_eq!(
            verdict(Some("EXPECTED"), 143, &a_full_page_of_green()),
            "pending"
        );
        // A word this code does not know is not a merge anybody may take.
        assert_eq!(verdict(Some("SOMETHING_NEW"), 1, &[green]), "pending");
        // And the page out-ranks a green verdict in the other direction: a red or an unfinished
        // context skein can SEE is not overruled by GitHub calling the commit green.
        assert_eq!(verdict(Some("SUCCESS"), 101, &[red]), "failing");
        assert_eq!(verdict(Some("SUCCESS"), 101, &[running]), "pending");
        // Nothing has run: unchanged, and it is why `totalCount` may not simply default to zero.
        assert_eq!(verdict(Some("EXPECTED"), 0, &[]), "none");

        // No `state` — every fixture written before SKEIN-232, and any answer GitHub gives without
        // one. The page is all there is, and it is read exactly as it always was…
        assert_eq!(verdict(None, 1, &[green]), "passing");
        assert_eq!(verdict(None, 2, &[green, red]), "failing");
        assert_eq!(verdict(None, 0, &[]), "none");
        // …except that a page which was CUT OFF has not earned "passing" on its own.
        assert_eq!(
            verdict(None, 143, &a_full_page_of_green()),
            "pending",
            "a walk of 100 of 143 contexts that found nothing wrong knows nothing about the 43"
        );
    }

    /// The queue says which pull request's checks it could not read (SKEIN-232).
    ///
    /// "pending" is the honest verdict for a cut-off list nobody gave a verdict for, and on its own
    /// it is indistinguishable from CI still running — which is a sentence somebody waits on. The
    /// blind spot is the difference, and it fires only where the truncation actually costs the
    /// answer: with GitHub's `state` present the cap costs a NAME and nothing else, and a blind spot
    /// per matrix build would be noise over a verdict that is right.
    #[test]
    fn a_check_list_the_queue_could_not_read_to_the_end_says_so() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");

        let node = |state: Option<&str>| {
            serde_json::to_string(&rollup_node(state, 143, &a_full_page_of_green())).unwrap()
        };
        let answer = |state: Option<&str>| {
            format!(
                r#"{{"data":{{"q0":{{"nodes":[{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                node(state)
            )
        };

        let (base, seen) = batched_github(false, 200, answer(None));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/rollup-cut"), true).expect("the queue answered");

        assert!(
            graphql_requests(&seen)[0]
                .contains("statusCheckRollup { state contexts(first: 100) { totalCount"),
            "the fragment must ask for GitHub's own verdict and the size of the list: {}",
            graphql_requests(&seen)[0]
        );
        assert_eq!(
            q.prs[0].checks, "pending",
            "a cut-off list with no verdict must not read as green"
        );
        assert!(
            q.blind_spots.iter().any(|b| b.contains("#7's checks")
                && b.contains("143")
                && b.contains("no rollup verdict")),
            "the row says `pending` and nothing says why it is not `passing`: {:?}",
            q.blind_spots
        );

        // The same pull request, with GitHub's verdict beside the same cut-off page: the answer is
        // certain, so there is nothing to warn about.
        let (base, _seen) = batched_github(false, 200, answer(Some("SUCCESS")));
        std::env::set_var("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        let q = queue(&batched_repo("acme/rollup-cut"), true).expect("the queue answered");
        assert_eq!(q.prs[0].checks, "passing");
        assert!(
            !q.blind_spots.iter().any(|b| b.contains("#7's checks")),
            "a verdict GitHub stands behind needs no blind spot beside it: {:?}",
            q.blind_spots
        );

        for key in ["SKEIN_HOME", "GH_TOKEN", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
        forget_host_token();
        forget_renames();
    }
}
