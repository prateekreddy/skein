//! What a pull request should have happen to it, written down once and applied by skein.
//!
//! The owner's ask, verbatim: *"once the PR where I am an author is approved, then apply a specific
//! tag that enabled CI and then let the CI be completed, if not successful flag so that we can fix
//! it. If successful and mergeble merge automatically and delete the branch. If not mergable for say
//! if the base branch moved, then rebase without losing the approvals … then do the same."*
//!
//! # Guarded steps, not a script
//!
//! A workflow is an ordered list of **steps**, each a set of conditions on GitHub's current answer
//! and one action. It is re-evaluated from scratch every time the queue is read, and at most one
//! step fires per evaluation.
//!
//! That is not a stylistic choice. A script would have to survive a forty-minute CI run, a restarted
//! server, a rate limit and a closed laptop, and would have to keep its own place in the sequence. A
//! guarded step set keeps no place: **the state on GitHub is the program counter.** Crash anywhere
//! and the next poll resumes from wherever the pull request actually is, because that is the only
//! place the position was ever kept.
//!
//! One step per evaluation for the same reason. The moment an action lands, what skein believes is
//! one action out of date — the label is on but no check has been queued yet, so "checks passing" is
//! still true *from the previous run* — and a cascade would merge on it.
//!
//! # A closed set of actions
//!
//! Six actions, and no way to add a seventh from a file. An open-ended "run this command" would be a
//! different feature with a different blast radius: every action here is one skein can describe in
//! the audit and a person can undo, and that property does not survive an escape hatch.
//!
//! # What this module does NOT do
//!
//! Decide anything, or perform anything. It defines the vocabulary and reads it back off disk.
//! Deciding is the evaluator's job and acting is the doer's, deliberately in that order: the
//! evaluator is pure and can be tested against every state in the owner's example without a network,
//! and nothing can act until the thing that decides can be shown to be right.
//!
//! See `docs/pr-workflow.md`, which also carries GitHub's own answer on what rebasing does to an
//! approval — the fact that decides what the `update-branch` step may promise.

use serde::{Deserialize, Serialize};

/// One thing that must be true about a pull request for a step to apply.
///
/// Every variant is answerable from the queue skein already fetches (`prq::Pr`) — no condition may
/// need a call of its own, or evaluating a workflow would cost a round trip per step per PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cond {
    /// Approved, by the repository's own reckoning, against the current head.
    Approved,
    /// Not approved — including an approval that was dismissed when the branch moved.
    NotApproved,
    /// Changes were requested and not yet resolved.
    ChangesRequested,
    /// This label is on it.
    Label(String),
    /// This label is not on it.
    NoLabel(String),
    /// The check rollup: `passing`, `failing`, `pending`, or `none`.
    Checks(String),
    /// GitHub says it can be merged as it stands.
    Mergeable,
    /// GitHub says it cannot — a conflict, or a base that has moved under it.
    NotMergeable,
    /// It is a draft.
    Draft,
    /// It is not a draft.
    Ready,
    /// You opened it. The one condition about a person, and it means the fleet's own login.
    Mine,
}

/// How a branch is brought up to date with its base.
///
/// Both move the merge base, and on a repository that dismisses stale approvals both therefore cost
/// the approval — see `docs/pr-workflow.md`. The choice is about the history you want, not about
/// keeping a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Update {
    Merge,
    Rebase,
}

/// How a pull request is merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeAs {
    Merge,
    Squash,
    Rebase,
}

/// A merge, and whether the branch goes with it.
///
/// **Deleting the branch is part of merging, and cannot be a step of its own.** That is not a
/// convenience — it is forced by what the queue can see. The queue is `is:pr is:open`
/// (`prq.rs`), so the moment a pull request merges it leaves the queue and nothing evaluates it
/// again: a following `delete-branch` step would never fire. And a `delete-branch` step that DID
/// fire, on a pull request still open, would delete the head branch of an open PR — which closes
/// it. So the action is useless in the one place it could run and harmful in the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merge {
    pub how: MergeAs,
    pub delete_branch: bool,
}

/// The one thing a step does. A closed set, on purpose — see the module note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Act {
    /// Put a label on it. This is how a repository's CI is usually started.
    AddLabel(String),
    RemoveLabel(String),
    /// Bring the branch up to date with its base.
    UpdateBranch(Update),
    Merge(Merge),
    /// Say something on the row and stop. The end state for anything a person has to look at.
    Flag(String),
    /// Do nothing, and keep waiting. Named rather than implied, because "waiting for CI" and "no
    /// step applies" are different answers and a person reading the row deserves the first one.
    Wait(String),
}

