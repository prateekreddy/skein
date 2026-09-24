//! Doing the one step the evaluator chose — and the gates in front of the reviewer half.
//!
//! [`perform`] takes a [`crate::workflow::Chosen`] and makes it happen, or stops the pull request
//! in writing. **A failure never retries**: the decision was made from facts the failure proves
//! stale, so the workflow stops and stays stopped until a person clears it. That is the module
//! note's "no blind retries", and it is the reason every act below returns a reason rather than a
//! bare error.
//!
//! The reviewer acts — [`Act::Read`], the two verdicts, [`Act::Audit`] — are the ones that can
//! post under your name, and they are the reason this file is longer than the acts it drives. Four
//! gates stand in front of them and **none of them can be written in a workflow file**: the repo's
//! `auto_review` flags, §10's trigger set ([`no_trigger_of_this_repos_fired`]), the author filter
//! ([`not_an_author_this_repo_reviews`]) and the repo's ceiling. [`the_loop_this_repo_has_built`]
//! is the fifth thing, and it does not gate — it reports, because §13's obligation is that the
//! self-approving loop must never be reachable *silently*.

use super::{
    add_label, chosen_by_hand, delete_branch, enabled, merge_pr, record, remove_label, stop,
    stopped, update_branch, Outcome,
};
use crate::workflow::{Act, Chosen, Update, Workflow};

/// The pull request an act is taken against, named the way GitHub's write APIs need it named.
///
/// `head_sha` anchors the two acts that can carry it: `update_branch` sends it as `expectedHeadOid`
/// and `merge_pr` sends it as `sha`, so GitHub refuses rather than acts if somebody pushed between
/// skein deciding and skein acting. That is the same rule as the anchor on a box: prove the thing
/// is what you think before touching it.
///
/// `add_label` and `remove_label` carry no head, and cannot: GitHub's issue-labels API accepts no
/// head parameter on either verb. That matters because `add-label:ci-queue` is the step that starts
/// CI, so a push landing mid-decision starts a run against a head skein never saw. The cost is a
/// wasted CI run and a front that goes round again — not a bad merge, because `merge_pr` re-checks
/// with `sha` and GitHub answers 409. The anchor is on the acts where being wrong ships something.
/// The table is in `docs/pr-workflow.md`, "Which acts carry a head anchor, and which two cannot".
pub struct Subject<'a> {
    /// The repo as skein knows it, which is where the stop is written down.
    pub repo_id: &'a str,
    /// `owner/name` as GitHub knows it.
    pub slug: &'a str,
    pub number: u64,
    /// The commit the decision was made about. The two acts that can carry it do — see the note on
    /// this struct for the two that cannot, and why it is the labels API rather than an oversight.
    pub head_sha: &'a str,
    pub head_ref: &'a str,
    /// What [`Act::Read`] needs, and what no other act does — see [`Reading`].
    pub reading: Option<Reading<'a>>,
}

/// The three things a reading needs that the five fields above cannot supply.
///
/// Carried as an `Option` rather than folded into [`Subject`] because it is honestly optional: the
/// five fields above are what GitHub's write APIs take, and every one of them is a `&str` a caller
/// can hold without having looked anything up. A reading needs the whole [`crate::repos::Repo`] —
/// its flags decide whether it may happen at all — and the whole [`crate::prq::Pr`], which is what
/// the reading path takes; the one production caller has both in hand already.
///
/// **`None` is not "read it anyway with defaults".** It is a caller that cannot read, and
/// [`Act::Read`] refuses out loud rather than inventing a `Repo` — which, with `auto_review`
/// defaulting off, would refuse for the wrong reason and read as a flag problem.
pub struct Reading<'a> {
    /// Whose flags decide whether skein may read this at all — `repos::auto_review_stands`.
    pub repo: &'a crate::repos::Repo,
    /// The pull request as the queue has it. The reading path anchors on `pr.head_sha`, which is
    /// the same commit [`Subject::head_sha`] carries.
    pub pr: &'a crate::prq::Pr,
    /// Who skein is acting as, for CODEOWNERS. One identity, the same one `read_waiting` uses.
    pub viewer: &'a str,
    /// What the step was decided from. `Act::Read` asks it a second question the evaluator does
    /// not: **which of §10's triggers fired**, which is a per-repo gate rather than a step
    /// condition and therefore cannot live in a workflow file.
    pub facts: &'a crate::workflow::Facts,
}

/// Answer one check this repository owes, at the commit the step was decided about —
/// `docs/pr-review.md` §8 and §15 step 5.
///
/// **One check per evaluation**, which is the engine's own rule rather than a throttle: a step is
/// chosen from the state that is there now, and a pass that answered three checks would be three
/// decisions made from one reading of the world. The next evaluation sees one fewer outstanding and
/// picks the next, and `Cond::ChecksSettled` starts holding when the last one is answered.
///
/// # Why the same guards as a reading, in the same order
///
/// An audit spends a model call and can post to the pull request, so every door [`read_now`] opens
/// this one opens too: [`crate::repos::auto_review_stands`] for the money, §10's trigger set and
/// author filter for whether this repository reviews this pull request at all, and the sha anchor
/// so an answer is never filed against a commit the pass did not evaluate. Sharing the shape rather
/// than the code is deliberate — they differ in what they spend it on, and a helper that took a
/// closure would hide which of the two a failure came from.
///
/// # What is recorded, and when
///
/// [`crate::owed::record`] runs **only after** [`crate::review::audit_owed`] returns an answer. A
/// check recorded on a turn that timed out would satisfy §8's condition with nothing behind it,
/// which is a verdict released by a failed model call — the exact shape of the failure the sha
/// guard and the sweep both exist to prevent.
fn audit_now(pr: &Subject) -> ReadStep {
    let Some(reading) = &pr.reading else {
        return ReadStep::Failed(format!(
            "this caller cannot audit #{} — it passed no repo, pull request or viewer (an `audit` \
             step is only takeable from the workflow pass)",
            pr.number
        ));
    };
    if let Some(why) = crate::repos::auto_review_stands_for(
        reading.repo,
        chosen_by_hand(&reading.repo.id, pr.number),
    ) {
        return ReadStep::Failed(why);
    }
    if let Some(why) = no_trigger_of_this_repos_fired(reading.repo, pr.number, reading.facts) {
        return ReadStep::Waited(why);
    }
    if let Some(why) = not_an_author_this_repo_reviews(reading.repo, reading.facts) {
        return ReadStep::Waited(why);
    }
    if reading.pr.head_sha != pr.head_sha {
        return ReadStep::Failed(format!(
            "the step was decided about {} and the audit would be recorded against {} — refusing \
             to audit #{} at a commit this pass did not evaluate",
            pr.head_sha, reading.pr.head_sha, pr.number
        ));
    }
    let Some(check) = the_first_check_still_owed(reading.repo, pr.number, pr.head_sha) else {
        // Not a fault: `Cond::ChecksOwed` and this lookup read the same three sets, and the pass
        // between the two is where the last one can be answered by another tick.
        return ReadStep::Waited(format!(
            "nothing #{} owes is outstanding at {}",
            pr.number, pr.head_sha
        ));
    };
    if reading.repo.auto_review_dry_run {
        return ReadStep::Waited(format!(
            "dry run: would audit #{} at {} for {}",
            pr.number,
            pr.head_sha,
            check.spelled()
        ));
    }
    let said = match crate::review::audit_owed(
        reading.repo,
        pr.number,
        pr.head_sha,
        &reading.pr.base_ref,
        check.owed(),
    ) {
        Ok(said) => said,
        // A wait rather than a stop, for `read_now`'s reason: an audit changes nothing outside
        // skein, its failures are the transient kind, and nothing downstream can act on one that
        // did not happen — `checks_owed` stays `Some(true)` and the verdict stays out of reach.
        Err(why) => {
            return ReadStep::Waited(format!(
                "#{} was not audited at {}: {why}",
                pr.number, pr.head_sha
            ))
        }
    };
    if let Err(why) = crate::owed::record(&reading.repo.id, pr.number, pr.head_sha, check) {
        // The turn HAPPENED — it may have posted a finding — so this is not a failure of the
        // audit. It is a failure to remember it, and the consequence is one repeated audit rather
        // than a verdict let through, so it says so and waits.
        return ReadStep::Waited(format!(
            "#{} was audited for {} at {} but the answer could not be written down ({why}), so it \
             will be asked again",
            pr.number,
            check.spelled(),
            pr.head_sha
        ));
    }
    ReadStep::Did(format!(
        "audited #{} at {} for {} — {said}",
        pr.number,
        pr.head_sha,
        check.spelled()
    ))
}

