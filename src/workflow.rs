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
    /// Somebody has approved it and nobody's refusal is standing — what a person means by the
    /// word. See [`Facts::approved`] for what "standing" can and cannot be proved from.
    ///
    /// **This deliberately does NOT mean "the repository is satisfied"**, which is the confusion
    /// that made the merge train dead on arrival for every repo where review is social
    /// (SKEIN-339). That question is [`Cond::ReviewSatisfied`], and a workflow that merges wants
    /// BOTH of them.
    Approved,
    /// Nobody's approval is standing.
    NotApproved,
    /// Changes were requested and not yet resolved.
    ChangesRequested,
    /// The repository's own review requirement is not in the way — either it is met, or the
    /// repository asks for no review at all.
    ///
    /// The other half of [`Cond::Approved`], and the half skein cannot reason about: on a
    /// protected branch GitHub's `reviewDecision` folds in CODEOWNERS, the required-approvals
    /// count and every other rule skein has no way to read. A train that merged on approvals it
    /// can count would be refused by GitHub — and a refused action is a stop somebody has to
    /// clear, per this module's no-blind-retries rule.
    ///
    /// See [`Facts::review_requirement_met`] for why a repository with no requirement satisfies
    /// this rather than failing it.
    ReviewSatisfied,
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
    /// The base has commits this branch lacks — GitHub's `mergeStateStatus` says `BEHIND`.
    Behind,
    /// Known up to date with its base. Not merely "not behind": an unknown state satisfies
    /// neither this nor [`Cond::Behind`] — see [`holds`].
    Current,
    /// Its base is the repository's default branch. What keeps a merge train off stacked
    /// children: a child's base is its parent's branch, and merging it would merge into the
    /// parent, not ship it (docs/pr-workflow.md, "The merge train"). **Not the only thing that
    /// keeps them out** — writing it in `matches` is how a child stays off the train altogether,
    /// but a merge is refused whatever the file says; see [`instead_of_merging_off_the_trunk`].
    BaseTrunk,
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
    ///
    /// **These are conditions on ACTING, not only on being claimed** (SKEIN-279). An assignment
    /// answers *which* workflow is responsible for a pull request. It does not assert that the
    /// workflow's own conditions hold, and it never could — the person choosing a workflow on a row
    /// is not restating its file. So `matches` is read on both roads: [`claims`] uses it to decide
    /// what this workflow takes on by itself, and [`unmet`] holds it back from acting wherever they
    /// do not hold, however it came to carry the pull request.
    ///
    /// Written this way round because the alternative cannot be made safe. If an assignment meant
    /// "guards and all off", then every condition anybody writes here is a guard that silently does
    /// not apply on one of the two roads — which is how a hand-assigned stacked child was merged
    /// into its parent's branch (SKEIN-237), and that fix had to be moved out of `matches` and into
    /// the act to hold at all. One escape hatch remains and it is the honest one: a workflow with
    /// no `matches` states no conditions, so it acts wherever it is assigned.
    ///
    /// Holding back costs nothing and demands nothing. It is not a stop and not a timed wait: the
    /// pull request is simply not carried for the purpose of acting ([`crate::prwork::Carries`]),
    /// so it never becomes the front of a serial train, no clock runs on it, and the moment the
    /// condition becomes true it joins in. "Put this on the train, it will go when it is approved"
    /// is therefore exactly what happens.
    #[serde(default)]
    pub matches: Vec<Cond>,
    /// One at a time, per **(repo, workflow)** — not per repo. The sweep orders this workflow's
    /// carrying pull requests oldest-first (lowest number) and lets only the first one without a
    /// stop act each pass;
    /// everyone behind the front simply waits, and a stopped front is passed over — that is the
    /// "skip failures and move ahead" the owner asked for. A parallel train re-runs CI on every
    /// sibling after every merge, which is the tax serial exists to avoid
    /// (`docs/pr-workflow.md`, "The merge train").
    ///
    /// Two serial workflows carrying pull requests in one repo therefore have two fronts, and two
    /// pull requests act in a single pass — `sweep`'s `fronts` map is keyed on the flow name. One
    /// train is the configuration this was designed for and cannot tell the difference; a second
    /// one hands back exactly the re-run tax serial was chosen to avoid.
    /// `tests/merge_train_shape.rs` pins both the behaviour and the wording.
    #[serde(default)]
    pub serial: bool,
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
    /// See [`Workflow::serial`]. Defaulted, so every file written before the merge train still
    /// reads — and serialized always, so an editor's round trip cannot drop it.
    #[serde(default)]
    serial: bool,
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
pub const CONDITIONS: [(&str, &str); 15] = [
    ("approved", "somebody has approved it"),
    ("not-approved", "nobody's approval is standing"),
    ("changes-requested", "changes were requested"),
    (
        "review-satisfied",
        "the repository's own review requirement is met, or it asks for none",
    ),
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
    ("behind", "the base has commits this branch lacks"),
    ("current", "known up to date with its base"),
    ("base:trunk", "its base is the repository's default branch"),
];

/// Every action that can be written. See [`CONDITIONS`] for why this is a table.
pub const ACTIONS: [(&str, &str); 10] = [
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
    (
        "wait:<why>",
        "say this and do nothing this pass — the train's own way of standing still",
    ),
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
            "review-satisfied" => Ok(Cond::ReviewSatisfied),
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
            "behind" => Ok(Cond::Behind),
            "current" => Ok(Cond::Current),
            // Only `trunk`, deliberately: a base named outright (`base:main`) would be a workflow
            // that silently stops fitting the repo the day its default branch is renamed, and the
            // trunk is the one base a merge train may ship to.
            "base" => match arg {
                "trunk" => Ok(Cond::BaseTrunk),
                _ => Err(format!(
                    "base can only be trunk — the repository's default branch — not {arg:?}"
                )),
            },
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
            serial: flow.serial,
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
        workflow: flows.iter().map(written_flow).collect(),
    };
    serde_json::to_vec_pretty(&written).map_err(|e| format!("could not write workflows: {e}"))
}

fn written_flow(w: &Workflow) -> WrittenFlow {
    WrittenFlow {
        name: w.name.clone(),
        matches: w.matches.iter().map(spell_cond).collect(),
        serial: w.serial,
        steps: w
            .steps
            .iter()
            .map(|s| WrittenStep {
                when: s.when.iter().map(spell_cond).collect(),
                act: spell_act(&s.act),
            })
            .collect(),
    }
}

/// One workflow in the shape the file has and the editor edits, as JSON.
///
/// THE one spelling, shared with [`to_bytes`], because the server used to rebuild this shape by
/// hand — name, matches, steps — and the hand copy silently dropped `serial` the day it was added.
/// The owner's file said `"serial": true`; the editor payload said nothing; a save from that editor
/// would have written the file back WITHOUT it, and the train would have quietly gone parallel. A
/// second serializer is the same defect as a second parser, and this is its funeral.
pub fn editor_shape(w: &Workflow) -> serde_json::Value {
    serde_json::to_value(written_flow(w)).unwrap_or_default()
}