/// One guarded step: every condition must hold, and then this happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub when: Vec<Cond>,
    pub act: Act,
}

/// A named set of steps, and who they apply to by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    /// Conditions a pull request must meet for this workflow to be offered automatically. Empty
    /// means it is never automatic — it only ever runs where somebody assigned it by hand.
    ///
    /// The owner asked for both: "every PR I author" as a rule, and assignment on a single row. A
    /// per-PR assignment always wins over a match, in both directions — including an explicit "no
    /// workflow" on a PR a rule would otherwise claim.
    #[serde(default)]
    pub matches: Vec<Cond>,
    pub steps: Vec<Step>,
}

/// The file as it is written down. Deliberately not [`Workflow`]: what is on disk is strings, and
/// turning strings into a closed vocabulary is the whole of the checking this module does.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Written {
    #[serde(default)]
    workflow: Vec<WrittenFlow>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct WrittenFlow {
    name: String,
    #[serde(default)]
    matches: Vec<String>,
    #[serde(default)]
    steps: Vec<WrittenStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct WrittenStep {
    #[serde(default)]
    when: Vec<String>,
    /// `do` in the file, because that is what it is. It is a keyword in Rust and nowhere else.
    #[serde(rename = "do")]
    act: String,
}

/// Every condition that can be written, with the spelling used on disk and in the UI.
///
/// One table, so the parser and the picker cannot disagree about what exists — a dropdown offering
/// something the parser refuses is the same defect as a parser accepting something no dropdown can
/// produce, and both are found only by a person typing it.
pub const CONDITIONS: [(&str, &str); 11] = [
    ("approved", "approved, against the commit that is there now"),
    (
        "not-approved",
        "not approved, or the approval was dismissed",
    ),
    ("changes-requested", "changes were requested"),
    ("label:<name>", "carries this label"),
    ("no-label:<name>", "does not carry this label"),
    (
        "checks:<state>",
        "checks are passing, failing, pending or none",
    ),
    ("mergeable", "GitHub can merge it as it stands"),
    (
        "not-mergeable",
        "a conflict, or the base has moved under it",
    ),
    ("draft", "still a draft"),
    ("ready", "marked ready for review"),
    ("mine", "you opened it"),
];

/// Every action that can be written. See [`CONDITIONS`] for why this is a table.
pub const ACTIONS: [(&str, &str); 9] = [
    (
        "add-label:<name>",
        "put a label on it — usually what starts CI",
    ),
    ("remove-label:<name>", "take a label off"),
    ("update-branch:rebase", "rebase the branch onto its base"),
    ("update-branch:merge", "merge the base into the branch"),
    ("merge:squash", "squash and merge"),
    (
        "merge:squash+delete",
        "squash and merge, then delete the branch",
    ),
    ("merge:merge", "merge with a merge commit"),
    ("merge:merge+delete", "merge, then delete the branch"),
    ("flag:<why>", "say this on the row, and stop"),
];

/// One word of the vocabulary, taken apart for a picker.
///
/// The tables are the source for both the parser and the cockpit's dropdowns, so the split between
/// "which kind" and "what argument" is made HERE rather than by the page re-parsing `label:<name>`
/// with a regex. A second parser is a second thing to disagree with the first, and the disagreement
/// would appear as a workflow somebody builds in the UI and cannot save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Word {
    /// The part before the colon: `label`, `merge`, `checks`.
    pub kind: String,
    /// What goes after it, as a word for a placeholder — `name`, `state`, `why` — or empty where
    /// the word takes no argument.
    pub arg: String,
    /// The whole spelling as it is written in the file, `<…>` included.
    pub spelling: String,
    pub help: String,
    /// Where the argument is a closed set, every value it may take. Empty means free text.
    ///
    /// `checks` is the one that has this, and it exists because a text box would accept `green` —
    /// which is exactly the mistake the first fixture written against this vocabulary made. A word
    /// with four possible values should be four things you can choose, not four things you can
    /// mistype.
    pub choices: Vec<String>,
}

