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
///
/// `trunk` is [`crate::prq::Queue::trunk`] — the repository's default branch, `""` when not
/// known. `behind` keeps `mergeable`'s three values: `BEHIND` is yes, `""` and `UNKNOWN` are
/// *no answer* rather than no, and everything else GitHub says (`CLEAN`, `BLOCKED`, `DIRTY`,
/// `UNSTABLE`, …) is a head GitHub has compared with its base and not found behind.
///
/// **An unknown trunk is `None`, not `false`.** Both keep a train from shipping into what it only
/// believes is the trunk, but they are different situations and only one of them is the pull
/// request's fault: a base that is not the trunk is a stacked child and stops, a trunk skein
/// cannot see is skein's own blindness and waits.
/// [`crate::workflow::instead_of_merging_off_the_trunk`] is where that difference is spent.
pub fn facts_of(pr: &crate::prq::Pr, viewer: &str, trunk: &str) -> crate::workflow::Facts {
    crate::workflow::Facts {
        approved: pr.review_decision == "APPROVED",
        changes_requested: pr.review_decision == "CHANGES_REQUESTED",
        labels: pr.labels.clone(),
        checks: pr.checks.clone(),
        mergeable: pr.mergeable,
        draft: pr.draft,
        mine: !viewer.is_empty() && pr.author.eq_ignore_ascii_case(viewer),
        behind: match pr.merge_state.as_str() {
            "BEHIND" => Some(true),
            "" | "UNKNOWN" => None,
            _ => Some(false),
        },
        base_is_trunk: match trunk.is_empty() {
            true => None,
            false => Some(pr.base_ref == trunk),
        },
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

/// Every stopped pull request in this repo, in numeric order, in the shape the counts payload
/// carries ([`crate::prq::StoppedPr`] — the type is the payload's, the file is this module's).
///
/// Numeric rather than the file's own: the stops are keyed by strings, and `"10"` sorting before
/// `"9"` is not an order anybody asked to read a banner in.
pub fn stops(repo_id: &str) -> Vec<crate::prq::StoppedPr> {
    let mut out: Vec<crate::prq::StoppedPr> = read_stops(repo_id)
        .into_iter()
        .filter_map(|(number, why)| {
            number
                .parse::<u64>()
                .ok()
                .map(|number| crate::prq::StoppedPr { number, why })
        })
        .collect();
    out.sort_by_key(|s| s.number);
    out
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
        // A person clearing a stop is an event the timeline must show — without it, a journal
        // reads "stopped … did …" with no sign of the hand that let it move again.
        record(
            repo_id,
            number,
            "",
            0,
            "cleared",
            "the stop was cleared — the workflow may act again",
        );
    }
}

/// One line of a pull request's workflow history — the durable answer to "what happened to this
/// one, and in what order".
///
/// The stop file says only the *latest* reason; the audit log belongs to the host and mixes every
/// box's events. This is skein's own per-PR timeline, written the moment something happens, read
/// oldest-first.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JournalEntry {
    /// When, as epoch milliseconds.
    pub at_ms: i64,
    /// Which workflow acted. Empty for events no workflow owns — a person clearing a stop.
    pub flow: String,
    /// Which step, 1-based. `0` means "not a step": a clear, or a stop written by hand.
    pub step: usize,
    /// `"did"` | `"stopped"` | `"cleared"`.
    pub kind: String,
    /// The human sentence — the same one the audit and the row carry.
    pub what: String,
}

/// Where a repo's workflow journal lives: beside the stops, keyed the same way.
fn journal_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-journal.json")
}

