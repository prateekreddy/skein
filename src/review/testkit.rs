//! The fixtures more than one of these modules reads a pull request through.
//!
//! They live together because they are shared, not because they are alike: a stub GitHub and a
//! stub `claude` on a real socket, a committed git checkout to mirror from, and the `Pr` and
//! [`Known`] values whose defaults would otherwise be typed out at each call site. A second copy
//! of any of them is a second place for the accounting they exist to make checkable to be wrong.

use super::summary::{Depth, Known, Summary};
use crate::repos::Repo;
use std::fs;

/// Test-only: read one HTTP request off a socket properly — headers to the blank line, then
/// exactly Content-Length bytes of body. The fixed-size single read the stubs used truncated the
/// batched GraphQL request when SKEIN-209 folded five searches into one call, and a stub that
/// answers a half-read request answers the wrong question.
#[cfg(test)]
pub(super) fn read_request(stream: &std::net::TcpStream) -> (String, String) {
    use std::io::{BufRead as _, Read as _};
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    reader.read_line(&mut head).ok();
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
    (
        head.trim_end().to_string(),
        String::from_utf8_lossy(&body).into_owned(),
    )
}

/// Make `id`'s mirror from a local checkout.
///
/// `ensure_mirror` clones from `repo.source`, and these fixtures deliberately set that to a
/// real GitHub URL so the slug matches the stub — cloning it would reach the network. It used
/// to prefer an adopted repo's `source_tree`, which is what made these fixtures offline; there
/// are no adopted repos any more, so the fixture makes the mirror itself.
///
/// A mirror that cannot be read is not a neutral condition here: a repo whose tree is
/// unreadable has its summaries served WITHOUT being cached (SKEIN-117), which is what the
/// drafting tests assert on.
#[cfg(unix)]
pub(super) fn mirror_from(id: &str, checkout: &std::path::Path) {
    let mirror = crate::repos::mirror_path(id);
    std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
    let out = std::process::Command::new("git")
        .args(["clone", "--quiet", "--mirror"])
        .arg(checkout)
        .arg(&mirror)
        .output()
        .expect("git clone --mirror");
    assert!(
        out.status.success(),
        "mirroring the fixture checkout: {out:?}"
    );
}

/// A GitHub that answers with two pull requests — #21 waiting on your review, #22 where you
/// are only mentioned — and a `claude` that answers whichever prompt it is handed: the MERGED
/// summary-and-review shape when the prompt carries `REVIEW:` (a yours-to-give visit), the
/// OVERALL shape for a standalone review draft, the stage-1 shape otherwise. Review-drafting
/// calls are counted into a file, because "it drafted nothing" looks identical whether or not
/// the model was asked, and the dedupe tests below are ABOUT how often it was asked. Every
/// request's first line is also appended to `home/hits`, so a test can count DOWNLOADS —
/// the one-diff-fetch rule is about the wire, not about what landed on disk.
///
/// Both pull requests carry a commit date of NOW: the settle hour is gone (owner decision,
/// 2026-08-24), so the pass must read and draft a branch that is still moving.
#[cfg(unix)]
pub(super) fn drafting_fixture(home: &std::path::Path) -> std::path::PathBuf {
    drafting_fixture_for(home, "crit", false)
}

/// The same wire, serving the OTHER shape this pass has to work: a queue where every pull
/// request is one YOU opened (`q2`, the `author:` search) — #31 proposed and #32 still a
/// draft, both in `Lane::Waiting` because that is where `build_pr` files what you wrote.
///
/// One fixture rather than two, because the thing the authored tests assert is a COUNT of model
/// calls and diff downloads, and a second stub would be a second place for that accounting to
/// be wrong in.
#[cfg(unix)]
pub(super) fn authored_fixture(home: &std::path::Path) -> std::path::PathBuf {
    drafting_fixture_for(home, "mine", true)
}