/// How a condition is written down. The inverse of [`Cond::parse`], and tested against it.
pub fn spell_cond(cond: &Cond) -> String {
    match cond {
        Cond::Approved => "approved".into(),
        Cond::NotApproved => "not-approved".into(),
        Cond::ChangesRequested => "changes-requested".into(),
        Cond::ReviewSatisfied => "review-satisfied".into(),
        Cond::Label(l) => format!("label:{l}"),
        Cond::NoLabel(l) => format!("no-label:{l}"),
        Cond::Checks(s) => format!("checks:{s}"),
        Cond::Mergeable => "mergeable".into(),
        Cond::NotMergeable => "not-mergeable".into(),
        Cond::Draft => "draft".into(),
        Cond::Ready => "ready".into(),
        Cond::Mine => "mine".into(),
        Cond::Behind => "behind".into(),
        Cond::Current => "current".into(),
        Cond::BaseTrunk => "base:trunk".into(),
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
    /// **Somebody has approved it, and nobody's refusal is standing.** What a person means by
    /// "approved", and nothing more than that.
    ///
    /// This used to say *"Approved against the commit that is there NOW. An approval of an earlier
    /// head is not one."* Both halves were wrong, and the sentence is worth keeping in view
    /// because it is what made the error look settled (SKEIN-339).
    ///
    /// It was built as `pr.review_decision == "APPROVED"` alone, and GitHub's `reviewDecision`
    /// does not answer "has anybody approved this". It answers "is this branch's review
    /// requirement satisfied", so it is `APPROVED` only where branch protection REQUIRES a review
    /// and the requirement is met, and `null` on every repository where review is social — however
    /// many approvals a pull request carries. On the owner's own queue that read `APPROVED` on
    /// **zero of twenty-one** open pull requests, two of which the owner had personally approved
    /// (`GET /api/repos/gadget-demo/review`, 2026-08-26). `CHANGES_REQUESTED` still surfaced,
    /// because a refusal is not gated on a requirement — which is exactly why the field looked
    /// like it worked, and why the documented merge train sat on `matches: ["…", "approved", …]`
    /// claiming nothing at all, with no error, no flag and no stop to notice.
    ///
    /// The second half was wrong for a different reason: whether pushing a commit ends an approval
    /// is the repository's `dismiss_stale_reviews` setting, not a property of the word. With it
    /// off, `reviewDecision` stays `APPROVED` across pushes and GitHub means it. So "standing" here
    /// means *GitHub still counts it*, which is the only sense skein can honestly claim — see
    /// `docs/pr-workflow.md`, "The finding: rebasing and approvals", for the setting and its
    /// citations.
    ///
    /// **A standing refusal outranks a standing approval**, which is why this and
    /// [`Facts::changes_requested`] still cannot both hold. That is now a rule rather than an
    /// accident of both being read off one field: with approvals countable, a workflow written
    /// before SKEIN-339 as `matches: ["approved"] → merge` would otherwise start merging over a
    /// reviewer who had said no, and a behaviour change that merges is the one kind this module
    /// will not make quietly.
    ///
    /// **Anybody's approval, not just yours** (SKEIN-356). This used to say that `prq::Pr` carried
    /// the repository's verdict and *your* last review and nobody else's — so on a repository with
    /// no review requirement a third party's approval was invisible and this read false. That was
    /// the safe direction and still a gap, and it closed the way that paragraph said it would:
    /// `prq::Pr::standing_approvals` counts the approvals GitHub holds against the current head
    /// from any reviewer, off the same `latestOpinionatedReviews` your own verdict comes from, and
    /// `prwork::facts_of` reads it.
    ///
    /// What remains outside this field is what only `reviewDecision` can see — CODEOWNERS, a
    /// required-approvals count, any rule branch protection applies — and that is deliberately
    /// [`Facts::review_requirement_met`]'s job rather than this one.
    pub approved: bool,
    pub changes_requested: bool,
    /// **Is the repository's own review requirement in the way?** `Some(true)` it is met,
    /// `Some(false)` it is not, `None` there is no requirement to meet.
    ///
    /// [`Facts::approved`] is what people decided; this is what the repository demands, and they
    /// are two questions with two answers. On a protected branch GitHub's `reviewDecision` folds
    /// in CODEOWNERS, the required-approvals count and rules skein has no API to read — so it
    /// remains the authority on whether a merge will be allowed at all, and losing that was never
    /// the point of SKEIN-339.
    ///
    /// **`None` means "nothing to satisfy", and [`Cond::ReviewSatisfied`] therefore HOLDS on it.**
    /// That breaks this module's usual discipline — [`Facts::mergeable`], [`Facts::behind`] and
    /// [`Facts::base_is_trunk`] all have a third value that satisfies neither of its conditions —
    /// and it breaks it on purpose. Those three are three-valued because skein does not yet KNOW;
    /// waiting is the honest answer and the answer arrives on a later poll. This one is
    /// three-valued because GitHub has answered and the answer is *there is no requirement here*.
    /// No later poll changes it. Reading that as "not satisfied" is SKEIN-339 itself, re-committed
    /// under a new word: a condition that is false forever on every repository where review is
    /// social, in a `matches` that then claims nothing, silently.
    ///
    /// The cost of that choice, stated rather than left implied: a queue remembered on disk by a
    /// skein from before `review_decision` existed also deserialises to `""` and lands here as
    /// `None`, so such a queue reads as "no review requirement". The pull request still needs
    /// [`Facts::approved`] before a train touches it, and the queue is re-fetched within the
    /// minute.
    pub review_requirement_met: Option<bool>,
    /// The labels skein saw — which may not be all of them. Read with [`Facts::labels_whole`].
    pub labels: Vec<String>,
    /// **Did skein see every label there is?** (SKEIN-373)
    ///
    /// A pair rather than one field, because `labels` alone cannot answer a question about a name
    /// that is not in it. GitHub's `labels` connection is paged — `prq::LABELS_FETCHED` — and the
    /// query used not to ask how many there were, so a pull request with more labels than the page
    /// arrived short and read as complete. [`Cond::NoLabel`] then answered "that label is not on
    /// this pull request" about a label it had never been sent, and a `hold` that happened to sort
    /// past the cap stopped holding anything.
    ///
    /// **False satisfies neither [`Cond::Label`] nor [`Cond::NoLabel`] for a name that was not
    /// seen** — the same discipline as [`Facts::mergeable`], [`Facts::behind`] and
    /// [`Facts::base_is_trunk`], and for the same reason: skein does not know, so it waits rather
    /// than answering. A name that WAS seen is still on the pull request whatever the cap did, so
    /// `label:` on it holds as it always did; truncation can only ever take a condition away.
    ///
    /// **`Default` is false**, which is this module's fail-closed value and not an oversight:
    /// `Facts::default()` is a fact-set nobody looked anything up for, and a default that let
    /// `no-label:` hold would answer, from nothing, the question this field exists to stop being
    /// answered from nothing. `prwork::facts_of` states it from the queue; a fixture that means
    /// "these are all the labels" says so.
    pub labels_whole: bool,
    /// `passing` | `failing` | `pending` | `none`.
    pub checks: String,
    /// `None` when GitHub has not worked it out yet, which it reports as `UNKNOWN` for a while
    /// after every push. **Not the same as "cannot be merged"** — see [`holds`].
    pub mergeable: Option<bool>,
    pub draft: bool,
    /// You opened it.
    pub mine: bool,
    /// Does the base have commits this branch lacks? `None` when GitHub has not said —
    /// `mergeStateStatus: UNKNOWN`, or a queue from before the field existed. Three-valued for
    /// the same reason as [`Facts::mergeable`] — see [`holds`].
    pub behind: Option<bool>,
    /// Is the base ref the repository's default branch? `None` when skein does not know what the
    /// trunk IS — the lookup failed, or has not happened yet. Three-valued for the same reason as
    /// [`Facts::mergeable`] and [`Facts::behind`], and here it earns its third value twice over:
    /// "based on a branch that is not the trunk" is a stacked child, which must never be merged,
    /// while "skein cannot see what the trunk is" is a transient blindness that must not become a
    /// permanent stop. [`next`] tells those two apart — see [`instead_of_merging_off_the_trunk`].
    pub base_is_trunk: Option<bool>,
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
///
/// `behind` and `current` keep the same discipline, for the merge train's sake: **unknown
/// satisfies neither.** GitHub reports `mergeable: true` for a branch that is merely behind, so
/// the train's merge step requires `current` explicitly — and if unknown counted as current,
/// merging a branch whose behind-ness is unknown could merge code CI never tested against the
/// current trunk (`docs/pr-workflow.md`, "The merge train").
pub fn holds(cond: &Cond, facts: &Facts) -> bool {
    match cond {
        Cond::Approved => facts.approved,
        Cond::NotApproved => !facts.approved,
        Cond::ChangesRequested => facts.changes_requested,
        // The one three-valued fact whose unknown-shaped third value satisfies its condition. Not
        // an oversight and not a shortcut: `None` here is GitHub saying the repository asks for no
        // review, which is a permanent answer, where the `None`s below are skein not knowing yet.
        // Read the other way this condition is false forever on every social-review repo, which is
        // SKEIN-339 with a new spelling — see [`Facts::review_requirement_met`].
        Cond::ReviewSatisfied => facts.review_requirement_met != Some(false),
        // A label skein was sent is on the pull request, and a page that was cut off cannot make
        // that untrue — so this reads the list as it always did.
        Cond::Label(want) => facts.labels.iter().any(|l| l == want),
        // Its opposite is not symmetrical, and that asymmetry is the whole of SKEIN-373. "This
        // label is absent" is a claim about the labels skein did NOT receive, so a short list
        // cannot make it: `labels_whole` is the third value, and unknown satisfies neither
        // condition, exactly as `mergeable`'s does. The cost is stated where it falls — the
        // queue's blind spots name the pull request and the size of the hole — and it is the
        // cheaper of the two errors: a `hold` label past the cap used to read as absent, which is
        // a merge over somebody's hold.
        Cond::NoLabel(want) => facts.labels_whole && !facts.labels.iter().any(|l| l == want),
        Cond::Checks(want) => &facts.checks == want,
        Cond::Mergeable => facts.mergeable == Some(true),
        Cond::NotMergeable => facts.mergeable == Some(false),
        Cond::Draft => facts.draft,
        Cond::Ready => !facts.draft,
        Cond::Mine => facts.mine,
        Cond::Behind => facts.behind == Some(true),
        Cond::Current => facts.behind == Some(false),
        Cond::BaseTrunk => facts.base_is_trunk == Some(true),
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
            // The one thing a written-down step may not talk skein into. See below.
            act: instead_of_merging_off_the_trunk(&s.act, facts).unwrap_or_else(|| s.act.clone()),
        })
}

/// **Skein never merges a pull request into a branch it cannot see is the trunk.** What this
/// returns in place of that merge, or `None` when the step is one it has nothing to say about.
///
/// `docs/pr-workflow.md` ("Stacks need no stack model") stakes the whole stack design on one
/// sentence — *the train only ever touches a PR whose base is the trunk* — and until SKEIN-237 the
/// only thing holding it up was `base:trunk` in a workflow's `matches`. That guard is evaluated on
/// exactly one of the two roads to acting: [`claims`] reads `matches`, and a workflow somebody
/// assigned by hand never goes past it ([`crate::prwork::carries`]). One hand-assigned stacked
/// child was therefore merged into its PARENT's branch and its branch deleted — the child's commits
/// on the parent rather than shipped, the rest of the stack cut loose behind it, and nothing about
/// it undoable from skein. The owner's own documented `ship-mine` (`matches: ["mine"]`) had the
/// same hole down the *matched* road, on any stacked pull request they authored.
///
/// So the guard lives here, where BOTH roads pass, and it is about the action rather than about a
/// file: a merge is the one act in the closed set that cannot be taken back, and the base is the
/// one fact that says where its commits land.
///
/// The two answers are deliberately different, and that difference is the whole of what makes this
/// recoverable:
///
/// * **The base is known not to be the trunk** — a stacked child. That is a standing fact about
///   this pull request, not a hiccup, so it becomes a [`Act::Flag`]: the workflow stops on it, in
///   writing, with the reason, and a serial train passes it over and keeps moving. It rejoins by
///   itself when its parent merges and GitHub retargets it onto the trunk.
/// * **The trunk is not known at all** — the lookup failed, usually a rate limit
///   (`crate::prq::trunk_of` remembers a failure as `""`). Blindness is not a verdict. It becomes
///   [`Act::Wait`], which writes nothing down and stops nothing: the moment skein can see the
///   trunk again the same pull request merges, with no stop for anybody to clear.
///
/// There is deliberately no way to spell "merge off the trunk on purpose". `base:trunk` is the only
/// thing the vocabulary can say about a base, so a workflow cannot express a deliberate merge into
/// a parent branch — and if one is ever wanted, the fix is a word for it, not a hole here.
pub fn instead_of_merging_off_the_trunk(act: &Act, facts: &Facts) -> Option<Act> {
    match act {
        Act::Merge(_) => merging_off_the_trunk(facts.base_is_trunk),
        _ => None,
    }
}

/// The rule itself, over the ONE fact it reads.
///
/// Split out for a caller that has no [`Facts`] and never will: the merge a PERSON presses
/// (`crate::prwork::merge_by_hand`, SKEIN-338). That path is not a workflow — there is no file, no
/// step, no `matches` — so it has a base ref and a trunk and nothing else, and until SKEIN-338 it
/// had no trunk guard at all. `grep -rn instead_of_merging_off_the_trunk src/` showed the guard
/// reachable only from `next` and `perform`, and `$SKEIN_PR_WORKFLOWS` is off on the owner's fleet,
/// so the guarded road was the one nobody was driving.
///
/// **A function of `Option<bool>` rather than of `&Facts`, and that is the load-bearing part.** The
/// alternative was `Facts { base_is_trunk, ..Default::default() }` at the hand path's call site,
/// which is a lie the day this rule starts reading a second field — the defaults would answer for
/// facts nobody looked up, silently, on the one act that cannot be taken back. Narrowing the
/// argument to what the rule actually reads makes that impossible to write.
///
/// `Some(true)` is `None`: nothing to say, merge away.
pub fn merging_off_the_trunk(base_is_trunk: Option<bool>) -> Option<Act> {
    match base_is_trunk {
        Some(true) => None,
        Some(false) => Some(Act::Flag(
            "this is not based on the trunk — merging it would land its commits on its base \
             branch instead of shipping them, and delete the branch. It rejoins when its base \
             becomes the repository's default branch"
                .into(),
        )),
        None => Some(Act::Wait(
            "skein does not yet know this repository's default branch, and will not merge into a \
             base it cannot check"
                .into(),
        )),
    }
}

/// Does this workflow claim this pull request on its own?
///
/// Empty `matches` means never — a workflow with no rule runs only where somebody assigned it by
/// hand. That is the safe direction: the cost of a rule that never fires is that you assign it
/// yourself; the cost of one that fires on everything is a merge you did not ask for.
///
/// The only conditions that may be unmet and still claim are [`Cond::Approved`] and
/// [`Cond::ReviewSatisfied`], and only for the one reason in [`the_reviewer_said_no_instead`].
///
/// **This is only half of what `matches` decides.** It answers which pull requests the workflow
/// takes on by itself; [`unmet`] answers whether it may act on one at all, which is the half a
/// hand-assigned pull request used to skip (SKEIN-279). Both are read off the same evaluation
/// below, so the two answers cannot drift apart.
pub fn claims(flow: &Workflow, facts: &Facts) -> bool {
    !flow.matches.is_empty() && unmet(flow, facts).is_empty()
}

/// Which of this workflow's `matches` do NOT hold, spelled as they are written on disk.
///
/// **The guard half of `matches`, in a form a person can be shown.** `matches` says which pull
/// requests this workflow is responsible for AND, on every one it governs, the conditions under
/// which it may act — see [`Workflow::matches`]. Empty means neither: no rule to claim by, and no
/// condition to hold back for.
///
/// Spelled rather than counted because the sentence built from this is read on a row, next to a
/// workflow somebody chose by hand and is waiting on: "holding — `approved` is not true yet" is
/// checkable against the file, where "1 condition unmet" is something to go and work out.
///
/// [`the_reviewer_said_no_instead`] applies here exactly as it does to a claim, and it must: a
/// pull request whose reviewer asked for changes is one the workflow is still responsible for, and
/// its documented first step is written for that case. Reporting `approved` as unmet there would
/// hold the pull request one pass short of the step that exists to catch it.
pub fn unmet(flow: &Workflow, facts: &Facts) -> Vec<String> {
    flow.matches
        .iter()
        .filter(|cond| !holds(cond, facts) && !the_reviewer_said_no_instead(cond, facts))
        .map(spell_cond)
        .collect()
}

/// **A reviewer saying no is not a pull request leaving the workflow.** True where `matches` asks
/// for an approval and what came back was a refusal.
///
/// `approved` and `changes-requested` cannot hold together — [`Facts::approved`] is false whenever
/// a refusal stands — so a workflow whose `matches` requires the first releases the pull request at
/// the exact moment the second becomes true. `review-satisfied` behaves the same way and for the
/// same reason: `CHANGES_REQUESTED` is the repository's requirement being NOT met, so a `matches`
/// asking for it lets go on the same event (SKEIN-339 added that word; the trap it walks into is
/// this one).
///
/// That made a step nobody could reach. The owner's own documented merge train
/// (`docs/pr-workflow.md`, "The train, written down") is `matches: ["ready", "approved",
/// "review-satisfied", "base:trunk"]` with `{"when": ["changes-requested"], "do": "flag:changes were requested —
/// resolve them to rejoin the train"}` as its FIRST step — written for precisely this, and dead by
/// construction: the pull request stopped carrying the workflow one pass before the step could
/// fire. What the owner saw was a pull request disappearing off the train with no stop, no banner,
/// no journal line and nothing on the row (SKEIN-247), on the one event where somebody had said in
/// as many words that it needed a person.
///
/// **Why this is a rule about `matches` and not a word in the file.** The vocabulary has no `or`,
/// so "approved, or a reviewer said no" cannot be written down; and the two obvious ways round it
/// are both worse than the bug. Dropping `approved` from `matches` puts every unreviewed pull
/// request in the repository on the train, where the documented steps rebase its branch and start
/// CI on it. Adding a step for it changes nothing, because steps are only ever evaluated for a
/// pull request the workflow already claims. So the fix belongs where the claim is decided, as a
/// statement about what `matches` MEANS: it says which pull requests a workflow is responsible
/// for, and asking somebody for an approval does not stop being your question when the answer is
/// no.
///
/// **It claims nothing extra.** Only a pull request whose reviewer has actually refused, and only
/// against a `matches` that already asked about review — every other unmet condition still
/// releases it, and a workflow that mentions neither `approved` nor `review-satisfied` is
/// untouched. In particular an unreviewed pull request (GitHub's `REVIEW_REQUIRED`, or no review
/// at all) is neither approved nor changes-requested, so it stays off the train: that one is the
/// ordinary state of every open pull request, and pulling it in would rebase branches and start CI
/// on work nobody has looked at.
///
/// **And it does not strand it.** Everything downstream is unchanged: the workflow's own steps
/// decide what happens, a `flag:` writes the stop and reaches the banner like any other, and a
/// serial train passes a stopped pull request over and keeps moving. A workflow with no
/// `changes-requested` step falls to its catch-all `wait:`, which is bounded by
/// [`crate::prwork::a_wait_that_will_not_end_on_its_own`] — so the silence has nowhere left to hide.
fn the_reviewer_said_no_instead(cond: &Cond, facts: &Facts) -> bool {
    matches!(cond, Cond::Approved | Cond::ReviewSatisfied) && facts.changes_requested
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
            // An ordinary pull request off the trunk. Said rather than defaulted, because
            // `Facts::default()` means "skein has not learned this repo's trunk", and a merge is
            // refused there on purpose — see [`instead_of_merging_off_the_trunk`].
            base_is_trunk: Some(true),
            // The labels this row lists are ALL of its labels — said rather than defaulted, for
            // the same reason as the line above. `Facts::default()` is skein having looked nothing
            // up, and a `no-label:` may not hold on that (SKEIN-373); the pull request this table
            // walks is an ordinary one whose whole label set fits in the page.
            labels_whole: true,
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

    /// **A label list that was cut off answers neither question about a label it never saw**
    /// (SKEIN-373).
    ///
    /// `prq::LABELS_FETCHED` pages GitHub's labels connection, and until SKEIN-373 the query did
    /// not ask how many there were — so a pull request with more labels than the page arrived
    /// short and read as complete. `no-label:` then answered a question about names it had never
    /// been sent, and the answer it gave was always the permissive one: absent. Measured on
    /// `acme/testbed#20`, 22 labels arrived as 20.
    ///
    /// Asserted as a property over a set of names rather than as an outcome for one pair, because
    /// the rule is what matters and the rule is asymmetric: **truncation can only ever take a
    /// condition away.** A label that arrived is on the pull request whatever the cap did, so
    /// `label:` is untouched; "this label is absent" is a claim about the part that did not
    /// arrive, so a short list cannot make it.
    #[test]
    fn a_condition_about_a_label_skein_never_saw_holds_neither_way() {
        let facts = |labels: &[&str], labels_whole| Facts {
            approved: true,
            base_is_trunk: Some(true),
            mergeable: Some(true),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            labels_whole,
            ..Default::default()
        };
        let arrived = ["ci", "hold"];
        // Names inside the page and names outside it, plus the empty one — a `Cond` is built from
        // a file people edit and nothing stops it naming a label that does not exist.
        let names = ["ci", "hold", "area/mod-21", "release", ""];

        // **Whole**: every name gets exactly one of the two answers. That is the property a short
        // list was silently claiming.
        let complete = facts(&arrived, true);
        for name in names {
            assert!(
                holds(&Cond::Label(name.into()), &complete)
                    != holds(&Cond::NoLabel(name.into()), &complete),
                "`{name}`: on a list skein saw all of, a label is either there or not there, and \
                 the two conditions must always disagree about it"
            );
        }

        // **Short**: a name that arrived keeps its answer, and a name that did not arrive gets
        // neither — the third value, exactly as `mergeable`'s unknown satisfies neither
        // `mergeable` nor `not-mergeable`.
        let short = facts(&arrived, false);
        for name in arrived {
            assert!(
                holds(&Cond::Label(name.into()), &short),
                "`{name}` arrived, and a cap somewhere past it took the condition away — \
                 truncation may only ever remove an answer skein does not have"
            );
            assert!(!holds(&Cond::NoLabel(name.into()), &short));
        }
        for name in names.iter().filter(|n| !arrived.contains(n)) {
            assert!(
                !holds(&Cond::Label((*name).into()), &short)
                    && !holds(&Cond::NoLabel((*name).into()), &short),
                "`{name}` was never sent to skein and a condition answered about it anyway — the \
                 permissive answer here is a merge over a label nobody could see"
            );
        }

        // And what that is worth, in the one shape it costs something: a workflow told to merge
        // anything nobody has put a hold on. On a complete list it merges; with the list short and
        // `hold` unaccounted for, it does nothing and says nothing — which is the safe direction,
        // and the queue's blind spots are where the silence is broken (`prq::queue_within`).
        let train = &from_bytes(
            br#"{"workflow":[{"name":"w","matches":["mine"],
                 "steps":[{"when":["approved","no-label:hold"],"do":"merge:squash"}]}]}"#,
        )
        .unwrap()[0];
        assert_eq!(
            next(train, &facts(&["ci"], true)).map(|c| c.act),
            Some(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false
            })),
            "the counter-case failed: with every label seen and no hold among them, the merge is \
             the right answer and must still happen"
        );
        assert_eq!(
            next(train, &facts(&["ci"], false)),
            None,
            "a pull request whose label list was cut off was merged on `no-label:hold` — the hold \
             may be one of the labels skein never received, which is SKEIN-373 exactly"
        );
    }

    /// The merge train, READ OUT of `docs/pr-workflow.md` ("The train, written down").
    ///
    /// Read rather than copied, and it is the same discipline as
    /// `prq::the_only_other_reader_of_this_list_stands_down_when_it_is_partial` reading the server
    /// source: a fixture copied from a document proves the fixture. Both tests below make claims
    /// about the train somebody will actually run, so both have to be looking at it — and one
    /// source means the two can never disagree about what the train is.
    fn documented_train() -> Workflow {
        let doc = std::fs::read_to_string("docs/pr-workflow.md").expect("the workflow document");
        let written = doc
            .split_once("### The train, written down")
            .expect("the document no longer writes the train down")
            .1
            .split_once("```json")
            .expect("the train is no longer a json block")
            .1
            .split_once("```")
            .expect("the json block never ends")
            .0;
        from_bytes(written.as_bytes())
            .unwrap_or_else(|e| panic!("the train in docs/pr-workflow.md does not parse: {e}"))
            .remove(0)
    }

    /// A reviewer saying no does not take the pull request off the train (SKEIN-247).
    ///
    /// The train's FIRST step is `changes-requested → flag`, and until this rule existed no pull
    /// request could ever reach it: `matches` asks for `approved`, `approved` and
    /// `changes-requested` are two readings of one GitHub field, so the pull request stopped
    /// carrying the workflow one pass before the step written for it could fire. It left the train
    /// with no stop, no banner and nothing on the row — on the one event where a human had said in
    /// as many words that it needed a person.
    ///
    /// The third assertion is the one that keeps this from being a licence. An unreviewed pull
    /// request is neither approved nor changes-requested, and it must stay OFF: on the documented
    /// steps, claiming it rebases its branch and starts CI on work nobody has looked at.
    #[test]
    fn a_reviewer_saying_no_does_not_take_a_pull_request_off_the_train() {
        let train = &documented_train();
        // `base_is_trunk: Some(true)` and not a draft — the other two things `matches` asks for,
        // so the only thing moving between these three facts is the review decision.
        let on_the_trunk = Facts {
            base_is_trunk: Some(true),
            checks: "passing".into(),
            ..Default::default()
        };

        let approved = Facts {
            approved: true,
            ..on_the_trunk.clone()
        };
        assert!(
            claims(train, &approved),
            "the train must claim what it is for"
        );

        // Exactly what `facts_of` builds from `review_decision == "CHANGES_REQUESTED"`: no
        // approval standing, a refusal standing, and the repository's requirement NOT met. All
        // three, and the third one matters — `matches` asks for `review-satisfied` as well since
        // SKEIN-339, so a fixture that left it at its default would let this pass while the second
        // condition quietly dropped the pull request off the train on the same event as the first.
        let said_no = Facts {
            approved: false,
            changes_requested: true,
            review_requirement_met: Some(false),
            ..on_the_trunk.clone()
        };
        assert!(
            claims(train, &said_no),
            "a pull request whose reviewer requested changes left the train silently — its own \
             `flag:changes were requested` step can never fire, so nothing writes a stop and \
             nothing reaches the banner"
        );
        assert_eq!(
            next(train, &said_no),
            Some(Chosen {
                step: 0,
                act: Act::Flag("changes were requested — resolve them to rejoin the train".into()),
            }),
            "it is carried but the step written for it is not what fires"
        );

        // Nobody has reviewed it yet — GitHub's `REVIEW_REQUIRED`, or no review at all.
        let unreviewed = Facts {
            approved: false,
            changes_requested: false,
            ..on_the_trunk
        };
        assert!(
            !claims(train, &unreviewed),
            "an unreviewed pull request was pulled onto the train, where the documented steps \
             rebase its branch and start CI on work nobody has looked at"
        );

        // And the rule is about a `matches` that ASKED about approval. A workflow that never
        // mentions it is untouched in both directions.
        let mine = &from_bytes(
            br#"{"workflow":[{"name":"ship-mine","matches":["mine"],"steps":[{"when":[],"do":"merge:squash"}]}]}"#,
        )
        .unwrap()[0];
        assert!(
            !claims(mine, &said_no),
            "a rule about whose PR it is started reading review decisions"
        );
    }

    /// The documented train asks both review questions, and a repository with no review
    /// requirement satisfies the second one (SKEIN-339).
    ///
    /// Read off `docs/pr-workflow.md` rather than a fixture, because the document IS the train the
    /// owner is told to run and the defect lived in the file as much as in the code. Two halves,
    /// and neither is sufficient alone:
    ///
    /// * asking only `approved` merges past a branch protection rule skein cannot see — GitHub
    ///   refuses, and a refused act is a stop somebody has to clear;
    /// * asking only `review-satisfied` merges anything on a repository that requires no review,
    ///   because there is nothing there to be unsatisfied.
    ///
    /// The last assertion is the one that was false for the whole life of the feature: on such a
    /// repository `review_requirement_met` is `None`, and if that read as "not satisfied" the
    /// train would claim nothing — which is SKEIN-339 exactly, moved one word to the left.
    #[test]
    fn the_documented_train_asks_about_people_and_about_branch_protection() {
        let train = &documented_train();
        for want in [Cond::Approved, Cond::ReviewSatisfied] {
            assert!(
                train.matches.contains(&want),
                "the train in docs/pr-workflow.md no longer asks for `{}` in its `matches`. The \
                 two are different questions — has anybody approved it, and is the repository's \
                 own requirement in the way — and a train that merges needs both answered",
                spell_cond(&want)
            );
        }

        // A repository that requires no review, and somebody has approved it.
        let social = Facts {
            approved: true,
            review_requirement_met: None,
            base_is_trunk: Some(true),
            checks: "passing".into(),
            ..Default::default()
        };
        assert!(
            claims(train, &social),
            "the train does not claim an approved pull request on a repository where review is \
             social — the state twenty of the owner's twenty-one open pull requests were in, and \
             it claimed none of them, silently"
        );

        // And the protection is still protection where there IS one.
        let protected = Facts {
            review_requirement_met: Some(false),
            ..social
        };
        assert_eq!(
            unmet(train, &protected),
            vec!["review-satisfied".to_string()],
            "an approval skein can count itself was allowed to stand in for a branch protection \
             rule it cannot read"
        );
    }

    /// Every step of the documented merge train is reachable by SOME pull request the train claims
    /// for itself.
    ///
    /// The general form of SKEIN-247, and the test that would have caught it. A step is written
    /// down to be run; one that no pull request can ever reach is a promise the file makes and the
    /// engine cannot keep, and it is invisible — the step reads correctly, parses correctly, and
    /// simply never happens. `matches` and `steps` are checked against each other nowhere else:
    /// `claims` reads the first and `next` reads the second, and until this test nothing compared
    /// them.
    ///
    /// The train comes from [`documented_train`], so this is a claim about the train the document
    /// actually tells somebody to run.
    ///
    /// The search is exhaustive over every fact any condition in the vocabulary can read — a few
    /// thousand combinations, which is nothing, and the alternative is choosing the states by hand
    /// and choosing exactly the ones that pass.
    #[test]
    fn every_step_of_the_documented_train_is_reachable() {
        let train = &documented_train();

        // Every label the file has an opinion about, so the search covers carrying it and not.
        let mut labels: Vec<String> = Vec::new();
        for cond in train
            .matches
            .iter()
            .chain(train.steps.iter().flat_map(|s| &s.when))
        {
            if let Cond::Label(name) | Cond::NoLabel(name) = cond {
                if !labels.contains(name) {
                    labels.push(name.clone());
                }
            }
        }

        let mut reached = vec![false; train.steps.len()];
        // Every review state `prwork::facts_of` can BUILD, rather than every combination the three
        // fields can hold — and the difference is the whole of SKEIN-339. The old comment here
        // read "GitHub's four answers for `reviewDecision` … one field", enumerated three tuples,
        // and was therefore blind to the state the owner's entire fleet was actually in: a
        // repository that requires no review, where `reviewDecision` is `""` and an approval is
        // real anyway. That state is the fourth line below, and adding it is what makes this test
        // able to fail for the reason the train was dead.
        //
        //   reviewDecision      → (approved, changes_requested, review_requirement_met)
        let review_states = [
            // APPROVED — a requirement exists and somebody met it.
            (true, false, Some(true)),
            // CHANGES_REQUESTED — a refusal, which outranks any approval standing behind it.
            (false, true, Some(false)),
            // REVIEW_REQUIRED — a requirement exists and nobody has met it.
            (false, false, Some(false)),
            // "" and a standing approval — no requirement, and a person has said yes anyway.
            (true, false, None),
            // "" and nothing — no requirement, nobody has looked.
            (false, false, None),
        ];
        for (approved, changes_requested, review_requirement_met) in review_states {
            for draft in [true, false] {
                for mine in [true, false] {
                    for base_is_trunk in [Some(true), Some(false), None] {
                        for mergeable in [Some(true), Some(false), None] {
                            for behind in [Some(true), Some(false), None] {
                                for checks in ["passing", "failing", "pending", "none"] {
                                    // Whether skein saw every label is a fact a condition reads
                                    // (SKEIN-373), so the search covers both — and it covers them
                                    // for a reason this test can state: with the list short, every
                                    // `no-label:` in the file stops holding, and a step reachable
                                    // ONLY through one of those is a step a heavily-labelled pull
                                    // request can never take. Leaving it out would make the search
                                    // exhaustive over a vocabulary the engine no longer has.
                                    for labels_whole in [true, false] {
                                        for on in 0..(1u32 << labels.len()) {
                                            let facts = Facts {
                                                approved,
                                                changes_requested,
                                                review_requirement_met,
                                                draft,
                                                mine,
                                                base_is_trunk,
                                                mergeable,
                                                behind,
                                                checks: checks.into(),
                                                labels_whole,
                                                labels: labels
                                                    .iter()
                                                    .enumerate()
                                                    .filter(|(i, _)| on & (1 << i) != 0)
                                                    .map(|(_, name)| name.clone())
                                                    .collect(),
                                            };
                                            if !claims(train, &facts) {
                                                continue;
                                            }
                                            if let Some(chosen) = next(train, &facts) {
                                                reached[chosen.step] = true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let unreachable: Vec<String> = reached
            .iter()
            .enumerate()
            .filter(|(_, hit)| !**hit)
            .map(|(i, _)| format!("step {} ({})", i + 1, spell_act(&train.steps[i].act)))
            .collect();
        assert!(
            unreachable.is_empty(),
            "the documented train has steps no pull request it claims can ever reach, so they \
             read correctly, parse correctly and never happen: {}",
            unreachable.join(", ")
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

    /// The merge train's three words parse, and the one with an argument is a closed set.
    ///
    /// `base` takes only `trunk`, on purpose: a base named outright (`base:main`) is a workflow
    /// that silently stops fitting the repo the day its default branch is renamed, and the trunk
    /// is the one base a train may ship to (`docs/pr-workflow.md`, "The merge train").
    #[test]
    fn the_train_vocabulary_parses_and_base_takes_only_trunk() {
        assert_eq!(Cond::parse("behind"), Ok(Cond::Behind));
        assert_eq!(Cond::parse("current"), Ok(Cond::Current));
        assert_eq!(Cond::parse("base:trunk"), Ok(Cond::BaseTrunk));

        let why = Cond::parse("base:main").expect_err("a named base must be refused");
        assert!(
            why.contains("trunk"),
            "the refusal must say what base CAN be: {why}"
        );
        assert!(
            Cond::parse("base").is_err(),
            "a base with nothing after it is not one"
        );

        // And the unknown-word error offers the new spellings, because "what CAN I say" is
        // always the reader's next question.
        let why = Cond::parse("caboose").expect_err("a word nobody defined must be refused");
        for word in ["behind", "current", "base:trunk"] {
            assert!(
                why.contains(word),
                "{word:?} missing from the listing: {why}"
            );
        }
    }

    /// Unknown behind-ness satisfies neither `behind` nor `current`.
    ///
    /// Same discipline as `mergeable`, and with the same teeth: GitHub reports `mergeable: true`
    /// for a branch that is merely behind, so the train's merge step leans on `current` — and if
    /// unknown counted, the train would merge code CI never tested against the current trunk.
    #[test]
    fn unknown_behindness_satisfies_neither_behind_nor_current() {
        let facts = |behind, base_is_trunk| Facts {
            behind,
            base_is_trunk,
            ..Default::default()
        };
        let off = Some(false);
        assert!(holds(&Cond::Behind, &facts(Some(true), off)));
        assert!(!holds(&Cond::Current, &facts(Some(true), off)));
        assert!(holds(&Cond::Current, &facts(Some(false), off)));
        assert!(!holds(&Cond::Behind, &facts(Some(false), off)));
        assert!(
            !holds(&Cond::Behind, &facts(None, off)) && !holds(&Cond::Current, &facts(None, off)),
            "unknown behind-ness satisfied a condition it must satisfy neither of"
        );
        assert!(holds(&Cond::BaseTrunk, &facts(None, Some(true))));
        assert!(!holds(&Cond::BaseTrunk, &facts(None, Some(false))));
        // And a trunk skein has not learned yet satisfies it no more than a base that is not the
        // trunk does — the third value, kept out of the condition on purpose.
        assert!(
            !holds(&Cond::BaseTrunk, &facts(None, None)),
            "an unknown trunk claimed a base as the trunk anyway"
        );
    }

    /// A merge is refused when skein cannot see it is merging into the trunk — and the two ways
    /// it cannot see are answered differently.
    ///
    /// The defect this exists for (SKEIN-237) merged a hand-assigned stacked child into its
    /// PARENT's branch and deleted the child's branch, because `base:trunk` lived only in
    /// `matches` and an assignment never reads `matches`. The guard is on the ACTION now, so it
    /// holds down every road to acting and whatever the file says.
    ///
    /// The half that matters as much as the refusal is which refusal. A base that is known not to
    /// be the trunk is a standing fact about that pull request — it flags, so a train stops on it
    /// loudly, says why, and moves on. A trunk skein has not learned is skein's own blindness,
    /// usually a rate limit — it waits, writing nothing down, so the limit lifting is all it takes
    /// for the same pull request to merge.
    #[test]
    fn a_merge_is_refused_on_a_base_that_is_not_known_to_be_the_trunk() {
        let flow = &from_bytes(EXAMPLE).unwrap()[0];
        let facts = |base_is_trunk| Facts {
            approved: true,
            labels: vec!["ci".into()],
            checks: "passing".into(),
            mergeable: Some(true),
            mine: true,
            base_is_trunk,
            ..Default::default()
        };

        // Trunk-based: the merge the owner asked for, untouched.
        let chosen = next(flow, &facts(Some(true))).expect("the merge step applies");
        assert_eq!(
            chosen.act,
            Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true
            })
        );

        // A stacked child: the SAME step is chosen — so the row still names the line that would
        // have acted — and what it does is stop.
        let child = next(flow, &facts(Some(false))).expect("the merge step still applies");
        assert_eq!(
            child.step, chosen.step,
            "the refusal must name the step that would have merged, not some other one"
        );
        match &child.act {
            Act::Flag(why) => assert!(
                why.contains("not based on the trunk"),
                "a stacked child was stopped without being told why: {why}"
            ),
            other => panic!("a stacked child was going to be {other:?} — its commits would land on its parent's branch and its branch would be deleted"),
        }

        // Trunk unknown: a wait, not a stop. Nothing is written down, so nothing has to be
        // cleared when skein can see again.
        match next(flow, &facts(None)).expect("the merge step still applies").act {
            Act::Wait(why) => assert!(
                why.contains("default branch"),
                "the wait did not say what skein cannot see: {why}"
            ),
            other => panic!("a merge went ahead, or became a stop, on a repository whose trunk skein does not know: {other:?}"),
        }

        // And the guard is about merges alone: every other action is the file's own business.
        for act in [
            Act::AddLabel("ci".into()),
            Act::UpdateBranch(Update::Rebase),
            Act::Flag("look".into()),
            Act::Wait("hold".into()),
        ] {
            assert_eq!(
                instead_of_merging_off_the_trunk(&act, &facts(Some(false))),
                None,
                "{act:?} was refused for a reason that only applies to merging"
            );
        }
    }

    /// **The two spellings of the trunk guard cannot drift apart.**
    ///
    /// SKEIN-338 split the rule out of `instead_of_merging_off_the_trunk` so the merge a person
    /// presses could consult it without inventing a `Facts` it has never looked up
    /// (`crate::prwork::merge_by_hand`). A split like that is only safe while the two agree, and
    /// "they agree" is a claim about every act and every value of the fact, not about the three
    /// cases somebody thought of — so it is asserted over the product rather than sampled.
    ///
    /// The second half is why the split was worth making: the facts around `base_is_trunk` must not
    /// be able to change the answer. If they could, the caller that passes only `base_is_trunk`
    /// would be silently answering from defaults for facts nobody read.
    #[test]
    fn the_trunk_guard_reads_the_base_and_nothing_else() {
        let acts = [
            Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            }),
            Act::Merge(Merge {
                how: MergeAs::Rebase,
                delete_branch: false,
            }),
            Act::AddLabel("ci".into()),
            Act::RemoveLabel("ci".into()),
            Act::UpdateBranch(Update::Rebase),
            Act::UpdateBranch(Update::Merge),
            Act::Flag("look".into()),
            Act::Wait("hold".into()),
        ];
        // Two worlds that disagree about everything EXCEPT the base — approved or not, green or
        // red, draft or ready, mine or not, behind or current, mergeable or not.
        let worlds = [
            |base_is_trunk| Facts {
                base_is_trunk,
                ..Default::default()
            },
            |base_is_trunk| Facts {
                approved: true,
                changes_requested: true,
                review_requirement_met: Some(true),
                labels: vec!["ci".into(), "hold".into()],
                checks: "failing".into(),
                mergeable: Some(true),
                draft: true,
                mine: true,
                behind: Some(true),
                labels_whole: true,
                base_is_trunk,
            },
        ];

        for act in &acts {
            for base_is_trunk in [Some(true), Some(false), None] {
                let want = match act {
                    Act::Merge(_) => merging_off_the_trunk(base_is_trunk),
                    _ => None,
                };
                for (which, world) in worlds.iter().enumerate() {
                    assert_eq!(
                        instead_of_merging_off_the_trunk(act, &world(base_is_trunk)),
                        want,
                        "world {which}: the guard answered differently from the rule the hand \
                         merge consults, for {act:?} with base_is_trunk {base_is_trunk:?} — the \
                         two roads to a merge no longer agree, or the rule now reads a fact its \
                         other caller does not pass"
                    );
                }
            }
        }

        // And the rule itself is total in the direction that matters: a base known to be the trunk
        // is the ONLY value that lets a merge through. Anything else — a known-wrong base, or no
        // answer at all — has something to say instead.
        for base_is_trunk in [Some(true), Some(false), None] {
            assert_eq!(
                merging_off_the_trunk(base_is_trunk).is_none(),
                base_is_trunk == Some(true),
                "a merge was allowed on base_is_trunk {base_is_trunk:?}, or refused on the trunk"
            );
        }
    }

    /// `serial` survives being read, written back, and saved.
    ///
    /// The cockpit editor sends a whole file through [`save`], which re-serializes from what was
    /// PARSED — so a field the round trip dropped would be a train that quietly went parallel the
    /// first time somebody edited an unrelated workflow.
    /// The editor payload is the file's own shape — the regression this guards: the server once
    /// rebuilt it by hand and the copy dropped `serial`, so the cockpit under-reported a running
    /// train and a save from that editor would have stripped the field from the file.
    #[test]
    fn the_editor_shape_carries_serial_and_the_file_spelling() {
        let flows = from_bytes(
            br#"{ "workflow": [ { "name": "t", "serial": true,
                 "steps": [ { "when": ["approved"], "do": "add-label:ci" } ] } ] }"#,
        )
        .unwrap();
        let shape = editor_shape(&flows[0]);
        assert_eq!(
            shape.get("serial").and_then(|v| v.as_bool()),
            Some(true),
            "the editor payload lost `serial` — the hand-serializer bug is back"
        );
        // The step keeps the file's own key for the action.
        assert_eq!(
            shape["steps"][0].get("do").and_then(|v| v.as_str()),
            Some("add-label:ci"),
            "the editor payload spells the action under `do`, as the file does"
        );
    }

    #[test]
    fn serial_survives_the_round_trip_and_the_save() {
        let file = br#"{"workflow":[
          {"name":"merge-train","serial":true,"matches":["mine"],"steps":[{"when":[],"do":"merge:squash+delete"}]},
          {"name":"plain","steps":[{"when":[],"do":"flag:look"}]}]}"#;
        let flows = from_bytes(file).unwrap();
        assert!(flows[0].serial, "serial was not read");
        assert!(
            !flows[1].serial,
            "a file that says nothing means not serial"
        );

        let again = from_bytes(&to_bytes(&flows).unwrap()).unwrap();
        assert_eq!(again, flows, "serial did not survive the round trip");

        // And through the save path itself, filesystem included.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let saved = save(file).unwrap();
        assert!(saved[0].serial);
        let reloaded = load().unwrap();
        assert_eq!(reloaded, flows, "what save wrote is not what load reads");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A picker built from the vocabulary produces words the parser takes.
    ///
    /// The page renders a kind and, where there is one, a box for its argument — and then joins them
    /// back with a colon. That join is the moment a UI can produce something no file could contain,
    /// so it is done against the same table the parser reads and asserted here.
    #[test]
    fn what_a_picker_would_build_is_what_the_parser_reads() {
        for word in conditions() {
            // The whole spelling where there is no argument, because a word like `base:trunk` is
            // one entry, not a kind with a box beside it — same rule as `update-branch:rebase`.
            let built = match word.arg.is_empty() {
                true => word.spelling.clone(),
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
    /// Every action the parser takes is one a picker offers — the direction the table→parser test
    /// below cannot see.
    ///
    /// [`Act::Wait`] lived in [`Act::parse`] and [`spell_act`] and NOT in [`ACTIONS`], so the two
    /// steps the documented merge train stands still on could not be built or restored in the
    /// cockpit, and a typo'd `waitt:` got an "it can be:" list with the wanted word missing from it
    /// — [`unknown`] reads the same table. A table that is authoritative in one direction only is
    /// not a table, it is a coincidence (SKEIN-248/315).
    ///
    /// Written over a spelled example of each variant rather than over the variants themselves,
    /// because an enum's cases cannot be enumerated here. **The list below is the thing to extend
    /// when a variant is added**, and the compiler helps: `Act` is matched exhaustively in
    /// [`spell_act`], so a new variant cannot be added without touching that function, and this
    /// test is named in its neighbourhood.
    ///
    /// Two assertions per variant, and they fail for different reasons: the head must be a word
    /// somebody can pick, and the whole spelling must parse back to what spelled it — a table row
    /// whose spelling the parser then refuses is the same defect facing the other way.
    #[test]
    fn every_action_the_parser_takes_is_one_a_picker_offers() {
        let heads: std::collections::BTreeSet<&str> = ACTIONS
            .iter()
            .map(|(spelling, _)| spelling.split(':').next().unwrap_or(spelling))
            .collect();
        for act in [
            Act::AddLabel("x".into()),
            Act::RemoveLabel("x".into()),
            Act::UpdateBranch(Update::Rebase),
            Act::UpdateBranch(Update::Merge),
            Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false,
            }),
            Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            }),
            Act::Merge(Merge {
                how: MergeAs::Merge,
                delete_branch: false,
            }),
            Act::Merge(Merge {
                how: MergeAs::Merge,
                delete_branch: true,
            }),
            Act::Merge(Merge {
                how: MergeAs::Rebase,
                delete_branch: false,
            }),
            Act::Flag("why".into()),
            Act::Wait("why".into()),
        ] {
            let spelled = spell_act(&act);
            let head = spelled.split(':').next().unwrap_or(&spelled);
            assert!(
                heads.contains(head),
                "the parser takes `{spelled}` and no picker offers it, so it can be run and not \
                 written: ACTIONS heads are {heads:?}"
            );
            assert_eq!(
                Act::parse(&spelled).ok().as_ref(),
                Some(&act),
                "`{spelled}` does not parse back to what spelled it"
            );
        }
    }

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
