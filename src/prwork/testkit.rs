//! One fake of each thing this module's tests need, so five test suites cannot pose five
//! different GitHubs.
//!
//! Everything here is used by tests in more than one submodule — that is the entry criterion, and
//! it is why [`crate::prwork::acts`]'s `merge_world` and [`crate::prwork::perform`]'s `readable`
//! are NOT here: a fake with one caller belongs beside its caller, where the test that reads it
//! can see what it poses.
//!
//! [`github`] is the important one. It is a real listener on loopback speaking canned responses,
//! and it hands back the request log, so a test asserts what skein SENT rather than what a mock
//! was told to expect.

use super::*;
use crate::workflow::{Act, Chosen, Workflow};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// The credential every stub GitHub below is called with.
///
/// Prefixed `skein-test-` deliberately: a fixture that looked like a real token
/// (`gho_…`, `ghp_…`) is indistinguishable from one in a grep, and this tree has already had
/// to sweep a client's real strings out of its fixtures once.
pub(super) fn fixture_token() -> crate::secret::Secret {
    crate::secret::Secret::new("skein-test-github-token")
}

/// A GitHub that records what skein sent it, and answers however the test says.
///
/// Every assertion here is about a request that CHANGES somebody's repository, so what is
/// checked is the wire: the method, the path, and the body. A doer tested through its own
/// return value would pass while merging with the wrong method, or without the head it decided
/// on — which is the failure that matters, because that one merges a commit nobody looked at.
pub(super) fn github(status: u16) -> (String, Arc<Mutex<Vec<String>>>) {
    let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = heard.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let said = String::from_utf8_lossy(&buf[..n]).to_string();
            let head = said.lines().next().unwrap_or_default().to_string();
            let body = said
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or_default()
                .to_string();
            seen.lock().unwrap().push(format!("{head} {body}"));
            // The node id lookup always works: it is not what any of these tests are about.
            // GraphQL answers in GraphQL's shape, because `github::graphql` reads `data` and
            // would report a perfectly good mutation as a failure otherwise.
            let (status, answer) = if head.starts_with("GET") && head.contains("/pulls/") {
                (200, r#"{"node_id":"PR_node"}"#.to_string())
            } else if head.contains("/graphql") {
                (
                    status,
                    r#"{"data":{"updatePullRequestBranch":{"pullRequest":{"headRefOid":"new"}}}}"#
                        .to_string(),
                )
            } else {
                (status, r#"{"merged":true}"#.to_string())
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
        }
    });
    (base, heard)
}

pub(super) fn flow() -> Workflow {
    crate::workflow::from_bytes(
        br#"{"workflow":[{"name":"ship-mine","steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
    )
    .unwrap()
    .remove(0)
}

pub(super) fn subject(head_sha: &str) -> Subject<'_> {
    Subject {
        repo_id: "demo",
        slug: "acme/thing",
        number: 41,
        head_sha,
        head_ref: "feat",
        // No reading: this helper stands in for every act but `read`, and a `read` step taken
        // from here must refuse rather than quietly invent a repo (see `Reading`).
        reading: None,
    }
}

pub(super) fn chosen(act: Act) -> Chosen {
    Chosen { step: 3, act }
}

/// A remembered queue for `repo_id` listing exactly these open pull requests, put where
/// `prq::remembered` reads it.
///
/// That is the state a real counts poll runs in and not a convenience: a stop can only be
/// written by a pass that read this repo's queue, so "a repo with stops and no remembered
/// queue" is the cold-start case, never the steady one. `queue_within` deliberately neither
/// caches nor remembers under `cfg!(test)`, so nothing is here unless a test says so.
pub(super) fn remember_open(repo_id: &str, numbers: &[u64], whole: bool) {
    let prs: Vec<serde_json::Value> = numbers
        .iter()
        .map(|n| {
            serde_json::json!({
                "number": n,
                "title": format!("pull request {n}"),
                "author": "me",
                "url": format!("https://github.com/acme/thing/pull/{n}"),
                "head_ref": format!("feat-{n}"),
                "head_sha": "deadbeef",
                "base_ref": "main",
                "draft": false,
                "updated_at": "",
                "committed_at": "",
                "checks": "passing",
                "my_review": "none",
                "review_is_current": true,
                "reasons": ["author"],
                "lane": "needs-you",
                "box_name": "",
            })
        })
        .collect();
    let queue: crate::prq::Queue = serde_json::from_value(serde_json::json!({
        "repo_id": repo_id,
        "slug": "acme/thing",
        "viewer": "me",
        "ai": false,
        "prs": prs,
        "blind_spots": [],
        "whole": whole,
    }))
    .expect("a queue in the shape prq writes one");
    crate::prq::remember_for_test(&queue);
}

/// A pull request at a head of the test's choosing, and nothing else varying.
pub(super) fn pr_at(head_sha: &str) -> crate::prq::Pr {
    serde_json::from_value(serde_json::json!({
        "number": 41, "title": "t", "author": "someone", "url": "u",
        "head_ref": "feat", "head_sha": head_sha, "base_ref": "main",
        "draft": false, "updated_at": "", "committed_at": "",
        "labels": [], "labels_total": 0,
        "review_decision": "APPROVED", "standing_approvals": 1,
        "mergeable": true, "merge_state": "CLEAN", "checks": "passing",
        "my_review": "none", "review_is_current": false,
        "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
    }))
    .unwrap()
}

/// A reading as `review.rs` would have written it — through `review::Summary` itself, so the
/// field names on disk are the ones that module owns rather than ones this test invented.
pub(super) fn a_reading(
    head_sha: &str,
    depth: crate::review::Depth,
    swept: bool,
) -> crate::review::Summary {
    crate::review::Summary {
        owed_triggered: None,
        findings_block: None,
        number: 41,
        head_sha: head_sha.into(),
        depth,
        line: "it changes a thing.".into(),
        detail: String::new(),
        flags: Vec::new(),
        signals: Vec::new(),
        yours: Vec::new(),
        others: 0,
        ownership_unknown: String::new(),
        unread_because: String::new(),
        read_outside_box: String::new(),
        not_reread: String::new(),
        not_posted: String::new(),
        computed: true,
        budget_stopped: false,
        stopped_at_box: false,
        swept,
    }
}

/// Where `review::cache_path` files a reading of exactly this commit.
///
/// **The copy lives here, and only here.** Production reads through `review::cached`, so this
/// is the one place that still spells the filename by hand — and it has to, because `review`
/// exposes no writer: a test that wants a reading on disk must know where one goes. That makes
/// it exactly the right copy to keep. It cannot make production agree with itself while both
/// drift; it can only disagree with production, which is the failure the pinning test below is
/// looking for.
pub(super) fn reading_path(repo_id: &str, number: u64, head_sha: &str) -> std::path::PathBuf {
    let key: String = head_sha
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(40)
        .collect();
    crate::prq::review_dir(repo_id)
        .join("summaries")
        .join(format!("{number}-{key}.json"))
}

/// Filed where the engine looks for it, through the engine's own expression.
pub(super) fn file_the_reading(repo_id: &str, s: &crate::review::Summary) {
    let path = reading_path(repo_id, s.number, &s.head_sha);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(s).unwrap()).unwrap();
}

/// A repo with every flag a reading needs, and nothing else varying.
pub(super) fn a_repo_that_may_be_read() -> crate::repos::Repo {
    crate::repos::Repo {
        id: "demo".into(),
        read_prs: true,
        auto_review: true,
        ..Default::default()
    }
}