fn words(table: &[(&str, &str)]) -> Vec<Word> {
    table
        .iter()
        .map(|(spelling, help)| {
            let (kind, rest) = split(spelling);
            Word {
                kind: kind.to_string(),
                // `<name>` is a placeholder; `rebase` in `update-branch:rebase` is part of the word
                // itself and the picker must offer it as its own entry rather than as a blank.
                arg: rest
                    .strip_prefix('<')
                    .and_then(|r| r.strip_suffix('>'))
                    .unwrap_or_default()
                    .to_string(),
                spelling: spelling.to_string(),
                help: help.to_string(),
                choices: match kind {
                    "checks" => ["passing", "failing", "pending", "none"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    _ => Vec::new(),
                },
            }
        })
        .collect()
}

/// Every condition a picker may offer.
pub fn conditions() -> Vec<Word> {
    words(&CONDITIONS)
}

/// Every action a picker may offer.
pub fn actions() -> Vec<Word> {
    words(&ACTIONS)
}

fn split(atom: &str) -> (&str, &str) {
    match atom.split_once(':') {
        Some((head, rest)) => (head.trim(), rest.trim()),
        None => (atom.trim(), ""),
    }
}

/// What to say when something is not in the vocabulary: the word, and every word that is.
///
/// The list rather than "unknown condition", because the reader's next question is always "well,
/// what CAN I say" and a message that does not answer it sends them to the source.
fn unknown(kind: &str, atom: &str, table: &[(&str, &str)]) -> String {
    let known = table
        .iter()
        .map(|(spelling, _)| *spelling)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{kind} {atom:?} is not one skein knows. It can be: {known}")
}

impl Cond {
    /// Read one condition, or say why not.
    pub fn parse(atom: &str) -> Result<Cond, String> {
        let (head, arg) = split(atom);
        let named = |what: &str| match arg.is_empty() {
            true => Err(format!(
                "{head:?} needs a {what} after a colon, as in {head}:something"
            )),
            false => Ok(arg.to_string()),
        };
        match head {
            "approved" => Ok(Cond::Approved),
            "not-approved" => Ok(Cond::NotApproved),
            "changes-requested" => Ok(Cond::ChangesRequested),
            "label" => Ok(Cond::Label(named("label")?)),
            "no-label" => Ok(Cond::NoLabel(named("label")?)),
            "checks" => {
                let state = named("state")?;
                match state.as_str() {
                    "passing" | "failing" | "pending" | "none" => Ok(Cond::Checks(state)),
                    _ => Err(format!(
                        "checks can be passing, failing, pending or none — not {state:?}"
                    )),
                }
            }
            "mergeable" => Ok(Cond::Mergeable),
            "not-mergeable" => Ok(Cond::NotMergeable),
            "draft" => Ok(Cond::Draft),
            "ready" => Ok(Cond::Ready),
            "mine" => Ok(Cond::Mine),
            _ => Err(unknown("condition", atom, &CONDITIONS)),
        }
    }
}

impl Act {
    /// Read one action, or say why not.
    pub fn parse(atom: &str) -> Result<Act, String> {
        let (head, arg) = split(atom);
        let named = |what: &str| match arg.is_empty() {
            true => Err(format!(
                "{head:?} needs a {what} after a colon, as in {head}:something"
            )),
            false => Ok(arg.to_string()),
        };
        match head {
            "add-label" => Ok(Act::AddLabel(named("label")?)),
            "remove-label" => Ok(Act::RemoveLabel(named("label")?)),
            "update-branch" => match arg {
                "rebase" => Ok(Act::UpdateBranch(Update::Rebase)),
                "merge" => Ok(Act::UpdateBranch(Update::Merge)),
                _ => Err(format!(
                    "update-branch is rebase or merge — not {arg:?}. Neither keeps an approval on a \
                     repository that dismisses stale ones; see docs/pr-workflow.md"
                )),
            },
            "merge" => {
                // `+delete` rather than a step of its own — see [`Merge`].
                let (how, delete_branch) = match arg.strip_suffix("+delete") {
                    Some(how) => (how, true),
                    None => (arg, false),
                };
                let how = match how {
                    "squash" => MergeAs::Squash,
                    "merge" => MergeAs::Merge,
                    "rebase" => MergeAs::Rebase,
                    _ => {
                        return Err(format!(
                            "merge is squash, merge or rebase, each optionally +delete to remove \
                             the branch as well — not {arg:?}"
                        ))
                    }
                };
                Ok(Act::Merge(Merge { how, delete_branch }))
            }
            "flag" => Ok(Act::Flag(named("reason")?)),
            "wait" => Ok(Act::Wait(named("reason")?)),
            _ => Err(unknown("action", atom, &ACTIONS)),
        }
    }
}

/// Where the fleet's workflows are written down.
///
/// Beside skein's own state rather than in a repo: the owner asked for several workflows assignable
/// to any pull request in any repo, so they belong to the fleet. Which PR carries which is a
/// separate, per-repo question.
pub fn workflows_path() -> std::path::PathBuf {
    crate::config::skein_home().join("workflows.json")
}

/// Read every workflow, or refuse the file.
///
/// **All or nothing.** A file with one bad step does not load its good ones: a workflow that
/// silently lost the step between "checks passed" and "merge" is a workflow that merges without
/// checks, and half of an automation is worse than none of it. The error names the workflow, the
/// step and the word.
pub fn load() -> Result<Vec<Workflow>, String> {
    let path = workflows_path();
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        // No file is not a fault: it is a fleet where nobody has written one.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    from_bytes(&raw)
}

/// The half of [`load`] that has no filesystem in it, so every refusal can be tested.
pub fn from_bytes(raw: &[u8]) -> Result<Vec<Workflow>, String> {
    if raw.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Vec::new());
    }
    let written: Written =
        serde_json::from_slice(raw).map_err(|e| format!("this is not a workflow file: {e}"))?;
    let mut out = Vec::new();
    for flow in written.workflow {
        let at = |what: &str| format!("workflow {:?}: {what}", flow.name);
        if flow.name.trim().is_empty() {
            return Err("a workflow with no name cannot be assigned to anything".into());
        }
        if flow.steps.is_empty() {
            return Err(at("has no steps, so it would never do anything"));
        }
        let mut matches = Vec::new();
        for atom in &flow.matches {
            matches.push(Cond::parse(atom).map_err(|why| at(&why))?);
        }
        let mut steps = Vec::new();
        for (n, step) in flow.steps.iter().enumerate() {
            let at = |what: &str| format!("workflow {:?}, step {}: {what}", flow.name, n + 1);
            let mut when = Vec::new();
            for atom in &step.when {
                when.push(Cond::parse(atom).map_err(|why| at(&why))?);
            }
            steps.push(Step {
                when,
                act: Act::parse(&step.act).map_err(|why| at(&why))?,
            });
        }
        if out.iter().any(|w: &Workflow| w.name == flow.name) {
            return Err(at(
                "is defined twice, and skein cannot tell which one you meant",
            ));
        }
        out.push(Workflow {
            name: flow.name,
            matches,
            steps,
        });
    }
    Ok(out)
}

