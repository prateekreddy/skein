//! The merge a person presses, with the guards that make it safe to press. (SKEIN-338)
//!
//! [`merge`](super::merge) holds the `PUT` and knows nothing about bases. This is what puts the
//! two facts in front of it that a merge turns on, both read live: that the pull request is based
//! on the repository's trunk, and that its head is still the commit the person read. Opening step
//! 7 of a stack, reading it and pressing merge once merged step 6 into step 7 (SKEIN-237), which
//! is what the base check is for.

use super::{base_and_head, merge, trunk_of};

/// Merge pull request `number`, if it is on the trunk and still at `seen_head`.
///
/// The order is deliberate. Base first, then head. A stacked child is wrong to merge at *any* head,
/// so "the branch moved" would be a distraction in front of it, and the reader who re-read and
/// pressed again would get the real refusal on the second press instead of the first.
pub fn merge_by_hand(slug: &str, number: u64, seen_head: &str) -> Result<String, String> {
    // Before any request, because there is no request worth making. An empty `seen_head` means the
    // caller cannot say which commit the person was looking at, and a merge that cannot name its
    // revision is the unguarded merge this function exists to replace.
    if seen_head.trim().is_empty() {
        return Err(format!(
            "skein does not know which commit of #{number} you are looking at, and will not merge \
             a revision it cannot name. Refresh the queue and read the change again."
        ));
    }
    // One request for both facts, live. Not the queue: `base_ref` moves under a stacked child the
    // moment its parent lands, and `head_sha` moves on every push. An `Err` stops the merge: see
    // `base_and_head` for why this is the one read whose failure must not fall back.
    let (base_ref, live_head) = base_and_head(slug, number)?;
    // `""` is "not known", which refuses in its own words rather than calling the pull request
    // stacked.
    let trunk = trunk_of(slug);
    if trunk.is_empty() {
        return Err(format!(
            "#{number} is based on {base_ref} — skein does not yet know this repository's \
             default branch, and will not merge into a base it cannot check"
        ));
    }
    if base_ref != trunk {
        return Err(format!(
            "#{number} is based on {base_ref} — this is not based on the trunk — merging it \
             would land its commits on its base branch instead of shipping them, and delete the \
             branch. It rejoins when its base becomes the repository's default branch"
        ));
    }
    // Checked here as well as sent as `sha`, and both are wanted. This one can say what the head
    // moved TO, which GitHub's 409 cannot; the `sha` on the wire closes the window between this
    // check and the merge, which no check up here can.
    if live_head != seen_head {
        return Err(format!(
            "the branch moved since you read it — you read {}, #{number} is now at {}. Read the \
             new code, then merge.",
            short(seen_head),
            short(&live_head)
        ));
    }
    merge(slug, number, seen_head)
}

