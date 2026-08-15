//! The review queue end to end, against a stubbed `gh`.
//!
//! The unit tests in `src/prq.rs` cover lane and check derivation from one JSON item. What they
//! cannot cover is the part where a PR goes missing: three separate searches are merged into one
//! list, and a mistake there drops rows rather than mislabelling them. That is the failure this
//! file exists for — a queue you trust and that quietly under-reports is worse than no queue.
//!
//!   cargo test --test review_queue

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A `gh` that answers from fixture files instead of GitHub.
///
/// Dispatch is on argv, exactly as the real thing is called: `api user`, `api user/teams`, and
/// `pr list … --search <term>`. Each search reads `search-<term>.json` if present, else returns an
/// empty list — so a test declares only the queries it cares about.
fn stub_gh(dir: &Path, login: &str, teams: &str) -> PathBuf {
    let bin = dir.join("gh");
    fs::write(
        &bin,
        format!(
            r#"#!/bin/sh
# args: $@
if [ "$1" = "api" ] && [ "$2" = "user" ]; then printf '%s\n' '{login}'; exit 0; fi
if [ "$1" = "api" ] && [ "$2" = "user/teams" ]; then {teams}; fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  term=""
  while [ $# -gt 0 ]; do
    if [ "$1" = "--search" ]; then term="$2"; fi
    shift
  done
  safe=$(printf '%s' "$term" | tr -c 'a-zA-Z0-9' '_')
  if [ -f "{dir}/fail-$safe" ]; then printf 'HTTP 403: forbidden\n' >&2; exit 1; fi
  f="{dir}/search-$safe.json"
  if [ -f "$f" ]; then cat "$f"; else printf '[]\n'; fi
  exit 0
fi
printf 'unexpected: %s\n' "$*" >&2
exit 1
"#,
            login = login,
            teams = teams,
            dir = dir.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    bin
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

fn repo(id: &str) -> skein::Repo {
    skein::Repo {
        id: id.into(),
        source: "https://github.com/acme/thing.git".into(),
        work: "/nonexistent".into(),
        store: "/nonexistent".into(),
        agent: "claude".into(),
        plane_project: String::new(),
        sync_connection: String::new(),
        sync_gateway_url: String::new(),
    }
}

/// One PR item, with the fields `gh pr list --json` would return.
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

/// Point skein's home and `gh` at a scratch directory for the duration of one test.
fn setup(login: &str, teams: &str) -> (Env, PathBuf) {
    // Ignore poisoning: one failing test must not cascade into every other test panicking on the
    // lock, which buries the real failure. The guarded data is `()`.
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir::TempDir::new("skein-review").unwrap();
    let path = dir.path().to_path_buf();
    let gh = stub_gh(&path, login, teams);
    std::env::set_var("SKEIN_GH_BIN", &gh);
    std::env::set_var("SKEIN_HOME", &path);
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
    let (_env, dir) = setup("me", "exit 1");
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
    let (_env, dir) = setup("me", "exit 1");
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
    let (_env, dir) = setup("me", "exit 1");
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
fn teams_are_queried_when_gh_can_list_them() {
    let (_env, dir) = setup("me", "printf 'acme/core\\n'; exit 0");
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
    let (_env, dir) = setup("me", "exit 1");
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
    let (_env, dir) = setup("me", "exit 1");
    let approved = pr_json(
        1,
        "decided",
        r#","latestReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"sha1"}}]"#,
    );
    let stale = pr_json(
        2,
        "moved on",
        r#","latestReviews":[{"author":{"login":"me"},"state":"APPROVED","commit":{"oid":"older"}}]"#,
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
    let (_env, dir) = setup("me", "exit 1");
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

/// An archived PR that is no longer open is dead weight in the file. Pruning must not be able to
/// hide a still-open PR, so it only ever removes numbers absent from the fetched open set.
#[test]
fn the_archive_is_pruned_to_prs_that_are_still_open() {
    let (_env, dir) = setup("me", "exit 1");
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
    let (_env, _dir) = setup("me", "exit 1");
    let mut r = repo("local");
    r.source = "/Users/me/code/thing".into();
    let err = skein::prq::queue(&r, true).unwrap_err();
    assert!(err.contains("no GitHub remote"), "{err}");
}
