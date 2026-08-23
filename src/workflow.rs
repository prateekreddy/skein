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

/// The one thing a step does. A closed set, on purpose — see the module note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Act {
    /// Put a label on it. This is how a repository's CI is usually started.
    AddLabel(String),
    RemoveLabel(String),
    /// Bring the branch up to date with its base.
    UpdateBranch(Update),
    Merge(MergeAs),
    /// Delete the head branch. Only ever after a merge — see the evaluator.
    DeleteBranch,
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
pub const ACTIONS: [(&str, &str); 8] = [
    (
        "add-label:<name>",
        "put a label on it — usually what starts CI",
    ),
    ("remove-label:<name>", "take a label off"),
    ("update-branch:rebase", "rebase the branch onto its base"),
    ("update-branch:merge", "merge the base into the branch"),
    ("merge:squash", "squash and merge"),
    ("merge:merge", "merge with a merge commit"),
    ("delete-branch", "delete the head branch"),
    ("flag:<why>", "say this on the row, and stop"),
];

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
            "merge" => match arg {
                "squash" => Ok(Act::Merge(MergeAs::Squash)),
                "merge" => Ok(Act::Merge(MergeAs::Merge)),
                "rebase" => Ok(Act::Merge(MergeAs::Rebase)),
                _ => Err(format!("merge is squash, merge or rebase — not {arg:?}")),
            },
            "delete-branch" => Ok(Act::DeleteBranch),
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
        Act::Merge(MergeAs::Squash) => "merge:squash".into(),
        Act::Merge(MergeAs::Merge) => "merge:merge".into(),
        Act::Merge(MergeAs::Rebase) => "merge:rebase".into(),
        Act::DeleteBranch => "delete-branch".into(),
        Act::Flag(why) => format!("flag:{why}"),
        Act::Wait(why) => format!("wait:{why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
              { "when": ["approved", "mergeable", "checks:passing"], "do": "merge:squash" }
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
        assert_eq!(flow.steps[4].act, Act::Merge(MergeAs::Squash));

        // And it survives being written back out, because the cockpit edits these. An editor that
        // cannot read back what it wrote loses a step, and the step it loses is the one nobody
        // notices until a pull request merges without it.
        let again = from_bytes(&to_bytes(&flows).unwrap()).unwrap();
        assert_eq!(again, flows, "a workflow did not survive the round trip");
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
          {"name":"w","steps":[{"when":[],"do":"delete-branch"}]},
          {"name":"w","steps":[{"when":[],"do":"merge:squash"}]}]}"#;
        assert!(
            from_bytes(bad).is_err(),
            "a name that means two things means neither"
        );

        // And no file at all is a fleet where nobody has written one, which is not a fault.
        assert_eq!(from_bytes(b"").unwrap(), Vec::new());
        assert_eq!(from_bytes(b"  \n ").unwrap(), Vec::new());
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
