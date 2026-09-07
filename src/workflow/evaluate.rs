//! The deciding: at most one step, and the two choices no workflow file may make.
//!
//! [`next`] picks one [`Chosen`] step from GitHub's current answer, or `None` — which is what a
//! healthy workflow says most of the time and is a real answer rather than a failure to decide.
//! **One step per evaluation**, because the moment an action lands what skein believes is one
//! action out of date, and a cascade would merge on it.
//!
//! Beside it are the overrides. A workflow file is a thing a person edits, so two of its choices
//! are taken away from it here rather than trusted to it:
//! [`instead_of_merging_off_the_trunk`] and [`instead_of_approving_what_was_not_wholly_read`] are
//! applied whatever the file asked for.
//!
//! Nothing here acts. The same function drives the dry run and the tick, so what a person is shown
//! before they trust it is by construction what will happen — acting is `crate::prwork::perform`.

use super::{holds, spell_cond, Act, Cond, Facts, Workflow};

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
            act: instead_of_merging_off_the_trunk(&s.act, facts)
                .or_else(|| instead_of_approving_what_was_not_wholly_read(&s.act, facts))
                .unwrap_or_else(|| s.act.clone()),
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

/// **The second thing a written-down step may not talk skein into: approving what it did not
/// wholly read** (`docs/pr-review.md` §7c).
///
/// The owner chose unattended approvals, and this is not a gate on that choice — it is the one rule
/// the design proposes as absolute wherever the ceiling sits. Its reason is a measurement rather
/// than a principle: the box this engine is modelled on posted an `APPROVED` and a "not approving"
/// from the same account 53 seconds apart, because one pass had never opened the file with the
/// defect in it. Access is not the same as having looked — that box had a checkout the whole time.
///
/// The two answers differ for exactly the reason [`instead_of_merging_off_the_trunk`]'s do, and the
/// distinction is carried over deliberately:
///
/// * **The pass is known not to have covered the change** — the sweep accounted for it and came
///   back short. That is a standing fact about this reading, so it becomes [`Act::Flag`]: the
///   workflow stops, in writing, and a person can see that it will not approve. A later reading of
///   the same head clears it by covering the change.
/// * **Coverage is not known at all** — no reading has been made, or nothing can answer yet, which
///   is where every fact stands today. That is blindness, and blindness is not a verdict. It
///   becomes [`Act::Wait`], which writes nothing down and stops nothing.
///
/// **Only the approval.** Findings and a request for changes are untouched, because §7c permits
/// both on a partial pass: a reader who saw half a change and found a bug in that half has
/// something true to say. It is the verdict that discharges a review, and only that, which needs
/// the whole of it.
///
/// As with the trunk, there is deliberately no way to spell "approve without reading all of it".
pub fn instead_of_approving_what_was_not_wholly_read(act: &Act, facts: &Facts) -> Option<Act> {
    match act {
        Act::PostApproval => match facts.reading_whole {
            Some(true) => None,
            Some(false) => Some(Act::Flag(
                "not approving: the reading did not cover the whole change".into(),
            )),
            None => Some(Act::Wait(
                "waiting for a reading that covers the whole change".into(),
            )),
        },
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
    // `super::*` for this file's own items, private ones included; `crate::workflow::*` for
    // the rest of the engine, which was one namespace before this module became a directory
    // and still is from outside. A test reads the vocabulary the way a caller does.
    use super::*;
    #[allow(unused_imports)]
    use crate::workflow::*;

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

    /// **No workflow can spell "approve what it did not wholly read"** (`docs/pr-review.md` §7c).
    ///
    /// The flow is written the way a person would write it and states **no condition at all** about
    /// coverage — which is the case that matters. Somebody who writes `post-approval` on its own
    /// must not thereby have written an unconditional approval, because the rule §7c states is not
    /// a gate a file may decline: the owner chose unattended approvals, and this is the one thing
    /// that stays true wherever that switch sits.
    ///
    /// One flow, three runs, and the ONLY thing that differs between them is what skein knows about
    /// the reading — so the flow's own text cannot be what produced the difference.
    ///
    /// Sabotage: drop `instead_of_approving_what_was_not_wholly_read` from [`next`], and the first
    /// two rows come back as `PostApproval`.
    #[test]
    fn no_workflow_can_spell_approving_a_change_it_did_not_wholly_read() {
        let flow = from_bytes(
            br#"{"workflow":[{"name":"review","steps":[{"when":[],"do":"post-approval"}]}]}"#,
        )
        .expect("the reviewer vocabulary parses off disk")
        .remove(0);

        // Nothing has read it. Blindness is not a verdict: nothing is written down and nothing
        // stops, so the moment a reading covers the change the same flow approves.
        let blind = next(
            &flow,
            &Facts {
                reading_whole: None,
                ..Default::default()
            },
        )
        .expect("a step with no conditions always applies");
        assert!(
            matches!(blind.act, Act::Wait(_)),
            "with no reading at all a workflow approved anyway: {:?}",
            blind.act
        );

        // The sweep accounted for the pass and came back short. A standing fact about this
        // reading, so it stops in writing.
        let partial = next(
            &flow,
            &Facts {
                reading_whole: Some(false),
                ..Default::default()
            },
        )
        .expect("a step with no conditions always applies");
        assert!(
            matches!(partial.act, Act::Flag(_)),
            "a pass that did not cover the change approved it: {:?}",
            partial.act
        );

        // And the rule is not a refusal to approve at all — it is a refusal to approve THIS.
        let whole = next(
            &flow,
            &Facts {
                reading_whole: Some(true),
                ..Default::default()
            },
        )
        .expect("a step with no conditions always applies");
        assert!(
            matches!(whole.act, Act::PostApproval),
            "a reading that covered the whole change was still not allowed to approve: {:?}",
            whole.act
        );
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
                                                // The documented train is the AUTHOR side and
                                                // says none of the reviewer's words, so the
                                                // reviewer facts are left as skein having looked
                                                // nothing up — which holds no reviewer condition
                                                // at all (`docs/pr-review.md` §7).
                                                ..Default::default()
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
                checks_owed: None,
                replied_to_me: None,
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
                // The reviewer's facts turned up as loud as they go, for the same reason as every
                // line above: the guard must read the base and nothing else, and a fact it might
                // one day be tempted to read is one this world has to disagree about.
                review_requested: true,
                my_review: "approved".into(),
                my_review_current: true,
                reviews_whole: true,
                head_sha: "abc".into(),
                reading_sha: Some("abc".into()),
                reading_whole: Some(true),
                findings_blocking: Some(true),
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
}