/// The next check this repository owes that nobody has answered at this commit.
///
/// The same three sets [`what_this_change_still_owes`] intersects, returning the check rather than
/// whether there is one — two readers of one rule, which is why the intersection itself lives in
/// [`crate::owed::outstanding`] and neither of these implements it.
pub(super) fn the_first_check_still_owed(
    repo: &crate::repos::Repo,
    number: u64,
    head_sha: &str,
) -> Option<crate::owed::Check> {
    let said = crate::review::cached(&repo.id, number, head_sha)?;
    if said.head_sha != head_sha || said.depth == crate::review::Depth::Unread {
        return None;
    }
    let fired = crate::owed::read(&said.owed_triggered?).0;
    let (set, _refused) = crate::owed::for_repo(repo.owed_checks.as_ref());
    let done = crate::owed::answered(&repo.id, number, head_sha);
    crate::owed::outstanding(&set, &fired, &done)
        .first()
        .copied()
}

/// What one verdict step came to. [`ReadStep`]'s shape and the same three answers, because a
/// verdict has the same third case: **drafted, and waiting for a person.** That is what a ceiling
/// below the verdict means, and folding it into `Err` would stop a workflow that is behaving
/// exactly as it was configured to.
enum VerdictStep {
    Did(String),
    Waited(String),
    Failed(String),
}

/// What one `read` step came to. Its own type because a reading has a third answer the other acts
/// do not: *nothing to do, and that is fine* — the reading is already on disk at this head, or the
/// repo is in dry run. Folding that into `Err` would stop the workflow on a pull request nothing
/// is wrong with.
enum ReadStep {
    /// A model call was spent and a reading now exists at this head.
    Did(String),
    /// Nothing was spent, and nothing is wrong. Says why.
    Waited(String),
    /// A step a person wrote that skein may not take. Stops, loudly, like any other failure.
    Failed(String),
}

