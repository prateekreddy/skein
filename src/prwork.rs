//! Doing what a workflow decided — the half with consequences.
//!
//! [`crate::workflow`] is a vocabulary and a decision and has no way to touch anything. This is the
//! part that merges pull requests and deletes branches on its own, which is what the owner asked
//! for: *"merge automatically and delete the branch"*, reaffirmed after the risk was put to them.
//!
//! Three things make that recoverable rather than a decision nobody can see, and they exist BEFORE
//! any action can fire rather than after the first surprise.
//!
//! # The switch
//!
//! [`enabled`] is off by default and one key turns every workflow in the fleet off, reachable
//! without the cockpit. Not a per-workflow flag: the moment somebody wants this stopped, they want
//! it stopped, and hunting through several places for the one that is still on is not a thing to
//! ask of a person who has just watched something merge.
//!
//! # The record
//!
//! Every action reaches the host's audit log — the one skein does not own, through the warden, the
//! same sink a box's own lifecycle events use. With the authority on it: which workflow, which
//! step. "Skein merged #41" is a fact; "*ship-mine*, step 4, merged #41" is one somebody can act
//! on, because it says which line to change so it does not happen again.
//!
//! # No blind retries
//!
//! An action that fails because the world moved is not a transient error. A merge that 409s because
//! somebody pushed while skein was deciding must not be attempted again on the next poll: the same
//! decision was made from facts that are now provably stale, and a loop that re-tries it is a loop
//! that eventually wins the race. So a failure STOPS that pull request's workflow, in writing, with
//! the reason — and it stays stopped until a person clears it.
//!
//! That is deliberately stronger than "retry a few times". The actions here are outward-facing and
//! most are hard to undo; the cost of stopping too eagerly is that somebody presses a button, and
//! the cost of retrying too eagerly is a merge nobody asked for.

use crate::workflow::{Act, Chosen, MergeAs, Update, Workflow};
use std::path::PathBuf;

/// May skein act on pull requests at all?
///
/// **Off unless it is switched on.** Every other default in skein leans toward showing you more;
/// this one leans the other way, because the thing being defaulted is not a reading but a merge.
/// `$SKEIN_PR_WORKFLOWS=on|off` overrides, so it can be turned off from the command line that
/// starts the server — no cockpit, no config edit, on a fleet that is doing something you want
/// stopped now.
pub fn enabled() -> bool {
    match std::env::var("SKEIN_PR_WORKFLOWS").ok().as_deref() {
        Some("on" | "1" | "true" | "yes") => true,
        Some("off" | "0" | "false" | "no") => false,
        _ => crate::config::load_config().pr_workflows,
    }
}

/// What happened when skein tried to take a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It happened. The string is what the audit and the row say.
    Did(String),
    /// Nothing to do — the step was `wait`, which is an answer and not an absence.
    Waited(String),
    /// The workflow has stopped on this pull request, and will not act again until a person clears
    /// it. Either because a step said `flag`, or because an action failed.
    Stopped(String),
}

/// Where a repo's stopped pull requests are written down.
///
/// Beside the review state for that repo, and on the host: this is skein's own memory of a decision
/// it made, not something a box should be able to edit.
fn stops_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-stops.json")
}

/// Why this pull request's workflow is stopped, if it is.
pub fn stopped(repo_id: &str, number: u64) -> Option<String> {
    read_stops(repo_id).remove(&number.to_string())
}

