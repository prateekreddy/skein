//! **The reviewer's steps, composed into a workflow somebody could actually switch on.**
//!
//! `docs/pr-review.md` §15 built a vocabulary — `read`, `audit`, `post-changes`, `post-approval`
//! and the conditions that guard them — one step at a time, and every test of it so far has been a
//! test of one piece. Nothing had ever written the pieces down together, and a fleet has no default
//! workflows: `~/.skein/workflows.json` is a file a person writes. So the whole reviewer engine
//! could be complete, gated, tested and unreachable, because nobody had a flow to turn on.
//!
//! This walks the example in §9 of that document, **parsed out of the document itself** with the
//! parser production uses. Two things follow from reading it there rather than restating it here:
//! an example that stops parsing fails this, and an example that parses but no longer reaches the
//! steps it claims to fails it too.
//!
//! CLAUDE.md's rule for this project is the reason it is worth a file of its own:
//!
//! > A feature that cannot be written as a composition of the five primitives means the primitive
//! > set is wrong, and the fix is the primitive set, not a mechanism beside it.
//!
//! Composing it is what found the gap recorded at the bottom.

use skein::workflow::{next, Act, Facts, Workflow};

/// The workflow §9 documents, taken out of the document.
///
/// The fence is found by its info string rather than by counting blocks, so re-ordering the
/// document cannot silently point this at something else.
fn documented_flow() -> Vec<Workflow> {
    let doc = std::fs::read_to_string("docs/pr-review.md").expect("the design document");
    let open = doc
        .find("```json reviewer-workflow")
        .expect("docs/pr-review.md no longer carries a ```json reviewer-workflow fence");
    let body = &doc[open + "```json reviewer-workflow".len()..];
    let close = body.find("```").expect("the fence is not closed");
    skein::workflow::from_bytes(body[..close].as_bytes())
        .expect("the documented workflow does not parse with the parser production uses")
}

fn the_flow() -> Workflow {
    documented_flow().into_iter().next().expect("one workflow")
}

/// **The ladder, walked in the order a real pull request climbs it.**
///
/// Each row is the state the previous row's action leaves behind, so this is the engine's own
/// sequence rather than four independent assertions: read, audit what the change owes, then the
/// verdict. `next` takes the FIRST step whose conditions all hold, which is why `read` is written
/// last in the file — it is the fallback for every state the specific steps do not claim, and the
/// specific ones take over the moment a reading exists at the head.
///
/// **What would make this fail:** re-ordering the documented steps so `read` comes first, which
/// makes it the answer for ever and no verdict is ever reached. That is not a hypothetical — it is
/// the shape a person writes the file in on the first try.
#[test]
fn the_documented_reviewer_workflow_climbs_from_a_reading_to_a_verdict() {
    let flow = the_flow();
    let act = |facts: &Facts| next(&flow, facts).map(|c| c.act);

    // 1. Nothing known. `reading-current` and `reading-stale` both fail on unknown, `checks-owed`
    //    and `checks-settled` both fail on unknown — so only the fallback claims it.
    assert_eq!(
        act(&Facts::default()),
        Some(Act::Read),
        "a pull request skein has never read did not get read"
    );

    // 2. A reading at this head, and the diff owes an audit.
    let owing = Facts {
        reading_sha: Some("abc".into()),
        head_sha: "abc".into(),
        checks_owed: Some(true),
        reading_whole: Some(true),
        ..Default::default()
    };
    assert_eq!(
        act(&owing),
        Some(Act::Audit),
        "a change that removes lines went straight to a verdict — docs/pr-review.md §8"
    );

    // 3. The audit is recorded, and the sweep accounted for the whole change.
    let settled = Facts {
        checks_owed: Some(false),
        ..owing.clone()
    };
    assert_eq!(
        act(&settled),
        Some(Act::PostApproval),
        "an audited, wholly-read change never reached a verdict"
    );

    // 4. The same, with no sweep behind the reading. The step's own `reading-whole` holds it back,
    //    and the fallback takes it — which is the right answer and better than waiting: a re-read
    //    is how the sweep that §7c makes an approval wait on gets bought at all.
    let unswept = Facts {
        reading_whole: None,
        ..settled.clone()
    };
    assert_eq!(
        act(&unswept),
        Some(Act::Read),
        "an approval was reachable on a reading nothing accounted for"
    );
}

