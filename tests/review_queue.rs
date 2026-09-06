//! The review queue end to end, against a stubbed GitHub.
//!
//! The unit tests in `src/prq.rs` cover lane and check derivation from one JSON item. What they
//! cannot cover is the part where a PR goes missing: three separate searches are merged into one
//! list, and a mistake there drops rows rather than mislabelling them. That is the failure this
//! file exists for — a queue you trust and that quietly under-reports is worse than no queue.
//!
//!   cargo test --test review_queue

mod common;

use common::{env_lock, fake_github, Scratch};
use std::fs;
use std::path::{Path, PathBuf};

/// A GitHub that answers from fixture files instead of the real one.
///
/// It used to be a stub `gh` on `$PATH`; skein reads the API directly now, so the stub is an API.
/// Dispatch is on the request: `/user`, `/user/teams`, and one GraphQL search whose `q` variable
/// names the query. Each search reads `search-<term>.json` if present, else answers empty — so a
/// test declares only the queries it cares about.
///
/// One connection at a time, closed after each answer, on a thread that lives as long as the
/// process (`common::fake_github`) — a test's stub outliving its test is harmless here because
/// each gets its own port.
fn stub_github(dir: &Path, login: &str, teams_ok: bool) -> String {
    let root = dir.to_path_buf();
    let login = login.to_string();
    fake_github(move |req| {
        // One line per request, so a test can assert what an operation COSTS in GitHub calls —
        // the merged queue's whole promise is a number here staying put.
        {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("hits.log"))
            {
                let _ = f.write_all(b"x\n");
            }
        }
        let path = &req.path;
        if path.starts_with("/user/teams") {
            match teams_ok {
                true => (
                    200,
                    r#"[{"slug":"core","organization":{"login":"acme"}}]"#.to_string(),
                ),
                // What a token without `read:org` actually gets.
                false => (403, r#"{"message":"Requires read:org"}"#.to_string()),
            }
        } else if path.starts_with("/user") {
            (200, format!(r#"{{"login":"{login}"}}"#))
        } else if path.starts_with("/graphql")
            && root.join("too-heavy").exists()
            && String::from_utf8_lossy(&req.body).contains("\"q1\"")
        {
            // GitHub's EDGE shedding a request its backend did not finish: nginx's own HTML,
            // which the API never produces — reported live as `502 Bad Gateway` on the owner's
            // five-alias refresh (SKEIN-266). Refused only while the request carries more than
            // one search, so the split retry lands on the branch below and the test can tell
            // "GitHub is down" from "GitHub would not take it all at once".
            (
                502,
                "<html><head><title>502 Bad Gateway</title></head><body>nginx</body></html>"
                    .to_string(),
            )
        } else if path.starts_with("/graphql") && root.join("dead-request").exists() {
            // The whole request dying, rather than one alias inside it — a 5xx, and what the
            // network and the rate-limit hold both look like from here. Every membership
            // search of a refresh rides this one request (SKEIN-209), so nothing comes back
            // at all, which is the shape SKEIN-229 turns on.
            (500, r#"{"message":"Server Error"}"#.to_string())
        } else if path.starts_with("/graphql") {
            // The batched wire (SKEIN-209): one request, every membership search an alias
            // `q0..qN`, one variable each. A term with a `fail-<term>` marker answers the way
            // GitHub delivers a partial failure — `data.qN: null` plus an errors entry whose
            // `path` names the alias — so the queue's per-rule blind spots stay testable.
            let sent: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            let mut aliases: Vec<(usize, String)> = sent
                .get("variables")
                .and_then(|v| v.as_object())
                .map(|vars| {
                    vars.iter()
                        .filter_map(|(k, v)| {
                            let i: usize = k.strip_prefix('q')?.parse().ok()?;
                            Some((i, v.as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            aliases.sort();
            let mut data = Vec::new();
            let mut errors = Vec::new();
            for (i, q) in &aliases {
                // The term is what follows `is:open ` — the part the caller asked for.
                let term = q.rsplit("is:open ").next().unwrap_or("").trim().to_string();
                let safe = safe_term(&term);
                if root.join(format!("fail-{safe}")).exists() {
                    data.push(format!(r#""q{i}":null"#));
                    errors.push(format!(
                        r#"{{"message":"HTTP 403: forbidden","path":["q{i}"]}}"#
                    ));
                } else {
                    let nodes = std::fs::read_to_string(root.join(format!("search-{safe}.json")))
                        .unwrap_or_else(|_| "[]".to_string());
                    data.push(format!(r#""q{i}":{{"nodes":{nodes}}}"#));
                }
            }
            let payload = match errors.is_empty() {
                true => format!(r#"{{"data":{{{}}}}}"#, data.join(",")),
                false => format!(
                    r#"{{"data":{{{}}},"errors":[{}]}}"#,
                    data.join(","),
                    errors.join(",")
                ),
            };
            (200, payload)
        } else {
            (404, format!(r#"{{"message":"no stub for {path}"}}"#))
        }
    })
}

fn safe_term(term: &str) -> String {
    term.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn put_search(dir: &Path, term: &str, json: &str) {
    fs::write(dir.join(format!("search-{}.json", safe_term(term))), json).unwrap();
}

/// Make one query fail the way GitHub does when a token lacks a scope.
fn fail_search(dir: &Path, term: &str) {
    fs::write(dir.join(format!("fail-{}", safe_term(term))), "").unwrap();
}

/// Take GitHub down for the whole refresh, rather than for one search inside it.
fn dead_request(dir: &Path) {
    fs::write(dir.join("dead-request"), "").unwrap();
}

fn repo(id: &str) -> skein::repos::Repo {
    skein::repos::Repo {
        id: id.into(),
        // Reading unattended is off unless somebody says so, per repo — the queue tests are about
        // what the queue SAYS, not about what skein would go and read from it.
        read_prs: false,
        source: "https://github.com/acme/thing.git".into(),
        store: "/nonexistent".into(),
        agent: "claude".into(),
        plane_project: String::new(),
        sync_connection: String::new(),
        review_queue: true,
        sync_gateway_url: String::new(),
        ..Default::default()
    }
}

/// One PR item, in the shape the GraphQL search returns.
fn pr_json(number: u64, title: &str, extra: &str) -> String {
    format!(
        r#"{{"number":{number},"title":"{title}","author":{{"login":"someone"}},"url":"https://github.com/acme/thing/pull/{number}","headRefName":"feat-{number}","headRefOid":"sha{number}","baseRefName":"main","isDraft":false,"updatedAt":"2026-08-1{number}T00:00:00Z"{extra}}}"#
    )
}

/// One test at a time, and one scratch home per test, both released when `Env` drops.
///
/// Cargo runs the tests in one integration binary as parallel threads of a **single process**, and
/// `$SKEIN_HOME` / `$SKEIN_GH_BIN` are process-global. Without the lock every test races: one
/// test's stubbed `gh` answers another's queries, and the symptom is empty queues and a missing
/// blind spot rather than an error.
/// **Field order is the drop order**, and it is load-bearing: the scratch directory has to go
/// before the lock does. With the lock released first, the next test takes it, makes its own
/// directory at the same path, and this one's `Drop` then deletes it underneath — which is exactly
/// what four tests here started doing the moment the per-test directory stopped carrying a unique
/// name of its own.
struct Env {
    _dir: Scratch,
    _lock: std::sync::MutexGuard<'static, ()>,
}

/// Point skein's home and its GitHub at a scratch directory for the duration of one test.
fn setup(login: &str, teams: bool) -> (Env, PathBuf) {
    let lock = env_lock();
    let dir = Scratch::temp("skein-review");
    let path = dir.to_path_buf();
    let api = stub_github(&path, login, teams);
    std::env::set_var("SKEIN_GITHUB_API", &api);
    std::env::set_var("SKEIN_HOME", &path);
    // A token, because the queue refuses to run without one now — the credential is skein's rather
    // than `gh`'s, so the test has to supply it the way a fleet would.
    std::env::set_var("GH_TOKEN", "test-token");
    skein::prq::forget_host_token();
    // The batch-width memo is per process and keyed by slug, like the rename and trunk memos
    // beside it — and every test here refreshes `acme/thing`. Without this, the test that proves
    // a too-heavy batch is split leaves every later test asking one search at a time.
    skein::prq::forget_batch_widths();
    (
        Env {
            _dir: dir,
            _lock: lock,
        },
        path,
    )
}

/// The whole reason this file exists: a PR returned by more than one query must appear once, with
/// every reason it qualified under. Dropping the duplicate is easy; dropping the *reason* is the
/// subtle version, and it breaks the author/reviewer/mentioned filter rather than the list.
#[test]
fn a_pr_matching_several_queries_appears_once_with_every_reason() {
    let (_env, dir) = setup("me", false);
    let both = format!("[{}]", pr_json(1, "shared", ""));
    put_search(&dir, "review-requested:me", &both);
    put_search(&dir, "author:me", &both);
    put_search(&dir, "mentions:me", "[]");

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(q.prs.len(), 1, "one PR, not one per query");
    assert_eq!(q.prs[0].number, 1);
    assert_eq!(
        q.prs[0].reasons,
        vec![skein::prq::Reason::Reviewer, skein::prq::Reason::Author],
        "both memberships kept, in query order"
    );
}

#[test]
fn every_query_contributes_its_own_prs() {
    let (_env, dir) = setup("me", false);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(1, "a", "")),
    );
    put_search(&dir, "author:me", &format!("[{}]", pr_json(2, "b", "")));
    put_search(&dir, "mentions:me", &format!("[{}]", pr_json(3, "c", "")));

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    let mut numbers: Vec<u64> = q.prs.iter().map(|p| p.number).collect();
    numbers.sort();
    assert_eq!(numbers, vec![1, 2, 3]);
}

/// The queue must say what it could not see. A `gh` without `read:org` cannot list your teams, so
/// every PR where only a team of yours was asked to review is missing — and silence there is
/// exactly the omission that costs a merge.
#[test]
fn a_queue_that_cannot_see_your_teams_says_so() {
    let (_env, dir) = setup("me", false);
    put_search(&dir, "review-requested:me", "[]");

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert!(
        q.blind_spots
            .iter()
            .any(|b| b.contains("team review requests")),
        "expected a stated blind spot, got {:?}",
        q.blind_spots
    );
}

#[test]
fn teams_are_queried_when_github_can_list_them() {
    let (_env, dir) = setup("me", true);
    put_search(&dir, "review-requested:me", "[]");
    put_search(
        &dir,
        "team-review-requested:acme/core",
        &format!("[{}]", pr_json(9, "team", "")),
    );

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(q.prs.len(), 1, "the team query must be run");
    assert_eq!(
        q.prs[0].reasons,
        vec![skein::prq::Reason::Team("acme/core".into())]
    );
    assert!(
        q.blind_spots.is_empty(),
        "nothing was hidden: {:?}",
        q.blind_spots
    );
}

/// One failing query must not empty the queue, and must not pass unnoticed: the surviving queries
/// still contribute, and the loss is stated. Silently returning the shorter list is the behaviour
/// this guards against — it looks identical to "nothing needs you".
#[test]
fn a_failing_query_is_reported_rather_than_swallowed() {
    let (_env, dir) = setup("me", false);
    fail_search(&dir, "review-requested:me");
    put_search(&dir, "author:me", &format!("[{}]", pr_json(5, "mine", "")));

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(q.prs.len(), 1, "the queries that worked still contribute");
    assert_eq!(q.prs[0].number, 5);
    assert!(
        q.blind_spots
            .iter()
            .any(|b| b.contains("review-requested:me") && b.contains("403")),
        "the failure must name the query and carry gh's reason, got {:?}",
        q.blind_spots
    );
}

/// **What brings a row back to you is GitHub asking you again — not the head moving** (SKEIN-354).
///
/// This test asserted the opposite until the owner overruled it, verbatim: "approved should come
/// only if my review status on the PR is approved rn, if I approved and then some file I own
/// changed, so github asks me to review again then it should show that." The rule it used to guard
/// compared the sha you reviewed against the head that is there now, so a rebase or a typo fix took
/// your approval off you and put the row back in your queue. On the owner's live queue that
/// comparison was false on all 26 rows, so the rule cleared nothing he had ever done.
///
/// Written as an INVARIANT rather than as a lane per input, because a test that asserts an outcome
/// for one input pair is exactly how the last regression in this area shipped. The head sha is
/// varied across the pair that must agree, and the only thing separating Waiting from NeedsYou is
/// whether GitHub is asking.
#[test]
fn your_review_stands_until_github_asks_you_again_not_until_the_head_moves() {
    let (_env, dir) = setup("me", false);
    // Both connections carry the review, which is what GitHub returns for a real approval —
    // `latestOpinionatedReviews` is the authority and `latestReviews` only the fallback, and a
    // fixture that populated one of them would be quietly testing which branch was taken.
    let approved_at = |oid: &str| {
        let node = format!(
            r#"{{"author":{{"login":"me"}},"state":"APPROVED","commit":{{"oid":"{oid}"}}}}"#
        );
        format!(
            r#","latestReviews":{{"nodes":[{node}]}},"latestOpinionatedReviews":{{"nodes":[{node}]}}"#
        )
    };
    const ASKED: &str = r#","reviewRequests":{"nodes":[{"requestedReviewer":{"login":"me"}}]}"#;

    // 1 and 2 differ in one thing only: whether the head has moved out from under the approval.
    // `pr_json` gives #1 the head `sha1`, which is the revision it was approved at; #2 was approved
    // at a revision that is no longer there.
    let at_head = pr_json(
        1,
        "approved, and that is still the revision",
        &approved_at("sha1"),
    );
    let moved = pr_json(
        2,
        "approved, and the branch has moved since",
        &approved_at("older"),
    );
    // 3 differs from 2 in one thing only: GitHub is asking again, by name.
    let asked_again = pr_json(
        3,
        "moved, and github is asking you by name",
        &format!("{}{ASKED}", approved_at("older")),
    );
    let never = pr_json(4, "you have never said anything", "");
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{at_head},{moved},{asked_again},{never}]"),
    );

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    let lane = |n: u64| q.prs.iter().find(|p| p.number == n).unwrap().lane;

    assert_eq!(
        lane(1),
        skein::prq::Lane::Waiting,
        "an approval against the revision that is there is off your plate"
    );
    assert_eq!(
        lane(2),
        skein::prq::Lane::Waiting,
        "the head moving under an approval nobody withdrew must NOT put the row back on you — \
         GitHub is not asking you for anything, and this is the rule the owner overruled"
    );
    assert_eq!(
        lane(3),
        skein::prq::Lane::NeedsYou,
        "a re-request by name is what puts it back, against that same moved head"
    );
    assert_eq!(
        lane(4),
        skein::prq::Lane::NeedsYou,
        "a review you never gave was never standing"
    );

    // The invariant itself, stated once rather than left implied by the four rows above: where your
    // approval is on record, the lane is a function of whether GitHub is asking you again and of
    // NOTHING else. A rule that reintroduced any other input — the head, a timer, a push — would
    // satisfy every assertion above and fail here.
    for (n, asking) in [(1u64, false), (2, false), (3, true)] {
        let pr = q.prs.iter().find(|p| p.number == n).unwrap();
        assert_eq!(
            pr.my_review, "approved",
            "#{n} must have your approval on record"
        );
        assert_eq!(
            pr.my_review_requested, asking,
            "#{n}: the fixture must differ only in whether GitHub is asking"
        );
        assert_eq!(
            pr.lane == skein::prq::Lane::NeedsYou,
            asking,
            "#{n}: with an approval on record, needing you must mean exactly that GitHub asked \
             you again — head {}, review_is_current {}",
            pr.head_sha,
            pr.review_is_current
        );
    }
}

#[test]
fn archiving_moves_a_pr_out_of_needs_you_and_back() {
    let (_env, dir) = setup("me", false);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(1, "set aside", "")),
    );

    skein::prq::set_archived("acme", 1, true).unwrap();
    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(q.prs[0].lane, skein::prq::Lane::Archived);

    skein::prq::set_archived("acme", 1, false).unwrap();
    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(q.prs[0].lane, skein::prq::Lane::NeedsYou);
}

/// SKEIN-144: "not until CI is green" is not an archive. A snooze is keyed to the head sha it was
/// taken at, so the author's next push — the thing that turns red rows green here — is what brings
/// the row back, with no act and no timer. The archive keeps its own semantics untouched
/// (`archiving_moves_a_pr_out_of_needs_you_and_back` above): that one holds until a human says so.
#[test]
fn a_snoozed_pr_leaves_needs_you_and_returns_when_its_head_moves() {
    // `true`, and it is load-bearing: a token that cannot list teams cannot ask
    // `team-review-requested:`, so the refresh may not read a pull request's absence as
    // proof it is closed (SKEIN-262). The prune this test is about needs every rule asked.
    let (_env, dir) = setup("me", true);
    // pr_json(1, …) reports head sha1 — the sha the reviewer's row was showing.
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(1, "red until pushed", "")),
    );
    skein::prq::set_snoozed("acme", 1, Some("sha1")).unwrap();
    skein::prq::set_snoozed("acme", 42, Some("gone")).unwrap(); // merged since

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(
        q.prs[0].lane,
        skein::prq::Lane::Archived,
        "out of Needs you"
    );
    assert!(
        q.prs[0].snoozed,
        "and the row says why: set aside, not archived"
    );
    assert_eq!(
        skein::prq::snoozed("acme"),
        std::collections::BTreeMap::from([(1u64, "sha1".to_string())]),
        "a snooze on a PR that is no longer open is pruned like the archive is"
    );

    // The author pushes: same PR, new head. Nobody calls anything.
    put_search(
        &dir,
        "review-requested:me",
        &format!(
            "[{}]",
            pr_json(1, "red until pushed", "").replace("sha1", "sha1b")
        ),
    );
    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(
        q.prs[0].lane,
        skein::prq::Lane::NeedsYou,
        "the push IS the un-snooze — the row returns by itself"
    );
    assert!(!q.prs[0].snoozed);
    assert!(
        skein::prq::snoozed("acme").is_empty(),
        "the spent entry is dropped, so a revert to the old sha cannot re-hide the row"
    );
}

/// An archived PR that is no longer open is dead weight in the file. Pruning must not be able to
/// hide a still-open PR, so it only ever removes numbers absent from the fetched open set.
#[test]
fn the_archive_is_pruned_to_prs_that_are_still_open() {
    // `true`, and it is load-bearing: a token that cannot list teams cannot ask
    // `team-review-requested:`, so the refresh may not read a pull request's absence as
    // proof it is closed (SKEIN-262). The prune this test is about needs every rule asked.
    let (_env, dir) = setup("me", true);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(1, "open", "")),
    );
    skein::prq::set_archived("acme", 1, true).unwrap();
    skein::prq::set_archived("acme", 42, true).unwrap(); // merged since

    let _ = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(
        skein::prq::archived("acme"),
        vec![1],
        "the still-open one survives, the closed one is dropped"
    );
}

/// GitHub refusing the batch is not GitHub being unavailable (SKEIN-266).
///
/// Batching took five requests per repo down to one — and made that one the most expensive thing
/// skein sends, so GitHub's edge sometimes sheds it: reported live on the owner's fleet as `502 Bad
/// Gateway` in nginx's own HTML, an hour after the same request came back as a 200 with no body.
/// The queue then showed nothing at all for the one repo they watch. Splitting is what turns "too
/// heavy" back into an answer, and it costs the extra requests only when it has to.
#[test]
fn a_batch_github_will_not_take_is_asked_in_halves_rather_than_given_up_on() {
    let (_env, dir) = setup("me", false);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(1, "open", "")),
    );
    put_search(&dir, "author:me", &format!("[{}]", pr_json(2, "open", "")));
    fs::write(dir.join("too-heavy"), "").unwrap();

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(
        q.prs.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![2, 1],
        "the refresh was given up on instead of being asked in halves: {:?}",
        q.blind_spots
    );
    // The standing one — this fixture's token cannot list teams — is expected and stays. What must
    // NOT be there is a word about the refresh having failed: it did not, it was asked twice.
    assert!(
        !q.blind_spots.iter().any(|b| b.contains("did not answer")
            || b.contains("membership searches are missing")
            || b.contains("query failed")),
        "a refresh that succeeded by splitting reported itself as missing: {:?}",
        q.blind_spots
    );
}

/// A GitHub outage must not erase the owner's own decisions (SKEIN-229).
///
/// The prune above reads a pull request's ABSENCE as proof it is closed. When the whole request
/// dies every search comes back empty, absence is total, and both files were rewritten to nothing
/// — fleet-wide, because the badge poll runs this for every repo every three minutes, so a single
/// rate-limit window cost every set-aside and every snooze with no record of what was there.
#[test]
fn a_refresh_that_saw_nothing_keeps_every_set_aside_and_snoozed_pr() {
    let (_env, dir) = setup("me", false);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(1, "open", "")),
    );
    skein::prq::set_archived("acme", 500, true).unwrap();
    skein::prq::set_snoozed("acme", 501, Some("deadbeef")).unwrap();

    dead_request(&dir);
    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert!(
        q.prs.is_empty(),
        "the outage returns no pull requests at all, got {:?}",
        q.prs
    );
    assert!(
        !q.blind_spots.is_empty(),
        "and the queue itself is honest about having gone blind"
    );
    assert_eq!(
        skein::prq::archived("acme"),
        vec![500],
        "a refresh that ANSWERED nothing must not read its empty list as `nothing is open`"
    );
    assert_eq!(
        skein::prq::snoozed("acme"),
        std::collections::BTreeMap::from([(501u64, "deadbeef".to_string())]),
        "and the same for a snooze, which is the owner's decision rather than a cache"
    );
}

/// The other half of that rule, and why it is not "skip the prune when the list came back empty":
/// a repo with genuinely nothing open answered, so it still prunes. Note the teams blind spot is
/// present throughout — `answered` is about the searches skein ran, not about a clean queue.
#[test]
fn a_queue_that_really_is_empty_still_prunes() {
    // `true`, and it is load-bearing: a token that cannot list teams cannot ask
    // `team-review-requested:`, so the refresh may not read a pull request's absence as
    // proof it is closed (SKEIN-262). The prune this test is about needs every rule asked.
    let (_env, dir) = setup("me", true);
    put_search(&dir, "review-requested:me", "[]");
    skein::prq::set_archived("acme", 500, true).unwrap();
    skein::prq::set_snoozed("acme", 501, Some("deadbeef")).unwrap();

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert!(
        q.prs.is_empty(),
        "nothing is open, and every search said so"
    );
    assert!(
        skein::prq::archived("acme").is_empty(),
        "an empty answer is still an answer, so the dead archive entry goes"
    );
    assert!(
        skein::prq::snoozed("acme").is_empty(),
        "and so does the spent snooze"
    );
}

/// The partial case, which is the distinction the batching was careful to preserve: four aliases
/// answer and one dies. The archived pull request would have come back only under the dead one, so
/// between them the four good answers "prove" it is closed — and they prove nothing of the sort.
#[test]
fn an_archived_pr_that_only_the_failed_query_would_return_survives() {
    let (_env, dir) = setup("me", false);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(1, "open", "")),
    );
    // 7 is the owner's own pull request, so `author:me` is the only search that would list it —
    // and that is the search that goes dark.
    fail_search(&dir, "author:me");
    skein::prq::set_archived("acme", 7, true).unwrap();
    skein::prq::set_snoozed("acme", 7, Some("sha7")).unwrap();

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert!(
        q.blind_spots.iter().any(|b| b.contains("author:me")),
        "expected the dead alias to be named, got {:?}",
        q.blind_spots
    );
    assert_eq!(
        skein::prq::archived("acme"),
        vec![7],
        "one dark query is enough: what it would have returned is not known to be closed"
    );
    assert!(
        skein::prq::snoozed("acme").contains_key(&7),
        "and the snooze on the same pull request survives with it"
    );
}

/// A search asks GitHub for a hundred at a time. One that comes back with exactly a hundred has
/// almost certainly been cut off, so it says nothing about the pull requests past the cut — and
/// the prune reads precisely that silence. SKEIN-231 owns saying the truncation out loud; this
/// asserts only that it cannot cost the owner an archive entry in the meantime.
#[test]
fn a_search_cut_off_at_the_page_does_not_prune_what_it_never_reached() {
    let (_env, dir) = setup("me", false);
    let full: Vec<String> = (1..=100).map(|n| pr_json(n, "open", "")).collect();
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", full.join(",")),
    );
    skein::prq::set_archived("acme", 500, true).unwrap();

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    assert_eq!(q.prs.len(), 100, "the page itself is served in full");
    assert_eq!(
        skein::prq::archived("acme"),
        vec![500],
        "500 may be on page two, and a full page is no evidence that it is closed"
    );
}

