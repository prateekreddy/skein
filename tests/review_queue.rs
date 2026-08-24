//! The review queue end to end, against a stubbed GitHub.
//!
//! The unit tests in `src/prq.rs` cover lane and check derivation from one JSON item. What they
//! cannot cover is the part where a PR goes missing: three separate searches are merged into one
//! list, and a mistake there drops rows rather than mislabelling them. That is the failure this
//! file exists for — a queue you trust and that quietly under-reports is worse than no queue.
//!
//!   cargo test --test review_queue

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
/// process. A test's stub outliving its test is harmless here because each gets its own port.
fn stub_github(dir: &Path, login: &str, teams_ok: bool) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = dir.to_path_buf();
    let login = login.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
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
            let mut stream = stream;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let mut length = 0usize;
            // The request line, then headers. `Content-Length` is the only one that matters: the
            // GraphQL body has to be read to know which search this is.
            reader.read_line(&mut request).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            if length > 0 {
                reader.read_exact(&mut body).ok();
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, payload) = if path.starts_with("/user/teams") {
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
            } else if path.starts_with("/graphql") {
                // The batched wire (SKEIN-209): one request, every membership search an alias
                // `q0..qN`, one variable each. A term with a `fail-<term>` marker answers the way
                // GitHub delivers a partial failure — `data.qN: null` plus an errors entry whose
                // `path` names the alias — so the queue's per-rule blind spots stay testable.
                let sent: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
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
                        let nodes =
                            std::fs::read_to_string(root.join(format!("search-{safe}.json")))
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
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                )
                .as_bytes(),
            );
        }
    });
    format!("http://127.0.0.1:{port}")
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

fn repo(id: &str) -> skein::repos::Repo {
    skein::repos::Repo {
        id: id.into(),
        // Reading unattended is off unless somebody says so, per repo — the queue tests are about
        // what the queue SAYS, not about what skein would go and read from it.
        read_prs: false,
        source: "https://github.com/acme/thing.git".into(),
        source_tree: "/nonexistent".into(),
        store: "/nonexistent".into(),
        agent: "claude".into(),
        plane_project: String::new(),
        sync_connection: String::new(),
        review_queue: true,
        sync_gateway_url: String::new(),
    }
}

/// One PR item, in the shape the GraphQL search returns.
fn pr_json(number: u64, title: &str, extra: &str) -> String {
    format!(
        r#"{{"number":{number},"title":"{title}","author":{{"login":"someone"}},"url":"https://github.com/acme/thing/pull/{number}","headRefName":"feat-{number}","headRefOid":"sha{number}","baseRefName":"main","isDraft":false,"updatedAt":"2026-08-1{number}T00:00:00Z"{extra}}}"#
    )
}

/// Cargo runs the tests in one integration binary as parallel threads of a **single process**, and
/// `$SKEIN_HOME` / `$SKEIN_GH_BIN` are process-global. Without this every test races: one test's
/// stubbed `gh` answers another's queries, and the symptom is empty queues and a missing blind spot
/// rather than an error. Held for the whole of each test, released when `Env` drops.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Env {
    _lock: std::sync::MutexGuard<'static, ()>,
    _dir: tempdir::TempDir,
}

/// Point skein's home and its GitHub at a scratch directory for the duration of one test.
fn setup(login: &str, teams: bool) -> (Env, PathBuf) {
    // Ignore poisoning: one failing test must not cascade into every other test panicking on the
    // lock, which buries the real failure. The guarded data is `()`.
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir::TempDir::new("skein-review").unwrap();
    let path = dir.path().to_path_buf();
    let api = stub_github(&path, login, teams);
    std::env::set_var("SKEIN_GITHUB_API", &api);
    std::env::set_var("SKEIN_HOME", &path);
    // A token, because the queue refuses to run without one now — the credential is skein's rather
    // than `gh`'s, so the test has to supply it the way a fleet would.
    std::env::set_var("GH_TOKEN", "test-token");
    skein::prq::forget_host_token();
    (
        Env {
            _lock: lock,
            _dir: dir,
        },
        path,
    )
}

mod tempdir {
    //! A three-line temp dir, so this test file pulls in no dependency the crate does not already
    //! have. Removed on drop, like `src/testutil.rs`'s — a test's scratch space outliving the test
    //! is a leak like any other.
    use std::path::{Path, PathBuf};
    pub struct TempDir(PathBuf);
    impl TempDir {
        pub fn new(prefix: &str) -> std::io::Result<Self> {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir()
                .join(format!("{prefix}-{n}-{:?}", std::thread::current().id()));
            std::fs::create_dir_all(&p)?;
            Ok(Self(p))
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
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

#[test]
fn lanes_follow_your_review_against_the_current_head() {
    let (_env, dir) = setup("me", false);
    let approved = pr_json(
        1,
        "decided",
        r#","latestReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"sha1"}}]}"#,
    );
    let stale = pr_json(
        2,
        "moved on",
        r#","latestReviews":{"nodes":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"older"}}]}"#,
    );
    let fresh = pr_json(3, "untouched", "");
    put_search(
        &dir,
        "review-requested:me",
        &format!("[{approved},{stale},{fresh}]"),
    );

    let q = skein::prq::queue(&repo("acme"), true).unwrap();
    let lane = |n: u64| q.prs.iter().find(|p| p.number == n).unwrap().lane;
    assert_eq!(lane(1), skein::prq::Lane::Waiting);
    assert_eq!(
        lane(2),
        skein::prq::Lane::NeedsYou,
        "new commits undo an approval"
    );
    assert_eq!(lane(3), skein::prq::Lane::NeedsYou);
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
    let (_env, dir) = setup("me", false);
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
    let (_env, dir) = setup("me", false);
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