/// **A workflow that leaves `reading-whole` out still cannot approve**, because the guard is in
/// `next` and not in the file.
///
/// The documented flow states the condition, and a person copying it will keep it. This is about
/// the one who does not: §7c's rule — *"a box once shipped an APPROVED and a 'not approving' from
/// the same account 53 seconds apart because one pass had never opened the file with the defect in
/// it"* — may not be something a workflow can write its way past.
///
/// **What would make this fail:** moving `instead_of_approving_what_was_not_wholly_read` out of
/// `next` and into the documented conditions, which is exactly the refactor that looks tidy.
#[test]
fn a_workflow_that_omits_the_coverage_condition_still_cannot_approve_unswept_work() {
    let careless: Vec<Workflow> = skein::workflow::from_bytes(
        br#"{"workflow":[{"name":"careless","steps":[
             {"when":["reading-current"],"do":"post-approval"}]}]}"#,
    )
    .expect("a workflow that states less than it should still parses");
    let chosen = next(
        &careless[0],
        &Facts {
            reading_sha: Some("abc".into()),
            head_sha: "abc".into(),
            reading_whole: None,
            ..Default::default()
        },
    )
    .map(|c| c.act);
    assert!(
        matches!(chosen, Some(Act::Wait(_))),
        "a workflow wrote its way past §7c and approved a reading nothing accounted for: {chosen:?}"
    );
}

/// **`post-changes` is in the workflow and cannot be reached from the condition that guards it** —
/// found by composing the steps, which is the only way it could have been found.
///
/// The step is written `["reading-current", "checks-settled", "findings-blocking"]`, and it is the
/// right way to write it. But `prwork::facts_of_in` sets `findings_blocking: None` unconditionally
/// and always has — deliberately, and its comment says why: *"the findings are on GitHub — the
/// reading posts its own review and skein keeps no copy"*. `Cond::FindingsBlocking` holds only on
/// `Some(true)`, so in production it never holds, and this step never fires.
///
/// **The consequence, stated plainly: the engine can approve unattended and can never refuse.**
/// That is exactly the asymmetry §13 records the argument about, arrived at from the other end —
/// not as a policy somebody chose but as a gap in what skein knows about its own reading.
///
//// **The engine can refuse, and only on an answer somebody actually gave.**
///
/// This test used to assert the GAP. `facts_of_in` wrote `findings_blocking: None`
/// unconditionally — the findings live on GitHub, skein keeps no copy (§5), and nothing in the
/// adapter could read them — so `Act::PostChanges` was written into the workflow and unreachable by
/// its intended guard. The old test read the adapter's source for that literal and said, in its own
/// message, that the day the fact became answerable it should be deleted and the engine trusted
/// with a refusal. That day is 2026-09-03.
///
/// What made it answerable was not access to GitHub. The sweep — the turn that already accounts for
/// what the reading covered — is now asked whether what it raised must block, and the answer is
/// recorded against the sha in `review::Summary::findings_block`, exactly as `owed_triggered` is.
///
/// So the assertion inverts: the step fires on `Some(true)`, and on nothing else. **`None` is the
/// half worth keeping** — the two-stage path runs no sweep, a sweep that did not finish said
/// nothing, and an answer that would not parse is not an answer. Every one of those must leave the
/// refusal unreachable, because a refusal posted from a fact nobody looked up is the same failure
/// as an approval granted by silence, pointed the other way.
#[test]
fn a_refusal_fires_on_a_blocking_reading_and_on_no_other_answer() {
    let flow = the_flow();
    let current = Facts {
        reading_sha: Some("abc".into()),
        head_sha: "abc".into(),
        checks_owed: Some(false),
        reading_whole: Some(true),
        ..Default::default()
    };
    let with = |blocking| {
        next(
            &flow,
            &Facts {
                findings_blocking: blocking,
                ..current.clone()
            },
        )
        .map(|c| c.act)
    };

    assert_eq!(
        with(Some(true)),
        Some(Act::PostChanges),
        "a reading that said its findings block cannot reach the refusal step, so the engine can \
         still only ever approve"
    );
    assert_ne!(
        with(Some(false)),
        Some(Act::PostChanges),
        "a reading that said nothing blocks reached the refusal anyway"
    );
    assert_ne!(
        with(None),
        Some(Act::PostChanges),
        "the refusal fired off a fact nobody looked up — no sweep ran, or its answer would not \
         parse, and skein requested changes on somebody's pull request on the strength of it"
    );
}