/// Write the file, having first proved skein can read back what it is about to write.
///
/// The editor sends a whole file, so this is the one moment a person can replace every workflow in
/// the fleet with something that does not parse. It is checked BEFORE the write, not after: a
/// refusal that arrives after the old file is gone is a refusal that cost somebody their workflows.
///
/// Returns what was saved, so a caller can answer with what it will read back rather than with what
/// it was handed.
pub fn save(raw: &[u8]) -> Result<Vec<Workflow>, String> {
    let flows = from_bytes(raw)?;
    let path = workflows_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // Written from what was PARSED rather than from the bytes, so the file on disk is always in the
    // shape this module writes — an editor cannot leave a comment, a stray field or an ordering
    // that reads back differently the next time.
    let body = to_bytes(&flows)?;
    let temp = path.with_extension("json.new");
    std::fs::write(&temp, &body).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(flows)
}

/// Write workflows back, in the shape [`load`] reads.
///
/// Round-tripping matters more here than in most places: the cockpit edits these, and an editor
/// that cannot read back what it wrote is one that quietly loses a step.
pub fn to_bytes(flows: &[Workflow]) -> Result<Vec<u8>, String> {
    let written = Written {
        workflow: flows
            .iter()
            .map(|w| WrittenFlow {
                name: w.name.clone(),
                matches: w.matches.iter().map(spell_cond).collect(),
                steps: w
                    .steps
                    .iter()
                    .map(|s| WrittenStep {
                        when: s.when.iter().map(spell_cond).collect(),
                        act: spell_act(&s.act),
                    })
                    .collect(),
            })
            .collect(),
    };
    serde_json::to_vec_pretty(&written).map_err(|e| format!("could not write workflows: {e}"))
}

/// How a condition is written down. The inverse of [`Cond::parse`], and tested against it.
pub fn spell_cond(cond: &Cond) -> String {
    match cond {
        Cond::Approved => "approved".into(),
        Cond::NotApproved => "not-approved".into(),
        Cond::ChangesRequested => "changes-requested".into(),
        Cond::Label(l) => format!("label:{l}"),
        Cond::NoLabel(l) => format!("no-label:{l}"),
        Cond::Checks(s) => format!("checks:{s}"),
        Cond::Mergeable => "mergeable".into(),
        Cond::NotMergeable => "not-mergeable".into(),
        Cond::Draft => "draft".into(),
        Cond::Ready => "ready".into(),
        Cond::Mine => "mine".into(),
    }
}

