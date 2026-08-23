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

/// What a workflow sees, built from what GitHub said.
///
/// The one place the translation happens. Two facts about a pull request are easy to confuse and
/// this is where they are kept apart:
///
/// * **`approved` is the REPOSITORY's verdict**, not yours. `Pr::my_review` is what you last said,
///   and being one approver of six is not the same fact as the pull request being approved. A
///   workflow that merges must read the first one.
/// * **`mergeable` stays three-valued.** GitHub says UNKNOWN for a while after every push, and
///   `workflow::holds` turns on unknown being neither mergeable nor not-mergeable — so flattening
///   it here would undo that one layer below the test that protects it.
pub fn facts_of(pr: &crate::prq::Pr, viewer: &str) -> crate::workflow::Facts {
    crate::workflow::Facts {
        approved: pr.review_decision == "APPROVED",
        changes_requested: pr.review_decision == "CHANGES_REQUESTED",
        labels: pr.labels.clone(),
        checks: pr.checks.clone(),
        mergeable: pr.mergeable,
        draft: pr.draft,
        mine: !viewer.is_empty() && pr.author.eq_ignore_ascii_case(viewer),
    }
}

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

/// Which workflow a pull request carries, and how it came to carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carries {
    /// Somebody chose this one on the row. Outranks every rule, in both directions.
    Assigned(String),
    /// A workflow's own `matches` claimed it.
    Matched(String),
    /// Excluded by hand — the row said "no workflow", and no rule may override that.
    ///
    /// A distinct answer from [`Carries::Nothing`] and the whole reason assignment is a
    /// three-valued thing: with the tick sweeping every repo in the registry, "not this one" has to
    /// be sayable about a single pull request. Otherwise the only way to exclude one is to edit the
    /// rule for everybody.
    Excluded,
    /// No rule claims it and nobody assigned one.
    Nothing,
}

impl Carries {
    /// The workflow's name, where there is one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Carries::Assigned(name) | Carries::Matched(name) => Some(name),
            Carries::Excluded | Carries::Nothing => None,
        }
    }
}

fn assign_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-assigned.json")
}

