//! The closed vocabulary: what a workflow may say, and how each word is written down.
//!
//! Two halves that cannot be separated. The types are what the rest of the engine matches on; the
//! tables beside them are the spelling those types have on disk and in the cockpit's dropdowns.
//! They live in one file because `CONDITIONS` and `ACTIONS` are the source for BOTH the parser and
//! the picker — see [`CONDITIONS`] for why that is one table rather than two — and a parser in a
//! different file from the table it parses against is exactly the disagreement that arrangement
//! exists to make impossible.
//!
//! Nothing here reads a fact or decides anything. See [`super::facts`] for what may be asked about
//! a pull request and [`super::evaluate`] for which step fires.

use serde::{Deserialize, Serialize};
// In scope for the doc links, not for the code. These items are in sibling files now, and
// rustdoc resolves an intra-doc link against what the file it appears in imports — so
// without this line every `[`Facts::approved`]` below silently stops being a link. Nothing gates
// `cargo doc`, which is exactly why the rot would be invisible.
#[allow(unused_imports)]
use super::{claims, holds, instead_of_merging_off_the_trunk, unmet, Facts};

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

    /// **GitHub is asking you for a review, by name** — `prq::Pr::my_review_requested`.
    ///
    /// The first of the reviewer's words (`docs/pr-review.md` §6). It is a floor rather than a
    /// census: a request made of a TEAM you belong to arrives as the team, and without `read:org`
    /// with no name at all, so this can be false where GitHub would say you were asked. The error
    /// only ever falls towards NOT claiming you, which is the direction every guard here leans.
    ReviewRequested,
    /// **You have never decided on it, and skein saw every review.**
    ///
    /// A claim about the reviews that did NOT arrive, so it is [`Cond::NoLabel`]'s shape and not
    /// [`Cond::Label`]'s: `prq::Pr::my_review` is read out of a capped connection, so a viewer
    /// whose own row sorted past the cap reads as never having decided. See
    /// [`Facts::reviews_whole`] — where skein did not see the whole list this holds neither way
    /// and the pull request waits (`docs/pr-review.md` §7b).
    ///
    /// A comment is deliberately not a decision, which is `prq`'s own rule for the lane and is
    /// followed here so the two cannot disagree about what deciding is.
    Unreviewed,
    /// **A reading exists, at the head that is there now.** [`Facts::reading_sha`] equals
    /// [`Facts::head_sha`].
    ///
    /// The sha guard of `docs/pr-review.md` §4: a memoryless engine splits reading from posting
    /// across polls, and the head can move between them by design, so a post whose reading was
    /// made at another commit would describe tree A anchored to tree B.
    ReadingCurrent,
    /// **A reading exists, at an older commit.** The other side of the guard: the next step is to
    /// read again, never to post what the old reading said.
    ///
    /// Unknown satisfies neither this nor [`Cond::ReadingCurrent`] — see [`Facts::reading_sha`].
    ReadingStale,
    /// **The sweep ran and accounted for every changed file, at one commit.**
    ///
    /// `docs/pr-review.md` §7c, and the one condition an approval may not be posted without: a box
    /// once shipped an `APPROVED` and a "not approving" from the same account 53 seconds apart
    /// because one pass had never opened the file with the defect in it. Access is not the same as
    /// having looked, so the evidence is the sweep rather than the size of the diff.
    ///
    /// Three-valued, and unknown does not hold — see [`Facts::reading_whole`].
    ReadingWhole,
    /// **The reading found something that must block.** Three-valued for the same reason as
    /// [`Cond::ReadingWhole`]; see [`Facts::findings_blocking`].
    FindingsBlocking,
    /// **Your own approval or refusal is against the head that is there now.**
    ///
    /// [`Facts::my_review`] and [`Facts::my_review_current`] together, and neither of them
    /// [`Facts::approved`] (`docs/pr-review.md` §7a). It is the freshness half of the box's
    /// compare-and-set — the half that survives a single-tick engine — and it is what stops a
    /// verdict left at an older commit reading as one given about this code.
    VerdictStanding,
    /// **This repository owes a check that this change fired and nobody has answered at this
    /// commit** — `docs/pr-review.md` §8.
    ///
    /// The scar as a condition. The step it guards is [`Act::Audit`], and the sentence §8 writes it
    /// as is *"if the diff deletes lines and no deletion audit is recorded at this sha, the next
    /// step is `Audit`, not a post"*.
    ///
    /// Three-valued, and unknown holds neither this nor [`Cond::ChecksSettled`] — see
    /// [`Facts::checks_owed`]. What is owed is computed from the diff, so a pull request with no
    /// reading at this head has no answer here rather than a convenient one.
    ChecksOwed,
    /// **Nothing is owed at this commit**: either nothing fired, or everything that did has been
    /// answered. The other side of the guard, and what a post waits on.
    ChecksSettled,
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
    /// **Read this pull request at the head it is at now** — `docs/pr-review.md` §6, done by
    /// `crate::prwork::read_now`.
    ///
    /// It spends a reading and files it against `(number, head_sha)`, which is what makes
    /// [`Cond::ReadingCurrent`] and [`Cond::ReadingWhole`] answerable on the next evaluation. §11
    /// moves where that reading RUNS — into this pull request's own review box, standing detached
    /// at the head — and changes nothing about what this act means.
    ///
    /// **It is not a read-only act.** The session that does the reading posts its own comment
    /// review to GitHub from inside that box, under the credential it was handed; it is forbidden
    /// only the two verdicts, and forbidden them by its prompt. See the module note.
    Read,
    /// Submit what the reading found, as a comment review (`prq::Verdict::Comment`).
    PostFindings,
    /// Submit a refusal (`prq::Verdict::RequestChanges`).
    PostChanges,
    /// Submit an approval (`prq::Verdict::Approve`). The one action [`Cond::ReadingWhole`] is a
    /// required condition of, and of nothing else — `docs/pr-review.md` §7c.
    PostApproval,
    /// One owed check from `docs/pr-review.md` §8, recorded against the sha — the scar the
    /// interviewed box carried across four hours, written as a condition rather than as a
    /// paragraph of prompt a fresh agent may or may not weigh.
    ///
    /// **No argument, deliberately.** The owed checks are a per-repo file (§8, §15 step 5); a
    /// string here would be the escape hatch this set exists not to have — the thing a person
    /// could write a command into.
    Audit,
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