/// **A pull request with more labels than the page says how many it lost** (SKEIN-373).
///
/// The unit tests in `src/prq.rs` prove one node parses into a row that knows its label list is
/// short. What they cannot prove is the part a person actually meets: whether the QUEUE says so.
/// This is the same shape as `a_search_cut_off_at_the_page_does_not_prune_what_it_never_reached`
/// above and it is here for the same reason — a truncated GitHub answer that looks complete is the
/// failure this file exists for.
///
/// The fixture is the measured one: `acme/testbed#20` carries 22 labels, and the
/// query's page of 20 delivered `area/mod-01` … `area/mod-20` with nothing anywhere saying two
/// were gone.
///
/// The counter-case is in the same test on purpose. A blind spot on every pull request would be
/// noise, and noise is what stops blind spots being read at all — so the pull request whose labels
/// all arrived must produce no sentence.
#[test]
fn a_pull_request_with_more_labels_than_the_page_says_how_many_it_lost() {
    let (_env, dir) = setup("me", false);
    let page: Vec<String> = (1..=20)
        .map(|i| format!(r#"{{"name":"area/mod-{i:02}"}}"#))
        .collect();
    let cut = pr_json(
        20,
        "labelled by area",
        &format!(
            r#","labels":{{"totalCount":22,"nodes":[{}]}}"#,
            page.join(",")
        ),
    );
    let whole = pr_json(
        21,
        "an ordinary one",
        r#","labels":{"totalCount":2,"nodes":[{"name":"ci"},{"name":"hold"}]}"#,
    );
    put_search(&dir, "review-requested:me", &format!("[{cut},{whole}]"));

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    let row = |n: u64| q.prs.iter().find(|p| p.number == n).expect("the row");

    assert_eq!(
        row(20).labels.len(),
        20,
        "the page itself is served in full"
    );
    assert_eq!(
        row(20).labels_total,
        Some(22),
        "GitHub's own count of the labels never reached the queue, so nothing downstream can tell \
         this list is short — the defect exactly"
    );
    assert!(
        !row(20).labels_whole(),
        "a row carrying 20 of 22 labels is claiming to carry all of them"
    );

    let said: Vec<&String> = q
        .blind_spots
        .iter()
        .filter(|s| s.contains("#20's labels"))
        .collect();
    assert_eq!(
        said.len(),
        1,
        "a label list cut off at the page is not said out loud, in a queue whose rule is that a \
         limit skein hit is said out loud (c34faea): {:?}",
        q.blind_spots
    );
    let spot = said[0];
    assert!(
        spot.contains("22") && spot.contains("20"),
        "the sentence must carry the size of the hole and not only its existence: {spot}"
    );
    assert!(
        spot.contains("no-label"),
        "the blind spot must say what the truncation COSTS — `no-label:` conditions stop holding \
         on this pull request, so a merge train that looked stuck has a reason here: {spot}"
    );

    // The counter-case: nothing was lost, so nothing is said.
    assert!(
        row(21).labels_whole() && row(21).labels_total == Some(2),
        "the second row's labels all arrived and it must know it"
    );
    assert!(
        !q.blind_spots.iter().any(|s| s.contains("#21's labels")),
        "a pull request whose labels all arrived was reported as short — a blind spot on every \
         row is a blind spot nobody reads: {:?}",
        q.blind_spots
    );
}

#[test]
fn a_repo_with_no_github_remote_has_no_queue() {
    let (_env, _dir) = setup("me", false);
    let mut r = repo("local");
    r.source = "/Users/me/code/thing".into();
    let err = skein::prq::queue(&r, true).unwrap_err();
    assert!(err.contains("no GitHub remote"), "{err}");
}

/// The merged queue (SKEIN-146): every repo in one answer, a switched-off repo reported rather
/// than omitted — and NOT ONE GitHub request beyond what the badge's own poll already spends,
/// because it serves what `counts()` built. Provable only here: the in-process queue cache is
/// disabled under `cfg!(test)`, and this crate compiles the library without it.
#[test]
fn the_merged_queue_is_every_repo_and_costs_no_extra_github_call() {
    let (_env, dir) = setup("me", true);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(11, "waiting", "")),
    );

    let mut one = repo("mq-one");
    let mut two = repo("mq-two");
    let mut dark = repo("mq-dark");
    one.id = "mq-one".into();
    two.id = "mq-two".into();
    dark.review_queue = false;
    skein::repos::save_repos(&[one, two, dark]).unwrap();

    let hits = || {
        std::fs::read_to_string(dir.join("hits.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };

    // The badge's poll runs first, as it does live — this is what fills the cache.
    let counts = skein::prq::counts();
    assert_eq!(counts.iter().filter(|c| c.skipped.is_empty()).count(), 2);
    let spent_on_counts = hits();
    assert!(
        spent_on_counts > 0,
        "the stub must actually have been asked"
    );

    let m = skein::prq::merged(false);
    assert_eq!(m.queues.len(), 2, "every asked repo is in the one answer");
    let ids: Vec<_> = m.queues.iter().map(|q| q.repo_id.as_str()).collect();
    assert_eq!(ids, ["mq-one", "mq-two"]);
    for q in &m.queues {
        assert_eq!(
            q.prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![11],
            "each repo's own rows survive the merge ({})",
            q.repo_id
        );
    }
    assert_eq!(
        m.skipped.len(),
        1,
        "a repo skein chose not to ask about is reported, never omitted"
    );
    assert_eq!(m.skipped[0].repo_id, "mq-dark");
    assert_eq!(
        hits(),
        spent_on_counts,
        "the merged queue serves what counts() already built — zero further GitHub requests"
    );
}

/// GitHub removes you from `review-requested:` the moment you submit ANY review — a comment-only
/// one included. So the day after the owner posted a drafted comment on PR 577, it was gone from
/// every search the queue ran: "PR 577 is still not visible while it is clearly open". Acting on
/// your queue must never be what empties it, which is what the `reviewed-by:` search is for.
#[test]
fn a_pull_request_you_have_reviewed_stays_in_the_queue() {
    let (_env, dir) = setup("me", true);
    // 577, the day after: no longer review-requested, only reviewed-by.
    put_search(
        &dir,
        "reviewed-by:me",
        &format!(
            "[{}]",
            pr_json(577, "research documents land in bedrock", "")
        ),
    );
    let mut r = repo("mq-reviewed");
    r.id = "mq-reviewed".into();
    skein::repos::save_repos(&[r.clone()]).unwrap();

    let q = skein::prq::queue(&r, false).unwrap();
    assert_eq!(
        q.prs.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![577],
        "a PR you have reviewed vanished from the queue"
    );
    assert!(
        q.prs[0].reasons.contains(&skein::prq::Reason::Reviewed),
        "the row says why it is here: {:?}",
        q.prs[0].reasons
    );
    // A comment is deliberately not a decision, so it is still your move.
    assert_eq!(q.prs[0].lane, skein::prq::Lane::NeedsYou);
}

/// The merged queue paints what it remembers instead of blocking — per repo, the same rule the
/// per-repo route has. Regressed once: `merged()` called `queue()` cold and the pane waited on
/// every repo's searches again ("the PRs page is again waiting on refreshing on first loads").
#[test]
fn the_merged_queue_paints_what_it_remembers_instead_of_blocking() {
    let (_env, dir) = setup("me", true);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(31, "waiting", "")),
    );
    let mut r = repo("mq-paint");
    r.id = "mq-paint".into();
    skein::repos::save_repos(&[r.clone()]).unwrap();

    // A first read writes the remembered copy to disk…
    assert!(skein::prq::queue(&r, false).unwrap().fresh);
    // …then the server restarts: the in-process cache is gone, the disk copy is not.
    skein::prq::invalidate("mq-paint");

    let m = skein::prq::merged(false);
    assert_eq!(m.queues.len(), 1);
    assert!(
        !m.queues[0].fresh,
        "a cold merged read must hand over the remembered copy, marked, not block on GitHub"
    );
    assert_eq!(
        m.queues[0].prs.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![31]
    );
}

/// SKEIN-206, reported live as "we aren't bombarding github right?" — we were. The pane retries a
/// stale answer at 4s/8s/16s/…, every retry landed in `merged(false)`, and each one spawned its
/// own FORCED background refresh: one pane-open ran several concurrent fetches per repo, four
/// GraphQL searches each. The guard admits one refresh per repo, and the refresh is un-forced so
/// a sibling's result that just landed is served from the cache instead of fetched again.
#[test]
fn an_expired_repo_refreshes_once_no_matter_how_many_panes_ask() {
    let (_env, dir) = setup("me", false);
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{}]", pr_json(2, "waiting", "")),
    );
    let mut r = repo("mq-guard");
    r.id = "mq-guard".into();
    skein::repos::save_repos(&[r.clone()]).unwrap();
    let hits = || {
        std::fs::read_to_string(dir.join("hits.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };

    // Prime the remembered copy, then measure what exactly ONE fetch costs in requests — asserted
    // as a measurement rather than a constant so the count cannot silently drift from the query
    // list it is really about.
    assert!(skein::prq::queue(&r, true).is_ok());
    let before = hits();
    assert!(skein::prq::queue(&r, true).is_ok());
    let per_fetch = hits() - before;
    assert!(per_fetch > 0, "the stub must actually have been asked");

    // The server restarts: in-process cache gone (expired, as far as merged() can tell), the
    // remembered copy on disk intact — the exact state every pane-open finds after a deploy.
    skein::prq::invalidate("mq-guard");

    // The retry storm: three merged reads in quick succession, none willing to block.
    let base = hits();
    for _ in 0..3 {
        let m = skein::prq::merged(false);
        assert_eq!(
            m.queues[0].prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![2],
            "every ask is still answered, from whichever copy is at hand"
        );
    }

    // Let the lone background refresh land: wait for its requests, then a beat longer to catch
    // any sibling that should not exist.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while hits() - base < per_fetch && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        hits() - base,
        per_fetch,
        "three panes asked while the repo was expired; GitHub was fetched exactly once"
    );

    // And once the refresh has landed, the next ask serves the fresh copy for free.
    let settled = hits();
    let m = skein::prq::merged(false);
    assert!(
        m.queues[0].fresh,
        "the landed refresh is what the pane gets now"
    );
    assert_eq!(
        hits(),
        settled,
        "a fresh cache answers without spending another request"
    );
}