fn read_assigned(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(assign_path(repo_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Put a workflow on one pull request, or take it off.
///
/// `name` empty means **excluded** — not "no opinion". Clearing the choice entirely is
/// [`unassign`], which lets the rules speak again. Three states, because with a rule sweeping every
/// repo in the fleet, "leave this one alone" is a thing somebody has to be able to say.
pub fn assign(repo_id: &str, number: u64, name: &str) -> Result<(), String> {
    let mut all = read_assigned(repo_id);
    all.insert(number.to_string(), name.to_string());
    write_assigned(repo_id, &all)
}

/// Forget any choice made on this pull request, and let the rules decide again.
pub fn unassign(repo_id: &str, number: u64) -> Result<(), String> {
    let mut all = read_assigned(repo_id);
    all.remove(&number.to_string());
    write_assigned(repo_id, &all)
}

fn write_assigned(
    repo_id: &str,
    all: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let path = assign_path(repo_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
}

/// What a choice made on the row means, in one place.
///
/// The mapping lived in the route and grew a bug there within an hour of being written: "let it run
/// again" sent no name, "no name" meant *forget the choice*, and so clearing a stop quietly took the
/// workflow off the pull request as well — one button doing a second thing nobody asked for. It is
/// here now because it is a rule about what an assignment IS, and a route is where rules go to be
/// untested.
///
/// - `name: Some("x")` — put x on it.
/// - `name: Some("")` — leave this one out, and let no rule claim it.
/// - `unassign` — forget the choice; the rules speak for it again.
/// - neither — **change nothing**. `clear_stop` alone is not a statement about what governs it.
pub fn apply(
    repo_id: &str,
    number: u64,
    name: Option<&str>,
    unassign_it: bool,
    clear_stop: bool,
) -> Result<(), String> {
    if clear_stop {
        clear(repo_id, number);
    }
    match (name, unassign_it) {
        (Some(name), _) => assign(repo_id, number, name),
        (None, true) => unassign(repo_id, number),
        (None, false) => Ok(()),
    }
}

/// Which workflow governs this pull request.
///
/// The choice on the row wins over every rule. Where there is none, the first workflow whose
/// `matches` claims it does — and a workflow with no `matches` claims nothing, ever
/// ([`crate::workflow::claims`]).
///
/// An assignment naming a workflow that no longer exists is [`Carries::Nothing`] rather than an
/// error: the file it named was edited, and the honest thing is to act on nothing rather than to
/// guess which of the remaining ones was meant. The row says so.
pub fn carries(
    repo_id: &str,
    number: u64,
    facts: &crate::workflow::Facts,
    flows: &[Workflow],
) -> Carries {
    match read_assigned(repo_id).get(&number.to_string()) {
        Some(name) if name.is_empty() => return Carries::Excluded,
        Some(name) => {
            return match flows.iter().any(|f| &f.name == name) {
                true => Carries::Assigned(name.clone()),
                false => Carries::Nothing,
            }
        }
        None => {}
    }
    flows
        .iter()
        .find(|flow| crate::workflow::claims(flow, facts))
        .map(|flow| Carries::Matched(flow.name.clone()))
        .unwrap_or(Carries::Nothing)
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

/// What a workflow would do to one pull request, and why — without doing any of it.
///
/// The dry run the owner asked to see before trusting this, and the same [`crate::workflow::next`]
/// the tick uses. Deliberately the same function: a preview computed a second way is a preview that
/// can disagree with what happens, and the whole point of showing it is that it cannot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Standing {
    /// The workflow's name, or empty.
    pub workflow: String,
    /// `assigned` | `matched` | `excluded` | `none` — how it came to carry that workflow, because
    /// "you chose this" and "a rule chose this" are different things to see on a row.
    pub how: String,
    /// The step it would take next, spelled as it is written in the file. Empty when nothing
    /// applies, which is what a healthy workflow says most of the time.
    pub next: String,
    /// Which step that is, 1-based, for a row that wants to say "waiting on step 2".
    pub step: usize,
    /// Why it is stopped, if it is. A stopped workflow does nothing until this is cleared.
    pub stopped: String,
}

/// Everything the cockpit needs to draw one pull request's workflow state.
pub fn standing(
    repo_id: &str,
    number: u64,
    facts: &crate::workflow::Facts,
    flows: &[Workflow],
) -> Standing {
    let carried = carries(repo_id, number, facts, flows);
    let how = match &carried {
        Carries::Assigned(_) => "assigned",
        Carries::Matched(_) => "matched",
        Carries::Excluded => "excluded",
        Carries::Nothing => "none",
    };
    let flow = carried
        .name()
        .and_then(|name| flows.iter().find(|f| f.name == name));
    let chosen = flow.and_then(|flow| crate::workflow::next(flow, facts));
    Standing {
        workflow: carried.name().unwrap_or_default().to_string(),
        how: how.to_string(),
        next: chosen
            .as_ref()
            .map(|c| crate::workflow::spell_act(&c.act))
            .unwrap_or_default(),
        step: chosen.as_ref().map(|c| c.step + 1).unwrap_or(0),
        stopped: stopped(repo_id, number).unwrap_or_default(),
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

    /// Which workflow governs a pull request, and who gets the last word.
    ///
    /// The owner's answer was "rules, plus a per-PR override" — and the override matters more than
    /// it looks, because the tick sweeps every repo in the registry. Without a way to say "not this
    /// one" about a single pull request, the only way to exclude one is to edit the rule for
    /// everybody, which is how a rule stops being written honestly.
    ///
    /// So the choice on a row wins in BOTH directions: it can put a workflow on a pull request no
    /// rule claims, and it can keep every rule off one.
    #[test]
    fn the_row_has_the_last_word_over_a_rule() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[
              {"name":"ship-mine","matches":["mine"],"steps":[{"when":[],"do":"merge:squash"}]},
              {"name":"by-hand","steps":[{"when":[],"do":"flag:look at this"}]}]}"#,
        )
        .unwrap();
        let mine = crate::workflow::Facts {
            mine: true,
            ..Default::default()
        };
        let theirs = crate::workflow::Facts {
            mine: false,
            ..Default::default()
        };

        // A rule claims what it matches, and nothing else.
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Matched("ship-mine".into())
        );
        assert_eq!(carries("demo", 2, &theirs, &flows), Carries::Nothing);

        // A workflow with no rule of its own is never picked up by matching — it exists to be
        // chosen, and choosing it works on a pull request no rule would have claimed.
        assign("demo", 2, "by-hand").unwrap();
        assert_eq!(
            carries("demo", 2, &theirs, &flows),
            Carries::Assigned("by-hand".into())
        );

        // And the row overrules a rule that would otherwise have claimed it.
        assign("demo", 1, "by-hand").unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Assigned("by-hand".into())
        );

        // "No workflow" is a thing you can say, and it is not the same as saying nothing. This is
        // the one that keeps a fleet-wide rule usable.
        assign("demo", 1, "").unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Excluded,
            "a rule reclaimed a pull request that was excluded by hand"
        );

        // Clearing the choice is different again: the rules speak for it once more.
        unassign("demo", 1).unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Matched("ship-mine".into())
        );

        // An assignment naming a workflow that has since been deleted acts on nothing, rather than
        // guessing which of the survivors was meant.
        assign("demo", 3, "the-one-that-was-deleted").unwrap();
        assert_eq!(carries("demo", 3, &mine, &flows), Carries::Nothing);

        std::env::remove_var("SKEIN_HOME");
    }

    /// Letting a stopped workflow run again does not also take the workflow off.
    ///
    /// The bug this exists for was written and found within an hour: "let it run again" sends no
    /// workflow name, no name meant "forget the choice", and so one button quietly did two things —
    /// the second being to un-assign the workflow somebody had chosen. On a fleet where a rule
    /// would then re-claim the pull request, that is a change of behaviour nobody asked for,
    /// arriving through a button labelled something else.
    #[test]
    fn clearing_a_stop_does_not_change_what_governs_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"by-hand","steps":[{"when":[],"do":"flag:hi"}]}]}"#,
        )
        .unwrap();
        let facts = crate::workflow::Facts::default();

        apply("demo", 5, Some("by-hand"), false, false).unwrap();
        stop("demo", 5, "CI is red");
        assert!(stopped("demo", 5).is_some());

        // The button, and only the button.
        apply("demo", 5, None, false, true).unwrap();
        assert_eq!(stopped("demo", 5), None, "the stop was not cleared");
        assert_eq!(
            carries("demo", 5, &facts, &flows),
            Carries::Assigned("by-hand".into()),
            "letting it run again silently took the workflow off it"
        );

        // And forgetting the choice is its own request, which still works.
        apply("demo", 5, None, true, false).unwrap();
        assert_eq!(carries("demo", 5, &facts, &flows), Carries::Nothing);

        std::env::remove_var("SKEIN_HOME");
    }

    /// What GitHub said becomes what a workflow sees, and UNKNOWN survives the trip.
    ///
    /// The queue is the only source of facts, so anything lost here is lost to every decision. Two
    /// things are easy to get wrong and both are asserted:
    ///
    /// * approved is the REPOSITORY's verdict, not yours. Being one approver of six is not the
    ///   pull request being approved, and a workflow that merges must read the first one.
    /// * UNKNOWN is not "cannot be merged". GitHub says it for a while after every push; read as a
    ///   conflict it rebases on a guess, and that rebase costs the approval authorising the merge
    ///   on any repository that dismisses stale approvals.
    #[test]
    fn what_github_said_becomes_what_a_workflow_sees() {
        // Built from JSON rather than a struct literal: `Pr` gains fields regularly, and a literal
        // is the thing that stops compiling for a reason unrelated to what is being tested. How the
        // fields get there from GitHub's own answer is prq's to prove, and it does.
        let pr = |decision: &str, mergeable: Option<bool>| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "Me", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": ["ci", "needs docs"],
                "review_decision": decision,
                "mergeable": mergeable,
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        let approved = facts_of(&pr("APPROVED", Some(true)), "me");
        assert!(
            approved.approved,
            "the repository approved it and skein did not see that"
        );
        assert!(!approved.changes_requested);
        assert_eq!(approved.mergeable, Some(true));
        // Labels come through by name, including one with a space in it — which also has to survive
        // being put back in a URL when a step removes it.
        assert_eq!(
            approved.labels,
            vec!["ci".to_string(), "needs docs".to_string()]
        );
        assert!(
            approved.mine,
            "the author is the viewer, in whatever case GitHub spells it"
        );

        let conflicting = facts_of(&pr("REVIEW_REQUIRED", Some(false)), "me");
        assert!(!conflicting.approved);
        assert_eq!(conflicting.mergeable, Some(false));

        // The one that matters.
        let unknown = facts_of(&pr("APPROVED", None), "me");
        assert_eq!(
            unknown.mergeable, None,
            "an unknown mergeable state was given an answer on the way to the workflow"
        );
        assert!(
            !crate::workflow::holds(&crate::workflow::Cond::NotMergeable, &unknown)
                && !crate::workflow::holds(&crate::workflow::Cond::Mergeable, &unknown),
            "unknown satisfied one of the two conditions it must satisfy neither of"
        );

        // Changes requested is its own state, not the absence of approval.
        let blocked = facts_of(&pr("CHANGES_REQUESTED", Some(true)), "me");
        assert!(blocked.changes_requested && !blocked.approved);

        // And somebody else's pull request is not yours, however it is spelled.
        assert!(!facts_of(&pr("APPROVED", Some(true)), "someone-else").mine);
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