/// Every condition that can be written, with the spelling used on disk and in the UI.
///
/// One table, so the parser and the picker cannot disagree about what exists — a dropdown offering
/// something the parser refuses is the same defect as a parser accepting something no dropdown can
/// produce, and both are found only by a person typing it.
pub const CONDITIONS: [(&str, &str); 24] = [
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
    // The reviewer's words (`docs/pr-review.md` §6). Same table, because a picker offering one
    // vocabulary and a parser taking two is the drift these tables exist to make impossible.
    (
        "review-requested",
        "GitHub is asking you for a review by name",
    ),
    (
        "unreviewed",
        "you have never decided on it, and skein saw every review",
    ),
    (
        "reading-current",
        "skein has read it at the head it is at now",
    ),
    ("reading-stale", "skein has read it, at an older commit"),
    (
        "reading-whole",
        "that reading accounted for every changed file, at one commit",
    ),
    (
        "findings-blocking",
        "the reading found something that must block",
    ),
    (
        "verdict-standing",
        "your approval or refusal is against the head it is at now",
    ),
    (
        "checks-owed",
        "this change fired a check this repo owes, and nobody has answered it at this commit",
    ),
    (
        "checks-settled",
        "nothing this repo owes is outstanding at this commit",
    ),
];

/// Every action that can be written. See [`CONDITIONS`] for why this is a table.
pub const ACTIONS: [(&str, &str); 15] = [
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
    // The reviewer's actions (§15 steps 3-5). Four of the five act; `post-findings` is offered,
    // spelled and parsed, and `crate::prwork::perform` refuses it out loud — see the module note
    // for why a vestige stays in the picker.
    ("read", "read it at the head it is at now"),
    ("post-findings", "post what the reading found, as a comment"),
    ("post-changes", "post a refusal — changes requested"),
    ("post-approval", "post an approval"),
    (
        "audit",
        "do one check this repository owes a reviewer, and record it against the sha",
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
            "review-requested" => Ok(Cond::ReviewRequested),
            "unreviewed" => Ok(Cond::Unreviewed),
            "reading-current" => Ok(Cond::ReadingCurrent),
            "reading-stale" => Ok(Cond::ReadingStale),
            "reading-whole" => Ok(Cond::ReadingWhole),
            "findings-blocking" => Ok(Cond::FindingsBlocking),
            "verdict-standing" => Ok(Cond::VerdictStanding),
            "checks-owed" => Ok(Cond::ChecksOwed),
            "checks-settled" => Ok(Cond::ChecksSettled),
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
            // No arguments, on purpose: see [`Act::Audit`]. A reviewer action names itself and
            // nothing else, so there is nothing here for a file to smuggle a command through.
            "read" => Ok(Act::Read),
            "post-findings" => Ok(Act::PostFindings),
            "post-changes" => Ok(Act::PostChanges),
            "post-approval" => Ok(Act::PostApproval),
            "audit" => Ok(Act::Audit),
            _ => Err(unknown("action", atom, &ACTIONS)),
        }
    }
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
        Cond::ReviewRequested => "review-requested".into(),
        Cond::Unreviewed => "unreviewed".into(),
        Cond::ReadingCurrent => "reading-current".into(),
        Cond::ReadingStale => "reading-stale".into(),
        Cond::ReadingWhole => "reading-whole".into(),
        Cond::FindingsBlocking => "findings-blocking".into(),
        Cond::VerdictStanding => "verdict-standing".into(),
        Cond::ChecksOwed => "checks-owed".into(),
        Cond::ChecksSettled => "checks-settled".into(),
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
        Act::Read => "read".into(),
        Act::PostFindings => "post-findings".into(),
        Act::PostChanges => "post-changes".into(),
        Act::PostApproval => "post-approval".into(),
        Act::Audit => "audit".into(),
    }
}
#[cfg(test)]
mod tests {
    // `super::*` for this file's own items, private ones included; `crate::workflow::*` for
    // the rest of the engine, which was one namespace before this module became a directory
    // and still is from outside. A test reads the vocabulary the way a caller does.
    use super::*;
    #[allow(unused_imports)]
    use crate::workflow::*;

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
            // The reviewer's five (`docs/pr-review.md` §6). Four of them act; `post-findings` is
            // refused by `crate::prwork::perform` and has to be spellable and pickable anyway, for
            // the reason the whole table exists: a word a file can carry and a picker cannot build
            // is a workflow somebody writes by hand and then cannot edit.
            Act::Read,
            Act::PostFindings,
            Act::PostChanges,
            Act::PostApproval,
            Act::Audit,
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