/// Take one step, or say why not.
pub fn perform(
    pr: &Subject,
    flow: &Workflow,
    chosen: &Chosen,
    token: &crate::secret::Secret,
) -> Outcome {
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
        // §15 step 3: the one reviewer action that is wired. It spends a model call, writes a
        // reading to the cache, and lets that session post its own comment review from inside the
        // box. What it never posts is a VERDICT — that is step 4, and the two arms below.
        Act::Read => match read_now(pr) {
            ReadStep::Did(what) => Ok(what),
            ReadStep::Failed(why) => Err(why),
            // Returned rather than folded into `done`, because a wait is not an outcome the
            // journal wants a line for on every pass: `Read` is chosen again on the next one, and
            // "already read #41 at abc1234" written every tick would bury the actions.
            ReadStep::Waited(why) => return Outcome::Waited(why),
        },
        // §15 step 4: the verdicts. `Read` already posts the findings — the reading session does
        // it from inside the box, which is what `docs/pr-review.md` §6 means by "it reads, and
        // posts" — so these two are the half nothing did.
        Act::PostChanges | Act::PostApproval => {
            let verdict = match chosen.act {
                Act::PostApproval => crate::prq::Verdict::Approve,
                _ => crate::prq::Verdict::RequestChanges,
            };
            match post_verdict(pr, flow, chosen, verdict, token) {
                VerdictStep::Did(what) => Ok(what),
                VerdictStep::Failed(why) => Err(why),
                // Below the ceiling is not a fault: §10 says anything past it "is drafted and
                // waits for you", and a stop would need clearing for a repo behaving as set up.
                VerdictStep::Waited(why) => return Outcome::Waited(why),
            }
        }
        // **`post-findings` is a vestige, and saying so is better than wiring it.** §9's table gave
        // findings their own row when the design assumed skein would post them; since `dda3d3b` the
        // reading session posts its own comment review with `gh` from inside its checkout, and
        // skein keeps no copy of it. So a step here would post the SUMMARY — a different artefact —
        // beside a review that is already on the pull request, and a reader would get the same
        // reading twice in two voices. The act stays in the vocabulary because §9's table is a
        // person's mental model, and removing a row from it silently is worse than refusing one
        // out loud.
        Act::PostFindings => Err("post-findings is not wired, and deliberately: the reading \
             session posts its own comment review from inside its checkout, so a step here would \
             post the summary beside a review that is already there. Use `read`, which reads and \
             posts (docs/pr-review.md §6)"
            .into()),
        // §15 step 5: the scar, as a step. One owed check per evaluation, asked of the reading's
        // own session and recorded against the commit it was asked about.
        Act::Audit => match audit_now(pr) {
            ReadStep::Did(what) => Ok(what),
            ReadStep::Failed(why) => Err(why),
            ReadStep::Waited(why) => return Outcome::Waited(why),
        },
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

/// **Did any trigger this repo asked for actually fire?** `None` when one did — `docs/pr-review.md`
/// §10's trigger set, which was a stored field deciding nothing until this.
///
/// Two different silences, said differently, because they need different actions from a person.
/// A set whose triggers are real and none fired is the ordinary state of a queue: this pull request
/// is simply not one of the events this repo asked to be woken by, and there is nothing to fix. A
/// set with **no trigger this build can compute** is a repo switched on and inert — §10 says that
/// state must *say* it is off rather than present as on — and the only way out is to change the
/// set, so the sentence names the words that cannot fire.
///
/// An unrecognised word is treated exactly as an uncomputable one: a trigger from a newer skein is
/// one this build cannot tell has fired. Both fail towards not reading. See `workflow::read_wake`.
fn no_trigger_of_this_repos_fired(
    repo: &crate::repos::Repo,
    number: u64,
    facts: &crate::workflow::Facts,
) -> Option<String> {
    // **The set this pull request is governed by, not the repo's** — §10's "overridable per pull
    // request". `repos::triggers_for` answers the repo's own words unless somebody has said
    // otherwise about this one, so the ordinary case is unchanged and the sentences below go on
    // naming the words that actually apply.
    let words = crate::repos::triggers_for(repo, number);
    let wanted: Vec<crate::workflow::Wake> = words
        .iter()
        .filter_map(|word| crate::workflow::read_wake(word))
        .collect();
    if wanted.is_empty() {
        // Every word in the set is one this build cannot answer — or the set is empty, which
        // `auto_review_stands` has already refused, so reaching here means the first.
        return Some(format!(
            "automatic review is on for {} with a trigger set this build cannot act on ({}) — no \
             reading can ever be woken by it, so change the set or switch the repo off",
            repo.id,
            match words.is_empty() {
                true => "it is empty".to_string(),
                false => words.join(", "),
            }
        ));
    }
    let fired = crate::workflow::woke(facts);
    if fired.iter().any(|w| wanted.contains(w)) {
        return None;
    }
    Some(format!(
        "no trigger {} asks for has fired on this one — it wakes on {}{}",
        repo.id,
        wanted
            .iter()
            .map(|w| w.spelled())
            .collect::<Vec<_>>()
            .join(", "),
        match fired.is_empty() {
            // Naming what DID fire is the difference between "nothing is happening" and "the set
            // is the wrong shape": a person who sees `approved-commits fired` beside a set of
            // `requested` knows immediately which line to change.
            true => String::new(),
            false => format!(
                ", and what fired here is {}",
                fired
                    .iter()
                    .map(|w| w.spelled())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    ))
}

/// **Whose pull requests may this repo's engine read?** `mine` or `all` — §10's `auto_review_authors`.
///
/// `mine` is the default and the intended use: reviewing what your own boxes open. An outside
/// contributor's pull request is a different risk with a different audience, and it is the first
/// place a wrong verdict is seen by somebody who did not opt into any of this.
///
/// **A word this build does not know reads as `mine`**, the narrow one. Same rule as
/// `repos::Ceiling` and `workflow::read_wake`: this is a permission, and a value skein cannot
/// understand must never widen what it does unattended. That is the opposite of `place::Purpose`'s
/// lenient reader, and deliberately — there an unknown value costs a box nobody can reach.
fn not_an_author_this_repo_reviews(
    repo: &crate::repos::Repo,
    facts: &crate::workflow::Facts,
) -> Option<String> {
    if repo.auto_review_authors.trim() == "all" || facts.mine {
        return None;
    }
    Some(format!(
        "{} reviews only pull requests you opened, and this is somebody else's (its \
         auto_review_authors is {:?})",
        repo.id,
        repo.auto_review_authors.trim()
    ))
}

/// **Has this repo been configured into the loop?** — `docs/pr-review.md` §13, the one obligation
/// the owner's decision came with.
///
/// > engine reviews → engine approves → `Facts::approved` → label, await CI, merge, delete branch
///
/// skein approving its own work and merging it, with nobody in it. The owner's answer was *keep
/// them apart, per repo* — *"If needed, we can just chain them by saying merge all approved ones;
/// how it reached approved is not needed by the merge train."* That reading is right, and it is why
/// this costs nothing to build: the train reads `Facts::approved` and has no interest in
/// **provenance**, so keeping the two apart is a *configuration* and chaining them is the same
/// configuration switched on deliberately. No mechanism has to know the difference, and none
/// should — a train that asked who approved would be a second place where "does this count" is
/// decided, which is how `Facts::approved` came to be wrong in the first place.
///
/// **What is owed is therefore a sentence, not a guard.** A person who switches auto-review on for
/// a repo that already has a train has just built the loop, and nothing would say so. This is the
/// house rule applied to a configuration rather than to a failure: say it, rather than let it be
/// discovered.
///
/// # Four conditions, and the fourth is why this is not noisy
///
/// §13 sketched this as "both on". Built, it is narrower, because `auto_review_ceiling` was
/// designed after that paragraph was written:
///
/// 1. **workflows can act at all** — [`enabled`], the fleet's one kill switch;
/// 2. **some workflow merges** — a `Merge` act in a step somewhere in the file;
/// 3. **the engine may act on this repo** — `repos::auto_review_stands`;
/// 4. **the ceiling reaches an approval.** A `comment` or `changes` ceiling never posts one, so
///    there is no approval for a train to read and no loop to warn about. That is the default a
///    repo is switched on at, so the ordinary way of turning auto-review on does not trip this.
///
/// Deliberately **not** asked: whether a train's `matches` claims any particular pull request.
/// That is a per-pull-request question needing `Facts`, and this is a question about a repo's
/// settings. Being early is the right direction for a warning about self-approving merges.
pub fn the_loop_this_repo_has_built(repo: &crate::repos::Repo) -> Option<String> {
    if !enabled() {
        return None;
    }
    if crate::repos::auto_review_stands(repo).is_some() {
        return None;
    }
    if repo.auto_review_ceiling < crate::repos::Ceiling::Approve {
        return None;
    }
    let trains: Vec<String> = crate::workflow::load()
        .unwrap_or_default()
        .into_iter()
        .filter(|flow| {
            flow.steps
                .iter()
                .any(|step| matches!(step.act, Act::Merge(_)))
        })
        .map(|flow| flow.name)
        .collect();
    if trains.is_empty() {
        return None;
    }
    Some(format!(
        "an approval this engine posts on {} will merge it — automatic review is on with a \
         ceiling of `approve`, and {} {} in this fleet. That composition was chosen rather than \
         prevented (docs/pr-review.md §13); lower `auto_review_ceiling` to keep verdicts waiting \
         for a person, or take {} off this repo.",
        repo.id,
        match trains.len() {
            1 => "the workflow",
            _ => "the workflows",
        },
        trains.join(", "),
        match trains.len() {
            1 => "that workflow",
            _ => "those workflows",
        },
    ))
}

/// **Post a verdict under the reader's name** — `docs/pr-review.md` §15 step 4, and the only thing
/// skein does that a person cannot take back by pressing something.
///
/// # Why the reading session is still forbidden to do this
///
/// §13 records the owner's decision as *"lift the prohibition"*, and what they asked for is that
/// skein post verdicts unattended. This delivers that, and it does **not** lift the prohibition in
/// the prompt — the reading session still may not approve or request changes, in as many words.
///
/// The difference is mechanism, and it is the whole reason every guard in this design exists:
///
/// * the **ceiling** below is a value in a config file, and a session never sees it;
/// * the **sha guard** (§4) is `Cond::ReadingCurrent`, evaluated here from facts;
/// * **§7c** — a partial pass may never approve — is `instead_of_approving_what_was_not_wholly_read`,
///   an override on the evaluator that no workflow file can defeat;
/// * the **audit** is `record` and the warden, naming which workflow and which step.
///
/// A session that posted its own verdict would be outside all four. So the prohibition stays where
/// it is and the engine takes the verdict, which is the same outcome through the machine that can
/// be argued with. That is a deviation from §13's letter and it is recorded there.
///
/// # The attribution §13 said was missing
///
/// > Nothing records who posted — skein keeps no copy of a review any more, by design, so an engine
/// > verdict is indistinguishable from the owner's, on GitHub and in the queue.
///
/// The body carries it, which is the one place a person actually looks: the workflow, the step, and
/// the commit the reading was made against. The journal and the warden have it too, but those are
/// skein's own records, and a verdict that discharges somebody's review must say what left it on
/// the pull request itself.
fn post_verdict(
    pr: &Subject,
    flow: &Workflow,
    chosen: &Chosen,
    verdict: crate::prq::Verdict,
    token: &crate::secret::Secret,
) -> VerdictStep {
    let Some(reading) = &pr.reading else {
        return VerdictStep::Failed(format!(
            "this caller cannot post on #{} — it passed no repo, so there is no ceiling to check \
             a verdict against, and an unattended post with no ceiling is the one thing this must \
             never do",
            pr.number
        ));
    };
    let repo = reading.repo;
    if let Some(why) =
        crate::repos::auto_review_stands_for(repo, chosen_by_hand(&repo.id, pr.number))
    {
        return VerdictStep::Failed(why);
    }
    if let Some(why) = no_trigger_of_this_repos_fired(repo, pr.number, reading.facts) {
        return VerdictStep::Waited(why);
    }
    if let Some(why) = not_an_author_this_repo_reviews(repo, reading.facts) {
        return VerdictStep::Waited(why);
    }
    // **The ceiling**, and it is the last gate before something appears under somebody's name.
    // Ordered by consequence, so one comparison covers all three positions — which is the whole
    // argument for a ceiling over three checkboxes (§10).
    let wants = match verdict {
        crate::prq::Verdict::Approve => crate::repos::Ceiling::Approve,
        crate::prq::Verdict::RequestChanges => crate::repos::Ceiling::Changes,
        crate::prq::Verdict::Comment => crate::repos::Ceiling::Comment,
    };
    if wants > repo.auto_review_ceiling {
        return VerdictStep::Waited(format!(
            "#{} is ready for {}, and {} goes no further than {} on its own — so it waits for you",
            pr.number,
            wants.spelled(),
            repo.id,
            repo.auto_review_ceiling.spelled(),
        ));
    }
    if repo.auto_review_dry_run {
        return VerdictStep::Waited(format!(
            "dry run: would post {} on #{} at {}",
            wants.spelled(),
            pr.number,
            pr.head_sha
        ));
    }
    // The same anchor `read_now` refuses on, for a much sharper reason: a verdict filed against a
    // commit this pass did not evaluate is a review describing tree A anchored to tree B, which is
    // §3's own account of what a memoryless engine gets wrong.
    if reading.pr.head_sha != pr.head_sha {
        return VerdictStep::Failed(format!(
            "the step was decided about {} and the verdict would be filed against {} — refusing \
             to post on #{} at a commit this pass did not evaluate",
            pr.head_sha, reading.pr.head_sha, pr.number
        ));
    }
    let by = format!("{} step {}", flow.name, chosen.step + 1);
    let body = format!(
        "skein posted this automatically — *{by}*, against `{}`.\n\nThe reading it is based on is \
         the review already on this pull request. Turn it off for this repository with \
         `auto_review`, or lower `auto_review_ceiling` to keep verdicts waiting for a person.",
        pr.head_sha
    );
    // `drafted_at` is the same head, which is what makes it "assume current": the reading is
    // current — `Cond::ReadingCurrent` is what let this step be chosen — so there is nothing to
    // re-anchor and no displaced comments to fold in.
    match crate::prq::submit_review_with_comments(crate::prq::ReviewPost {
        slug: pr.slug,
        number: pr.number,
        head_sha: pr.head_sha,
        verdict,
        body: &body,
        comments: &[],
        drafted_at: pr.head_sha,
        // `perform`'s own token, which is the one every other act here is given. It is
        // `prq::host_token` either way today — the tick sources it from the same function — and
        // that is the point of passing it rather than the reason not to: the credential a verdict
        // is posted under is now visible at the call site instead of reached for two modules away.
        token,
    }) {
        Ok(_) => VerdictStep::Did(format!(
            "posted {} on #{} at {} under your name",
            wants.spelled(),
            pr.number,
            pr.head_sha
        )),
        Err(why) => VerdictStep::Failed(why),
    }
}

/// Read this pull request at the head the step was decided about — `docs/pr-review.md` §15 step 3.
///
/// **Wired to the reading skein already has**, rather than to a second one beside it. Everything
/// this needs is in [`crate::review::summarise`]: it stands the change up in a checkout, runs the
/// sweep that accounts for what it covered, and writes a [`crate::review::Summary`] keyed on
/// `(number, head_sha)` — which is the same cache `facts_of_in` reads `reading_sha` and
/// `reading_whole` back out of. So one `read` step closes the engine's own loop: the next
/// evaluation of the same workflow sees `ReadingCurrent`, and where the sweep answered,
/// `ReadingWhole`.
///
/// **It posts a review, and never a verdict.** With a credential in hand the reading session posts
/// its own COMMENT review on the pull request, with `gh` from inside its own checkout — that is
/// `review::merged_prompt`'s posting arm, switched on by `review::acting_credential` — and skein
/// keeps no copy of it. What it may not post is a verdict: approve and request-changes are §15
/// step 4, they are [`post_verdict`]'s under a ceiling, and the session's own prompt forbids both
/// in as many words. [`Act::PostFindings`] refuses for the other side of the same fact — the
/// review is already there, so a step that posted the summary beside it would say it twice.
///
/// # Why a failed reading waits rather than stops
///
/// Every other act in `perform` turns a failure into a stop, and the module note argues that hard:
/// a decision made from facts a failure has just proved stale must not be made again. A reading is
/// the one act that is not like that. It changes nothing outside skein, its failures are the
/// ordinary transient kind — the day's spend ceiling, a diff that would not download, a model call
/// that timed out — and `review.rs` already fixes the direction they fail in: an unread pull
/// request is [`crate::review::Depth::Unread`], `ReadingWhole` does not hold, and
/// [`Act::PostApproval`] is unreachable. Nothing downstream can act on a reading that did not
/// happen, so the fail-closed behaviour is in the type rather than in this stop.
///
/// Stopping here would instead demand a person clear a workflow because a budget rolled over at
/// midnight. And it would not even save the model call: `review::note_tried` already writes the
/// failure against the head, so the next pass is told rather than charged.
///
/// The wait is never silent — it carries `unread_because` verbatim, which is the sentence written
/// to be shown to a person.
fn read_now(pr: &Subject) -> ReadStep {
    let Some(reading) = &pr.reading else {
        // A caller that cannot read, reported as that. Never a default `Repo`: with `auto_review`
        // off by default it would refuse with "automatic review is switched off", and somebody
        // would go and turn on a flag that was never the problem.
        return ReadStep::Failed(format!(
            "this caller cannot read #{} — it passed no repo, pull request or viewer (a `read` \
             step is only takeable from the workflow pass)",
            pr.number
        ));
    };
    // The money door, and the one place it is asked on the acting path. `read_prs` first, then
    // `auto_review`, then a trigger set that could wake it — `repos::auto_review_stands` layers
    // them so the sentence names the OUTER switch that is shut.
    if let Some(why) = crate::repos::auto_review_stands_for(
        reading.repo,
        chosen_by_hand(&reading.repo.id, pr.number),
    ) {
        return ReadStep::Failed(why);
    }
    // §10's chain, in §10's order: the trigger set and the author filter are asked here, AFTER the
    // repo may act at all and BEFORE the step's own conditions have any consequence. Waits rather
    // than stops, because neither is a fault — they are the flags working. A pull request this
    // repo does not review is one that queues, which is what §9 says "off" means.
    if let Some(why) = no_trigger_of_this_repos_fired(reading.repo, pr.number, reading.facts) {
        return ReadStep::Waited(why);
    }
    if let Some(why) = not_an_author_this_repo_reviews(reading.repo, reading.facts) {
        return ReadStep::Waited(why);
    }
    // The anchor, the same rule `merge_pr` and `update_branch` obey: prove the thing is what you
    // think before touching it. `Subject::head_sha` is the commit the step was DECIDED about and
    // `reading.pr.head_sha` is the commit that would be READ, and a reading filed against a commit
    // the engine did not evaluate is the anchoring failure this whole design is about.
    if reading.pr.head_sha != pr.head_sha {
        return ReadStep::Failed(format!(
            "the step was decided about {} and the reading would be filed against {} — refusing \
             to read #{} at a commit this pass did not evaluate",
            pr.head_sha, reading.pr.head_sha, pr.number
        ));
    }
    // Before the model call and after the flags, so a dry run answers exactly what a live one
    // would have been asked and costs nothing. It is a wait rather than a `Did`, because nothing
    // was done — and because a `Did` every two minutes for as long as the dry run is on would fill
    // the journal with an action that never happened.
    if reading.repo.auto_review_dry_run {
        return ReadStep::Waited(format!(
            "dry run: would read #{} at {}",
            pr.number, pr.head_sha
        ));
    }
    // `Unasked`, deliberately: this fires without anybody present, on every push, which is exactly
    // the spend the day's ceiling exists to bound. `Trigger::Asked` would exempt an unattended
    // engine from the limit written for skein's own initiative.
    //
    // Never `force`: a reading already on disk at this head IS the answer, and re-buying it every
    // pass is the loop this reads the cache to avoid.
    let identities = [reading.viewer.to_string()];
    let said = crate::review::summarise(
        reading.repo,
        pr.slug,
        reading.pr,
        &identities,
        false,
        crate::review::Trigger::Unasked,
    );
    let number = pr.number;
    let head = pr.head_sha;
    match said.depth {
        crate::review::Depth::Unread => ReadStep::Waited(format!(
            "#{number} is not read at {head}: {}",
            said.unread_because
        )),
        // Not computed and not unread means the cache answered. Nothing was spent and nothing is
        // wrong: the step will be chosen again next pass and answer from the cache again, until a
        // condition that depends on the reading moves the workflow on.
        _ if !said.computed => ReadStep::Waited(format!("#{number} is already read at {head}")),
        _ => ReadStep::Did(read_did(number, head, &said)),
    }
}

/// **What the journal says about a reading that happened** — its own function so it can be read
/// back without a repository, a queue and a model call in front of it.
///
/// Two facts beside the line, and the whole of the care here is that they never say the same thing
/// twice:
///
/// * **whether a sweep spoke for it** (§7c). Said on the line because it is what decides whether
///   an approval is reachable at all, and "skein read it" without it is the claim that failed 53
///   seconds apart.
/// * **whether it ran in its box** ([`crate::review::Summary::read_outside_box`], SKEIN-799).
///
/// The second ENDS with the first when there was no sweep — the same fact, in the same words,
/// because an approval put out of reach is the consequence of losing the box that a reader most
/// needs. So the parenthetical steps aside in exactly that case and the notice says it once. When
/// the sweep did run there is nothing to collide with and the parenthetical stands as it always
/// has.
fn read_did(number: u64, head: &str, said: &crate::review::Summary) -> String {
    let coverage = match (said.swept, said.read_outside_box.is_empty()) {
        (true, _) => Some("the sweep accounted for every changed file"),
        (false, true) => Some("no sweep accounted for it, so an approval stays out of reach"),
        (false, false) => None,
    };
    format!(
        "read #{number} at {head}{}: {}{}",
        coverage
            .map(|clause| format!(" ({clause})"))
            .unwrap_or_default(),
        said.line.trim(),
        // The reading happened and is worth having, so the notice trails it rather than displacing
        // it: what changed is where it ran, not whether it ran.
        match said.read_outside_box.as_str() {
            "" => String::new(),
            notice => format!(" — {notice}"),
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;

    /// **The journal line never says the sweep's sentence twice** (SKEIN-799).
    ///
    /// `Summary::read_outside_box` and this line's own parenthetical are about the same fact when
    /// there was no sweep, and they say it in the same words on purpose — so the line has to
    /// choose. All four combinations, because the trap is only in one of them and the other three
    /// are what prove the first is not being paid for with a fact dropped elsewhere.
    ///
    /// **What makes it fail:** printing the parenthetical unconditionally, which is what the line
    /// did before the notice existed. The `(false, notice)` case then carries the clause twice.
    #[test]
    fn the_journal_line_says_the_sweeps_consequence_once_at_most() {
        let clause = "no sweep accounted for it, so an approval stays out of reach";
        let notice = crate::review::summary_notice_for_test();
        // Case-insensitively, and the difference is real: the notice capitalises it because it is
        // a sentence of its own, and this line carries it mid-sentence inside brackets. Same fact,
        // same words, one letter apart — which is exactly the kind of near-duplicate a reader
        // notices and a `contains` does not.
        assert!(
            notice.to_lowercase().contains(clause),
            "this test is about a collision that no longer exists: {notice}"
        );

        let reading = |swept: bool, outside: bool| {
            let mut said =
                crate::prwork::testkit::a_reading("abc123", crate::review::Depth::Line, swept);
            if outside {
                said.read_outside_box = notice.clone();
            }
            read_did(41, "abc123", &said)
        };

        // No box lost: unchanged in both directions, which is the whole of what the other three
        // arms are here to protect.
        assert_eq!(
            reading(true, false),
            "read #41 at abc123 (the sweep accounted for every changed file): it changes a thing."
        );
        assert_eq!(
            reading(false, false),
            format!("read #41 at abc123 ({clause}): it changes a thing.")
        );

        // A box lost, and a sweep that spoke anyway: nothing collides, so both are said.
        let swept_and_outside = reading(true, true);
        assert!(
            swept_and_outside.contains("(the sweep accounted for every changed file)")
                && swept_and_outside.contains("Read outside its box"),
            "a reading that lost its box and was swept anyway lost one of the two facts: \
             {swept_and_outside}"
        );

        // And the collision itself.
        let neither = reading(false, true);
        assert_eq!(
            neither.to_lowercase().matches(clause).count(),
            1,
            "the journal line said the same sentence twice: {neither}"
        );
        assert!(
            neither.contains("Read outside its box") && neither.to_lowercase().contains(clause),
            "the collision was resolved by dropping a fact rather than by saying it once: \
             {neither}"
        );
    }

    #[allow(unused_imports)]
    use crate::prwork::facts::facts_of;
    #[allow(unused_imports)]
    use crate::prwork::testkit::*;
    #[allow(unused_imports)]
    use crate::prwork::*;
    #[allow(unused_imports)]
    use crate::workflow::{Act, Chosen, Merge, MergeAs, Update, Workflow};
    #[allow(unused_imports)]
    use std::io::{Read, Write};
    #[allow(unused_imports)]
    use std::sync::{Arc, Mutex};

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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        let (base, heard) = github(200);
        env.set("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: true,
            })),
            &fixture_token(),
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
    }

    /// An action that failed is not tried again, and the reason is kept.
    ///
    /// A merge 409s when somebody pushed while skein was deciding. Retrying is not resilience: the
    /// decision was made from facts that failure has just proved stale, so the same decision would
    /// be made again, and a loop like that eventually wins the race.
    #[test]
    fn an_action_that_failed_stops_the_workflow_rather_than_looping() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("SKEIN_PR_WORKFLOWS", "on");
        let (base, heard) = github(409);
        env.set("SKEIN_GITHUB_API", &base);

        let act = Act::Merge(Merge {
            how: MergeAs::Squash,
            delete_branch: false,
        });
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(act.clone()),
            &fixture_token(),
        );
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
        let out = perform(&subject("abc"), &flow(), &chosen(act), &fixture_token());
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");
        assert_eq!(
            heard.lock().unwrap().len(),
            before,
            "a stopped workflow tried the same failing action again"
        );

        // And a person can let it run again.
        clear("demo", 41).expect("the stop must clear");
        assert_eq!(stopped("demo", 41), None);
    }

    /// The whole chain, on the one event the train had no way to say anything about (SKEIN-247):
    /// a reviewer requests changes, and it becomes a stop on the row and a line on the banner.
    ///
    /// Four links, and every one of them was broken by the first: `facts_of` reads GitHub's
    /// verdict, `carries` decides the pull request is still the train's, `perform` runs the step
    /// written for it, and `stops` puts it where somebody sees it. This is asserted here rather
    /// than only in `workflow` because the defect was that the chain never STARTED — a unit test
    /// of `claims` alone would have passed against a train nothing ever reached.
    ///
    /// No network: `Act::Flag` is answered before `perform` touches the wire, which is what lets
    /// the whole path be walked without a GitHub.
    #[test]
    fn a_reviewer_requesting_changes_becomes_a_stop_and_a_banner_line() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("SKEIN_PR_WORKFLOWS", "on");

        // The documented train's first two steps, and its `matches` — docs/pr-workflow.md, "The
        // train, written down".
        let flows = crate::workflow::from_bytes(
            r#"{"workflow":[{"name":"merge-train","serial":true,
                 "matches":["ready","approved","review-satisfied","base:trunk"],
                 "steps":[
                   {"when":["changes-requested"],"do":"flag:changes were requested - resolve them to rejoin the train"},
                   {"when":["label:ci-queue","checks:passing","mergeable","current"],"do":"merge:squash+delete"},
                   {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#
                .as_bytes(),
        )
        .unwrap();
        let pr = |decision: &str| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 7, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "abc", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": decision, "mergeable": true,
                "merge_state": "CLEAN", "checks": "passing", "my_review": "none",
                "review_is_current": false, "reasons": [], "lane": "needs-you",
                "box_name": "demo-feat",
            }))
            .unwrap()
        };

        // Approved, and on the train. This is the state it is in the pass BEFORE the review lands.
        let approved = facts_of(&pr("APPROVED"), "me", "main");
        assert_eq!(
            carries("demo", 7, &approved, &flows),
            Carries::Matched("merge-train".into())
        );

        // The reviewer says no, and every reading of review moves at once: no approval stands, a
        // refusal does, and the repository's requirement is not met.
        let said_no = facts_of(&pr("CHANGES_REQUESTED"), "me", "main");
        assert!(
            !said_no.approved && said_no.changes_requested,
            "a refusal must outrank any approval standing behind it — `approved` and \
             `changes-requested` are allowed to disagree with each other and never to hold together"
        );
        assert_eq!(
            said_no.review_requirement_met,
            Some(false),
            "a refusal is the repository's requirement NOT met, and `review-satisfied` in the \
             train's `matches` turns on that"
        );
        assert_eq!(
            carries("demo", 7, &said_no, &flows),
            Carries::Matched("merge-train".into()),
            "the pull request left the train the moment a reviewer said no — silently, and one \
             pass before the step written for exactly this could fire"
        );

        let chosen = crate::workflow::next(&flows[0], &said_no).expect("a step must apply");
        // #7, not the shared `subject()` helper's #41: everything below reads the stop and the
        // journal by number, and a mismatch here would assert against an empty file.
        let seven = Subject {
            repo_id: "demo",
            slug: "acme/thing",
            number: 7,
            head_sha: "abc",
            head_ref: "feat",
            reading: None,
        };
        let outcome = perform(&seven, &flows[0], &chosen, &fixture_token());
        assert_eq!(
            outcome,
            Outcome::Stopped("changes were requested - resolve them to rejoin the train".into()),
            "the step that fired was not the one the reviewer's answer is about"
        );

        // On the row, in the timeline, and on the banner — the three places a person looks.
        assert!(stopped("demo", 7).is_some(), "nothing was written down");
        let timeline = journal("demo", 7);
        assert_eq!(
            timeline
                .iter()
                .map(|e| (e.kind.as_str(), e.step))
                .collect::<Vec<_>>(),
            vec![("stopped", 1)],
            "the journal does not say which step stopped it: {timeline:?}"
        );
        remember_open("demo", &[7], true);
        assert_eq!(
            stops("demo").iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![7],
            "the stop never reached the banner the counts poll draws"
        );

        // And the train passes it over rather than parking on it, which is the point of stopping
        // it rather than leaving it carried and idle.
        let line = trains(
            "demo",
            &[
                (7, "merge-train".to_string()),
                (9, "merge-train".to_string()),
            ],
            &flows,
        );
        assert_eq!(
            line[0].front,
            Some(9),
            "a stopped pull request held the line"
        );

        std::env::remove_var("SKEIN_PR_WORKFLOWS");
        std::env::remove_var("SKEIN_HOME");
    }

    // ─────────────── §15 step 3: the one reviewer act that is wired ───────────────

    /// A subject carrying everything `Act::Read` needs, anchored at one commit.
    fn readable<'a>(
        repo: &'a crate::repos::Repo,
        pr: &'a crate::prq::Pr,
        head_sha: &'a str,
        facts: &'a crate::workflow::Facts,
    ) -> Subject<'a> {
        Subject {
            repo_id: "demo",
            slug: "acme/thing",
            number: 41,
            head_sha,
            head_ref: "feat",
            reading: Some(Reading {
                repo,
                pr,
                viewer: "owner",
                facts,
            }),
        }
    }

    /// Facts that pass §10's two read-side gates, so a test about anything else is not silently
    /// about them: GitHub asked you by name (the default trigger set's only member) and the pull
    /// request is yours (the default author filter).
    fn woken_and_mine() -> crate::workflow::Facts {
        crate::workflow::Facts {
            review_requested: true,
            mine: true,
            ..Default::default()
        }
    }

    /// Switch the workflow engine on in a home of this test's own.
    fn a_fleet_where_workflows_run(home: &std::path::Path) -> crate::testutil::EnvPins {
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home).set("SKEIN_PR_WORKFLOWS", "on");
        env
    }

    /// **The money door, on the acting path.** A `read` step against a repo whose automatic review
    /// is off must stop, and the stop must name the switch that is shut.
    ///
    /// **What would make this fail:** deleting the `repos::auto_review_stands` call from
    /// `read_now`. Then a repo with every reviewer flag off would be read anyway, and this asserts
    /// on `Outcome::Stopped` — so the act would come back `Waited` or `Did` and the match panics.
    #[test]
    fn a_read_step_where_automatic_review_is_off_stops_and_names_the_switch() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // Reading is on for the repo; automatic review is not. That is the ordinary state of every
        // repo in the registry, because `auto_review` defaults off and nothing turns it on.
        let repo = crate::repos::Repo {
            auto_review: false,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("automatic review is switched off"),
                "the stop must name the switch a person would go and turn on: {why}"
            ),
            other => panic!("a read ran on a repo that never asked for one: {other:?}"),
        }
        // And it is a stop like any other — written down, so the next pass does not try again.
        assert!(
            stopped("demo", 41).is_some(),
            "the refusal was not written down"
        );
    }

    /// The outer switch wins. A repo skein may not read at all must not report the inner flag as
    /// the reason — somebody would go and turn on `auto_review` and watch nothing happen.
    ///
    /// **What would make this fail:** reordering `auto_review_stands` to test `auto_review` before
    /// `read_prs`. Both are off here, so the sentence would name the inner one.
    #[test]
    fn a_read_step_on_a_repo_skein_may_not_read_blames_the_outer_switch() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            read_prs: false,
            auto_review: false,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("reading is switched off"),
                "with both switches shut, the reason must be the outer one: {why}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// **A pull request somebody assigned the reviewer flow to acts in a repo whose engine is
    /// off** — §10's layer 7 over layer 3, and the last unbuilt link in that chain.
    ///
    /// > on, in a repo that is off — assign the reviewer flow to that one pull request
    ///
    /// Observed at the seam rather than at the model call: with `auto_review` off and nothing
    /// assigned, `read_now` refuses at the flag and says which switch is shut. With the same repo
    /// and an assignment on the row it gets past that flag and lands on the NEXT link — the trigger
    /// set — which is a wait rather than a refusal. The two sentences are how you can tell which
    /// layer stopped it, which is the whole reason `auto_review_stands_for` returns prose.
    ///
    /// **What would make this fail:** dropping the `&& !assigned` from layer 3, which makes the
    /// first row stop saying "switched off"; or letting the assignment past layer 1, which the
    /// third row catches — the money door is the one thing a per-PR switch may never open.
    #[test]
    fn a_pull_request_assigned_by_hand_acts_where_the_repo_is_off_but_never_where_reading_is() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let engine_off = crate::repos::Repo {
            auto_review: false,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        // Facts with no trigger fired, so the layer AFTER the one under test is reachable and
        // distinguishable: this must never get as far as a model call.
        let quiet = crate::workflow::Facts::default();

        let refused = perform(
            &readable(&engine_off, &pr, "abc1234", &quiet),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &refused {
            Outcome::Stopped(why) => assert!(
                why.contains("automatic review is switched off"),
                "the wrong layer refused it: {why}"
            ),
            other => panic!("a repo with the engine off acted: {other:?}"),
        }

        // The same repo, with somebody's choice on the row. The stop the refusal above wrote is
        // cleared first: `perform` answers a remembered stop before it evaluates anything, so
        // without this the second call returns the FIRST call's sentence and the assertion below
        // would pass or fail on a decision that was never made again.
        clear("demo", 41).unwrap();
        assign("demo", 41, "the-flow").unwrap();
        assert!(
            chosen_by_hand("demo", 41),
            "the assignment was not written where it is read"
        );
        let now = perform(
            &readable(&engine_off, &pr, "abc1234", &quiet),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &now {
            Outcome::Waited(why) => assert!(
                why.contains("trigger"),
                "the assignment got past layer 3 but stopped somewhere unexpected: {why}"
            ),
            other => panic!(
                "an assigned pull request did not get past `auto_review` being off: {other:?}"
            ),
        }

        // **And an exclusion is not an assignment.** An empty name is a person saying "no rule may
        // touch this one"; reading it as a switch-on would act on exactly the pull request that was
        // taken out of reach.
        assign("demo", 41, "").unwrap();
        assert!(
            !chosen_by_hand("demo", 41),
            "an excluded pull request read as one somebody switched on"
        );

        // **Layer 1 is never opened by layer 7.** A repo skein may not read at all refuses with a
        // sentence naming reading, assignment or no assignment.
        clear("demo", 41).unwrap();
        assign("demo", 41, "the-flow").unwrap();
        let no_reading = crate::repos::Repo {
            read_prs: false,
            ..engine_off.clone()
        };
        match perform(
            &readable(&no_reading, &pr, "abc1234", &quiet),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        ) {
            Outcome::Stopped(why) => assert!(
                why.contains("reading is switched off"),
                "an assignment opened the money door: {why}"
            ),
            other => panic!("an assignment read a repo skein may not read: {other:?}"),
        }
    }

    /// A dry run says what it would have done and buys nothing.
    ///
    /// **What would make this fail:** deleting the `auto_review_dry_run` early return. `summarise`
    /// would then run for real — and with this pull request out of reading scope it comes back
    /// `Unread`, so the outcome is still a `Waited` but its sentence is the scope refusal rather
    /// than the dry-run one, and the `contains` assertion fails.
    #[test]
    fn a_read_step_in_dry_run_says_what_it_would_do_and_buys_nothing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            auto_review_dry_run: true,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => {
                assert!(
                    why.contains("dry run") && why.contains("abc1234"),
                    "a dry run must say which commit it would have read: {why}"
                );
            }
            other => panic!("a dry run did something: {other:?}"),
        }
        // Nothing was written anywhere: no stop, and no reading on disk.
        assert_eq!(stopped("demo", 41), None, "a dry run stopped the workflow");
        assert!(
            !reading_path("demo", 41, "abc1234").exists(),
            "a dry run filed a reading"
        );
    }

    /// The anchor. A reading filed against a commit this pass did not evaluate is the anchoring
    /// failure the whole reviewer design exists to stop, so it is refused rather than filed.
    ///
    /// **What would make this fail:** deleting the `reading.pr.head_sha != pr.head_sha` guard.
    /// The act would then go on to the dry-run check and this asserts `Stopped`.
    #[test]
    fn a_read_step_refuses_a_commit_the_pass_did_not_evaluate() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // Dry run as well, so that removing the anchor does not merely swap one refusal for
        // another: without the guard this reaches the dry-run wait, which is not a stop.
        let repo = crate::repos::Repo {
            auto_review_dry_run: true,
            ..a_repo_that_may_be_read()
        };
        // The step was decided about `abc1234`; the pull request in hand has moved to `def5678`.
        let moved = pr_at("def5678");
        let out = perform(
            &readable(&repo, &moved, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("abc1234") && why.contains("def5678"),
                "the refusal must name both commits, or nobody can tell which moved: {why}"
            ),
            other => panic!("a reading was filed against a commit nothing evaluated: {other:?}"),
        }
    }

    /// A caller with nothing to read with says so, and does not blame a flag.
    ///
    /// **What would make this fail:** treating `Reading: None` as "read it with a default `Repo`".
    /// `Repo::default()` has `auto_review` off, so the refusal would come back naming the flag —
    /// and somebody would go and switch on automatic review for a repo where it was never the
    /// problem. The assertion is that the sentence does NOT name it.
    #[test]
    fn a_read_step_from_a_caller_with_nothing_to_read_with_does_not_blame_a_flag() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // `subject()` is the helper every non-reviewer test uses, and it carries no reading.
        let out = perform(
            &subject("abc1234"),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => {
                assert!(
                    why.contains("passed no repo"),
                    "the refusal must name the caller: {why}"
                );
                assert!(
                    !why.contains("automatic review is switched off"),
                    "a wiring fault was reported as a flag somebody should go and change: {why}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// **A reading that did not happen waits; it does not stop the workflow.**
    ///
    /// Every other act in `perform` turns a failure into a stop, and this one deliberately does
    /// not: an unread pull request is `Depth::Unread`, `ReadingWhole` does not hold and an
    /// approval is unreachable, so the fail-closed behaviour is already in the type. Stopping
    /// as well would make a person clear a workflow because a day's budget rolled over.
    ///
    /// Driven through the real refusal rather than a stub: this pull request is not one skein
    /// reads unasked — nobody requested the viewer and the viewer did not open it — so
    /// `review::unasked_scope` turns it away before any model call.
    ///
    /// **What would make this fail:** mapping `Depth::Unread` to `ReadStep::Failed`. The outcome
    /// becomes `Stopped` and a stop appears on disk, and both assertions below catch it.
    #[test]
    fn a_reading_that_did_not_happen_waits_rather_than_stopping_the_workflow() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("#41") && !why.is_empty(),
                "the wait must carry the reason the reading did not happen: {why}"
            ),
            other => panic!("a reading skein declined to make stopped the workflow: {other:?}"),
        }
        assert_eq!(
            stopped("demo", 41),
            None,
            "a pull request skein chose not to read now needs a person to clear it"
        );
    }

    /// A reading already on disk at this head is the answer, and is not bought again.
    ///
    /// **What would make this fail:** passing `force: true` to `summarise`, or treating
    /// `Summary::computed` as "a reading exists" rather than "a model call was spent". Either way
    /// the outcome becomes `Did` and the journal gains a line every two minutes for a reading
    /// nobody made.
    #[test]
    fn a_reading_already_on_disk_at_this_head_is_not_bought_again() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        // Filed at exactly this head, through the same expression `review::cached` reads.
        file_the_reading(
            "demo",
            &a_reading("abc1234", crate::review::Depth::Line, true),
        );

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("already read"),
                "a cached reading must be reported as one: {why}"
            ),
            other => panic!("skein re-bought a reading it already had: {other:?}"),
        }
        // And the journal is untouched — the whole reason a cache hit is a wait.
        assert!(
            journal("demo", 41).is_empty(),
            "a reading that cost nothing wrote a line into the journal"
        );
    }

    /// **A trigger set that decides something.** The owner's third ask — *"the trigger is just
    /// review requested state but not new commits"* — is the default set, so this is the default
    /// behaviour and not an edge.
    ///
    /// **What would make this fail:** deleting the `no_trigger_of_this_repos_fired` call from
    /// `read_now`. The reading would then go ahead on a pull request no trigger in the repo's set
    /// woke, and the outcome would carry the scope refusal rather than the trigger sentence.
    #[test]
    fn a_pull_request_no_trigger_in_this_repos_set_woke_is_not_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        // The head moved on one you approved — a real event, and one this repo did not ask for.
        // The default set is `requested` alone, which has NOT fired: nobody named you.
        let woken_by_something_else = crate::workflow::Facts {
            review_requested: false,
            mine: true,
            my_review: "approved".into(),
            my_review_current: false,
            ..Default::default()
        };
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_by_something_else),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => {
                assert!(
                    why.contains("requested"),
                    "the wait must name the triggers this repo does ask for: {why}"
                );
                assert!(
                    why.contains("approved-commits"),
                    "the wait must name what DID fire, or nobody can tell which line to change: \
                     {why}"
                );
            }
            other => panic!("a trigger this repo never asked for started a reading: {other:?}"),
        }
        assert_eq!(
            stopped("demo", 41),
            None,
            "a quiet trigger stopped the flow"
        );
    }

    /// A repo switched on with a trigger set nothing in this build can answer is **on and inert**,
    /// which §10 says must say so rather than present as running.
    ///
    /// **What would make this fail:** `read_wake` guessing rather than answering `None` for a word
    /// it does not know. The words below would then read as real triggers, the sentence would be
    /// the ordinary "no trigger fired" one, and the assertion on "cannot act on" fails.
    #[test]
    fn a_trigger_set_this_build_cannot_act_on_says_so_rather_than_sitting_inert() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            // Two words from nowhere — a trigger set written by a newer skein. This used to say
            // `reply`, which was in §10's table and unanswerable; it is answerable now, so the
            // only inert set left is one this build cannot read at all, which is the case that
            // was always the more likely one to meet in the wild.
            auto_review_on: vec!["reply-with-a-quote".into(), "on-a-tuesday".into()],
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("cannot act on") && why.contains("reply-with-a-quote"),
                "an inert trigger set must name itself: {why}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// `auto_review_authors` defaults to `mine`, and somebody else's pull request is left alone.
    ///
    /// **What would make this fail:** deleting the `not_an_author_this_repo_reviews` call. The
    /// reading would go ahead on a pull request the reader did not open, which is the first place
    /// a wrong verdict is seen by somebody who did not opt into any of this.
    #[test]
    fn a_repo_that_reviews_only_your_own_leaves_somebody_elses_pull_request_alone() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = a_repo_that_may_be_read();
        let pr = pr_at("abc1234");
        let theirs = crate::workflow::Facts {
            review_requested: true,
            mine: false,
            ..Default::default()
        };
        let out = perform(
            &readable(&repo, &pr, "abc1234", &theirs),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("only pull requests you opened"),
                "the wait must say whose pull requests this repo reviews: {why}"
            ),
            other => panic!("a contributor's pull request was read unattended: {other:?}"),
        }

        // And `all` opens it, or the flag has one position.
        let open_to_all = crate::repos::Repo {
            auto_review_authors: "all".into(),
            ..a_repo_that_may_be_read()
        };
        let out = perform(
            &readable(&open_to_all, &pr, "abc1234", &theirs),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                !why.contains("only pull requests you opened"),
                "`all` did not open the door: {why}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// A word this build does not know reads as `mine`, the narrow one — a permission may never be
    /// widened by a value skein cannot understand.
    ///
    /// **What would make this fail:** writing the check as `authors != "mine"` rather than
    /// `== "all"`. Then anything misspelled would open the repo to every author.
    #[test]
    fn an_author_filter_this_build_does_not_recognise_stays_narrow() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            auto_review_authors: "everyone".into(),
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let theirs = crate::workflow::Facts {
            review_requested: true,
            mine: false,
            ..Default::default()
        };
        let out = perform(
            &readable(&repo, &pr, "abc1234", &theirs),
            &flow(),
            &chosen(Act::Read),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("only pull requests you opened"),
                "an unrecognised author filter widened what skein does unattended: {why}"
            ),
            other => panic!("{other:?}"),
        }
    }

    // ─────────────── §15 step 4: the verdicts, and the ceiling on them ───────────────

    /// **The ceiling is the last gate before something appears under somebody's name**, and it
    /// holds by refusing to make the call at all — not by making it and hoping.
    ///
    /// `comment` is what a repo gets when it is switched on (§10), so this is the default state:
    /// findings unattended, verdicts waiting. The assertion that matters is the second one —
    /// nothing reached GitHub — because a ceiling that returned `Waited` after posting would read
    /// exactly the same on the row.
    ///
    /// **What would make this fail:** deleting the `wants > repo.auto_review_ceiling` check, or
    /// writing it as `>=`, which would let a repo post exactly the verdict it is capped at and
    /// nothing beyond — the off-by-one that looks like it works.
    #[test]
    fn a_verdict_past_the_ceiling_waits_and_never_reaches_github() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        env.set("SKEIN_GITHUB_API", &base);

        let repo = a_repo_that_may_be_read();
        assert_eq!(
            repo.auto_review_ceiling,
            crate::repos::Ceiling::Comment,
            "the fixture must be at the default ceiling, or this tests a state nobody starts in"
        );
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        match &out {
            Outcome::Waited(why) => assert!(
                why.contains("comment") && why.contains("waits for you"),
                "the wait must name the ceiling that stopped it: {why}"
            ),
            other => panic!("an approval went out past the repo's ceiling: {other:?}"),
        }
        assert!(
            heard.lock().unwrap().is_empty(),
            "the ceiling let the call happen: {:?}",
            heard.lock().unwrap()
        );
        assert_eq!(stopped("demo", 41), None, "a ceiling stopped the workflow");
    }

    /// **A refusal is reachable one notch below an approval**, which is the shape the interviewed
    /// box asked for: everything else unattended, and the verdict that discharges a review held
    /// back. A ceiling can express it and three checkboxes cannot (§10).
    ///
    /// **What would make this fail:** comparing the ceiling by anything but consequence — reversing
    /// `Ceiling`'s variant order, or deriving `Ord` off a different field. `changes` would then
    /// either block a refusal it permits or admit the approval it exists to hold.
    #[test]
    fn a_ceiling_at_changes_posts_a_refusal_and_still_holds_the_approval() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let mut env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        env.set("SKEIN_GITHUB_API", &base);
        // **No `GH_TOKEN` here, and its absence is the assertion.** This test used to set one,
        // because `prq::submit_review_with_comments` looked the credential up itself — so a test
        // about a CEILING could not run without arranging a credential two modules away. It takes
        // `perform`'s token now, which this test passes as "t" below. If the lookup ever comes
        // back, this test fails on a missing credential and says where.

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Changes,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let facts = woken_and_mine();
        let subject = readable(&repo, &pr, "abc1234", &facts);

        let held = perform(
            &subject,
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        assert!(
            matches!(held, Outcome::Waited(_)),
            "an approval went out at a `changes` ceiling: {held:?}"
        );
        assert!(
            heard.lock().unwrap().is_empty(),
            "the approval reached GitHub"
        );

        let posted = perform(
            &subject,
            &flow(),
            &chosen(Act::PostChanges),
            &fixture_token(),
        );
        assert!(
            matches!(posted, Outcome::Did(_)),
            "a refusal was held back at its own ceiling: {posted:?}"
        );
        let said = heard.lock().unwrap().join("\n");
        assert!(
            said.contains("REQUEST_CHANGES"),
            "the post was not a refusal: {said}"
        );
    }

    /// **The verdict says what left it**, which is the attribution §13 recorded as missing: *"an
    /// engine verdict is indistinguishable from the owner's, on GitHub and in the queue."*
    ///
    /// On the pull request itself, not only in skein's journal — a verdict that discharges
    /// somebody's review is read by people who cannot see skein's records at all.
    ///
    /// **What would make this fail:** posting an empty body, or one that names neither the step nor
    /// the commit. Either leaves a reader unable to tell an engine's approval from a person's.
    ///
    /// **Asserted whole, and that is the point.** This test used to check four `contains` —
    /// "skein posted this automatically", "ship-mine step 4", "abc1234", "auto_review" — and every
    /// one of them was satisfied for the life of a body that reached GitHub as
    /// `…against \`abc1234\`.\n\nThe reading it is based on is \` with a backslash hanging off the
    /// end of the line, because the literal was written with `\\n\\n` and `\\` where `\n\n` and a
    /// line continuation were meant. A substring test cannot see what is *between* the substrings,
    /// and this string is published under a person's own name on somebody else's repository. So
    /// the whole of it is pinned, and a deliberate rewording is meant to have to come through here.
    #[test]
    fn a_posted_verdict_names_the_workflow_the_step_and_the_commit() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let mut env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        env.set("SKEIN_GITHUB_API", &base);
        env.set("GH_TOKEN", "skein-test-gho");

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Did(_)), "{out:?}");

        // The review as it went on the wire, not the transcript around it: `submit_review_with_comments`
        // POSTs `{"event":…,"commit_id":…,"body":…}` to `/repos/<slug>/pulls/<n>/reviews`.
        let posted = {
            let heard = heard.lock().unwrap();
            heard
                .iter()
                .find(|r| r.starts_with("POST") && r.contains("/reviews"))
                .cloned()
                .unwrap_or_else(|| panic!("no review was posted: {heard:?}"))
        };
        let sent: serde_json::Value =
            serde_json::from_str(&posted[posted.find('{').expect("no JSON body")..])
                .unwrap_or_else(|e| panic!("the review body is not JSON ({e}): {posted}"));
        assert_eq!(sent["event"], "APPROVE", "not an approval: {posted}");

        // The whole body, character for character — see the note above this test. `\n\n` here is a
        // blank line in the rendered comment; a literal backslash-n would be the defect this pins.
        assert_eq!(
            sent["body"].as_str().unwrap_or_default(),
            "skein posted this automatically — *ship-mine step 4*, against `abc1234`.\n\nThe \
             reading it is based on is the review already on this pull request. Turn it off for \
             this repository with `auto_review`, or lower `auto_review_ceiling` to keep verdicts \
             waiting for a person.",
            "this is the text a stranger reads under your name on their pull request"
        );
    }

    /// A verdict is refused against a commit this pass did not evaluate — §3's "a review describing
    /// tree A anchored to tree B", which is the failure a memoryless engine makes and the sha guard
    /// exists to stop.
    ///
    /// **What would make this fail:** deleting the head comparison from `post_verdict`. The post
    /// would then go out against `Subject::head_sha` while the reading it rests on describes
    /// another commit.
    #[test]
    fn a_verdict_is_refused_against_a_commit_the_pass_did_not_evaluate() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let mut env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);
        let (base, heard) = github(200);
        env.set("SKEIN_GITHUB_API", &base);

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let moved = pr_at("def5678");
        let out = perform(
            &readable(&repo, &moved, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostApproval),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => assert!(
                why.contains("abc1234") && why.contains("def5678"),
                "the refusal must name both commits: {why}"
            ),
            other => panic!("a verdict was posted against an unevaluated commit: {other:?}"),
        }
        assert!(heard.lock().unwrap().is_empty(), "it reached GitHub anyway");
    }

    /// `post-findings` refuses, and the refusal says it is a vestige rather than a thing not built.
    ///
    /// The distinction is the whole value: "not built yet" invites somebody to wire it, and wiring
    /// it would post the summary beside a review the reading session already left.
    ///
    /// **What would make this fail:** giving this arm the "nothing is wired to it yet" refusal
    /// `audit` used to carry. `audit` has since been wired — [`audit_now`], `docs/pr-review.md` §8
    /// — and this one deliberately has not, so the refusal is where that difference is said out
    /// loud rather than left to be guessed at.
    #[test]
    fn post_findings_refuses_as_a_vestige_rather_than_as_something_unbuilt() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let _env = a_fleet_where_workflows_run(home.as_ref() as &std::path::Path);

        let repo = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let pr = pr_at("abc1234");
        let out = perform(
            &readable(&repo, &pr, "abc1234", &woken_and_mine()),
            &flow(),
            &chosen(Act::PostFindings),
            &fixture_token(),
        );
        match &out {
            Outcome::Stopped(why) => {
                assert!(
                    why.contains("posts its own comment review"),
                    "the refusal does not say why this is not wanted: {why}"
                );
                assert!(
                    why.contains("`read`"),
                    "the refusal names no step to use instead: {why}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    // ─────────────── §13's obligation: the loop must not be reachable silently ───────────────

    /// A workflows file with a merge step, written where `workflow::load` reads it.
    fn a_merge_train(home: &std::path::Path) {
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"ship-mine","steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap();
    }

    /// **Every condition of the loop is load-bearing, and the ceiling is why this is not noisy.**
    ///
    /// §13 sketched it as "both on". Built, it is four conditions, and the table walks each one off
    /// on its own so no single arm can be deleted without a row going red.
    ///
    /// The fourth is the one worth having: a repo switched on at the DEFAULT ceiling never posts an
    /// approval, so there is nothing for a train to read and nothing to warn about. Without it,
    /// every repo with auto-review on would carry a warning about a loop it cannot build — and a
    /// warning that fires when nothing is wrong is one people learn to scroll past, which is the
    /// same failure as not warning at all.
    ///
    /// **What would make this fail:** deleting any of the four checks from
    /// `the_loop_this_repo_has_built`; each has a row here that is the ONLY row it decides.
    #[test]
    fn the_self_approving_loop_is_reported_when_every_part_of_it_is_configured() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_PR_WORKFLOWS", "on");
        a_merge_train(home);

        let looped = crate::repos::Repo {
            auto_review_ceiling: crate::repos::Ceiling::Approve,
            ..a_repo_that_may_be_read()
        };
        let said = the_loop_this_repo_has_built(&looped).expect("the loop must be reported");
        assert!(
            said.contains("will merge it") && said.contains("ship-mine"),
            "the warning must say what happens and name the train that does it: {said}"
        );
        assert!(
            said.contains("auto_review_ceiling"),
            "the warning names no way out: {said}"
        );

        // The ceiling a repo is actually switched on at. Nothing to warn about: no approval is
        // posted, so no approval is read.
        for ceiling in [
            crate::repos::Ceiling::None,
            crate::repos::Ceiling::Comment,
            crate::repos::Ceiling::Changes,
        ] {
            let held = crate::repos::Repo {
                auto_review_ceiling: ceiling,
                ..a_repo_that_may_be_read()
            };
            assert_eq!(
                the_loop_this_repo_has_built(&held),
                None,
                "a ceiling of {} cannot post an approval, so there is no loop to warn about",
                ceiling.spelled()
            );
        }

        // The engine off, which is every repo by default.
        let engine_off = crate::repos::Repo {
            auto_review: false,
            ..looped.clone()
        };
        assert_eq!(the_loop_this_repo_has_built(&engine_off), None);

        // No workflow that merges: an approval that nothing acts on is just an approval.
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"label-only","steps":[{"when":[],"do":"add-label:ci-queue"}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            the_loop_this_repo_has_built(&looped),
            None,
            "a workflow that cannot merge was reported as a merge train"
        );
        a_merge_train(home);

        // And the fleet's one kill switch outranks all of it.
        env.set("SKEIN_PR_WORKFLOWS", "off");
        assert_eq!(
            the_loop_this_repo_has_built(&looped),
            None,
            "workflows are switched off for the whole fleet, so nothing merges anything"
        );
    }
}
