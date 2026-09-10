//! The requests that actually reach GitHub.
//!
//! One function per outward-facing act, and each is the only place its request body is written
//! down. That matters most for the head anchor: [`update_branch`] sends `expectedHeadOid` and
//! [`merge_pr`] sends `sha`, so neither can act on a branch that moved while skein was deciding —
//! and [`add_label`] / [`remove_label`] send no head at all, because a label is not a decision
//! about a commit. `docs/pr-workflow.md` carries that table and the reproduction commands for it.
//!
//! [`merge_by_hand`] is the cockpit's button rather than a workflow act, and it is here for the
//! reason SKEIN-338 exists: it is the ONE path a person can press, so it carries the trunk check
//! and the head anchor together. `tests/merge_guard.rs` asserts that nothing else calls the bare
//! `prq::merge`.

use crate::workflow::{Act, MergeAs, Update};

pub(super) fn add_label(
    slug: &str,
    number: u64,
    label: &str,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    crate::github::send_json(
        "POST",
        &format!("{}/issues/{number}/labels", crate::github::repo_path(slug)),
        token,
        &serde_json::json!({ "labels": [label] }),
    )
    .map(|_| ())
}

pub(super) fn remove_label(
    slug: &str,
    number: u64,
    label: &str,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    // A label may contain a space or a slash. Encoded rather than interpolated raw: a label called
    // `needs review` would otherwise produce a path GitHub answers 404 for, and the workflow would
    // stop on a step that was perfectly well written. The encoder moved to `github::path_segment`
    // so `delete_branch` below can reach it too — it was written here and forgotten there.
    let label = crate::github::path_segment(label);
    crate::github::send_json(
        "DELETE",
        &format!(
            "{}/issues/{number}/labels/{label}",
            crate::github::repo_path(slug)
        ),
        token,
        &serde_json::json!({}),
    )
    .map(|_| ())
}

/// Bring the branch up to date with its base.
///
/// GraphQL, because REST cannot rebase: `PUT …/update-branch` takes `expected_head_sha` and merges,
/// full stop. `updatePullRequestBranch` is what `gh pr update-branch --rebase` calls, and its
/// `updateMethod` is the only way to ask for a rebase over the API at all. Established in
/// `docs/pr-workflow.md`, against GitHub's live schema.
///
/// `expectedHeadOid` is not optional here even though it is in the schema: without it, a push that
/// landed while skein was deciding gets rebased sight unseen.
pub(super) fn update_branch(
    slug: &str,
    number: u64,
    head_sha: &str,
    how: Update,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    let id = node_id(slug, number, token)?;
    let method = match how {
        Update::Rebase => "REBASE",
        Update::Merge => "MERGE",
    };
    let query = "mutation($id: ID!, $oid: GitObjectID!, $how: PullRequestBranchUpdateMethod!) {\n\
       \x20 updatePullRequestBranch(input: {pullRequestId: $id, expectedHeadOid: $oid, \
         updateMethod: $how}) { pullRequest { headRefOid } }\n\
     }";
    crate::github::graphql(
        query,
        serde_json::json!({ "id": id, "oid": head_sha, "how": method }),
        token,
    )
    .map(|_| ())
}