fn read_stops(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(stops_path(repo_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_stops(
    repo_id: &str,
    stops: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let path = stops_path(repo_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(stops).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
}

/// Stop this pull request's workflow, and say why.
pub fn stop(repo_id: &str, number: u64, why: &str) {
    let mut stops = read_stops(repo_id);
    stops.insert(number.to_string(), why.to_string());
    if let Err(e) = write_stops(repo_id, &stops) {
        // Louder than most best-effort writes: if this does not land, the next poll re-attempts an
        // action that has already failed once, which is the loop the whole rule exists to prevent.
        eprintln!("skein: could not write down that #{number}'s workflow stopped ({e}) — it may be attempted again");
    }
}

/// Let it run again. What a person does after fixing whatever the reason was.
pub fn clear(repo_id: &str, number: u64) {
    let mut stops = read_stops(repo_id);
    if stops.remove(&number.to_string()).is_some() {
        let _ = write_stops(repo_id, &stops);
    }
}

/// Take one step, or say why not.
///
/// `head_sha` is passed to every action that changes the pull request, so GitHub refuses rather
/// than acts if somebody pushed between skein deciding and skein acting. That is the same rule as
/// the anchor on a box: prove the thing is what you think before touching it.
pub struct Subject<'a> {
    /// The repo as skein knows it, which is where the stop is written down.
    pub repo_id: &'a str,
    /// `owner/name` as GitHub knows it.
    pub slug: &'a str,
    pub number: u64,
    /// The commit the decision was made about. Every action that changes the pull request carries
    /// it, so GitHub refuses rather than acts if somebody pushed in between.
    pub head_sha: &'a str,
    pub head_ref: &'a str,
}

pub fn perform(pr: &Subject, flow: &Workflow, chosen: &Chosen, token: &str) -> Outcome {
    let (repo_id, slug, number, head_sha, head_ref) =
        (pr.repo_id, pr.slug, pr.number, pr.head_sha, pr.head_ref);
    // The switch is read here rather than only by the caller, because this is the function with the
    // consequences. A caller that forgot to check would be a bug that merges pull requests.
    if !enabled() {
        return Outcome::Stopped(
            "workflows are switched off for this fleet (Settings, or $SKEIN_PR_WORKFLOWS=on)"
                .into(),
        );
    }
    if let Some(why) = stopped(repo_id, number) {
        return Outcome::Stopped(why);
    }
    // The authority for everything below: which workflow, which step. It goes in the audit and on
    // the row, so a person can find the line that decided this.
    let by = format!("{} step {}", flow.name, chosen.step + 1);

    let done = match &chosen.act {
        Act::Wait(why) => return Outcome::Waited(why.clone()),
        Act::Flag(why) => {
            // A flag is the workflow saying it has gone as far as it can. Written down like any
            // other stop so the next poll does not simply say it again.
            stop(repo_id, number, why);
            return Outcome::Stopped(why.clone());
        }
        Act::AddLabel(label) => add_label(slug, number, label, token)
            .map(|_| format!("added the label {label:?} to #{number}")),
        Act::RemoveLabel(label) => remove_label(slug, number, label, token)
            .map(|_| format!("removed the label {label:?} from #{number}")),
        Act::UpdateBranch(how) => update_branch(slug, number, head_sha, *how, token).map(|_| {
            let how = match how {
                Update::Rebase => "rebase",
                Update::Merge => "merge",
            };
            // Said plainly, because the owner asked for a rebase that keeps approvals and GitHub
            // does not offer one. Whether the approval survived is the repository's setting, not
            // skein's doing — see docs/pr-workflow.md.
            format!(
                "updated #{number} with its base by {how} — if this repository dismisses stale \
                 approvals, that approval is now gone and it needs approving again"
            )
        }),
        Act::Merge(merge) => merge_pr(slug, number, head_sha, merge.how, token).and_then(|_| {
            match merge.delete_branch {
                false => Ok(format!("merged #{number}")),
                // Only after the merge landed. A branch deleted before it is merged closes the pull
                // request instead of shipping it.
                true => delete_branch(slug, head_ref, token)
                    .map(|_| format!("merged #{number} and deleted {head_ref}"))
                    // The merge DID happen. Reporting the whole step as failed would be a lie, and
                    // a retry would try to merge an already-merged pull request.
                    .or_else(|e| {
                        Ok(format!(
                            "merged #{number}, but {head_ref} is still there: {e}"
                        ))
                    }),
            }
        }),
    };

    match done {
        Ok(what) => {
            crate::warden_client::reported(&format!("pr-workflow:{}", flow.name), &what, &by);
            Outcome::Did(what)
        }
        Err(why) => {
            // Not a retry. See the module note: the decision was made from facts this failure has
            // just proved stale, and the next poll would make the same one.
            let why = format!("{by} could not be done: {why}");
            stop(repo_id, number, &why);
            crate::warden_client::reported(
                &format!("pr-workflow:{}", flow.name),
                &format!("stopped on #{number}"),
                &why,
            );
            Outcome::Stopped(why)
        }
    }
}

fn add_label(slug: &str, number: u64, label: &str, token: &str) -> Result<(), String> {
    crate::github::send_json(
        "POST",
        &format!("/repos/{slug}/issues/{number}/labels"),
        token,
        &serde_json::json!({ "labels": [label] }),
    )
    .map(|_| ())
}

fn remove_label(slug: &str, number: u64, label: &str, token: &str) -> Result<(), String> {
    // A label may contain a space or a slash. Encoded rather than interpolated raw: a label called
    // `needs review` would otherwise produce a path GitHub answers 404 for, and the workflow would
    // stop on a step that was perfectly well written.
    let label: String = label
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    crate::github::send_json(
        "DELETE",
        &format!("/repos/{slug}/issues/{number}/labels/{label}"),
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
fn update_branch(
    slug: &str,
    number: u64,
    head_sha: &str,
    how: Update,
    token: &str,
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
fn node_id(slug: &str, number: u64, token: &str) -> Result<String, String> {
    let pr = crate::github::get_json(&format!("/repos/{slug}/pulls/{number}"), token)?;
    pr.get("node_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("GitHub did not say what #{number}'s node id is"))
}

fn merge_pr(
    slug: &str,
    number: u64,
    head_sha: &str,
    how: MergeAs,
    token: &str,
) -> Result<(), String> {
    let method = match how {
        MergeAs::Squash => "squash",
        MergeAs::Merge => "merge",
        MergeAs::Rebase => "rebase",
    };
    crate::github::send_json(
        "PUT",
        &format!("/repos/{slug}/pulls/{number}/merge"),
        token,
        // `sha` is the head skein decided on. GitHub answers 409 if the branch has moved since,
        // which is exactly the answer wanted: somebody pushed, and the decision to merge was made
        // about code that is no longer what would be merged.
        &serde_json::json!({ "merge_method": method, "sha": head_sha }),
    )
    .map(|_| ())
}

fn delete_branch(slug: &str, head_ref: &str, token: &str) -> Result<(), String> {
    crate::github::send_json(
        "DELETE",
        &format!("/repos/{slug}/git/refs/heads/{head_ref}"),
        token,
        &serde_json::json!({}),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::Merge;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// A GitHub that records what skein sent it, and answers however the test says.
    ///
    /// Every assertion here is about a request that CHANGES somebody's repository, so what is
    /// checked is the wire: the method, the path, and the body. A doer tested through its own
    /// return value would pass while merging with the wrong method, or without the head it decided
    /// on — which is the failure that matters, because that one merges a commit nobody looked at.
    fn github(status: u16) -> (String, Arc<Mutex<Vec<String>>>) {
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

    fn flow() -> Workflow {
        crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"ship-mine","steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap()
        .remove(0)
    }

    fn subject(head_sha: &str) -> Subject<'_> {
        Subject {
            repo_id: "demo",
            slug: "acme/thing",
            number: 41,
            head_sha,
            head_ref: "feat",
        }
    }

    fn chosen(act: Act) -> Chosen {
        Chosen { step: 3, act }
    }

    /// Nothing happens on a fleet that has not switched this on.
    ///
    /// The first assertion of the feature, and the one worth being unable to break: this merges
    /// pull requests. A default that acts because a config file was missing is not one anybody
    /// would trust twice — so the check is inside the function with the consequences, not only in
    /// whoever calls it.
    #[test]
    fn a_fleet_that_has_not_switched_this_on_does_nothing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            })),
            "t",
        );
        assert!(
            matches!(out, Outcome::Stopped(_)),
            "it acted with the switch off: {out:?}"
        );
        assert!(
            heard.lock().unwrap().is_empty(),
            "a fleet with workflows off still reached GitHub: {:?}",
            heard.lock().unwrap()
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API"] {
            std::env::remove_var(key);
        }
    }

    /// A merge carries the head skein decided on, and the branch goes after the merge.
    #[test]
    fn a_merge_names_the_commit_it_was_decided_about() {
        let _g = crate::testutil::env_lock();
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
            "t",
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

    /// An action that failed is not tried again, and the reason is kept.
    ///
    /// A merge 409s when somebody pushed while skein was deciding. Retrying is not resilience: the
    /// decision was made from facts that failure has just proved stale, so the same decision would
    /// be made again, and a loop like that eventually wins the race.
    #[test]
    fn an_action_that_failed_stops_the_workflow_rather_than_looping() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(409);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let act = Act::Merge(Merge {
            how: MergeAs::Squash,
            delete_branch: false,
        });
        let out = perform(&subject("abc"), &flow(), &chosen(act.clone()), "t");
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("ship-mine") && why.contains("step 4"),
                "the stop must name the step that decided it: {why}"
            ),
            other => panic!("a 409 was not treated as a stop: {other:?}"),
        }
        assert!(
            stopped("demo", 41).is_some(),
            "the stop was not written down"
        );

        // The next poll. It must not reach GitHub at all.
        let before = heard.lock().unwrap().len();
        let out = perform(&subject("abc"), &flow(), &chosen(act), "t");
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");
        assert_eq!(
            heard.lock().unwrap().len(),
            before,
            "a stopped workflow tried the same failing action again"
        );

        // And a person can let it run again.
        clear("demo", 41);
        assert_eq!(stopped("demo", 41), None);

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// A rebase goes through GraphQL, names the head, and says what it may have cost.
    #[test]
    fn a_rebase_asks_graphql_and_says_what_it_may_have_cost() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc123"),
            &flow(),
            &chosen(Act::UpdateBranch(Update::Rebase)),
            "t",
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
}