#[cfg(unix)]
pub(super) fn drafting_fixture_for(
    home: &std::path::Path,
    repo_id: &'static str,
    authored: bool,
) -> std::path::PathBuf {
    std::env::set_var("SKEIN_HOME", home);
    std::env::set_var("SKEIN_REVIEW_AI", "on");
    let reviews_asked = home.join("reviews-asked");
    let sweeps = home.join("sweeps");
    let claude = home.join("claude-both.sh");
    // The merged prompt is the only one that says "the REVIEW half" — the stage prompts ask
    // for KIND/LINE and nothing else. It used to be matched on the literal `REVIEW:`, which
    // was in its answer format until the review stopped coming back to skein at all.
    // Branching on the prompt is what lets ONE binary serve every call the pass makes.
    std::fs::write(
        &claude,
        format!(
            concat!(
                // **The prompt arrives on STDIN, and is not an argument at all** (SKEIN-684). It
                // was `$4` until a reading became a conversation (SKEIN-393) and the command
                // line grew `--session-id <uuid>` between the model and the prompt; then the
                // last argument, until the payload came off argv entirely — a single argv
                // element is capped at `MAX_ARG_STRLEN`, and it is world-readable in
                // `/proc/<pid>/cmdline`, and a diff is neither small enough nor public enough
                // for either. Each move broke this fixture the same silent way: the stub matched
                // nothing, printed nothing, and nine tests reported that the pass had read
                // nothing at all.
                "#!/bin/sh\np=$(cat)\ncase \"$p\" in\n",
                // The second turn (SKEIN-393). It is answered "nothing new", which is what the
                // prompt says the expected outcome is — so the fixture exercises the path a
                // real sweep takes most of the time, and a sweep that invented findings here
                // would be the fixture teaching the assertions the wrong shape.
                //
                // Counted in its OWN file: the merged/critique count is what the SKEIN-263
                // tripwire reads, and adding a third word to it would rewrite what nine
                // existing assertions mean rather than leaving them saying what they said.
                "  *\"account for what it actually covered\"*) echo sweep >> {sweeps}; printf 'OVERALL: nothing new\\n';;\n",
                // Both halves of the merged answer carry the ORDINAL of the model call that
                // produced them, so a test can prove the summary on the row and the review
                // under it came out of the same reading (SKEIN-263) rather than merely both
                // existing. `wc -l` on the count file after appending IS this call's number.
                "  *\"the REVIEW half\"*) echo merged >> {count}; n=$(wc -l < {count} | tr -d ' '); printf 'KIND: fix\\nLINE: reading %s of this change.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\n' \"$n\";;\n",
                // The standalone critique prompt, which nothing reaches any more (SKEIN-263
                // deleted the drafter). Kept as a TRIPWIRE: a second drafter coming back would
                // put a `critique` line in this count, and the assertions that read it as
                // `["merged", "merged"]` would say so.
                "  *OVERALL:*) echo critique >> {count}; printf 'OVERALL: nothing to flag\\n';;\n",
                "  *) printf 'KIND: fix\\nLINE: it changes a thing.\\nEXPAND: no\\nFLAGS: none\\n';;\n",
                "esac\n"
            ),
            count = reviews_asked.display(),
            sweeps = sweeps.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &claude,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
    std::env::set_var("SKEIN_CLAUDE_BIN", &claude);
    std::env::set_var("GH_TOKEN", "gho_test");
    crate::prq::forget_host_token();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let hits = home.join("hits");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            use std::io::Write as _;
            let mut stream = stream;
            let (head, body) = read_request(&stream);
            // Every request's first line, so a test can count downloads on the wire.
            {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&hits)
                {
                    let _ = writeln!(f, "{head}");
                }
            }
            // A commit dated NOW: the settle hour is gone, and the pass must read a branch
            // that is still moving.
            let committed = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let node = |number: u64, author: &str, draft: bool| {
                format!(
                    r#"{{"number":{number},"title":"t","url":"u",
                           "isDraft":{draft},"author":{{"login":"{author}"}},"headRefName":"feat",
                           "headRefOid":"sha{number}","baseRefName":"main",
                           "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                           "latestReviews":{{"nodes":[]}},
                           "commits":{{"nodes":[{{"commit":{{"committedDate":"{committed}"}}}}]}}}}"#
                )
            };
            // The batched wire (SKEIN-209): q0 review-requested, q1 reviewed-by, q2 author,
            // q3 mentions — #21 waits on your review, #22 only mentions you. In the authored
            // shape, q2 instead: #31 yours and proposed, #32 yours and still a draft.
            let answer = if head.contains("/user/teams") {
                "[]".to_string()
            } else if head.contains("/user") {
                r#"{"login":"me"}"#.to_string()
            } else if body.contains("review-requested:") && authored {
                format!(
                    r#"{{"data":{{"q0":{{"nodes":[]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{},{}]}},"q3":{{"nodes":[]}}}}}}"#,
                    node(31, "me", false),
                    node(32, "me", true)
                )
            } else if body.contains("review-requested:") {
                format!(
                    r#"{{"data":{{"q0":{{"nodes":[{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[{}]}}}}}}"#,
                    node(21, "someone", false),
                    node(22, "someone", false)
                )
            } else if head.contains("/graphql") {
                r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
            } else {
                "{}".to_string()
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
        }
    });
    std::env::set_var("SKEIN_GITHUB_API", &base);

    // A real checkout behind the fixture repo — same reason as
    // `what_it_reads_unwatched_is_what_you_were_asked_to_review`'s: the drafting tests assert what
    // lands in the CACHE, and a repo whose mirror cannot be read has its summaries served without
    // being cached (SKEIN-117). The slug still comes from `source`, so the GitHub stub is untouched.
    let checkout = home.join("checkout");
    checkout_fixture(&checkout);
    crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
        "id": repo_id,
        "source": "https://github.com/acme/thing.git",
        "store": "",
        "read_prs": true,
    }))
    .unwrap()])
    .unwrap();
    mirror_from(repo_id, &checkout);
    // The queue micro-cache outlives a test's SKEIN_HOME; a stale hit would answer with a
    // queue read against another test's stub.
    crate::prq::invalidate(repo_id);
    reviews_asked
}