/// Enough of a sha to recognise, for a sentence a person reads. `get` rather than a slice so a
/// short or empty sha is returned whole instead of panicking on a merge refusal.
fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// A GitHub whose pull request can be posed: a base, a head, and whether the repository will
    /// say what its default branch is. Records every request line with its body.
    ///
    /// These tests are about what skein REFUSES, and a stub that answers everything the same way
    /// cannot tell a merge that was refused from one that was attempted and failed. The three
    /// answers here are the three facts a merge turns on.
    #[allow(clippy::type_complexity)]
    fn merge_world() -> (
        String,
        Arc<Mutex<(String, String, Option<String>, u16)>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let world: Arc<Mutex<(String, String, Option<String>, u16)>> = Arc::new(Mutex::new((
            "main".to_string(),
            "abc1234def".to_string(),
            Some("main".to_string()),
            200,
        )));
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (state, seen) = (world.clone(), heard.clone());
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
                let (base_ref, head_sha, trunk, merge_status) = state.lock().unwrap().clone();
                let (status, answer) = if head.contains("/user") {
                    (200, r#"{"login":"me"}"#.to_string())
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    match trunk {
                        // The repository, and what it calls its trunk.
                        Some(t) => (
                            200,
                            format!(r#"{{"full_name":"acme/thing","default_branch":"{t}"}}"#),
                        ),
                        // A repository GitHub answers about without naming a default branch,
                        // which `trunk_of` reads as `""`: skein cannot
                        // say what the trunk is.
                        //
                        // **Deliberately not a 403.** The first version of this stub posed an
                        // unknown trunk as a rate limit, which is the commonest real cause — and
                        // `github::rate_limited` engages a PROCESS-GLOBAL hold that stops every
                        // GitHub call in the test binary for fifteen minutes. Two unrelated tests
                        // in this module failed on it, in another module's words, with nothing at
                        // their own failure site to say why. The hold has no reset, so a fixture
                        // must never trip it.
                        None => (200, r#"{"full_name":"acme/thing"}"#.to_string()),
                    }
                } else if head.starts_with("GET /repos/acme/thing/pulls/41") {
                    (
                        200,
                        format!(
                            r#"{{"number":41,"node_id":"PR_n","base":{{"ref":"{base_ref}"}},"head":{{"sha":"{head_sha}"}}}}"#
                        ),
                    )
                } else if head.contains("/merge") {
                    match merge_status {
                        // GitHub's own words for a conditional merge whose branch moved. Quoted
                        // here so the translation is tested against what GitHub sends, not against
                        // what skein hopes it sends.
                        409 => (
                            409,
                            r#"{"message":"Head branch was modified. Review and try the merge again."}"#.to_string(),
                        ),
                        // And its words for a merge it will not attempt because the branch
                        // conflicts with its base. Quoted from the same place: SKEIN-385's commit
                        // measured `GitHub said 405: Pull Request has merge conflicts` against
                        // `acme/testbed#20`.
                        405 => (
                            405,
                            r#"{"message":"Pull Request has merge conflicts"}"#.to_string(),
                        ),
                        s => (s, r#"{"merged":true,"message":"Pull Request successfully merged"}"#.to_string()),
                    }
                } else {
                    (200, "{}".to_string())
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
        (base, world, heard)
    }

    /// **A merge a person presses reaches GitHub only when the base is the trunk and the head is
    /// still the one they read — and when it does, it names that head.**
    ///
    /// One assertion over a table rather than an outcome per pair, because the last regression in
    /// this area was exactly a per-pair test: `prq::merge` was tested for "it merges", which it did,
    /// and nobody asked what it merged. The claim here is a biconditional — the merge happens IF AND
    /// ONLY IF every guard is satisfied — so a guard that is deleted fails a row that expected a
    /// refusal, and a guard that is inverted fails the row that expected a merge. Neither can be
    /// made to pass by weakening the other.
    ///
    /// The two facts each row poses are the two the merge turns on and the two the queue is worst
    /// at: `base_ref` moves under a stacked child when its parent lands, `head_sha` moves on every
    /// push. See `crate::prq::base_and_head`.
    #[test]
    fn a_merge_by_hand_happens_only_on_the_trunk_at_the_head_you_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        env.set("SKEIN_GITHUB_API", &api);

        // (what it is named, base ref, live head, the head the reader says they saw, trunk)
        let table: &[(&str, &str, &str, &str, Option<&str>)] = &[
            ("clean", "main", "abc1234def", "abc1234def", Some("main")),
            ("no sha at all", "main", "abc1234def", "", Some("main")),
            ("blank sha", "main", "abc1234def", "   ", Some("main")),
            (
                "the branch moved",
                "main",
                "999999999",
                "abc1234def",
                Some("main"),
            ),
            (
                "a stacked child",
                "ladder/tenants-07",
                "abc1234def",
                "abc1234def",
                Some("main"),
            ),
            (
                "a stacked child whose head also moved",
                "ladder/tenants-07",
                "999999999",
                "abc1234def",
                Some("main"),
            ),
            (
                "the trunk is unknown",
                "main",
                "abc1234def",
                "abc1234def",
                None,
            ),
            (
                "the trunk is unknown and the base is odd",
                "ladder/tenants-07",
                "abc1234def",
                "abc1234def",
                None,
            ),
        ];

        for (name, base_ref, live, seen, trunk) in table {
            *world.lock().unwrap() = (
                base_ref.to_string(),
                live.to_string(),
                trunk.map(str::to_string),
                200,
            );
            // Both are memoised per process, and the trunk especially: without this every row after
            // the first would be answered from the first row's repository.
            crate::prq::forget_trunks();
            crate::prq::forget_host_token();
            heard.lock().unwrap().clear();

            let out = merge_by_hand("acme/thing", 41, seen);
            let calls = heard.lock().unwrap().clone();
            let merged: Vec<String> = calls
                .iter()
                .filter(|c| c.contains("/pulls/41/merge"))
                .cloned()
                .collect();

            // The rule, written once. Everything below compares against THIS rather than against a
            // literal per row, so a row cannot be made to pass by adjusting its own expectation.
            let should =
                trunk.is_some_and(|t| t == *base_ref) && !seen.trim().is_empty() && live == seen;

            assert_eq!(
                !merged.is_empty(),
                should,
                "{name}: a merge request {} GitHub when it should {} — base {base_ref:?}, trunk \
                 {trunk:?}, live head {live:?}, head read {seen:?}. Answer was {out:?}",
                match merged.is_empty() {
                    true => "never reached",
                    false => "reached",
                },
                match should {
                    true => "have",
                    false => "not have",
                },
            );
            assert_eq!(
                out.is_ok(),
                should,
                "{name}: merge_by_hand answered {out:?}, which disagrees with whether it merged"
            );
            // The whole point of the `sha`: whatever went out named the commit the person read, not
            // "whatever is there now".
            for call in &merged {
                assert!(
                    call.contains(&format!("\"sha\":\"{seen}\"")),
                    "{name}: the merge did not name the head the reader read ({seen:?}): {call}"
                );
            }
            // A refusal that still asked GitHub to merge and was turned down is not a guard — it is
            // GitHub guarding skein. Nothing may be attempted on a row that must not merge.
            if !should {
                assert!(
                    merged.is_empty(),
                    "{name}: the guard let the request out and relied on GitHub to refuse it: {merged:?}"
                );
            }
        }

        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **Each refusal says which guard refused, and a merge with no sha costs no request at all.**
    ///
    /// The table above proves the guards fire; this proves they are distinguishable, which is what
    /// makes them actionable. A reader told only "not merged" cannot tell "read the new code" from
    /// "this is a stacked child and never will merge from here", and those two need opposite
    /// responses.
    ///
    /// The base check running BEFORE the head check is asserted here rather than left to reading
    /// order: a stacked child is wrong to merge at any head, so being told its branch moved would
    /// send the reader to re-read a change that still must not merge.
    #[test]
    fn a_refused_merge_says_which_guard_refused_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        env.set("SKEIN_GITHUB_API", &api);
        let pose = |base: &str, live: &str, trunk: Option<&str>| {
            *world.lock().unwrap() = (
                base.to_string(),
                live.to_string(),
                trunk.map(str::to_string),
                200,
            );
            crate::prq::forget_trunks();
            crate::prq::forget_host_token();
            heard.lock().unwrap().clear();
        };

        // No sha: refused before anything is asked. A merge with nothing to name is not a question
        // worth putting to GitHub, and a round trip here would be a round trip on every press.
        pose("main", "abc1234def", Some("main"));
        let out = merge_by_hand("acme/thing", 41, "");
        let why = out.unwrap_err();
        assert!(
            why.contains("which commit") && why.contains("#41"),
            "a merge with no head read did not say that is what was wrong: {why}"
        );
        assert!(
            heard.lock().unwrap().is_empty(),
            "a merge that could not name a commit still spent a request: {:?}",
            heard.lock().unwrap()
        );

        // Moved: names both commits, because "it moved" without saying where to is not actionable.
        pose("main", "999999999", Some("main"));
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("moved") && why.contains("abc1234") && why.contains("9999999"),
            "the refusal did not name both the head that was read and the head that is there: {why}"
        );

        // A stacked child: names its base, and says the thing that makes it recoverable — that it
        // rejoins when its parent lands. This is SKEIN-237's sentence, reached from the hand path.
        pose("ladder/tenants-07", "abc1234def", Some("main"));
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("ladder/tenants-07") && why.contains("not based on the trunk"),
            "a stacked child was refused without saying it is one: {why}"
        );
        assert!(
            !why.contains("moved"),
            "a stacked child was refused for the wrong reason — the head check ran first: {why}"
        );

        // And with a head that ALSO moved, the base is still what it is told about: a child must
        // not be sent away to re-read a change it may never merge from here.
        pose("ladder/tenants-07", "999999999", Some("main"));
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("not based on the trunk") && !why.contains("moved since you read it"),
            "the base check did not run before the head check: {why}"
        );

        // Blind, not wrong. An unknown trunk is skein's own failure to see and says so, rather than
        // accusing the pull request of being stacked.
        pose("main", "abc1234def", None);
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("default branch") && !why.contains("not based on the trunk"),
            "an unknown trunk was reported as the pull request's fault: {why}"
        );

        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **A 409 from GitHub is reported as the branch having moved, not as GitHub's own prose.**
    ///
    /// The check inside `merge_by_hand` cannot close the window between reading the head and
    /// sending the merge — only the `sha` on the wire can — so this is the path where the guard
    /// actually holds, and its sentence has to be the same one the pre-check gives. Posed by moving
    /// the branch only in GitHub's ANSWER, which is what a push landing mid-request looks like.
    #[test]
    fn a_race_lost_to_a_push_reads_as_the_branch_moving() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        env.set("SKEIN_GITHUB_API", &api);
        // Everything checks out and the merge itself 409s — the branch moved between the check and
        // the PUT, which no amount of checking beforehand can prevent.
        *world.lock().unwrap() = (
            "main".to_string(),
            "abc1234def".to_string(),
            Some("main".to_string()),
            409,
        );
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();

        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("moved since you read it") && why.contains("abc1234"),
            "a 409 was passed through in GitHub's words instead of the reader's: {why}"
        );
        assert!(
            !why.contains("409"),
            "the raw status reached the reader: {why}"
        );
        // It got as far as trying, which is the difference between this and the pre-check.
        assert!(
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains("/pulls/41/merge")),
            "the 409 test never reached the merge, so it proves nothing about the 409"
        );

        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **A conflicted pull request is refused in a sentence about the pull request.** (SKEIN-411)
    ///
    /// The end of the road SKEIN-385 opened: that item put a refusal on screen, and what it put
    /// there was `GitHub said 405: Pull Request has merge conflicts` — a status code and somebody
    /// else's noun phrase. Posed here through `merge_by_hand`, not against
    /// `prq::it_conflicts_with_its_base` directly, because the translation and the press are wired
    /// together by one `map_err` in `prq::merge` and a unit test on the function proves nothing
    /// about that wire. Every guard passes, so the only thing that can refuse this merge is GitHub.
    #[test]
    fn a_merge_refused_for_conflicts_says_so_in_skein_s_words() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        env.set("SKEIN_GITHUB_API", &api);
        // On the trunk, at the head the reader read, and GitHub answers the PUT with a 405.
        *world.lock().unwrap() = (
            "main".to_string(),
            "abc1234def".to_string(),
            Some("main".to_string()),
            405,
        );
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();

        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("conflicts with its base") && why.contains("#41"),
            "a 405 for conflicts was passed through in GitHub's words instead of the reader's: \
             {why}"
        );
        assert!(
            why.contains("Resolve them on the branch"),
            "the refusal named the problem without naming the way out of it: {why}"
        );
        assert!(
            !why.contains("405") && !why.contains("GitHub said"),
            "the raw status reached the reader: {why}"
        );
        // The guards passed and the merge was actually attempted — without this the test would
        // also pass if `merge_by_hand` had refused before ever asking GitHub, which is a different
        // sentence about a different problem.
        assert!(
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains("/pulls/41/merge")),
            "the conflict test never reached the merge, so it proves nothing about the 405"
        );

        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }
}