/// How an action is written down. The inverse of [`Act::parse`], and tested against it.
pub fn spell_act(act: &Act) -> String {
    match act {
        Act::AddLabel(l) => format!("add-label:{l}"),
        Act::RemoveLabel(l) => format!("remove-label:{l}"),
        Act::UpdateBranch(Update::Rebase) => "update-branch:rebase".into(),
        Act::UpdateBranch(Update::Merge) => "update-branch:merge".into(),
        Act::Merge(m) => format!(
            "merge:{}{}",
            match m.how {
                MergeAs::Squash => "squash",
                MergeAs::Merge => "merge",
                MergeAs::Rebase => "rebase",
            },
            match m.delete_branch {
                true => "+delete",
                false => "",
            }
        ),
        Act::Flag(why) => format!("flag:{why}"),
        Act::Wait(why) => format!("wait:{why}"),
    }
}

/// Everything a workflow may ask about a pull request, as skein sees it right now.
///
/// Plain data with no GitHub in it, so the deciding can be tested against every state in the
/// owner's example without a network — and so that this module keeps its one dependency. Whoever
/// has a queue builds these; nothing here knows where they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Approved against the commit that is there NOW. An approval of an earlier head is not one.
    pub approved: bool,
    pub changes_requested: bool,
    pub labels: Vec<String>,
    /// `passing` | `failing` | `pending` | `none`.
    pub checks: String,
    /// `None` when GitHub has not worked it out yet, which it reports as `UNKNOWN` for a while
    /// after every push. **Not the same as "cannot be merged"** — see [`holds`].
    pub mergeable: Option<bool>,
    pub draft: bool,
    /// You opened it.
    pub mine: bool,
}

/// The step a workflow would take next, and where it is in the file.
///
/// The index is carried because everything downstream needs to name it: the audit says which step
/// acted, and the row says which step it is waiting on. "The third one" is the only durable name a
/// step has — they have no ids, deliberately, since a file people edit should not require them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub step: usize,
    pub act: Act,
}

/// Does this condition hold?
///
/// The one subtlety is `mergeable`. GitHub computes it asynchronously and says `UNKNOWN` for a
/// while after every push, so a workflow that treated unknown as "not mergeable" would rebase a
/// pull request for no reason — and on a repository that dismisses stale approvals, that rebase
/// costs the approval that authorised it (`docs/pr-workflow.md`). **Unknown satisfies neither
/// `mergeable` nor `not-mergeable`**: skein waits until GitHub has an answer.
pub fn holds(cond: &Cond, facts: &Facts) -> bool {
    match cond {
        Cond::Approved => facts.approved,
        Cond::NotApproved => !facts.approved,
        Cond::ChangesRequested => facts.changes_requested,
        Cond::Label(want) => facts.labels.iter().any(|l| l == want),
        Cond::NoLabel(want) => !facts.labels.iter().any(|l| l == want),
        Cond::Checks(want) => &facts.checks == want,
        Cond::Mergeable => facts.mergeable == Some(true),
        Cond::NotMergeable => facts.mergeable == Some(false),
        Cond::Draft => facts.draft,
        Cond::Ready => !facts.draft,
        Cond::Mine => facts.mine,
    }
}

/// The one step this workflow would take on this pull request, or nothing.
///
/// **One step, and the first one that applies.** Not a cascade: the moment an action lands, what
/// skein believes is one action out of date — the label is on but no check has been queued yet, so
/// `checks:passing` is still true from the *previous* run, and a cascade would merge on it. The
/// next poll re-reads GitHub, which is the only thing that can say what the label did.
///
/// `None` is a real answer and not a fallthrough: "no step applies" is what a healthy workflow says
/// most of the time, and whoever calls this shows it as such rather than as a failure to decide.
///
/// Nothing here acts. Same function drives the dry run and the tick, so what a person is shown
/// before they trust it is by construction what will happen.
pub fn next(flow: &Workflow, facts: &Facts) -> Option<Chosen> {
    flow.steps
        .iter()
        .enumerate()
        .find(|(_, step)| step.when.iter().all(|cond| holds(cond, facts)))
        .map(|(step, s)| Chosen {
            step,
            act: s.act.clone(),
        })
}