#[cfg(unix)]
pub(super) fn drafting_teardown() {
    drafting_teardown_for("crit")
}

#[cfg(unix)]
pub(super) fn drafting_teardown_for(repo_id: &str) {
    for key in [
        "SKEIN_HOME",
        "SKEIN_REVIEW_AI",
        "SKEIN_CLAUDE_BIN",
        "SKEIN_GITHUB_API",
        "GH_TOKEN",
    ] {
        std::env::remove_var(key);
    }
    crate::prq::invalidate(repo_id);
    crate::prq::forget_host_token();
}

/// A GitHub serving two repositories from one stub, keyed on the `repo:` term the batched
/// search carries (`prq::one_request` builds `repo:{slug} is:pr is:open {search}`):
/// `acme/busy` answers the `author:` alias with four pull requests you opened, `acme/quiet`
/// answers the `review-requested:` alias with one waiting on you. Registered in that order,
/// because the bug being asserted against is registry order.
#[cfg(unix)]
pub(super) fn two_repo_fixture(home: &std::path::Path) {
    std::env::set_var("SKEIN_HOME", home);
    std::env::set_var("SKEIN_REVIEW_AI", "on");
    let claude = home.join("claude-both.sh");
    std::fs::write(
        &claude,
        "#!/bin/sh\n# Read from stdin, which is where the prompt is (SKEIN-684) — see the sibling\n# fixture above for what putting it back on argv would cost.\np=$(cat)\n# The second turn asks a different question and must get a different answer: handed\n# the merged text back, parse_critique reads its summary LINE: as a comment anchor\n# and the review grows a finding nobody wrote (SKEIN-393).\ncase \"$p\" in\n  *\"account for what it actually covered\"*) printf 'OVERALL: nothing new\\n'; exit 0;;\nesac\nprintf 'KIND: fix\\nLINE: a reading.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(
        &claude,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
    std::env::set_var("SKEIN_CLAUDE_BIN", &claude);
    std::env::set_var("GH_TOKEN", "gho_test");
    crate::prq::forget_host_token();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            use std::io::Write as _;
            let mut stream = stream;
            let (head, body) = read_request(&stream);
            let committed = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let node = |number: u64, author: &str, updated: &str| {
                format!(
                    r#"{{"number":{number},"title":"t","url":"u",
                           "isDraft":false,"author":{{"login":"{author}"}},"headRefName":"feat",
                           "headRefOid":"sha{number}","baseRefName":"main",
                           "updatedAt":"{updated}","reviewDecision":"REVIEW_REQUIRED",
                           "latestReviews":{{"nodes":[]}},
                           "commits":{{"nodes":[{{"commit":{{"committedDate":"{committed}"}}}}]}}}}"#
                )
            };
            let answer = if head.contains("/user/teams") {
                "[]".to_string()
            } else if head.contains("/user") {
                r#"{"login":"me"}"#.to_string()
            } else if body.contains("repo:acme/busy") {
                format!(
                    r#"{{"data":{{"q0":{{"nodes":[]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{},{},{},{}]}},"q3":{{"nodes":[]}}}}}}"#,
                    node(41, "me", "2024-01-04T00:00:00Z"),
                    node(42, "me", "2024-01-03T00:00:00Z"),
                    node(43, "me", "2024-01-02T00:00:00Z"),
                    node(44, "me", "2024-01-01T00:00:00Z"),
                )
            } else if body.contains("repo:acme/quiet") {
                format!(
                    r#"{{"data":{{"q0":{{"nodes":[{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                    node(51, "someone", "2024-01-05T00:00:00Z"),
                )
            } else if head.contains("/graphql") {
                r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
            } else {
                "{}".to_string()
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
        }
    });
    std::env::set_var("SKEIN_GITHUB_API", &base);

    // A real checkout behind both, for the same reason every other reader test has one: a repo
    // whose mirror cannot be read is summarised BLIND and blind summaries are never cached
    // (SKEIN-117), which would turn this ordering assertion into a caching one.
    let checkout = home.join("checkout");
    checkout_fixture(&checkout);
    let repo = |id: &str, slug: &str| {
        serde_json::from_value::<Repo>(serde_json::json!({
            "id": id,
            "source": format!("https://github.com/{slug}.git"),
            "store": "",
            "read_prs": true,
        }))
        .unwrap()
    };
    crate::repos::save_repos(&[repo("busy", "acme/busy"), repo("quiet", "acme/quiet")]).unwrap();
    crate::prq::invalidate("busy");
    crate::prq::invalidate("quiet");
}

