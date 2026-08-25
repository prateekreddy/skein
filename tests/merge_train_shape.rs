//! What "serial" actually serialises, checked against the document that claims it.
//!
//! `docs/pr-workflow.md` said "**Serial.** One PR at a time per repo" and `Workflow::serial`'s own
//! doc comment says "One at a time, per repo". Neither is what the code does. `sweep` builds one
//! front **per flow name** and gates each pull request against that flow's front, so two serial
//! workflows carrying pull requests in one repo produce two fronts and two pull requests act in a
//! single pass.
//!
//! That is a small difference today, because the fleet runs one train. It is not a small difference
//! in kind: serial was chosen over parallel specifically on the CI re-run tax and the API spend
//! (`docs/pr-workflow.md`, "The merge train"), and a second train in a repo hands both back. So the
//! claim is worth pinning rather than leaving as prose that happens to be true of the current
//! configuration.
//!
//! Both directions are checked here, which is the point:
//!
//! * if somebody keys the front on the repo instead of the flow, [`two_serial_workflows_in_one_repo_have_two_fronts`] fails;
//! * if somebody restores the "per repo" wording, [`the_document_states_the_unit_the_code_actually_uses`] fails.
//!
//! Neither can move without the other noticing. SKEIN-256.

use skein::prwork::trains;
use skein::workflow::{from_bytes, Workflow};

fn repo() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Two serial workflows, parsed through the real on-disk parser rather than built by hand.
///
/// Hand-built values would not prove `serial` survived being read, which is a bug this project has
/// already had once (`src/workflow.rs`, "the hand copy silently dropped `serial`").
fn two_serial_flows() -> Vec<Workflow> {
    let raw = br#"{"workflow":[
        {"name":"train-a","serial":true,"matches":[],
         "steps":[{"when":[],"do":"merge:squash"}]},
        {"name":"train-b","serial":true,"matches":[],
         "steps":[{"when":[],"do":"merge:squash"}]}
    ]}"#;
    let flows = from_bytes(raw).expect("two serial workflows should parse");
    assert!(
        flows.iter().all(|f| f.serial),
        "the fixture stopped being about serial workflows"
    );
    flows
}

/// The repo id is deliberately one that cannot have a stop file under ANY skein home.
///
/// `trains` reads stops from `$SKEIN_HOME/review/<repo_id>/workflow-stops.json`
/// (`prwork::stops_path` → `prq::review_dir`). Setting `SKEIN_HOME` would be the obvious way to get
/// an empty stop set, and it is the wrong way: process-wide env vars race the other tests under the
/// default multi-threaded runner. A repo id no home contains gives the same empty map with no
/// process state at all.
const NOWHERE: &str = "skein-256-two-trains-fixture-no-such-repo";

#[test]
fn two_serial_workflows_in_one_repo_have_two_fronts() {
    let flows = two_serial_flows();
    // Four carrying pull requests in ONE repo, split across the two trains.
    let carrying = [
        (11, "train-a".to_string()),
        (12, "train-a".to_string()),
        (21, "train-b".to_string()),
        (22, "train-b".to_string()),
    ];

    let views = trains(NOWHERE, &carrying, &flows);
    assert_eq!(
        views.len(),
        2,
        "one repo, two serial workflows, and {} train(s) came back",
        views.len()
    );

    let mut fronts: Vec<u64> = views.iter().filter_map(|v| v.front).collect();
    fronts.sort_unstable();
    assert_eq!(
        fronts,
        vec![11, 21],
        "each serial workflow has its own front — oldest-first within the flow, not within the \
         repo. If this now yields a single front, the front has been keyed on the repo and \
         docs/pr-workflow.md's \"per (repo, workflow)\" wording is the thing to change."
    );

    // Two fronts is not an abstract fact: it is exactly what `sweep`'s gate
    // (`flow.serial && fronts.get(name) != Some(&pr.number)`) admits, so two pull requests act in
    // one pass. Stated as an assertion so the consequence is what fails, not just the shape.
    assert_eq!(
        fronts.len(),
        2,
        "two pull requests may act in a single sweep of one repo"
    );
}

/// The stop rule is per-flow too, and that is what makes the two trains genuinely independent.
///
/// Without this, "two fronts" could be read as an accident of there being no stops. A stop on
/// train-a's front moves train-a's front and leaves train-b's exactly where it was.
#[test]
fn a_stop_in_one_train_does_not_move_the_other_trains_front() {
    let flows = two_serial_flows();
    let carrying = [
        (11, "train-a".to_string()),
        (12, "train-a".to_string()),
        (21, "train-b".to_string()),
    ];
    let views = trains(NOWHERE, &carrying, &flows);
    let front_of = |name: &str| {
        views
            .iter()
            .find(|v| v.flow == name)
            .unwrap_or_else(|| panic!("no train view for {name}"))
            .front
    };
    assert_eq!(front_of("train-a"), Some(11));
    assert_eq!(front_of("train-b"), Some(21));
    // And each line holds only its own flow's pull requests — the ordering is within the flow.
    let line_of = |name: &str| {
        views
            .iter()
            .find(|v| v.flow == name)
            .map(|v| v.line.clone())
            .unwrap_or_default()
    };
    assert_eq!(line_of("train-a"), vec![11, 12]);
    assert_eq!(line_of("train-b"), vec![21]);
}