/// The pull request's GraphQL node id, which the mutation needs and the queue does not carry.
///
/// One extra read, and only on the rare step that rebases. GitHub reads are cheap here — the owner
/// said so explicitly — and adding a field to the queue's search for the sake of an action almost
/// no poll takes would make every poll pay for it.
pub(super) fn node_id(
    slug: &str,
    number: u64,
    token: &crate::secret::Secret,
) -> Result<String, String> {
    let pr = crate::github::get_json(
        &format!("{}/pulls/{number}", crate::github::repo_path(slug)),
        token,
    )?;
    pr.get("node_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("GitHub did not say what #{number}'s node id is"))
}

pub(super) fn merge_pr(
    slug: &str,
    number: u64,
    head_sha: &str,
    how: MergeAs,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    let method = match how {
        MergeAs::Squash => "squash",
        MergeAs::Merge => "merge",
        MergeAs::Rebase => "rebase",
    };
    crate::github::send_json(
        "PUT",
        &format!("{}/pulls/{number}/merge", crate::github::repo_path(slug)),
        token,
        // `sha` is the head skein decided on. GitHub answers 409 if the branch has moved since,
        // which is exactly the answer wanted: somebody pushed, and the decision to merge was made
        // about code that is no longer what would be merged.
        &serde_json::json!({ "merge_method": method, "sha": head_sha }),
    )
    .map(|_| ())
    .map_err(|e| conflicts_stopped_the_train(number, e))
}

/// GitHub's 405 for a conflicted branch, said to somebody reading a stopped train (SKEIN-423).
///
/// This `Err` does not go back to a person who is standing there: [`perform`] turns it into
/// `"{flow} step {n} could not be done: {why}"`, writes it down with [`stop`], and the cockpit
/// draws it days later under **Stopped.** in `revFlowBox` (`src/web/index.html`). Untranslated,
/// what that read was `GitHub said 405: Pull Request has merge conflicts` — SKEIN-411 fixed exactly
/// that sentence for the merge a person presses ([`merge_by_hand`] below, via
/// `prq::it_conflicts_with_its_base`) and scoped itself to that one road; this is the other.
///
/// **The gate is `prq::refused_for_conflicts`, and it is shared on purpose.** Which answers are
/// this refusal is a fact about `crate::github`'s two wrappers, and the rule — match the status
/// skein itself formatted, never GitHub's prose — is written out in full at
/// `prq::it_conflicts_with_its_base`. A second copy here would be a second thing to miss.
///
/// **The words are not shared, because the reader is not the same reader.** The press says
/// "Resolve them on the branch, push, then merge", which is what to do next when your finger is on
/// the button. A stop is read by somebody who was not watching, so this says what happened (the
/// merge was refused, and nothing was merged), what follows from it (the train has stopped and will
/// not try again by itself — [`stop`] is durable and [`perform`] returns early on it), and what to
/// do (resolve, push, then the button that is actually there, which `revFlowBox` labels
/// "let it run again").
///
/// Every other status stops with GitHub's answer verbatim, as it did before. A 409 here is the
/// branch having moved under a decision this train made — real, and not this sentence.
fn conflicts_stopped_the_train(number: u64, said: String) -> String {
    match crate::prq::refused_for_conflicts(&said) {
        false => said,
        true => format!(
            "#{number} conflicts with its base, so GitHub refused the merge and nothing was \
             merged. The train has stopped here and will not try again by itself. Resolve the \
             conflicts on the branch and push, then press \"let it run again\"."
        ),
    }
}

/// The merge a PERSON presses, with the two guards the merge train has and this road did not.
/// (SKEIN-338)
///
/// **There were two merges and they were not equally safe.** The train's went out with `sha`
/// ([`merge_pr`] above) and passed [`crate::workflow::instead_of_merging_off_the_trunk`] on both
/// roads into [`crate::workflow::next`]; the cockpit's merge chip called `prq::merge`, which sent
/// `{"merge_method": …}` and nothing else — no expected head, no base check.
/// `grep -rn instead_of_merging_off_the_trunk src/` found the guard reachable from `workflow.rs`
/// and `prwork.rs` only, never from that route. And `$SKEIN_PR_WORKFLOWS` is **off** on the fleet
/// this was found on, so the guarded road was the one nobody was driving: the only merge skein
/// actually offered was the unguarded one.
///
/// What that cost, on live data: opening step 7 of a stack (base
/// `ladder/tenants-07-auth-cutover`), reading it, and pressing merge would merge step 6 into step 7
/// — SKEIN-237 reproduced by hand, from the surface built for reading pull requests.
///
/// **Here rather than in `prq`, and it is the module graph that decides.** `docs/modules.toml` has
/// `prq.depends_on` without `workflow`, and `prwork` — "the half with consequences… nothing depends
/// on THIS except the tick and the routes" — already depends on both. So the guard composes from
/// where it can see both halves, `tools/module-check.py` needs no new edge, and the thing that
/// merges pull requests stays a leaf.
///
/// **It does not consult `enabled()`, and that is not the oversight it looks like.** `perform`
/// checks the switch because a caller that forgot would be a bug that merges pull requests by
/// itself. `$SKEIN_PR_WORKFLOWS` governs skein acting **unattended**; a person with their finger on
/// the button is not that, and the fleet where the switch is off is exactly the fleet where this
/// path is the only merge there is. Refusing here would remove the merge chip from every fleet that
/// has not opted into automation, which today is every fleet there is.
///
/// **Nor does it consult `stopped()`.** A workflow stop is a durable note that the TRAIN has gone
/// as far as it can and needs a person; a person then merging by hand is that note being answered,
/// not overridden. The guards below are the ones that survive a human being certain, because they
/// are about facts rather than about policy: what you are merging, and where it lands.
///
/// The order is deliberate. Base first, then head. A stacked child is wrong to merge at *any* head,
/// so "the branch moved" would be a distraction in front of it — and the reader who re-read and
/// pressed again would get the real refusal on the second press instead of the first.
pub fn merge_by_hand(slug: &str, number: u64, seen_head: &str) -> Result<String, String> {
    // Before any request, because there is no request worth making. An empty `seen_head` means the
    // caller cannot say which commit the person was looking at, and a merge that cannot name its
    // revision is the unguarded merge this function exists to replace — "assume current" is the
    // hole, not the fallback.
    if seen_head.trim().is_empty() {
        return Err(format!(
            "skein does not know which commit of #{number} you are looking at, and will not merge \
             a revision it cannot name. Refresh the queue and read the change again."
        ));
    }
    // One request for both facts, live. Not the queue: `base_ref` moves under a stacked child the
    // moment its parent lands, and `head_sha` moves on every push, so a merge decided from a
    // sixty-second cache is a merge decided from a photograph. An `Err` stops the merge — see
    // `prq::base_and_head` for why this is the one read whose failure must not fall back.
    let (base_ref, live_head) = crate::prq::base_and_head(slug, number)?;
    // The same memoised answer `prq::queue` uses, so the two roads to a merge cannot disagree about
    // what this repository's trunk is. `""` is "not known", which is `None` and not `false` — see
    // `crate::workflow::Facts::base_is_trunk`.
    let trunk = crate::prq::trunk_of(slug);
    let base_is_trunk = match trunk.is_empty() {
        true => None,
        false => Some(base_ref == trunk),
    };
    if let Some(instead) = crate::workflow::merging_off_the_trunk(base_is_trunk) {
        // `Flag` and `Wait` mean different things to a train — one is durable and one clears itself
        // — and exactly the same thing to a person standing at the button: not this, not now. The
        // sentence is the shared one so both roads refuse in the same words. Anything this rule
        // ever grows is ALSO a refusal here: a new answer that fell through to the merge below
        // would fail open on the one act that cannot be taken back.
        let why = match &instead {
            Act::Flag(why) | Act::Wait(why) => why.clone(),
            other => crate::workflow::spell_act(other),
        };
        return Err(format!("#{number} is based on {base_ref} — {why}"));
    }
    // Checked here as well as sent as `sha`, and both are wanted. This one can say what the head
    // moved TO, which GitHub's 409 cannot; the `sha` on the wire closes the window between this
    // check and the merge, which no check up here can. Neither is redundant — one is a better
    // sentence and the other is the guarantee.
    if live_head != seen_head {
        return Err(format!(
            "the branch moved since you read it — you read {}, #{number} is now at {}. Read the \
             new code, then merge.",
            short(seen_head),
            short(&live_head)
        ));
    }
    crate::prq::merge(slug, number, seen_head)
}

/// Enough of a sha to recognise, for a sentence a person reads. `get` rather than a slice so a
/// short or empty sha is returned whole instead of panicking on a merge refusal.
pub(super) fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Delete the head branch of a pull request that has just been merged.
///
/// `head_ref` is `headRefName` as GitHub reported it — the *author's* string, not skein's — so it
/// is encoded a segment at a time rather than interpolated. A branch called `release#2` used to
/// issue `DELETE /repos/o/r/git/refs/heads/release`, because curl never puts a fragment on the
/// wire: the wrong branch deleted, and the train then reporting that it had deleted `release#2`.
///
/// [`crate::github::path_segments`] and not [`crate::github::path_segment`], because a ref
/// legitimately contains slashes (`feat/x`) and GitHub's refs endpoint takes them as path
/// separators — so `%2F` there would 404 every branch anybody has ever named after a topic.
pub(super) fn delete_branch(
    slug: &str,
    head_ref: &str,
    token: &crate::secret::Secret,
) -> Result<(), String> {
    let head_ref = crate::github::path_segments(head_ref);
    crate::github::send_json(
        "DELETE",
        &format!(
            "{}/git/refs/heads/{head_ref}",
            crate::github::repo_path(slug)
        ),
        token,
        &serde_json::json!({}),
    )
    .map(|_| ())
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

    /// **A branch name reaches GitHub as one path segment, and a `#` in it does not truncate the
    /// URL into a different branch.**
    ///
    /// `head_ref` is `headRefName` as GitHub reports it, so its characters are the pull request
    /// author's choice, not skein's. Git forbids `~ ^ : ? * [ \` in a ref and allows `#` — and
    /// curl never puts a fragment on the wire, so `DELETE …/heads/release#2` used to arrive at
    /// GitHub as `DELETE …/heads/release`. That deletes a branch nobody asked about, on a
    /// repository where `release` exists, and the train then says it deleted `release#2`.
    ///
    /// The ordinary half is the half that makes the first one worth anything: a ref really does
    /// contain slashes, and those must stay separators or every topic branch 404s. The two
    /// together are why this is a *segment* encoder applied per part, and not `encode(head_ref)`.
    #[test]
    fn a_branch_name_reaches_github_as_one_path_segment() {
        let _env = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let deleted = |head_ref: &str| {
            heard.lock().unwrap().clear();
            let subject = Subject {
                repo_id: "demo",
                slug: "acme/thing",
                number: 41,
                head_sha: "abc123",
                head_ref,
                reading: None,
            };
            let out = perform(
                &subject,
                &flow(),
                &chosen(Act::Merge(Merge {
                    how: MergeAs::Squash,
                    delete_branch: true,
                })),
                &fixture_token(),
            );
            assert!(matches!(out, Outcome::Did(_)), "{out:?}");
            let said = heard.lock().unwrap().clone();
            said.iter()
                .find(|s| s.starts_with("DELETE /repos/acme/thing/git/refs/heads/"))
                .unwrap_or_else(|| panic!("no branch was deleted: {said:?}"))
                .clone()
        };

        let hostile = deleted("release#1");
        assert!(
            hostile.starts_with("DELETE /repos/acme/thing/git/refs/heads/release%231 "),
            "a `#` in a branch name still steers the request at another branch: {hostile}"
        );

        let ordinary = deleted("feat/nested/name");
        assert!(
            ordinary.starts_with("DELETE /repos/acme/thing/git/refs/heads/feat/nested/name "),
            "a topic branch's slashes were encoded, so every branch with one now 404s: {ordinary}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// A merge carries the head skein decided on, and the branch goes after the merge.
    #[test]
    fn a_merge_names_the_commit_it_was_decided_about() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc123"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            })),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Did(_)), "{out:?}");
        let said = heard.lock().unwrap().clone();
        let merge = said
            .iter()
            .find(|s| s.contains("/merge"))
            .unwrap_or_else(|| panic!("nothing was merged: {said:?}"));
        assert!(
            merge.starts_with("PUT /repos/acme/thing/pulls/41/merge"),
            "{merge}"
        );
        assert!(merge.contains("\"merge_method\":\"squash\""), "{merge}");
        // The head it decided about. Without it GitHub merges whatever is there now — which is the
        // one thing a workflow must never do, because the decision was made about something else.
        assert!(
            merge.contains("\"sha\":\"abc123\""),
            "the merge did not name the commit the decision was made about: {merge}"
        );
        // And the branch goes AFTER the merge, never before: deleting the head branch of a pull
        // request that is still open closes it instead of shipping it.
        let deleted = said.iter().position(|s| s.contains("git/refs/heads/feat"));
        let merged = said.iter().position(|s| s.contains("/merge"));
        assert!(
            deleted > merged,
            "the branch was deleted before the merge landed: {said:?}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// A GitHub whose pull request can be posed: a base, a head, and whether the repository will
    /// say what its default branch is. Records every request line with its body.
    ///
    /// Separate from [`github`] above because these tests are about what skein REFUSES, and a stub
    /// that answers everything the same way cannot tell a merge that was refused from one that was
    /// attempted and failed. The three answers here are the three facts a merge turns on.
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
                        // which `trunk_of` reads as `""` — the blindness
                        // `Facts::base_is_trunk: None` stands for.
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
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        // The switch is OFF for the whole table, on purpose: `$SKEIN_PR_WORKFLOWS` governs skein
        // acting unattended, and the fleet where it is off is precisely the fleet where this is the
        // only merge there is. A guard that only ran with automation on would guard nothing.
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);

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

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
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
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
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
        // accusing the pull request of being stacked — `Facts::base_is_trunk`'s `None` versus
        // `Some(false)`, spent here exactly as `instead_of_merging_off_the_trunk` spends it.
        pose("main", "abc1234def", None);
        let why = merge_by_hand("acme/thing", 41, "abc1234def").unwrap_err();
        assert!(
            why.contains("default branch") && !why.contains("not based on the trunk"),
            "an unknown trunk was reported as the pull request's fault: {why}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
        }
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
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
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

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
        }
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
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_MERGE_METHOD");
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
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

        for key in [
            "SKEIN_HOME",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
            "SKEIN_PR_WORKFLOWS",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_trunks();
        crate::prq::forget_host_token();
    }

    /// **Only a 405 that names conflicts stops the train in skein's words, and it never says
    /// 405.** (SKEIN-423)
    ///
    /// The same two directions `prq`'s
    /// `only_a_405_naming_conflicts_is_reported_as_conflicts_with_the_base` pins for the press, on
    /// the train's own sentence: a 409, a 422 or a rate limit whose body happens to carry the word
    /// "conflict" must stop with GitHub's answer verbatim, and a 405 for a draft or a blocking rule
    /// must too — a stop that sends somebody to resolve conflicts that are not there is worse than
    /// one that quotes a status, because they will go and look.
    ///
    /// Written here rather than left to the shared gate: the gate says WHICH answers, and this test
    /// is about what a stopped train SAYS, which is the half that is this module's.
    #[test]
    fn only_a_405_naming_conflicts_stops_the_train_in_skein_s_words() {
        // GitHub's own words for a conflicted merge, in both shapes `crate::github` wraps a non-2xx
        // in, across the statuses a merge actually draws.
        for status in [401, 403, 404, 405, 409, 422, 500, 502] {
            for said in [
                format!("GitHub said {status}: Pull Request has merge conflicts"),
                format!("GitHub answered {status}: <html>merge conflicts</html>"),
            ] {
                let out = conflicts_stopped_the_train(41, said.clone());
                let translated = out != said;
                assert_eq!(
                    translated,
                    status == 405,
                    "status {status} was {} translated into a stop about conflicts: {out}",
                    match translated {
                        true => "wrongly",
                        false => "not",
                    }
                );
                if translated {
                    assert!(
                        out.contains("conflicts with its base") && out.contains("#41"),
                        "the stop lost the pull request or what is wrong with it: {out}"
                    );
                    // What a stop has to carry that a press's refusal does not: it is read by
                    // somebody who was not watching, so it has to say that the merge did not
                    // happen, that nothing is going to happen next, and what to press.
                    assert!(
                        out.contains("nothing was merged"),
                        "the reader was not told whether the merge landed: {out}"
                    );
                    assert!(
                        out.contains("will not try again"),
                        "the reader was not told the train has given up until they act: {out}"
                    );
                    assert!(
                        out.contains("let it run again"),
                        "the stop names no way out of itself — `revFlowBox`'s button is the one \
                         thing in front of this reader: {out}"
                    );
                    assert!(
                        !out.contains("405"),
                        "the raw status survived into the reader's sentence: {out}"
                    );
                }
            }
        }

        // A 405 that is not about conflicts. GitHub answers every unmergeable pull request with
        // this status, and only one of the reasons is fixed by resolving anything.
        for said in [
            "GitHub said 405: Pull Request is not mergeable".to_string(),
            "GitHub said 405: Base branch was modified".to_string(),
            "GitHub answered 405: <html>no</html>".to_string(),
            "GitHub answered 405 with an empty body".to_string(),
        ] {
            assert_eq!(
                conflicts_stopped_the_train(41, said.clone()),
                said,
                "a 405 that says nothing about conflicts stopped the train as a conflict"
            );
        }

        // Not a status at all, and the word appearing somewhere it is not one.
        for said in [
            "GitHub sent nothing at all".to_string(),
            "the 405 in this sentence is not a status, and neither is this conflict".to_string(),
        ] {
            assert_eq!(
                conflicts_stopped_the_train(41, said.clone()),
                said,
                "an answer that was not a 405 stopped the train as conflicts with the base"
            );
        }
    }

    /// **A train stopped by conflicts says so where the stop is read.** (SKEIN-423)
    ///
    /// Driven through [`perform`] rather than against `conflicts_stopped_the_train` directly,
    /// for the reason `a_merge_refused_for_conflicts_says_so_in_skein_s_words` gives about the
    /// press: the translation and the act are joined by one `map_err` in [`merge_pr`], and a unit
    /// test on the function proves nothing about that wire — delete the `map_err` and the test
    /// above stays green. What is asserted is the thing a person actually reads, which is not the
    /// return value but the stop `revFlowBox` draws, so [`stopped`] is read back from the file.
    #[test]
    fn a_train_stopped_by_conflicts_says_so_in_skein_s_words() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        // `merge_world` and not `github(405)`: this turns on GitHub's real 405 BODY, and the
        // blanket stub answers every path with `{"merged":true}`, which carries no `message` and
        // so cannot pose the answer under test.
        let (api, world, heard) = merge_world();
        std::env::set_var("SKEIN_GITHUB_API", &api);
        world.lock().unwrap().3 = 405;

        let out = perform(
            &subject("abc123"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            })),
            &fixture_token(),
        );
        let why = match &out {
            Outcome::Stopped(why) => why.clone(),
            other => panic!("a 405 for conflicts was not a stop: {other:?}"),
        };
        // The merge was actually attempted. Without this the test would also pass on a stop written
        // by a guard that refused before ever asking GitHub, which is a different sentence about a
        // different problem.
        assert!(
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains("/pulls/41/merge")),
            "the conflict test never reached the merge, so it proves nothing about the 405"
        );
        // What `revFlowBox` draws under **Stopped.** — read back from the file, not from `out`,
        // because the file is what outlives the poll and is what somebody reads later.
        let filed = stopped("demo", 41).expect("the stop was not written down");
        assert_eq!(filed, why, "the stop filed is not the stop reported");
        assert!(
            filed.contains("conflicts with its base") && filed.contains("#41"),
            "a 405 for conflicts was filed in GitHub's words instead of the reader's: {filed}"
        );
        assert!(
            filed.contains("nothing was merged") && filed.contains("let it run again"),
            "the stop named the problem without saying what did not happen or what to press: \
             {filed}"
        );
        assert!(
            !filed.contains("405") && !filed.contains("GitHub said"),
            "the raw status reached the reader: {filed}"
        );
        // And the step that decided it is still on the front of the sentence — the translation
        // replaces GitHub's words, not `perform`'s attribution.
        assert!(
            filed.contains("ship-mine") && filed.contains("step 4"),
            "the stop no longer names the step that decided it: {filed}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_trunks();
    }

    /// A rebase goes through GraphQL, names the head, and says what it may have cost.
    #[test]
    fn a_rebase_asks_graphql_and_says_what_it_may_have_cost() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc123"),
            &flow(),
            &chosen(Act::UpdateBranch(Update::Rebase)),
            &fixture_token(),
        );
        let said = heard.lock().unwrap().clone();
        let call = said
            .iter()
            .find(|s| s.contains("/graphql"))
            .unwrap_or_else(|| panic!("nothing asked GraphQL: {said:?}"));
        // REST cannot rebase at all — it takes expected_head_sha and merges. This is the only way
        // to ask, and it is what `gh pr update-branch --rebase` does. See docs/pr-workflow.md.
        assert!(call.contains("updatePullRequestBranch"), "{call}");
        assert!(
            call.contains("REBASE"),
            "the rebase was sent as a merge: {call}"
        );
        assert!(
            call.contains("abc123"),
            "the rebase did not name the head it was deciding about: {call}"
        );
        // And the sentence tells the truth about the approval, which GitHub's own setting decides.
        match out {
            Outcome::Did(what) => assert!(
                what.contains("approving again"),
                "a rebase that may have dismissed the approval said nothing about it: {what}"
            ),
            other => panic!("{other:?}"),
        }

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// **A label reaches GitHub as ONE encoded path segment, `/` and `?` included** (SKEIN-633).
    ///
    /// The single-segment half of the contract, and the half a slug and a ref cannot demonstrate:
    /// both of those are legitimately more than one segment, so their slashes have to stay
    /// separators. A label's must not. GitHub allows a space, a `/` and a `?` in a label name, and
    /// `DELETE …/labels/urgent?now/later` interpolated raw is not a badly-formatted request about
    /// that label — it is a well-formed request about a *different* one, `urgent`, with `now/later`
    /// read as a query string.
    ///
    /// **What would make this fail:** interpolating `label` instead of encoding it. Asserted
    /// against the line the stub RECEIVED rather than one built here, so a `format!` that is wrong
    /// in both places cannot pass it.
    #[test]
    fn a_label_reaches_github_as_one_encoded_segment() {
        let _env = crate::testutil::env_lock();
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let removed = |label: &str| {
            heard.lock().unwrap().clear();
            let out = remove_label("acme/thing", 41, label, &fixture_token());
            let said = heard.lock().unwrap().clone();
            let line = said
                .iter()
                .find(|s| s.starts_with("DELETE "))
                .unwrap_or_else(|| {
                    panic!("nothing was sent about {label:?} — curl refused the URL: {out:?}")
                })
                .clone();
            asked(&line).to_string()
        };

        assert_eq!(
            removed("urgent?now/later"),
            "/repos/acme/thing/issues/41/labels/urgent%3Fnow%2Flater",
            "a `/` and a `?` in a label were not one segment"
        );
        // A space is the ordinary case and the one that made this encoder exist. It is also the
        // one curl refuses outright, so unencoded it fails the closure above rather than this
        // assertion — either way the label did not reach GitHub.
        assert_eq!(
            removed("needs review"),
            "/repos/acme/thing/issues/41/labels/needs%20review",
            "a space in a label was not encoded"
        );

        std::env::remove_var("SKEIN_GITHUB_API");
    }

    /// **A repository name is two segments and neither of them can reshape the path** (SKEIN-633).
    ///
    /// `slug` was the value every builder in this crate interpolated raw, on the reasoning that
    /// skein builds it — but skein does not: `update::slug_of` and `gitgate::slug_from_url` cut it
    /// out of whatever URL a person registered, and both check that it has two halves and nothing
    /// at all about what is inside them. `acme/thing?everything=else` interpolated raw makes
    /// `GET /repos/acme/thing?everything=else/pulls/41`, which asks GitHub about the repository
    /// `acme/thing` with a query string attached — a real repository, a 200, and an answer about
    /// something nobody asked for.
    ///
    /// **What would make this fail:** interpolating `slug` into the `format!` instead of calling
    /// `repo_path`, which is the shape every builder in this crate had before. The `/` between
    /// owner and name staying a separator is the other half: it is why the encoder is per-segment
    /// and `path_segment(slug)` would be wrong.
    #[test]
    fn a_repository_name_is_two_segments_and_neither_can_reshape_the_path() {
        let _env = crate::testutil::env_lock();
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = node_id("acme/thing?everything=else", 41, &fixture_token());
        assert!(out.is_ok(), "{out:?}");

        let said = heard.lock().unwrap().clone();
        let line = said
            .iter()
            .find(|s| s.starts_with("GET "))
            .unwrap_or_else(|| panic!("nothing was asked: {said:?}"))
            .clone();
        assert_eq!(
            asked(&line),
            "/repos/acme/thing%3Feverything%3Delse/pulls/41",
            "a `?` in a repository name reshaped the request: {line}"
        );
        std::env::remove_var("SKEIN_GITHUB_API");
    }

    /// **A repository name made of dots cannot climb out of `/repos`** (SKEIN-633).
    ///
    /// The one rule in [`crate::github::path_segment`] that is not about a single byte. `.` is
    /// unreserved, so a per-character escape leaves `..` exactly as it found it — and then
    /// RFC 3986 §5.2.4 removes it, which curl does itself before the request leaves the process.
    /// `/repos/acme/../../pulls/41` is normalised to `/pulls/41` on the wire: a path with a
    /// different prefix, asked with the caller's credential, and nothing in the code that built it
    /// looks wrong.
    ///
    /// **Two things defend this and the test refuses both of their absences.** Escaping alone is
    /// not enough: curl decodes `%2E` back to `.` before it normalises, so this assertion failed
    /// with `path_segment` already escaping the dots — measured, not reasoned about — and only
    /// passed once `call` also sent `--path-as-is`. Delete either the all-dots branch of
    /// `path_segment` or that flag and the stub records `GET /pulls/41`, which is why the second
    /// assertion names that exact string.
    #[test]
    fn a_repository_name_made_of_dots_cannot_climb_out_of_the_repos_path() {
        let _env = crate::testutil::env_lock();
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let _ = node_id("acme/../..", 41, &fixture_token());

        let said = heard.lock().unwrap().clone();
        let line = said
            .iter()
            .find(|s| s.starts_with("GET "))
            .unwrap_or_else(|| panic!("nothing was asked: {said:?}"))
            .clone();
        assert_eq!(
            asked(&line),
            "/repos/acme/%2E%2E/%2E%2E/pulls/41",
            "a dotted repository name walked up the path: {line}"
        );
        assert_ne!(
            asked(&line),
            "/pulls/41",
            "curl squashed the dot segments and the request left `/repos` entirely: {line}"
        );
        std::env::remove_var("SKEIN_GITHUB_API");
    }

    /// The path out of a recorded request line — `"GET /x/y HTTP/1.1 <body>"` is what
    /// [`crate::prwork::testkit::github`] stores, and the middle word is what went on the wire.
    fn asked(line: &str) -> &str {
        line.split(' ').nth(1).unwrap_or_default()
    }
}