/// Make a file look older than it is. Several call sites now, and a brace-heavy block inlined
/// four times is one that drifts.
pub(super) fn backdate(path: &std::path::Path, to: std::time::SystemTime) {
    let handle = fs::OpenOptions::new().write(true).open(path).unwrap();
    handle
        .set_times(fs::FileTimes::new().set_modified(to).set_accessed(to))
        .unwrap();
}

/// A GitHub that answers "is this pull request open" by number.
pub(super) fn stub_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).ok();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line.trim().is_empty() {
                    break;
                }
                line.clear();
            }
            recorder.lock().unwrap().push(path.clone());
            let body = match path.ends_with("/pulls/9") {
                true => r#"{"state":"closed"}"#,
                false => r#"{"state":"open"}"#,
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    (format!("http://127.0.0.1:{port}"), asked)
}

/// Run git in `dir`, with an identity, and refuse to continue if it failed.
pub(super) fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A committed checkout at `dir`, with `src/a.rs` and `web/b.js` — and no CODEOWNERS unless
/// the test adds one and commits again.
pub(super) fn checkout_fixture(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("web")).unwrap();
    std::fs::write(dir.join("src").join("a.rs"), "fn a() {}").unwrap();
    std::fs::write(dir.join("web").join("b.js"), "// b").unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "one"]);
}

/// A repo registered against `checkout`, adopted in place, so its mirror reads from disk.
pub(super) fn repo_at(id: &str, checkout: &std::path::Path) -> Repo {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "source": checkout.to_string_lossy(),
        "store": "",
    }))
    .unwrap()
}

/// A Pr literal for budget tests — the queue's own fields, one place.
pub(super) fn budget_pr(number: u64, head: &str) -> crate::prq::Pr {
    crate::prq::Pr {
        reasons: vec![crate::prq::Reason::Reviewer],
        lane: crate::prq::Lane::NeedsYou,
        ..crate::prq::blank_pr(number, head)
    }
}

/// A repo skein has mirrored, with two commits: the first adds a file the second deletes.
pub(super) fn a_repo_with_two_commits(home: &std::path::Path) -> (Repo, String, String) {
    let src = home.join("origin");
    fs::create_dir_all(&src).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&src)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(src.join("only-in-first.txt"), "one\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "one"]);
    let first = git(&["rev-parse", "HEAD"]);
    fs::remove_file(src.join("only-in-first.txt")).unwrap();
    fs::write(src.join("only-in-second.txt"), "two\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "two"]);
    let second = git(&["rev-parse", "HEAD"]);

    let repo: Repo = serde_json::from_value(serde_json::json!({
        "id": "acme",
        "source": src.to_string_lossy(),
        "store": "",
    }))
    .unwrap();
    crate::repos::ensure_mirror(&repo).expect("the fixture repo is mirrored");
    (repo, first, second)
}

/// A reading with everything in it — brief, signals, ownership, a drafted review.
pub(super) fn fat(number: u64, head: &str) -> Known {
    Known::new(
        Summary {
            // A fixture, and this is the honest value for one: nobody scanned a diff.
            owed_triggered: None,
            findings_block: None,

            swept: false,
            number,
            head_sha: head.into(),
            depth: Depth::Expanded,
            line: "the request timeout default drops from 30s to 5s.".into(),
            detail: "## What it does\n\nShortens how long a request waits.\n".into(),
            flags: vec!["default".into(), "behaviour".into()],
            yours: vec!["src/parser.rs".into()],
            others: 3,
            ownership_unknown: String::new(),
            signals: vec![crate::contracts::Signal {
                kind: "default".into(),
                what: "TIMEOUT moved from 30 to 5".into(),
                file: "src/parser.rs".into(),
                symbol: "TIMEOUT".into(),
            }],
            unread_because: String::new(),
            not_reread: String::new(),
            computed: false,
            budget_stopped: false,
        },
        false,
    )
}