fn read_journal(repo_id: &str) -> std::collections::BTreeMap<String, Vec<JournalEntry>> {
    // A corrupt or absent file reads as empty, never as an error: the journal is a record of what
    // happened, and losing it must not stop anything from happening.
    std::fs::read_to_string(journal_path(repo_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write one journal entry down, now.
///
/// Write-through like the stops file: every event lands on disk before the function returns, so a
/// server that dies mid-pass has still said what it did. Each pull request keeps its newest 50
/// entries — a train PR sees a handful of acts on its way to merged, so 50 covers weeks of
/// stop/clear churn without the file growing without bound.
fn record(repo_id: &str, number: u64, flow: &str, step: usize, kind: &str, what: &str) {
    let at_ms = now_ms();
    let mut all = read_journal(repo_id);
    let entries = all.entry(number.to_string()).or_default();
    entries.push(JournalEntry {
        at_ms,
        flow: flow.to_string(),
        step,
        kind: kind.to_string(),
        what: what.to_string(),
    });
    if entries.len() > 50 {
        let drop = entries.len() - 50;
        entries.drain(..drop);
    }
    let path = journal_path(repo_id);
    let write = || -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let body = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
    };
    if let Err(e) = write() {
        // Best-effort, said out loud: a journal that could not be written loses history, not
        // safety — the stop file is the one whose loss re-attempts an action.
        eprintln!("skein: could not journal #{number}'s workflow event ({e})");
    }
}

/// One pull request's workflow history, oldest first.
pub fn journal(repo_id: &str, number: u64) -> Vec<JournalEntry> {
    read_journal(repo_id)
        .remove(&number.to_string())
        .unwrap_or_default()
}

/// Every journaled pull request in this repo, in numeric order, each timeline oldest first.
pub fn journals(repo_id: &str) -> std::collections::BTreeMap<u64, Vec<JournalEntry>> {
    read_journal(repo_id)
        .into_iter()
        .filter_map(|(number, entries)| number.parse::<u64>().ok().map(|n| (n, entries)))
        .collect()
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
            //
            // The journal write sits HERE, beside the stop write, not inside `stop()`: `stop` is
            // also called by hands other than a workflow's, and those stops are not this flow's
            // step doing something — journaling them here keeps the flow and step honest.
            stop(repo_id, number, why);
            record(repo_id, number, &flow.name, chosen.step + 1, "stopped", why);
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
            record(repo_id, number, &flow.name, chosen.step + 1, "did", &what);
            Outcome::Did(what)
        }
        Err(why) => {
            // Not a retry. See the module note: the decision was made from facts this failure has
            // just proved stale, and the next poll would make the same one.
            let why = format!("{by} could not be done: {why}");
            stop(repo_id, number, &why);
            record(
                repo_id,
                number,
                &flow.name,
                chosen.step + 1,
                "stopped",
                &why,
            );
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

/// How long the front of a serial train may wait on something skein cannot see running.
///
/// Twenty minutes, and the number is chosen against the two things it must not get wrong. It has
/// to be long enough that a check which is merely slow to be QUEUED is never mistaken for one that
/// is not coming — GitHub Actions starts within seconds normally, and minutes on a busy runner
/// pool — and short enough that a person watching a train notices the same day. A wait on
/// something that IS running is not bounded by this at all (see [`a_wait_with_nothing_behind_it`]),
/// so no CI run, however long, is ever cut short by it.
pub const WAITING_ON_NOTHING_MS: i64 = 20 * 60 * 1000;

/// When this pull request started waiting on THIS step, if it is still waiting on it.
///
/// The NEWEST entry is the whole answer, and that is the point: anything at all having happened
/// since — an act, a flag, a person clearing a stop, a wait on a different step — means the wait
/// that was being timed ended, and whatever is being waited on now starts its own clock. So a
/// train that is making progress can never accumulate patience across the steps it walked through.
fn waiting_since(entries: &[JournalEntry], flow: &str, step: usize) -> Option<i64> {
    entries
        .last()
        .filter(|e| e.kind == "waiting" && e.flow == flow && e.step == step)
        .map(|e| e.at_ms)
}

/// The front of a serial train said `wait`. Start its clock, or stop it because the clock ran out.
///
/// **This is not a stale-state bug and the fix is not a re-read** (SKEIN-240). `prq::rollup`
/// answers `"none"` when nothing has ever run against a commit, and that reading is CORRECT and
/// CURRENT — it is the same answer whether CI is five seconds away or will never come, because
/// nothing GitHub sends distinguishes "no check yet" from "no check, ever, on this repository".
/// Asking again produces the same true answer for ever. The documented train has no step for
/// `checks:none` once its label is on, so the front falls to the catch-all `wait:` and holds the
/// line at one pass per two minutes, for ever, saying *"waiting for GitHub to catch up"* when
/// GitHub caught up long ago. `docs/pr-workflow.md` names exactly this: *"The failure mode to
/// avoid is not the stall. It is a **silent** stall."*
///
/// So the only thing that can tell those two apart is how long the waiting has gone on, and this
/// is where that is decided.
///
/// **A wait on something running is not bounded.** `checks: pending` is a check that has started,
/// and a real one can take the better part of an hour — the module note above builds the whole
/// guarded-step design around surviving "a forty-minute CI run". Cutting one short would be a
/// worse bug than the one this fixes. What is bounded is a wait with nothing behind it: no check
/// running, nothing in flight skein can point at, and a sentence that promises something is going
/// to change.
///
/// **Only a serial train.** A stop is a demand for somebody's attention, and it is earned when the
/// alternative is a queue that has stopped moving. A pull request on a workflow that blocks nobody
/// is not costing anything by waiting, and stopping it would be manufacturing work.
///
/// Recoverable, in the two ways that matter: the stop names the elapsed time and the step so the
/// sentence is checkable, and clearing it puts the pull request back in line — where, if the wait
/// really was on something slow, it simply waits again with a fresh clock.
fn a_wait_with_nothing_behind_it(
    repo_id: &str,
    number: u64,
    flow: &Workflow,
    chosen: &Chosen,
    facts: &crate::workflow::Facts,
    why: &str,
) -> Option<String> {
    if !flow.serial || facts.checks == "pending" {
        return None;
    }
    let step = chosen.step + 1;
    let now_ms = now_ms();
    let Some(since) = waiting_since(&journal(repo_id, number), &flow.name, step) else {
        // The first pass on this step: put the clock down and say nothing. A wait is the ordinary
        // state of a train and this entry is what makes it a *timed* one.
        record(repo_id, number, &flow.name, step, "waiting", why);
        return None;
    };
    if now_ms - since < WAITING_ON_NOTHING_MS {
        return None;
    }
    let minutes = (now_ms - since) / 60_000;
    let reason = format!(
        "step {step} has been waiting {minutes} minutes — {why:?} — and nothing is running \
         (checks: {}). Whatever was expected to start has not, so this wait will not end on its \
         own; if a label is meant to start CI here, check it is the one the repository's workflow \
         keys on. Clearing this stop puts it back in line.",
        match facts.checks.is_empty() {
            true => "none",
            false => facts.checks.as_str(),
        }
    );
    stop(repo_id, number, &reason);
    record(repo_id, number, &flow.name, step, "stopped", &reason);
    crate::warden_client::reported(
        &format!("pr-workflow:{}", flow.name),
        &format!("stopped waiting on #{number}"),
        &reason,
    );
    Some(reason)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One serial workflow's train in one repo: who is in line, who is at the front, who has been
/// passed over — the answer to "what is it working on, and which step is everyone else waiting
/// behind".
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrainView {
    /// The serial workflow's name.
    pub flow: String,
    /// The one pull request that may act this pass: the oldest carrying PR without a stop. `None`
    /// when the line is empty or everyone in it is stopped — a train with nobody to move.
    pub front: Option<u64>,
    /// Every carrying pull request in train order — oldest first, lowest number — front included.
    pub line: Vec<u64>,
    /// Only this flow's carrying pull requests that are stopped, with their reasons, in train
    /// order. The passed-over, not the whole repo's stop file.
    pub stopped: Vec<crate::prq::StoppedPr>,
}

/// Every serial workflow's train in this repo, from the same ordering-and-front rule the tick
/// acts on.
///
/// `prs` is (number, carried flow name) for the non-archived carrying pull requests — the caller
/// has already decided who carries what, because that needs facts this function should not
/// re-derive. This is the ONE place the train order and the front are computed: [`sweep`] calls
/// through it before acting, so a panel drawn from it cannot disagree with what the tick then
/// does. A non-serial workflow gets no view — a train is the serial thing.
pub fn trains(repo_id: &str, prs: &[(u64, String)], flows: &[Workflow]) -> Vec<TrainView> {
    let stops = read_stops(repo_id);
    flows
        .iter()
        .filter(|flow| flow.serial)
        .map(|flow| {
            // Oldest first — lowest number, the sort key the owner chose.
            let mut line: Vec<u64> = prs
                .iter()
                .filter(|(_, name)| *name == flow.name)
                .map(|(number, _)| *number)
                .collect();
            line.sort_unstable();
            // The first one without a stop is the front; a stopped PR is passed over — the "skip
            // failures and move ahead" (docs/pr-workflow.md, "The merge train").
            let front = line
                .iter()
                .copied()
                .find(|number| !stops.contains_key(&number.to_string()));
            let stopped = line
                .iter()
                .filter_map(|number| {
                    stops
                        .get(&number.to_string())
                        .map(|why| crate::prq::StoppedPr {
                            number: *number,
                            why: why.clone(),
                        })
                })
                .collect();
            TrainView {
                flow: flow.name.clone(),
                front,
                line,
                stopped,
            }
        })
        .collect()
}

/// One pass over the fleet: every repo skein manages, every pull request a workflow governs, one
/// step each.
///
/// **One step per pull request per pass, and the pass is the only thing that acts.** After an
/// action lands, what skein believes about that pull request is one action out of date — the label
/// is on but no check has been queued, so `checks:passing` is still true from the previous run. The
/// next pass re-reads GitHub, which is the only thing that can say what the action did.
///
/// **Every repo in the registry**, not only ones whose queue somebody has opened — the owner's
/// decision, and what makes this automation rather than a thing you have to remember to visit. A
/// repo with no workflow claiming anything costs one cached queue read.
///
/// Returns what it did, for the server's log. Every action is also in the host audit with its
/// authority; this is the line a person watching a terminal sees.
pub fn sweep() -> Vec<String> {
    // Nothing at all when the switch is off — not even a queue read. A feature that is switched off
    // should be invisible in every way somebody might notice, including a rate limit.
    if !enabled() {
        return Vec::new();
    }
    let flows = match crate::workflow::load() {
        Ok(flows) => flows,
        // A file with one bad step loads none of them (`workflow::from_bytes`), which is the right
        // answer and a silent one — so it is said here, where somebody watching the server sees it.
        Err(why) => {
            eprintln!("skein: no workflow is running — {why}");
            return Vec::new();
        }
    };
    if flows.is_empty() {
        return Vec::new();
    }
    let token = match crate::prq::host_token() {
        Ok(token) => token,
        Err(why) => {
            eprintln!("skein: workflows are on, and there is no GitHub token to act with — {why}");
            return Vec::new();
        }
    };

    let mut did = Vec::new();
    for repo in crate::repos::load_repos() {
        let Ok(queue) = crate::prq::queue(&repo, false) else {
            // A queue that cannot be read is not a reason to stop the fleet's other repos. The
            // review pane reports the failure with its reason; this pass simply has nothing to
            // decide from.
            continue;
        };
        // Who carries what, decided once for the whole repo before anyone may act: a serial
        // workflow's rule below is about the *whole* train, and a decision made one pull request
        // at a time could not see past the one in hand.
        let mut rows = Vec::new();
        for pr in &queue.prs {
            // A pull request you set aside is one you said "not now" about. A workflow acting on it
            // would be overruling that with a rule, which is the opposite of what setting aside is
            // for — and the row that says "archived" would be acting.
            if matches!(pr.lane, crate::prq::Lane::Archived) {
                continue;
            }
            let facts = facts_of(pr, &queue.viewer, &queue.trunk);
            let Some(name) = carries(&repo.id, pr.number, &facts, &flows)
                .name()
                .map(str::to_string)
            else {
                continue;
            };
            rows.push((pr, facts, name));
        }
        // The front of each serial train: carrying pull requests oldest-first (lowest number —
        // the sort key the owner chose), and the first one without a stop is the only one that
        // may act this pass. A stopped front is passed over rather than reported — that is the
        // "skip failures and move ahead" — and everyone behind the front is simply waiting, which
        // is the ordinary state of a train and not an event (`docs/pr-workflow.md`, "The merge
        // train"). A workflow whose every carrying PR is stopped has no front, and nobody acts.
        //
        // Computed by [`trains`] — the same function the cockpit's train panel reads — so what a
        // person is shown and what the tick then does cannot be two computations that drift apart.
        let carrying: Vec<(u64, String)> = rows
            .iter()
            .map(|(pr, _, name)| (pr.number, name.clone()))
            .collect();
        let fronts: std::collections::BTreeMap<String, u64> = trains(&repo.id, &carrying, &flows)
            .into_iter()
            .filter_map(|train| train.front.map(|front| (train.flow, front)))
            .collect();
        let mut acted_in_repo = false;
        for (pr, facts, name) in &rows {
            let Some(flow) = flows.iter().find(|f| &f.name == name) else {
                continue;
            };
            // Everyone but the front of a serial train is passed over: no action, and no stop —
            // being behind the front is where a train's pull requests live, not a fault.
            if flow.serial && fronts.get(name) != Some(&pr.number) {
                continue;
            }
            let Some(chosen) = crate::workflow::next(flow, facts) else {
                continue;
            };
            let subject = Subject {
                repo_id: &repo.id,
                slug: &queue.slug,
                number: pr.number,
                head_sha: &pr.head_sha,
                head_ref: &pr.head_ref,
            };
            match perform(&subject, flow, &chosen, &token) {
                Outcome::Did(what) => {
                    did.push(format!("{}: {what}", repo.id));
                    acted_in_repo = true;
                }
                // Waiting is the ordinary state and says nothing — but the front of a serial
                // train waiting on nothing is the line not moving, so it is timed. See
                // [`a_wait_with_nothing_behind_it`], which is the only thing standing between a
                // repo whose CI label starts nothing and a train parked for ever.
                Outcome::Waited(why) => {
                    a_wait_with_nothing_behind_it(&repo.id, pr.number, flow, &chosen, facts, &why);
                }
                // A stop has already been written down and audited by `perform`; repeating it here
                // every pass would bury the log.
                Outcome::Stopped(_) => {}
            }
        }
        // The queue is cached for a minute, and skein has just changed the thing it describes. Left
        // alone, the next pass would decide from facts it had itself made stale — which is the one
        // input a cascade needs to merge on a check that has not run.
        if acted_in_repo {
            crate::prq::invalidate(&repo.id);
        }
    }
    did
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

    /// The owner's example, walked to merged by the tick alone, with nothing open.
    ///
    /// The claim the whole feature makes: a pull request that is approved and green ends up merged
    /// without anybody pressing anything. Driven through `sweep` against a GitHub that answers from
    /// a fixture and CHANGES as skein acts on it — a label appears when skein adds one, checks go
    /// green once it is there — because a stub that answers the same thing every time cannot tell a
    /// workflow that advances from one that is stuck in a loop taking the same step.
    ///
    /// The other half of the claim is that it takes ONE step per pass. After an action lands, what
    /// skein believes is one action out of date, so a pass that kept going would decide the next
    /// step from facts it had just made stale — a label added, no check yet queued, `checks:passing`
    /// still true from the previous run, and it merges.
    #[test]
    fn the_tick_walks_a_pull_request_to_merged_one_step_per_pass() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The workflow, as the owner described it.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"ship-mine","matches":["mine"],"steps":[
              {"when":["approved","no-label:ci"],"do":"add-label:ci"},
              {"when":["checks:pending"],"do":"wait:CI is running"},
              {"when":["checks:failing"],"do":"flag:CI is red"},
              {"when":["approved","mergeable","checks:passing"],"do":"merge:squash+delete"},
              {"when":["approved","not-mergeable"],"do":"update-branch:rebase"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub whose answers move as skein acts on it.
        // **Green before the label goes on**, which is the state that makes "one step per pass" a
        // property with teeth. The branch passed CI on an earlier run, so `checks:passing` is true
        // AND the label is missing — both step 1 and step 4 apply at once. A pass that kept going
        // would add the label and then merge, in the same breath, on a check run that predates it.
        let state: Arc<Mutex<(bool, String)>> = Arc::new(Mutex::new((false, "passing".into())));
        let merged = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (world, seen) = (state.clone(), merged.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let (labelled, checks) = world.lock().unwrap().clone();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    // What the repository's default branch is. A real GitHub answers this and a
                    // stub that did not used to be harmless — until a merge started requiring
                    // skein to KNOW the base it is shipping into
                    // ([`crate::workflow::instead_of_merging_off_the_trunk`]), at which point a
                    // fixture with no trunk is a fixture where nothing may merge.
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/labels") {
                    // The label lands, and this repository's CI starts on it.
                    *world.lock().unwrap() = (true, "pending".into());
                    "[]".to_string()
                } else if head.contains("/merge") {
                    seen.lock().unwrap().push(head.clone());
                    r#"{"merged":true}"#.to_string()
                } else if head.starts_with("DELETE") {
                    seen.lock().unwrap().push(head.clone());
                    "{}".to_string()
                } else if head.contains("/graphql") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{{"number":7,"title":"t","url":"u",
                          "isDraft":false,"author":{{"login":"me"}},"headRefName":"feat",
                          "headRefOid":"abc","baseRefName":"main",
                          "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                          "mergeable":"MERGEABLE",
                          "labels":{{"nodes":[{}]}},
                          "latestReviews":{{"nodes":[]}},
                          "commits":{{"nodes":[{{"commit":{{
                             "committedDate":"2026-08-23T00:00:00Z",
                             "statusCheckRollup":{{"contexts":{{"nodes":[{}]}}}}}}}}]}}}}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        match labelled {
                            true => r#"{"name":"ci"}"#,
                            false => "",
                        },
                        match checks.as_str() {
                            "pending" => r#"{"status":"IN_PROGRESS"}"#,
                            "passing" => r#"{"status":"COMPLETED","conclusion":"SUCCESS"}"#,
                            _ => "",
                        },
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // **A pull request you set aside is left alone**, even by a rule that claims it. Setting
        // aside is a person saying "not now" about this one; a workflow acting on it would overrule
        // that with a rule, and the row would say "archived" while skein merged it.
        std::fs::create_dir_all(crate::prq::review_dir("demo")).unwrap();
        std::fs::write(crate::prq::review_dir("demo").join("archived.json"), b"[7]").unwrap();
        assert!(
            sweep().is_empty(),
            "a pull request that was set aside was acted on anyway"
        );
        std::fs::write(crate::prq::review_dir("demo").join("archived.json"), b"[]").unwrap();

        // Pass one: approved, unlabelled. The label that starts CI.
        let did = sweep();
        assert_eq!(did.len(), 1, "a pass took more than one step: {did:?}");
        assert!(did[0].contains("label"), "{did:?}");
        assert!(
            merged.lock().unwrap().is_empty(),
            "it merged in the same pass that started CI — on a check that had not run"
        );

        // Pass two: CI is running. Waiting is not an action, so nothing is reported and nothing is
        // done — and above all it does not merge.
        assert!(
            sweep().is_empty(),
            "waiting for CI was reported as doing something"
        );
        assert!(merged.lock().unwrap().is_empty());

        // CI goes green.
        state.lock().unwrap().1 = "passing".into();
        let did = sweep();
        assert_eq!(did.len(), 1, "{did:?}");
        assert!(did[0].contains("merged #7"), "{did:?}");
        let calls = merged.lock().unwrap().clone();
        assert!(
            calls.iter().any(|c| c.contains("/pulls/7/merge"))
                && calls.iter().any(|c| c.contains("git/refs/heads/feat")),
            "the branch did not go with the merge: {calls:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
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

        let approved = facts_of(&pr("APPROVED", Some(true)), "me", "main");
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

        let conflicting = facts_of(&pr("REVIEW_REQUIRED", Some(false)), "me", "main");
        assert!(!conflicting.approved);
        assert_eq!(conflicting.mergeable, Some(false));

        // The one that matters.
        let unknown = facts_of(&pr("APPROVED", None), "me", "main");
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
        let blocked = facts_of(&pr("CHANGES_REQUESTED", Some(true)), "me", "main");
        assert!(blocked.changes_requested && !blocked.approved);

        // And somebody else's pull request is not yours, however it is spelled.
        assert!(!facts_of(&pr("APPROVED", Some(true)), "someone-else", "main").mine);
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

    /// What GitHub's `mergeStateStatus` becomes on the way to a workflow, and what "trunk" means.
    ///
    /// `behind` keeps `mergeable`'s discipline — `""` and `UNKNOWN` are *no answer*, not "current"
    /// — because the merge step leans on `current`, and unknown read as current merges code CI
    /// never tested against the trunk (docs/pr-workflow.md, "The merge train"). And an unknown
    /// trunk claims nothing: `base_is_trunk` is `None` — no answer rather than "no", which is the
    /// direction that keeps a train parked rather than shipping into a branch it only believes is
    /// the trunk, without turning skein's own blindness into a stop somebody has to clear.
    #[test]
    fn the_train_facts_come_from_merge_state_and_the_trunk() {
        let pr = |merge_state: &str, base_ref: &str| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "me", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": base_ref,
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": "APPROVED", "mergeable": true,
                "merge_state": merge_state,
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        assert_eq!(
            facts_of(&pr("BEHIND", "main"), "me", "main").behind,
            Some(true)
        );
        assert_eq!(
            facts_of(&pr("CLEAN", "main"), "me", "main").behind,
            Some(false)
        );
        assert_eq!(
            facts_of(&pr("", "main"), "me", "main").behind,
            None,
            "a queue from before the field was given an answer it does not have"
        );
        assert_eq!(
            facts_of(&pr("UNKNOWN", "main"), "me", "main").behind,
            None,
            "UNKNOWN was flattened to an answer on the way to the workflow"
        );

        assert_eq!(
            facts_of(&pr("CLEAN", "main"), "me", "main").base_is_trunk,
            Some(true)
        );
        assert_eq!(
            facts_of(&pr("CLEAN", "feat-parent"), "me", "main").base_is_trunk,
            Some(false),
            "a stacked child's base was not recognised as one that is NOT the trunk"
        );
        // The third value, and the one that keeps a rate limit from becoming a stop: skein has not
        // learned this repository's default branch, which is not the same answer as "no".
        assert_eq!(
            facts_of(&pr("CLEAN", "main"), "me", "").base_is_trunk,
            None,
            "an unknown trunk was flattened to an answer on the way to the workflow"
        );
    }

    /// A stacked child never boards the train, and no stack model was needed.
    ///
    /// The one rule from docs/pr-workflow.md ("Stacks need no stack model"): the train only
    /// touches a PR whose base is the trunk. A child's base is its parent's *branch* — merging it
    /// would merge into the parent, not ship it — so `base:trunk` in `matches` keeps it out until
    /// GitHub retargets it onto the trunk after the parent merges.
    #[test]
    fn a_stacked_child_is_kept_out_by_its_matches() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,
                 "matches":["ready","approved","base:trunk"],
                 "steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap();
        let pr = |base_ref: &str| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 12, "title": "t", "author": "me", "url": "u",
                "head_ref": "feat-child", "head_sha": "abc", "base_ref": base_ref,
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": "APPROVED", "mergeable": true,
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        let child = facts_of(&pr("feat-parent"), "me", "main");
        assert_eq!(
            carries("demo", 12, &child, &flows),
            Carries::Nothing,
            "a stacked child was claimed by the train — it would merge into its parent's branch"
        );
        // And the same pull request, retargeted onto the trunk after its parent merged, is an
        // ordinary trunk-based PR the train claims — that is the whole stack mechanism.
        let retargeted = facts_of(&pr("main"), "me", "main");
        assert_eq!(
            carries("demo", 12, &retargeted, &flows),
            Carries::Matched("merge-train".into())
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A stacked child somebody put the train on by hand STOPS, and no merge reaches the wire.
    ///
    /// The other half of the rule above, and the one that was missing (SKEIN-237). `matches` is
    /// read by [`crate::workflow::claims`] and by nothing else: [`carries`] returns
    /// `Carries::Assigned` straight from the assignment file, [`sweep`] takes its `.name()`, and
    /// [`crate::workflow::next`] evaluates only `steps` — so on a documented merge train, whose
    /// `base:trunk` lives in `matches`, one hand assignment merged a child into its PARENT's
    /// branch and deleted the child's branch. Putting a workflow on a row by hand is an ordinary
    /// cockpit act; on an eighteen-deep stack it takes the rest of the stack with it, and there is
    /// no undo for a landed merge and a deleted branch.
    ///
    /// Driven through [`sweep`] against a GitHub that records every request, because the assertion
    /// that matters is about the wire: a doer tested through its return value would pass while
    /// merging. The workflow is the train exactly as `docs/pr-workflow.md` writes it down.
    #[test]
    fn a_hand_assigned_stacked_child_stops_instead_of_merging_into_its_parent() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, "The train, written down".
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","base:trunk"],
              "steps":[
                {"when":["changes-requested"],"do":"flag:changes were requested"},
                {"when":["not-mergeable"],"do":"flag:conflicts with the base"},
                {"when":["behind"],"do":"update-branch:rebase"},
                {"when":["checks:failing"],"do":"flag:CI failed"},
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #12 is a stacked child: based on `feat-parent`, and otherwise in the exact state that
        // makes the train's last real step fire — approved, ready, labelled, green, CLEAN.
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(head.clone());
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing") && !head.contains("/pulls") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[{"number":12,"title":"t","url":"u",
                      "isDraft":false,"author":{"login":"me"},"headRefName":"feat-12",
                      "headRefOid":"abc","baseRefName":"feat-parent",
                      "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                      "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                      "labels":{"nodes":[{"name":"ci-queue"}]},
                      "latestReviews":{"nodes":[]},
                      "commits":{"nodes":[{"commit":{
                        "committedDate":"2026-08-23T00:00:00Z",
                        "statusCheckRollup":{"contexts":{"nodes":[
                          {"status":"COMPLETED","conclusion":"SUCCESS"}]}}}}]}}]},
                      "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else {
                    r#"{"merged":true}"#.to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // The rule refuses it — that is `a_stacked_child_is_kept_out_by_its_matches` above. A
        // person puts the train on it by hand, which is the road that skipped every guard.
        assign("demo", 12, "merge-train").unwrap();
        let did = sweep();

        let calls = heard.lock().unwrap().clone();
        assert!(
            !calls.iter().any(|c| c.contains("/pulls/12/merge")),
            "the train merged a stacked child into feat-parent: {did:?} / {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("DELETE")),
            "a branch was deleted on a pull request that was never merged: {calls:?}"
        );
        // And it stopped rather than going quiet: a serial train passes a stop over and keeps
        // moving, and the reason is what somebody reads in the banner.
        let why = stopped("demo", 12).unwrap_or_else(|| {
            panic!("a stacked child was left silently blocking the front of the train: {did:?}")
        });
        assert!(
            why.contains("not based on the trunk"),
            "the stop does not say what is wrong: {why}"
        );
        // The timeline says which workflow and which step decided it — step 7 is the merge.
        let entries = journal("demo", 12);
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.kind.as_str(), e.flow.as_str(), e.step))
                .collect::<Vec<_>>(),
            vec![("stopped", "merge-train", 7)],
            "the refusal must name the line that would have merged: {entries:?}"
        );
        // And the dry run says the same thing. It is the same [`crate::workflow::next`], so a
        // person reading the workflows pane before they switch this on is shown the refusal rather
        // than the merge it used to promise.
        let flows = crate::workflow::load().unwrap();
        let facts = facts_of(
            &crate::prq::queue(&crate::repos::load_repos()[0], false)
                .unwrap()
                .prs[0],
            "me",
            "main",
        );
        let seen = standing("demo", 12, &facts, &flows);
        assert_eq!((seen.how.as_str(), seen.step), ("assigned", 7));
        assert!(
            seen.next.starts_with("flag:"),
            "the dry run promised something the tick will not do: {seen:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// One rate-limited refresh must not disable the merge train until somebody restarts skein.
    ///
    /// The shape (SKEIN-238), and it is the same shape twice over. `prq::queue_within` asks in the
    /// order `viewer()` [REST], `search_prs_all` [GraphQL], `trunk_of` [REST] — so a GraphQL-only
    /// limit passes the first, engages `crate::github`'s process-wide hold on the second, and the
    /// third is refused by that hold having asked GitHub nothing. `trunk_of` swallowed that
    /// refusal into `""` and REMEMBERED it, for the life of the process. Everything downstream
    /// then did exactly what it should with an unknown trunk: `base_is_trunk` none, `base:trunk`
    /// unsatisfied, `claims` false, `Carries::Nothing`, `sweep` moves on. A dead train, with no
    /// banner, no blind spot and no log line — nothing anybody could clear, because nothing said
    /// it was there.
    ///
    /// So what this asserts is RECOVERY, not correctness: skein is allowed to know nothing while
    /// GitHub is refusing it, and is not allowed to still know nothing one refresh after GitHub
    /// comes back. The fix is that only an ANSWER is remembered (`prq::trunk_of`); a failure is
    /// asked again, and during the hold that retry is refused before it is spent.
    #[test]
    fn a_rate_limited_refresh_does_not_disable_the_train_until_a_restart() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
        crate::prq::forget_renames();

        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","base:trunk"],
              "steps":[{"when":["no-label:ci-queue"],"do":"add-label:ci-queue"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub whose GRAPHQL quota alone is spent — REST is fine, which is the live shape:
        // skein's search is where nearly all of its quota goes. `/rate_limit` stays free and
        // answers, because that is where the hold learns how long to last.
        let spent = Arc::new(Mutex::new(true));
        let out = spent.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let answer = if head.starts_with("GET /rate_limit") {
                    format!(
                        r#"{{"resources":{{"core":{{"remaining":4000,"reset":{}}},
                          "search":{{"remaining":30,"reset":{}}},
                          "graphql":{{"remaining":0,"reset":{}}}}}}}"#,
                        now + 600,
                        now + 600,
                        now + 600
                    )
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") && *out.lock().unwrap() {
                    r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#
                        .to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[{"number":5,"title":"t","url":"u",
                      "isDraft":false,"author":{"login":"me"},"headRefName":"feat-5",
                      "headRefOid":"abc","baseRefName":"main",
                      "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                      "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                      "labels":{"nodes":[]},"latestReviews":{"nodes":[]},
                      "commits":{"nodes":[{"commit":{
                        "committedDate":"2026-08-23T00:00:00Z",
                        "statusCheckRollup":{"contexts":{"nodes":[
                          {"status":"COMPLETED","conclusion":"SUCCESS"}]}}}}]}}]},
                      "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else {
                    "[]".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // The refresh that lands inside the outage. Knowing nothing here is correct.
        let repo = crate::repos::load_repos().remove(0);
        let during = crate::prq::queue(&repo, true).expect("a blind queue still answers");
        assert_eq!(
            during.trunk, "",
            "skein claimed to know the trunk during an outage that refused the lookup"
        );

        // GitHub comes back: the quota returns and the hold is released.
        *spent.lock().unwrap() = false;
        let _cleared = crate::github::HoldClear::new();

        let after = crate::prq::queue(&repo, true).expect("a healthy GitHub answers");
        assert_eq!(
            after.prs.len(),
            1,
            "the recovered queue lost its pull request"
        );
        assert_eq!(
            after.trunk, "main",
            "one rate-limited refresh disabled the merge train until a restart: the failed trunk \
             lookup was remembered as an answer, so `base:trunk` can never hold again"
        );

        // And the train claims it again — the thing the memoised failure had silently switched
        // off. Asserted through `claims`, which is the gate the whole chain narrows to.
        let flows = crate::workflow::load().unwrap();
        let facts = facts_of(&after.prs[0], &after.viewer, &after.trunk);
        assert!(
            crate::workflow::claims(&flows[0], &facts),
            "the merge train still claims nothing after GitHub came back: {facts:?}"
        );
        // …and the tick acts on it, which is what "the train is running" means to a person.
        let did = sweep();
        assert!(
            did.iter().any(|line| line.contains("ci-queue")),
            "the train claimed #5 and still did nothing: {did:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
        crate::prq::forget_renames();
    }

    /// A serial workflow acts on the front of the train, and only the front.
    ///
    /// Oldest first — lowest number, the sort key the owner chose — and a stopped front is
    /// passed over so the train moves ahead of a failure rather than parking behind it
    /// (docs/pr-workflow.md, "The merge train"). The assertion is on the wire, the file's
    /// discipline: two pull requests both due the same action, and exactly one request leaves.
    #[test]
    fn a_serial_workflow_acts_on_the_front_of_the_train_only() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();

        let serial = br#"{"workflow":[{"name":"train","serial":true,"matches":["mine"],"steps":[
          {"when":["no-label:ci"],"do":"add-label:ci"}]}]}"#;
        std::fs::write(home.join("workflows.json"), serial).unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/serial.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // A GitHub with two open pull requests, both mine, both unlabelled — and #9 listed FIRST,
        // so a sweep that took the queue's own order would act on the wrong one.
        let labelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = labelled.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.contains("/labels") {
                    seen.lock().unwrap().push(head.clone());
                    "[]".to_string()
                } else if head.contains("/graphql") {
                    let node = |number: u64| {
                        format!(
                            r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                              "author":{{"login":"me"}},"headRefName":"feat-{number}",
                              "headRefOid":"abc","baseRefName":"main",
                              "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                              "mergeable":"MERGEABLE","labels":{{"nodes":[]}},
                              "latestReviews":{{"nodes":[]}},
                              "commits":{{"nodes":[{{"commit":{{
                                "committedDate":"2026-08-23T00:00:00Z",
                                "statusCheckRollup":{{"contexts":{{"nodes":[]}}}}}}}}]}}}}"#
                        )
                    };
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(9),
                        node(5)
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // Pass one: both #5 and #9 are due the same step. Exactly one request leaves, and it is
        // for #5 — the oldest, not the first in the queue's own order.
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "a serial workflow acted past the front of the train: {calls:?} ({did:?})"
        );
        assert!(
            calls[0].contains("/issues/5/labels"),
            "the train did not act on its oldest pull request: {calls:?}"
        );

        // The front stops — CI failed, say. The next pass skips it and moves ahead: #9 is the
        // front now. That pass-over is the "skip failures and move ahead", and it is silent,
        // because a stopped PR's story is in the stops file, not re-announced every pass.
        stop("demo", 5, "CI is red");
        labelled.lock().unwrap().clear();
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "a stopped front did not yield to the next in line: {calls:?} ({did:?})"
        );
        assert!(
            calls[0].contains("/issues/9/labels"),
            "the train did not move ahead of its stopped front: {calls:?}"
        );

        // And the stops read back in numeric order, the shape the banner row carries.
        let stops = stops("demo");
        assert_eq!(stops.len(), 1);
        assert_eq!((stops[0].number, stops[0].why.as_str()), (5, "CI is red"));

        // The same two pull requests under a NON-serial workflow: everyone due a step acts, which
        // is today's behavior and must stay — serial is a property of a workflow, not of the sweep.
        clear("demo", 5);
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"train","matches":["mine"],"steps":[
              {"when":["no-label:ci"],"do":"add-label:ci"}]}]}"#,
        )
        .unwrap();
        labelled.lock().unwrap().clear();
        let did = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            2,
            "a workflow that never asked to be serial was serialized: {calls:?} ({did:?})"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
    }

    /// A pull request whose CI never starts stops the train's clock, not the train.
    ///
    /// The bug (SKEIN-240) and, more importantly, what KIND of bug it is. It looks like the
    /// rate-limit family — a temporary condition that became permanent — and it is not one.
    /// `prq::rollup` says `"none"` when nothing has ever run against a commit, and that answer is
    /// correct, current and unchanging: nothing GitHub sends tells "no check yet" apart from "no
    /// check, ever, on this repository". There is no staler cache to drop and no re-read that
    /// helps. The documented train has no step for `checks:none` once its label is on, so the
    /// front falls to the catch-all `wait:` and holds the line at one pass per two minutes, for
    /// ever, over a sentence that says GitHub has not caught up when GitHub caught up long ago.
    ///
    /// The only thing that can tell the two apart is elapsed time, so this asserts a clock: the
    /// first pass writes the wait down, passes inside the twenty minutes change nothing, and the
    /// pass after it stops the pull request with a sentence naming the wait — at which point the
    /// serial train's existing pass-over rule moves it aside and #9, which has been in line all
    /// along, gets its turn.
    ///
    /// The back-dated journal entry is the clock: `record` stamps `now`, so the only way to reach
    /// the far side of twenty minutes in a test is to write the timeline the way it would look
    /// twenty minutes later.
    #[test]
    fn a_front_waiting_on_a_check_that_never_starts_stops_and_lets_the_train_past() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, ending in the catch-all that has no clock of its own.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","base:trunk"],
              "steps":[
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #5 is labelled and its label started nothing — `checks: none`, for ever. #9 is behind it
        // in the line, unlabelled, with a step of its own it has never been given a chance to take.
        let labelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = labelled.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing HTTP") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/labels") {
                    seen.lock().unwrap().push(head.clone());
                    "[]".to_string()
                } else if head.contains("/graphql") {
                    // No `statusCheckRollup` contexts and none claimed on either: `checks: none`.
                    let node = |number: u64, labels: &str| {
                        format!(
                            r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                              "author":{{"login":"me"}},"headRefName":"feat-{number}",
                              "headRefOid":"abc","baseRefName":"main",
                              "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                              "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                              "labels":{{"nodes":[{labels}]}},"latestReviews":{{"nodes":[]}},
                              "commits":{{"nodes":[{{"commit":{{
                                "committedDate":"2026-08-23T00:00:00Z",
                                "statusCheckRollup":{{"contexts":{{"nodes":[]}}}}}}}}]}}}}"#
                        )
                    };
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(5, r#"{"name":"ci-queue"}"#),
                        node(9, "")
                    )
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // Pass one, and four more inside the twenty minutes. #5 is the front and waits; #9 is
        // behind it and is passed over. This is the reported failure, and up to here it is CORRECT
        // — a wait that has not gone on long enough to be suspicious.
        for _ in 0..5 {
            let _ = sweep();
        }
        assert!(
            labelled.lock().unwrap().is_empty(),
            "the train acted past a waiting front: {:?}",
            labelled.lock().unwrap()
        );
        assert_eq!(stopped("demo", 5), None, "a wait was stopped far too early");
        // …and the wait was written down once, not once per pass, with the step it is on.
        let waits: Vec<JournalEntry> = journal("demo", 5)
            .into_iter()
            .filter(|e| e.kind == "waiting")
            .collect();
        assert_eq!(
            waits.len(),
            1,
            "five passes wrote {} waiting entries: one is the clock being started, none is no \
             clock at all, and more than one is a journal turning into a log file",
            waits.len()
        );
        assert_eq!(waits[0].step, 4, "the wait must name the step it is on");

        // Twenty minutes pass. Written into the timeline, because `record` stamps `now`.
        let mut all = journal("demo", 5);
        let last = all.len() - 1;
        all[last].at_ms -= WAITING_ON_NOTHING_MS + 1;
        let mut file: std::collections::BTreeMap<String, Vec<JournalEntry>> =
            serde_json::from_str(&std::fs::read_to_string(journal_path("demo")).unwrap()).unwrap();
        file.insert("5".into(), all);
        std::fs::write(
            journal_path("demo"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();

        // The pass on the far side of the clock: #5 stops, and says what it waited for.
        let _ = sweep();
        let why = stopped("demo", 5).unwrap_or_else(|| {
            panic!("a front that has waited twenty minutes on a check that never started is still holding the line, silently")
        });
        assert!(
            why.contains("waiting for GitHub to catch up") && why.contains("checks: none"),
            "the stop does not say what it waited for or why the wait cannot end: {why}"
        );
        assert!(
            why.contains("minutes"),
            "the stop does not say how long it waited, so nobody can judge it: {why}"
        );

        // And the line moves: the serial pass-over rule now finds #9 at the front, and it acts.
        let _ = sweep();
        let calls = labelled.lock().unwrap().clone();
        assert!(
            calls.iter().any(|c| c.contains("/issues/9/labels")),
            "#5 stopped and the train still did not move on to #9: {calls:?}"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_PR_WORKFLOWS",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// A check that IS running is never cut short, however long it takes.
    ///
    /// The other side of [`a_wait_with_nothing_behind_it`], and the more dangerous one: the bound
    /// that fixes SKEIN-240 is the only thing in skein that can stop a pull request for taking too
    /// long, and a CI run is allowed to take as long as it takes. The module note above builds the
    /// whole guarded-step design around surviving *"a forty-minute CI run"*, so a train that
    /// stopped one at twenty minutes would have traded a parked train for a broken one.
    ///
    /// `checks: pending` is the evidence that draws the line: a check that has started is
    /// something skein can point at and expect to end. The clock is not started, so it can never
    /// run out — asserted against a timeline that is a full day old, which is far past any bound
    /// this file could grow.
    #[test]
    fn a_wait_on_a_check_that_is_running_is_never_bounded() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flow = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,"steps":[
              {"when":[],"do":"wait:CI is running"}]}]}"#,
        )
        .unwrap()
        .remove(0);
        let chosen = Chosen {
            step: 0,
            act: Act::Wait("CI is running".into()),
        };
        let running = crate::workflow::Facts {
            checks: "pending".into(),
            ..Default::default()
        };

        // A day of waiting, written into the timeline — far past any bound this file could grow.
        // Each pull request gets its own, because a stop written by one assertion would otherwise
        // reset the next one's clock and it would pass without the rule ever being consulted.
        let a_day_of_waiting = |number: u64, flow: &str| {
            record("demo", number, flow, 1, "waiting", "CI is running");
            let mut all = journal("demo", number);
            let last = all.len() - 1;
            all[last].at_ms -= 24 * 60 * 60 * 1000;
            let mut file: std::collections::BTreeMap<String, Vec<JournalEntry>> =
                serde_json::from_str(&std::fs::read_to_string(journal_path("demo")).unwrap())
                    .unwrap();
            file.insert(number.to_string(), all);
            std::fs::write(
                journal_path("demo"),
                serde_json::to_vec_pretty(&file).unwrap(),
            )
            .unwrap();
        };
        a_day_of_waiting(7, "merge-train");

        assert_eq!(
            a_wait_with_nothing_behind_it("demo", 7, &flow, &chosen, &running, "CI is running"),
            None,
            "a check that is still running was stopped for taking too long — the bound must only \
             ever fall on a wait with nothing behind it"
        );
        assert_eq!(
            stopped("demo", 7),
            None,
            "a running check was stopped a day into a build that is allowed to take as long as it \
             takes"
        );

        // And the same wait with nothing running IS bounded, from the same timeline — so what
        // separates them is the evidence and not the clock.
        let nothing = crate::workflow::Facts {
            checks: "none".into(),
            ..Default::default()
        };
        assert!(
            a_wait_with_nothing_behind_it("demo", 7, &flow, &chosen, &nothing, "CI is running")
                .is_some(),
            "the bound never falls at all, on any wait"
        );

        // And a workflow that is not a train is left alone even then — on its OWN expired clock,
        // so the only thing that can spare it is being non-serial. A stop is a demand for
        // somebody's attention, earned when the alternative is a queue that has stopped moving; a
        // pull request blocking nobody is not costing anything by waiting, and stopping it would
        // be manufacturing work.
        let loose = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"loose","steps":[{"when":[],"do":"wait:CI is running"}]}]}"#,
        )
        .unwrap()
        .remove(0);
        a_day_of_waiting(8, "loose");
        assert_eq!(
            a_wait_with_nothing_behind_it("demo", 8, &loose, &chosen, &nothing, "CI is running"),
            None,
            "a workflow with no train behind it stopped a pull request for waiting, which costs a \
             person an interruption and nobody a queue"
        );
        assert_eq!(stopped("demo", 8), None, "and it wrote the stop down too");

        std::env::remove_var("SKEIN_HOME");
    }

    /// The journal keeps the timeline: an act, a flag, and the hand that cleared it, in order.
    ///
    /// The owner's words: *"I need to know exactly what is it working on, which step is it on,
    /// status of previous steps and so on."* The stops file answers only "why is it stopped now";
    /// this is the record of what already happened — written where the events happen, so a
    /// timeline read tomorrow says what the audit said today.
    #[test]
    fn the_journal_keeps_the_timeline_of_did_stopped_and_cleared() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, _heard) = github(200);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // An action that lands is a "did", carrying the flow, the 1-based step, and the sentence.
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false,
            })),
            "t",
        );
        assert!(matches!(out, Outcome::Did(_)), "{out:?}");
        let entries = journal("demo", 41);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(
            (
                entries[0].kind.as_str(),
                entries[0].flow.as_str(),
                entries[0].step
            ),
            ("did", "ship-mine", 4),
            "the entry must name the flow and the 1-based step: {entries:?}"
        );
        assert!(entries[0].what.contains("merged #41"), "{entries:?}");
        assert!(entries[0].at_ms > 0, "no timestamp: {entries:?}");

        // A flag is a "stopped", with the workflow's own reason.
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Flag("CI is red".into())),
            "t",
        );
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");

        // And a person clearing the stop is an event too — flow-less, step-less, but on record.
        clear("demo", 41);

        let entries = journal("demo", 41);
        let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["did", "stopped", "cleared"],
            "the timeline is not in the order things happened: {entries:?}"
        );
        assert_eq!(entries[1].what, "CI is red");
        assert_eq!(entries[1].step, 4, "a flag is a step and must say which");
        assert_eq!(
            (entries[2].flow.as_str(), entries[2].step),
            ("", 0),
            "a clear is nobody's step: {entries:?}"
        );

        // The other reader carries the same timelines, keyed numerically.
        let all = journals("demo");
        assert_eq!(all.keys().copied().collect::<Vec<_>>(), vec![41]);
        assert_eq!(all[&41].len(), 3);

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// A failed action journals the same "stopped" it writes to the stops file.
    #[test]
    fn a_failed_action_reaches_the_journal_as_stopped() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");
        let (base, _heard) = github(409);
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false,
            })),
            "t",
        );
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");
        let entries = journal("demo", 41);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].kind, "stopped");
        assert!(
            entries[0].what.contains("ship-mine") && entries[0].what.contains("could not be done"),
            "the journal must keep the failure's own sentence: {entries:?}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "SKEIN_PR_WORKFLOWS"] {
            std::env::remove_var(key);
        }
    }

    /// Each pull request keeps its newest fifty entries, and the oldest fall off.
    #[test]
    fn the_journal_caps_each_pull_request_at_its_newest_fifty() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        for i in 1..=55 {
            record("demo", 7, "train", 1, "did", &format!("event {i}"));
        }
        let entries = journal("demo", 7);
        assert_eq!(entries.len(), 50, "the cap did not hold");
        assert_eq!(
            entries[0].what, "event 6",
            "the OLDEST must fall off, not the newest"
        );
        assert_eq!(entries[49].what, "event 55");

        // Another pull request's timeline is untouched by #7's churn.
        record("demo", 9, "train", 1, "did", "only one");
        assert_eq!(journal("demo", 9).len(), 1);
        assert_eq!(journal("demo", 7).len(), 50);

        std::env::remove_var("SKEIN_HOME");
    }

    /// A corrupt journal reads as empty, never as an error — history lost is not action stopped.
    #[test]
    fn a_corrupt_journal_reads_as_empty() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        std::fs::create_dir_all(crate::prq::review_dir("demo")).unwrap();
        std::fs::write(journal_path("demo"), b"{ this is not json").unwrap();
        assert!(journal("demo", 7).is_empty());
        assert!(journals("demo").is_empty());
        // And the next write recovers rather than failing forever on the bad file.
        record("demo", 7, "train", 1, "did", "back on the rails");
        assert_eq!(journal("demo", 7).len(), 1);

        std::env::remove_var("SKEIN_HOME");
    }

    /// The train view names the front, the whole line, and the passed-over — and only for a
    /// workflow that is serial, because a train is the serial thing.
    ///
    /// This is the panel's read of the same rule the tick acts on ([`sweep`] calls [`trains`]
    /// too), so what it asserts is the rule itself: oldest first, the first unstopped one is the
    /// front, a stopped PR is in the line AND named with its reason.
    #[test]
    fn a_train_view_names_the_front_the_line_and_the_passed_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[
              {"name":"train","serial":true,"steps":[{"when":[],"do":"merge:squash"}]},
              {"name":"loose","steps":[{"when":[],"do":"add-label:ci"}]}]}"#,
        )
        .unwrap();
        stop("demo", 5, "CI is red");

        // Handed over scrambled, so the order below is the function's own and not the caller's.
        let prs = vec![
            (9, "train".to_string()),
            (3, "loose".to_string()),
            (5, "train".to_string()),
            (7, "train".to_string()),
        ];
        let views = trains("demo", &prs, &flows);
        assert_eq!(
            views.len(),
            1,
            "a non-serial workflow got a train view: {views:?}"
        );
        let view = &views[0];
        assert_eq!(view.flow, "train");
        assert_eq!(
            view.front,
            Some(7),
            "the front must be the oldest UNSTOPPED pull request"
        );
        assert_eq!(
            view.line,
            vec![5, 7, 9],
            "train order is oldest first, front included"
        );
        assert_eq!(view.stopped.len(), 1);
        assert_eq!(
            (view.stopped[0].number, view.stopped[0].why.as_str()),
            (5, "CI is red"),
            "the passed-over must be named with its reason"
        );

        // Everyone stopped: a train with nobody to move has no front, and still shows its line.
        stop("demo", 7, "conflicts");
        stop("demo", 9, "checks");
        let views = trains("demo", &prs, &flows);
        assert_eq!(views[0].front, None);
        assert_eq!(views[0].line, vec![5, 7, 9]);
        assert_eq!(views[0].stopped.len(), 3);

        std::env::remove_var("SKEIN_HOME");
    }
}
