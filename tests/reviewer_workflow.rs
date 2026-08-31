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
/// `Act::PostChanges` itself is not unreachable: a person can guard it on anything, and
/// `["label:blocked"]` would fire today. What cannot be reached is the *intended* guard.
///
/// This test asserts the gap rather than papering over it, so the day `findings_blocking` becomes
/// answerable it fails and says the workflow can be trusted with a refusal.
#[test]
fn a_refusal_is_written_into_the_workflow_and_nothing_in_the_queue_can_trigger_it() {
    let flow = the_flow();
    let current = Facts {
        reading_sha: Some("abc".into()),
        head_sha: "abc".into(),
        checks_owed: Some(false),
        reading_whole: Some(true),
        ..Default::default()
    };

    // The step IS there and IS reachable — given the fact.
    assert_eq!(
        next(
            &flow,
            &Facts {
                findings_blocking: Some(true),
                ..current.clone()
            }
        )
        .map(|c| c.act),
        Some(Act::PostChanges),
        "the workflow no longer carries a refusal step at all"
    );

    // And nothing in the queue produces that fact. Read off the adapter rather than asserted:
    // `facts_of_in` is the only production caller that builds `Facts` from a pull request.
    let adapter = std::fs::read_to_string("src/prwork.rs").expect("the adapter");
    assert!(
        adapter.contains("findings_blocking: None,"),
        "`facts_of_in` no longer hard-codes `findings_blocking: None` — if the queue can now say \
         whether a reading found something blocking, this test is the thing to delete, and the \
         engine can be trusted with a refusal as well as an approval"
    );
}