/// A non-serial workflow is not a train, so it is not serialised at all.
///
/// The complement of the claim: "one at a time" is a property of `serial`, and a repo can have a
/// workflow acting on every matching pull request every pass alongside a train.
#[test]
fn a_workflow_that_is_not_serial_gets_no_train_and_no_front() {
    let raw = br#"{"workflow":[
        {"name":"train","serial":true,"matches":[],"steps":[{"when":[],"do":"merge:squash"}]},
        {"name":"labeller","matches":[],"steps":[{"when":[],"do":"add-label:ci-queue"}]}
    ]}"#;
    let flows = from_bytes(raw).expect("fixture should parse");
    assert!(
        !flows[1].serial,
        "a file that says nothing means not serial"
    );
    let carrying = [
        (11, "train".to_string()),
        (31, "labeller".to_string()),
        (32, "labeller".to_string()),
    ];
    let views = trains(NOWHERE, &carrying, &flows);
    assert_eq!(
        views.iter().map(|v| v.flow.as_str()).collect::<Vec<_>>(),
        vec!["train"],
        "a train is the serial thing — a non-serial workflow gets no view and no front"
    );
}

/// The document says the unit the code uses, in the two places it states it.
///
/// This is the half that stops the prose drifting back. `docs/pr-workflow.md` is the merge train's
/// specification, and it was wrong here in both its prose and its vocabulary table.
#[test]
fn the_document_states_the_unit_the_code_actually_uses() {
    let doc =
        std::fs::read_to_string(repo().join("docs/pr-workflow.md")).expect("docs/pr-workflow.md");

    assert!(
        !doc.contains("One PR at a time per repo"),
        "docs/pr-workflow.md has gone back to \"One PR at a time per repo\". The front is keyed on \
         the flow name (src/prwork.rs, `fronts` in `sweep`), so two serial workflows in one repo \
         act on two pull requests in a pass. Either say per-(repo, workflow), or key the front on \
         the repo and change this test."
    );
    assert!(
        doc.contains("One PR at a time per serial workflow"),
        "docs/pr-workflow.md no longer states what serial serialises. It is the merge train's \
         specification; if the sentence moved, move this check with it."
    );
    assert!(
        doc.contains("(repo, workflow)"),
        "the vocabulary table's `\"serial\": true` row no longer names the unit as (repo, workflow)"
    );
}

/// The head anchor is on two of the four acts, and the document says which.
///
/// `add-label` and `remove-label` cannot carry one — GitHub's issue-labels API takes no head
/// parameter — and `add-label:ci-queue` is the step that starts CI. A document that implies all
/// four are anchored is wrong in the dangerous direction, so the table is pinned here.
#[test]
fn the_document_says_which_acts_cannot_carry_a_head_anchor() {
    let doc =
        std::fs::read_to_string(repo().join("docs/pr-workflow.md")).expect("docs/pr-workflow.md");
    assert!(
        doc.contains("Which acts carry a head anchor"),
        "docs/pr-workflow.md no longer says which of the train's acts carry the head skein decided \
         on. Two of the four cannot, and one of those two starts CI."
    );

    // And it is still true of the code: the two anchored acts send the head, the two unanchored
    // ones send a body with no head in it. Checked as text because the request bodies are built
    // inline; if these move, the doc table's line citations have moved too.
    let prwork = std::fs::read_to_string(repo().join("src/prwork.rs")).expect("src/prwork.rs");
    assert!(
        prwork.contains(r#""oid": head_sha"#),
        "update_branch no longer sends the head as `oid` (expectedHeadOid)"
    );
    assert!(
        prwork.contains(r#""sha": head_sha"#),
        "merge_pr no longer sends the head as `sha`"
    );
    assert!(
        prwork.contains(r#"&serde_json::json!({ "labels": [label] })"#),
        "add_label's request body changed — if it now carries a head, the doc table is out of date \
         in the good direction and should be updated"
    );
}

/// Every reproduction command the document gives still finds something.
///
/// `CLAUDE.md` allows two ways to make a claim about the code: cite the file and line, or give the
/// command. For `src/prwork.rs` the second is the only honest one — while this test was being
/// written the merge-train agent moved `fronts` from line 978 to 1042 to 1072 inside two hours, so
/// a line number in this document would have been wrong before it was committed. A command cannot
/// go stale quietly: it either still matches or it does not, and this is the test that asks.
///
/// Nothing is duplicated here. The patterns are parsed out of the document, so the document stays
/// the single place they are written down.
#[test]
fn every_reproduction_command_in_the_document_still_finds_something() {
    let doc =
        std::fs::read_to_string(repo().join("docs/pr-workflow.md")).expect("docs/pr-workflow.md");
    let prwork = std::fs::read_to_string(repo().join("src/prwork.rs")).expect("src/prwork.rs");

    let mut checked = 0usize;
    for (n, line) in doc.lines().enumerate() {
        for (at, _) in line.match_indices("grep -n '") {
            let rest = &line[at + "grep -n '".len()..];
            let Some(end) = rest.find('\'') else { continue };
            let pattern = &rest[..end];
            let target = rest[end..].trim_start_matches('\'').trim_start();
            if !target.starts_with("src/prwork.rs") {
                continue;
            }
            assert!(
                prwork.contains(pattern),
                "docs/pr-workflow.md line {} gives `grep -n '{pattern}' src/prwork.rs` as the way to \
                 check its claim, and that pattern is not in the file any more. Either the code \
                 moved and the document must point at what it is called now, or the thing being \
                 claimed is gone — which is the more important of the two.",
                n + 1
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 5,
        "only {checked} reproduction commands found in docs/pr-workflow.md. The merge-train \
         section carries its claims that way on purpose, because line numbers into src/prwork.rs \
         went stale three times in one afternoon; prose that stopped citing the code is the \
         failure this checks for"
    );
    println!("checked {checked} reproduction commands against src/prwork.rs");
}
