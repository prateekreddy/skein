//! The fakes the tests in this module share.
//!
//! **Every item carries `#[cfg(test)]` even though `mod.rs` already declares this module under
//! one.** The gates read `src/` as TEXT — `tools/rustcut.py` cuts what is marked test-only, and it
//! reads one file at a time — so a marker that lives only in the file next door leaves this one
//! looking like shipped code. `module-check` said so out loud the first time: `prq` depends on
//! `testutil`, from `env_lock()` calls that only ever run under `cargo test`.
//!
//! One fake of each thing, rather than one per test module: a stub GitHub that routes by path, a
//! stub that answers the batched search, and the two builders that turn a JSON literal into a
//! [`PrNode`]. A second copy of any of these is a test that passes against a shape nothing else
//! agrees with.

use super::node::PrNode;
use super::*;

/// A GitHub server that answers by path, so a rename can be told from an empty repository.
///
/// The single-body stub beside this cannot express the bug: it needs `/repos/<old>` to redirect
/// while `search` answers differently for the old name and the new one.
#[cfg(test)]
pub(super) fn routing_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = asked.clone();
    let mine = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).ok();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut length = 0usize;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line.trim().is_empty() {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap_or(0);
                }
                line.clear();
            }
            let mut body = vec![0u8; length];
            if length > 0 {
                reader.read_exact(&mut body).ok();
            }
            let body = String::from_utf8_lossy(&body).into_owned();
            recorder.lock().unwrap().push(format!("{path} {body}"));

            let (status, answer) = match path.as_str() {
                "/user" => (200, r#"{"login":"me"}"#.to_string()),
                p if p.starts_with("/user/teams") => (200, "[]".to_string()),
                // The rename, exactly as GitHub reports it.
                "/repos/acme/old-name" => (
                    301,
                    format!(r#"{{"message":"Moved Permanently","url":"{mine}/repositories/42"}}"#),
                ),
                "/repositories/42" => (200, r#"{"full_name":"acme/new-name"}"#.to_string()),
                "/repos/acme/new-name" => (200, r#"{"full_name":"acme/new-name"}"#.to_string()),
                "/graphql" => {
                    // The heart of it: the stale name matches nothing, with no error — which is
                    // what GitHub really does and why the queue went quietly empty. The batched
                    // wire: one request, aliases q0..q3 (no teams here), each its own search.
                    let hit = body.contains("acme/new-name") && body.contains("review-requested");
                    (
                        200,
                        match hit {
                            true => r#"{"data":{"q0":{"nodes":[{"number":7,"title":"a pull request","url":"u","isDraft":false,"author":{"login":"someone"},"headRefOid":"abc","updatedAt":"2026-08-01T00:00:00Z","latestReviews":{"nodes":[]},"reviewRequests":{"nodes":[]}}]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                            false => r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                        },
                    )
                }
                _ => (200, "{}".to_string()),
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
    (base, asked)
}

#[cfg(test)]
pub(super) fn item(json: &str) -> serde_json::Value {
    serde_json::from_str(json).unwrap()
}

/// One pull-request node **shaped the way GitHub's GraphQL answers one** — connections as
/// `{"totalCount": n, "nodes": [...]}`, the check rollup hanging off `commits(last: 1)`.
///
/// The fixtures below used to be written in the flattened shape `gh --json` emitted, because
/// that is what the parse read. It reads GitHub's own answer now, so they are written as GitHub
/// sends it — which is also the only shape a test can be wrong about in a way that matters: a
/// fixture in a shape nothing on the wire produces proves the parse works on nothing.
#[cfg(test)]
pub(super) fn node(json: &str) -> PrNode {
    node_of(&item(json))
}

/// The same, from a `json!` literal — and `unwrap`, not a default, on purpose: every field in
/// [`PrNode`] tolerates absence, so a fixture that fails to deserialise is one whose TYPES are
/// wrong, and a test that quietly parsed that into an empty node would assert on nothing.
#[cfg(test)]
pub(super) fn node_of(v: &serde_json::Value) -> PrNode {
    serde_json::from_value(v.clone()).expect("the fixture is a pull-request node")
}

/// Env plumbing every wire test here shares. Returns the guard that must stay alive.
#[cfg(test)]
pub(super) fn wired(base: &str) -> impl Drop {
    // The env lock, held for its Drop and never read — which is the whole point of it, and
    // what the dead-code warning was about. `crate::testutil::env_lock` is the one mechanism;
    // this only ties its lifetime to the environment it guards.
    struct Undo(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for Undo {
        fn drop(&mut self) {
            for key in ["GH_TOKEN", "SKEIN_GITHUB_API"] {
                std::env::remove_var(key);
            }
            forget_host_token();
        }
    }
    let guard = crate::testutil::env_lock();
    std::env::set_var("GH_TOKEN", "gho_test");
    std::env::remove_var("GITHUB_TOKEN");
    std::env::set_var("SKEIN_GITHUB_API", base);
    forget_host_token();
    Undo(guard)
}

// ---- SKEIN-209: the five membership searches travel in ONE GraphQL request ----

/// One PR node as the batched search returns it — the minimum the parser keys on.
#[cfg(test)]
pub(super) fn search_node(number: u64) -> String {
    format!(
        r#"{{"number":{number},"title":"pr {number}","url":"https://github.com/acme/x/pull/{number}","isDraft":false,"author":{{"login":"someone"}},"headRefName":"feat-{number}","headRefOid":"sha{number}","updatedAt":"2026-08-1{number}T00:00:00Z","latestReviews":{{"nodes":[]}}}}"#
    )
}

/// A GitHub for the batched wire: `/graphql` answers `status` + `graphql_body`, teams answer
/// one team (`acme/core`) when `teams`, and every request is recorded as `"METHOD path body"`
/// — the wire is the thing under test, exactly as `fake_github` argues above. `/rate_limit`
/// answers an unusable `{}` on purpose: learning a real reset is github.rs's own test's job,
/// and the flat fallback hold is all the queue side needs to prove here.
#[cfg(test)]
pub(super) fn batched_github(
    teams: bool,
    status: u16,
    graphql_body: String,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    batched_github_pages(teams, vec![(status, graphql_body)])
}

/// The same GitHub, answering a DIFFERENT status and body to each successive `/graphql` request
/// — the first, then the second, and the last one for every request after that.
///
/// Paging asks the same endpoint twice in one refresh and expects two different answers
/// (SKEIN-280), and a stub with one canned answer cannot tell a refresh that followed a cursor
/// from one that re-asked the same page.
#[cfg(test)]
pub(super) fn batched_github_pages(
    teams: bool,
    pages: Vec<(u16, String)>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    batched_github_answering(teams, move |n, _| pages[n.min(pages.len() - 1)].clone())
}

/// The same GitHub again, answering from the REQUEST rather than from a script: `answer` is
/// handed the call's ordinal and its body.
///
/// What the batch-width test needs and the sequence above cannot give it (SKEIN-278): a GitHub
/// that refuses the wide request **every** time and answers the narrow one every time. A
/// scripted stub that 504s only once cannot tell "skein remembered the width" from "GitHub
/// stopped refusing", which is the difference the whole item is about.
#[cfg(test)]
pub(super) fn batched_github_answering(
    teams: bool,
    answer: impl Fn(usize, &str) -> (u16, String) + Send + 'static,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let served = std::sync::atomic::AtomicUsize::new(0);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).ok();
            let mut parts = request.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let mut length = 0usize;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line.trim().is_empty() {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap_or(0);
                }
                line.clear();
            }
            let mut body = vec![0u8; length];
            if length > 0 {
                reader.read_exact(&mut body).ok();
            }
            let body = String::from_utf8_lossy(&body).into_owned();
            recorder
                .lock()
                .unwrap()
                .push(format!("{method} {path} {body}"));
            let (code, answer) = match path.as_str() {
                "/graphql" => {
                    let n = served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    answer(n, &body)
                }
                "/rate_limit" => (200, "{}".to_string()),
                p if p.starts_with("/user/teams") => match teams {
                    true => (
                        200,
                        r#"[{"slug":"core","organization":{"login":"acme"}}]"#.to_string(),
                    ),
                    // **What a token without `read:org` actually gets** — a 403, not an empty
                    // list. It used to answer `200 []`, which since SKEIN-262 is a different
                    // fact: an empty list is GitHub saying you are in no teams, and every test
                    // that meant "the scope is missing" was quietly asserting against the
                    // wrong one. `tests/review_queue.rs`'s stub has always answered 403 here.
                    false => (403, r#"{"message":"Requires read:org"}"#.to_string()),
                },
                "/user" => (200, r#"{"login":"me"}"#.to_string()),
                p if p.starts_with("/repos/") => (
                    200,
                    format!(
                        r#"{{"full_name":"{}","default_branch":"main"}}"#,
                        p.trim_start_matches("/repos/")
                    ),
                ),
                _ => (200, "{}".to_string()),
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

/// The repo the batched tests refresh. Distinct slugs per test, so the per-process rename and
/// trunk caches cannot leak one test's answers into another.
#[cfg(test)]
pub(super) fn batched_repo(slug: &str) -> crate::repos::Repo {
    serde_json::from_value(serde_json::json!({
        "id": slug.rsplit('/').next().unwrap_or(slug),
        "source": format!("https://github.com/{slug}.git"),
        "work": "",
        "store": "",
        "review_queue": true,
    }))
    .unwrap()
}

/// A GitHub that answers one canned body to every request — except on `dies_on`, where it
/// kills the connection mid-answer. `answer: None` kills every connection. For proving what a
/// code path does NOT ask for, and what it says when the one thing it does ask for dies.
#[cfg(test)]
pub(super) fn recording_github(
    answer: Option<&'static str>,
    dies_on: Option<&'static str>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = seen.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap_or(0);
            recorder
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).into_owned());
            let asked = recorder.lock().unwrap().last().cloned().unwrap_or_default();
            let dead = dies_on.is_some_and(|p| asked.contains(p));
            match answer.filter(|_| !dead) {
                None => {
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\nhalf an ans");
                    let _ = stream.flush();
                }
                Some(body) => {
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                }
            }
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

/// The recorded `/graphql` requests, whole.
#[cfg(test)]
pub(super) fn graphql_requests(seen: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|r| r.contains(" /graphql "))
        .cloned()
        .collect()
}