/// Does this workflow claim this pull request on its own?
///
/// Empty `matches` means never — a workflow with no rule runs only where somebody assigned it by
/// hand. That is the safe direction: the cost of a rule that never fires is that you assign it
/// yourself; the cost of one that fires on everything is a merge you did not ask for.
pub fn claims(flow: &Workflow, facts: &Facts) -> bool {
    !flow.matches.is_empty() && flow.matches.iter().all(|cond| holds(cond, facts))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's own workflow, as it would be written down. Shared by the tests that read it back
    /// and the ones that walk a pull request through it, so the thing being evaluated is the thing
    /// somebody asked for rather than a fixture shaped to pass.
    const EXAMPLE: &[u8] = br#"{
      "workflow": [{
        "name": "ship-mine",
        "matches": ["mine"],
        "steps": [
          { "when": ["approved", "no-label:ci"],        "do": "add-label:ci" },
          { "when": ["checks:pending"],                 "do": "wait:CI is running" },
          { "when": ["checks:failing"],                 "do": "flag:CI is red" },
          { "when": ["approved", "mergeable", "checks:passing"], "do": "merge:squash+delete" },
          { "when": ["approved", "not-mergeable"],      "do": "update-branch:rebase" }
        ]
      }]
    }"#;

    /// The owner's own example, written down and read back.
    ///
    /// Not a synthetic fixture on purpose: if the thing that was asked for cannot be said in this
    /// vocabulary, the vocabulary is wrong, and that is a thing to find out before anything can act
    /// on it. Every step here comes from the sentence in `docs/pr-workflow.md`.
    #[test]
    fn the_workflow_that_was_asked_for_can_be_written_down() {
        let file = br#"{
          "workflow": [{
            "name": "ship-mine",
            "matches": ["mine"],
            "steps": [
              { "when": ["approved", "no-label:ci"],        "do": "add-label:ci" },
              { "when": ["checks:pending"],                 "do": "wait:CI is running" },
              { "when": ["checks:failing"],                 "do": "flag:CI is red" },
              { "when": ["approved", "not-mergeable"],      "do": "update-branch:rebase" },
              { "when": ["approved", "mergeable", "checks:passing"], "do": "merge:squash+delete" }
            ]
          }]
        }"#;
        let flows = from_bytes(file).expect("the owner's example must be expressible");
        assert_eq!(flows.len(), 1);
        let flow = &flows[0];
        assert_eq!(flow.name, "ship-mine");
        assert_eq!(flow.matches, vec![Cond::Mine]);
        assert_eq!(flow.steps.len(), 5);
        assert_eq!(
            flow.steps[0],
            Step {
                when: vec![Cond::Approved, Cond::NoLabel("ci".into())],
                act: Act::AddLabel("ci".into()),
            }
        );
        assert_eq!(flow.steps[3].act, Act::UpdateBranch(Update::Rebase));
        assert_eq!(
            flow.steps[4].act,
            Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true
            }),
            "the owner asked for merge AND delete, which is one action because the queue only \
             lists open pull requests"
        );

        // And it survives being written back out, because the cockpit edits these. An editor that
        // cannot read back what it wrote loses a step, and the step it loses is the one nobody
        // notices until a pull request merges without it.
        let again = from_bytes(&to_bytes(&flows).unwrap()).unwrap();
        assert_eq!(again, flows, "a workflow did not survive the round trip");
    }

    /// The example, walked from opened to merged, one poll at a time.
    ///
    /// This is the whole feature asserted end to end without a network: at every state the pull
    /// request can be in, what would skein do next? Table-driven because the interesting failures
    /// are at the boundaries between states — and because a workflow that does the right thing in
    /// four states and merges in the fifth is worse than one that does nothing.
    #[test]
    fn the_example_walks_from_opened_to_merged_one_step_at_a_time() {
        let flow = &from_bytes(EXAMPLE).unwrap()[0];
        let facts = |approved, labels: &[&str], checks: &str, mergeable| Facts {
            approved,
            labels: labels.iter().map(|l| l.to_string()).collect(),
            checks: checks.into(),
            mergeable,
            mine: true,
            ..Default::default()
        };
        let act = |f: &Facts| next(flow, f).map(|c| c.act);

        // Opened, nobody has looked at it: a workflow that acted here would be acting on unreviewed
        // code, which is the whole thing the owner's first condition is for.
        assert_eq!(act(&facts(false, &[], "none", None)), None);

        // Approved. The label is what starts this repository's CI.
        assert_eq!(
            act(&facts(true, &[], "none", None)),
            Some(Act::AddLabel("ci".into()))
        );

        // CI is running. "Wait" is a said thing, not an absence — the row can tell somebody what it
        // is waiting for, which is the difference between patience and a stall.
        assert_eq!(
            act(&facts(true, &["ci"], "pending", None)),
            Some(Act::Wait("CI is running".into()))
        );

        // CI is red. It stops, and says so: the owner asked to be told rather than have it retried.
        assert_eq!(
            act(&facts(true, &["ci"], "failing", Some(true))),
            Some(Act::Flag("CI is red".into()))
        );

        // Green and mergeable: merge it, and the branch goes with it.
        assert_eq!(
            act(&facts(true, &["ci"], "passing", Some(true))),
            Some(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true
            }))
        );

        // The base moved under it.
        assert_eq!(
            act(&facts(true, &["ci"], "passing", Some(false))),
            Some(Act::UpdateBranch(Update::Rebase))
        );

        // **And the state that is neither.** GitHub says UNKNOWN for a while after every push while
        // it works out whether the branch merges. Treating that as "not mergeable" rebases a pull
        // request for no reason — and on a repository that dismisses stale approvals, that rebase
        // throws away the approval that authorised it. So skein waits for GitHub to have an answer.
        assert_eq!(
            act(&facts(true, &["ci"], "passing", None)),
            None,
            "an unknown mergeable state was treated as a conflict, and rebased on a guess"
        );

        // After the rebase the approval may be gone — that is the repository's setting, not skein's
        // doing (docs/pr-workflow.md). What matters here is that it does NOT go on merging.
        assert_eq!(act(&facts(false, &["ci"], "passing", Some(true))), None);
    }

    /// The first step that applies is the one that happens, and only that one.
    #[test]
    fn one_step_fires_and_it_is_the_first_that_applies() {
        let flow = &from_bytes(
            br#"{"workflow":[{"name":"w","steps":[
              {"when":["approved"],  "do":"add-label:ci"},
              {"when":["approved"],  "do":"merge:squash"}]}]}"#,
        )
        .unwrap()[0];
        let chosen = next(
            flow,
            &Facts {
                approved: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(chosen.step, 0, "a later step won over an earlier one");
        assert_eq!(chosen.act, Act::AddLabel("ci".into()));
    }

    /// A workflow with no rule of its own never claims a pull request.
    ///
    /// The safe direction, and worth a test because the opposite reads as more helpful: the cost of
    /// a rule that never fires is that you assign it yourself. The cost of one that fires on
    /// everything is a merge nobody asked for.
    #[test]
    fn a_workflow_with_no_rule_claims_nothing() {
        let mine = Facts {
            mine: true,
            approved: true,
            ..Default::default()
        };
        let flows = from_bytes(EXAMPLE).unwrap();
        assert!(claims(&flows[0], &mine), "a rule that matches must claim");
        assert!(
            !claims(
                &flows[0],
                &Facts {
                    mine: false,
                    ..mine.clone()
                }
            ),
            "somebody else's pull request was claimed by a rule about yours"
        );

        let unruled = &from_bytes(
            br#"{"workflow":[{"name":"w","steps":[{"when":[],"do":"merge:squash"}]}]}"#,
        )
        .unwrap()[0];
        assert!(
            !claims(unruled, &mine),
            "a workflow with no rule claimed a pull request anyway"
        );
    }

    /// A file skein does not fully understand does not half-load.
    ///
    /// The failure this prevents: a workflow whose "checks have passed" step was dropped because of
    /// a typo, leaving the merge step with nothing in front of it. Half an automation is worse than
    /// none — so the whole file is refused, and the message says which workflow, which step, and
    /// what it can say instead.
    #[test]
    fn a_file_it_does_not_understand_is_refused_whole() {
        let bad = br#"{
          "workflow": [{
            "name": "ship-mine",
            "steps": [
              { "when": ["approved"], "do": "add-label:ci" },
              { "when": ["checks:green"], "do": "merge:squash" }
            ]
          }]
        }"#;
        let why = from_bytes(bad).expect_err("a condition nobody defined must not load");
        assert!(
            why.contains("ship-mine") && why.contains("step 2"),
            "the refusal must say where to look: {why}"
        );
        assert!(
            why.contains("passing"),
            "and what can be said instead of the word it refused: {why}"
        );

        // An action nobody defined, with the same treatment — and the list, because "unknown
        // action" leaves the reader exactly where they were.
        let bad = br#"{"workflow":[{"name":"w","steps":[{"when":[],"do":"deploy:prod"}]}]}"#;
        let why = from_bytes(bad).expect_err("an action nobody defined must not load");
        assert!(
            why.contains("deploy:prod") && why.contains("add-label"),
            "the refusal names the word and the vocabulary: {why}"
        );

        // A step whose action needs an argument and was not given one.
        let bad = br#"{"workflow":[{"name":"w","steps":[{"when":[],"do":"add-label"}]}]}"#;
        assert!(
            from_bytes(bad).is_err(),
            "a label with no name is not a label"
        );

        // A workflow with no steps would sit on a pull request doing nothing, for ever, looking
        // like automation.
        let bad = br#"{"workflow":[{"name":"empty","steps":[]}]}"#;
        assert!(
            from_bytes(bad).is_err(),
            "a workflow that cannot act is not one"
        );

        // Two workflows with one name: whichever skein picked, half the assignments would mean the
        // other one.
        let bad = br#"{"workflow":[
          {"name":"w","steps":[{"when":[],"do":"merge:merge"}]},
          {"name":"w","steps":[{"when":[],"do":"merge:squash"}]}]}"#;
        assert!(
            from_bytes(bad).is_err(),
            "a name that means two things means neither"
        );

        // And no file at all is a fleet where nobody has written one, which is not a fault.
        assert_eq!(from_bytes(b"").unwrap(), Vec::new());
        assert_eq!(from_bytes(b"  \n ").unwrap(), Vec::new());
    }

    /// A picker built from the vocabulary produces words the parser takes.
    ///
    /// The page renders a kind and, where there is one, a box for its argument — and then joins them
    /// back with a colon. That join is the moment a UI can produce something no file could contain,
    /// so it is done against the same table the parser reads and asserted here.
    #[test]
    fn what_a_picker_would_build_is_what_the_parser_reads() {
        for word in conditions() {
            let built = match word.arg.is_empty() {
                true => word.kind.clone(),
                false => format!("{}:{}", word.kind, "ci"),
            };
            // `checks` is the one whose argument is a closed set rather than free text; the picker
            // offers those four, so the test uses one of them.
            let built = match word.kind.as_str() {
                "checks" => "checks:passing".to_string(),
                _ => built,
            };
            Cond::parse(&built)
                .unwrap_or_else(|why| panic!("a picker would build {built:?}, refused: {why}"));
        }
        for word in actions() {
            let built = match word.arg.is_empty() {
                true => word.spelling.clone(),
                false => format!("{}:{}", word.kind, "something"),
            };
            Act::parse(&built)
                .unwrap_or_else(|why| panic!("a picker would build {built:?}, refused: {why}"));
        }
        // And the words with no argument are offered whole, so `update-branch:rebase` is one entry
        // rather than a kind with a box beside it that somebody could type `sideways` into.
        let update = actions()
            .into_iter()
            .filter(|w| w.kind == "update-branch")
            .collect::<Vec<_>>();
        assert_eq!(
            update.len(),
            2,
            "the two ways to update a branch must both be offered"
        );
        assert!(update.iter().all(|w| w.arg.is_empty()));

        // And where the argument is a closed set, the picker offers the set. Every value it offers
        // has to parse, or the UI can build `checks:green` — which is what the first fixture written
        // against this vocabulary actually said.
        let checks = conditions()
            .into_iter()
            .find(|w| w.kind == "checks")
            .unwrap();
        assert!(
            !checks.choices.is_empty(),
            "checks has four values and offers none"
        );
        for value in &checks.choices {
            Cond::parse(&format!("checks:{value}"))
                .unwrap_or_else(|why| panic!("the picker offers checks:{value}, refused: {why}"));
        }
    }

    /// Every word in the pickers is a word the parser accepts.
    ///
    /// The tables exist so the cockpit's dropdowns and the parser cannot drift apart. That is only
    /// true if something checks it: a picker offering something the parser refuses is a workflow
    /// somebody builds in the UI and cannot save, and it is found by a person, in the one moment
    /// they were trusting the tool.
    #[test]
    fn every_word_the_pickers_offer_is_one_the_parser_takes() {
        for (spelling, _) in CONDITIONS {
            let atom = spelling
                .replace("<name>", "ci")
                .replace("<state>", "passing");
            let cond = Cond::parse(&atom).unwrap_or_else(|why| {
                panic!("the pickers offer {spelling:?}, which is refused: {why}")
            });
            assert_eq!(
                spell_cond(&cond),
                atom,
                "a condition does not write back the way it was read"
            );
        }
        for (spelling, _) in ACTIONS {
            let atom = spelling
                .replace("<name>", "ci")
                .replace("<why>", "look at this");
            let act = Act::parse(&atom).unwrap_or_else(|why| {
                panic!("the pickers offer {spelling:?}, which is refused: {why}")
            });
            assert_eq!(
                spell_act(&act),
                atom,
                "an action does not write back the way it was read"
            );
        }
    }
}
