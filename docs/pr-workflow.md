# Workflows on a pull request

What a workflow is, and the two facts about GitHub that decide its shape. Written before the engine,
because one of those facts changes what the feature can honestly promise.

The request, in the owner's words:

> once the PR where I am an author is approved, then apply a specific tag that enabled CI and then
> let the CI be completed, if not successful flag so that we can fix it. If successful and mergeble
> merge automatically and delete the branch. If not mergable for say if the base branch moved, then
> rebase without losing the approvals (github has a way to do this) and then do the same, let the CI
> complete then once done merge it and delete branch.

Everything here is **author-side** — pull requests the owner wrote. The reviewer role has no engine
at all, and `docs/pr-review.md` proposes one over this same machinery: a second vocabulary, not a
second engine.

## Guarded steps, not a script

A workflow is a **set of guarded steps over observable state**, re-evaluated every time the queue is
read. Each step is a condition on what GitHub says right now and an action that moves the pull
request one step along.

It is not a script that runs to completion, and the difference is not stylistic. A script has to
survive a 40-minute CI run, a restarted server, a rate limit and a laptop lid closing. A guarded
step set does not: **the state is the program counter.** Crash anywhere and the next poll picks up
from wherever GitHub actually is, because that is the only place the position was ever kept.

The owner's example is five steps and no new concepts:

| when | do |
|---|---|
| approved, and the CI label is not on it | add the label |
| checks are running | nothing — wait |
| checks failed | flag it, and stop |
| behind the base, and approved | update the branch (rebase) |
| mergeable, and checks passed | merge, then delete the branch |

**One step per evaluation, never a cascade.** After an action, what skein believes about the PR is
one action out of date — the label is added but no check has been queued yet, so "checks passing" is
still true from the *previous* run, and a cascade would merge on it. The next poll re-reads real
state; that is the whole safety property.

## The finding: rebasing and approvals

The owner's "github has a way to do this" is half right, and the half that is wrong is the half the
feature would have promised.

**There is a way to rebase, and it is not the REST endpoint.** `PUT /repos/{owner}/{repo}/pulls/
{n}/update-branch` takes `expected_head_sha` and nothing else — it merges the base into the head.
Rebase lives in GraphQL, which is what `gh pr update-branch --rebase` calls
([cli/cli#8953](https://github.com/cli/cli/pull/8953)). Confirmed against GitHub's live schema:

```console
$ gh api graphql -f query='{ __type(name: "PullRequestBranchUpdateMethod")
      { description enumValues { name description } } }'
{"description":"The possible methods for updating a pull request's head branch with the base branch.",
 "enumValues":[{"name":"MERGE","description":"Update branch via merge"},
               {"name":"REBASE","description":"Update branch via rebase"}]}

$ gh api graphql -f query='{ __type(name: "UpdatePullRequestBranchInput")
      { inputFields { name description } } }'
  pullRequestId    The Node ID of the pull request.
  expectedHeadOid  The head ref oid for the upstream branch.
  updateMethod     The update branch method to use. If omitted, defaults to 'MERGE'
```

`expectedHeadOid` is worth taking seriously rather than omitting: it makes the mutation fail if
somebody pushed since skein last looked, instead of rebasing a head skein has never seen. Same
discipline as the anchor check on a box — prove the thing is what you think before acting on it.

**Neither method preserves an approval when the repository dismisses stale ones.** GitHub's own
words, on protected branches:

> Optionally, you can choose to dismiss stale pull request approvals when commits are pushed that
> affect the diff in the pull request.
>
> In addition, with these settings, approving reviews will be dismissed as stale if the merge base
> introduces new changes after the review was submitted.

and, from the June 2023 changelog that tightened it:

> A pull request approval will be marked as stale when the merge base changes after a review is
> submitted.

Updating a branch is *exactly* "the merge base changes". The update **method** does not enter into
it: merge and rebase both move the merge base to the base tip. What decides whether the approval
survives is a repository setting skein does not control and cannot read around.

### What that means for the feature

Two things, and the second is the one worth stating out loud.

**Skein must not promise it.** The step does what it can — GraphQL, `REBASE`, `expectedHeadOid` —
and the row says the repository's branch protection decides whether the approval survives. Anything
warmer than that is a promise somebody else's setting will break.

**On a repo that dismisses stale approvals, the owner's workflow cannot complete on its own, and
that is correct.** Approve → base moves → rebase → *approval dismissed* → the "approved" guard is
false → it waits for a human. It has not failed and it must not retry: it is holding unapproved code
out of the base branch, which is what the setting exists for.

The failure mode to avoid is not the stall. It is a **silent** stall — a workflow sitting on a PR
for a day with nothing on screen saying why. So when skein's own update is what dismissed the
approval, the row says so in those words: *skein updated this branch, which dismissed your approval
— it needs approving again before this can go further.*

## Sources

- [Update a pull request branch (REST)](https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28) — `expected_head_sha`, no update method.
- [`gh pr update-branch --rebase` (cli/cli#8953)](https://github.com/cli/cli/pull/8953) — uses the `updatePullRequestBranch` GraphQL mutation.
- GitHub's live GraphQL schema, introspected above.
- [About protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches) — dismissal on merge-base change.
- [Security enhancements to required approvals (2023-06-06)](https://github.blog/changelog/2023-06-06-security-enhancements-to-required-approvals-on-pull-requests/) — the change that made merge-base movement dismiss approvals.

## The merge train (SKEIN-207)

The owner's second ask, 2026-08-24: per repo, take the oldest fully-approved PRs first; rebase,
apply the CI-enabling label, and merge+delete when green — one at a time; stacks too; skip anything
that fails and say so. Three decisions, made by the owner:

- **Serial.** One PR at a time per serial workflow. Only the front of that workflow's train is
  rebased, labeled and merged; everyone else in it waits. A parallel train re-runs CI on every
  sibling after every merge — the re-run tax and the API spend are why serial won.

  **The unit is the workflow, not the repo**, and the difference is worth stating because the
  obvious reading is wrong. `sweep` builds one front *per flow name*, a `BTreeMap<String, u64>`
  keyed by `train.flow` (`grep -n 'let fronts' src/prwork/sweep.rs`), and then gates each pull request on
  it (`grep -n 'flow.serial && fronts.get' src/prwork/sweep.rs`). Two
  serial workflows carrying pull requests in the same repo therefore have two fronts, and two pull
  requests act in one pass. That is the code's actual guarantee; a repo running one train, which is
  the configuration this was designed for, cannot tell the difference. A repo running two gets back
  exactly the re-run tax serial was chosen to avoid, so if a second train is ever wanted, the
  decision to re-take is whether the front should be keyed on the repo instead.
- **Any fully-approved PR**, not just the owner's. The train acts on the fleet's credential, so
  every label, merge and branch deletion shows under the owner's name (`prq::host_token`'s
  contract).
- **Stacks: merge the approved prefix.** Not atomic — the train ships from the bottom up as far as
  approvals reach.

### Which acts carry a head anchor, and which two cannot

`expectedHeadOid` above is the anchor discipline — prove the thing is what you think before acting
on it. It is worth writing down that **two of the train's four acts carry it and two do not**,
because the code's own comment used to read as though all four did, and that is the more dangerous
direction to be wrong in. It says so now — see
`grep -n 'anchors the two acts that can carry it' src/prwork/perform.rs`.

| act | carries the head skein decided on? | where |
|---|---|---|
| `update:rebase` / `update:merge` | **yes** — `expectedHeadOid` | `grep -n 'expectedHeadOid' src/prwork/acts.rs` |
| `merge:*` | **yes** — `sha` | `grep -n '"sha": head_sha' src/prwork/acts.rs` |
| `add-label:*` | **no** | `grep -n 'fn add_label' src/prwork/acts.rs` — POSTs to `/repos/{slug}/issues/{number}/labels`, body `{ "labels": [label] }`, no head |
| `remove-label:*` | **no** | `grep -n 'fn remove_label' src/prwork/acts.rs` — DELETEs `/repos/{slug}/issues/{number}/labels/{label}`, no head |

The two that do not are not an oversight and not fixable here: **GitHub's issue-labels API accepts
no head parameter at all**, on either verb. There is nothing to send.

What that costs, stated plainly rather than left implied: `add-label:ci-queue` is the step that
*starts CI*, and it is one of the two unanchored ones. So a push that lands between skein reading
the queue and skein applying the label starts a CI run against a head skein has never seen. The
train does not merge on it — `merge:*` re-checks with `sha` and GitHub answers 409 if the branch
moved — so the failure mode is a wasted CI run and a front that has to go round again, not a merge
of unreviewed code. The anchor is on the acts where being wrong would ship something.

### The other merge: the one a person presses (SKEIN-338)

**Derived.** There are exactly two places skein merges a pull request —
`grep -rn '/pulls/{number}/merge' src/` gives `src/prwork/acts.rs` and `src/prq/write.rs`, one each. Until
SKEIN-338 they were not equally safe, and the safe one was switched off on the owner's fleet.

| | the train's merge | the merge chip in the cockpit |
|---|---|---|
| where | `src/prwork/acts.rs`, `merge_pr` | `src/prq/write.rs`, `merge`, reached from `src/bin/skein-server.rs` |
| carries `sha` *(as it stood)* | yes, always | **no** — the body was `{"merge_method": …}` and nothing else |
| trunk check *(as it stood)* | yes — every act goes through `workflow::instead_of_merging_off_the_trunk` | **no** — the guard was reachable from `workflow.rs` and `prwork.rs` only, and this route was in neither |
| runs when `$SKEIN_PR_WORKFLOWS` is off | no | yes — and the switch is off on the owner's fleet |

The last row is what turned two gaps into one live defect: **the unguarded merge was the only merge
skein offered**. Reading step 7 of a stack (base `ladder/tenants-07-…`) and pressing merge would
merge step 6 into step 7 and delete its branch — SKEIN-237 reproduced by hand, from the surface
built for reading pull requests.

Both guards are now on both roads, composed rather than copied:

- **The head.** `prq::merge` takes an `expected_head` and refuses the empty string rather than
  defaulting to the live head — "assume current" was the hole, so a caller that cannot say what the
  reader saw is stopped instead of guessing (`grep -n 'does not know which commit' src/prq/write.rs src/prwork/acts.rs` — refused at both layers).
  A 409 is translated into *"the branch moved since you read it"* rather than left as GitHub's own
  prose, matched on the status skein itself formatted (`grep -n 'fn the_branch_moved' src/prq/write.rs`).
- **The base.** The rule was split out of `instead_of_merging_off_the_trunk` into
  `workflow::merging_off_the_trunk`, a function of `Option<bool>` and nothing else, so the hand path
  can consult it without inventing a `Facts` it never looked up. A `Facts { base_is_trunk, ..Default::default() }`
  at that call site would answer from defaults the day the rule reads a second field, on the one act
  that cannot be taken back; narrowing the argument makes that unwritable
  (`grep -n 'fn merging_off_the_trunk' src/workflow/evaluate.rs`).
- **Where they meet.** `prwork::merge_by_hand`, because `docs/modules.toml` gives `prq` no
  dependency on `workflow` and `prwork` — "nothing depends on THIS except the tick and the routes" —
  already depends on both. No new edge, and the thing that merges pull requests stays a leaf.

Two things it deliberately does **not** check, both of which the train does:

- **`$SKEIN_PR_WORKFLOWS`.** The switch governs skein acting unattended. A person with their finger
  on the button is not that, and the fleet where the switch is off is exactly the fleet where this
  is the only merge there is.
- **A workflow stop.** `Act::Flag` writes a note that the train has gone as far as it can and needs
  a person; a person then merging by hand is that note being answered, not overridden. The two
  guards that remain are about facts — what you are merging, and where it lands — not about policy.

The base is checked **before** the head, and the order is asserted
(`grep -n 'the base check did not run before the head check' src/prwork/acts.rs`): a stacked child is
wrong to merge at any head, so telling its reader the branch moved would send them off to re-read a
change that still must not merge from there.

**What stops a third one appearing.** `tests/merge_guard.rs` reads the crate rather than a running
server, because "this merge is guarded" stays true while an unguarded one is added beside it. It
asserts that every `/pulls/…/merge` in `src/` carries a `sha`, that the only caller of `prq::merge`
is inside `merge_by_hand`, and that the cockpit route never derives its expected head from GitHub —
which would make the `sha` agree with the live head by construction and guard nothing.

**Closed — SKEIN-365, and both halves.** This paragraph described a confirmation that said
*"Merge #N? This lands it on the base branch"* and named neither the commit nor which branch, and a
`revAct` that sent `drafted_at` only when there were line notes — so a merge posted `""` and the
server fell back to the queue row's sha, which can lag the diff on screen. Re-derived from the code
on 2026-09-06 (`grep -n 'SKEIN-365' src/web/index.html`): the confirmation now reads
`Merge #<n> (<sha>) into <base>?` with the sentence *"…is the commit on screen, and the one skein
sends"*, and both facts fall away together rather than naming a base nobody checked; and `drafted_at`
is `mergeHead` on a merge, so the press carries the sha the reader was looking at.

### Two words for approval, because GitHub answers two questions (SKEIN-339)

`approved` and `review-satisfied` look like the same condition and are not, and the train needs
both. The difference is not a nicety: reading one as the other is what kept the train from claiming
a single pull request on the owner's own fleet, silently, for the whole life of the feature.

**`reviewDecision` does not mean "somebody approved this".** It means "this branch's review
*requirement* is satisfied", so GitHub sets it to `APPROVED` only where branch protection requires a
review and the requirement is met, and leaves it null wherever review is social — however many
approvals a pull request carries. `CHANGES_REQUESTED` still surfaces either way, because a refusal
is not gated on a requirement, which is precisely why the field reads as though it works.

Measured, on the owner's live queue — `GET /api/repos/gadget-demo/review`, 21 open PRs,
2026-08-26:

```
review_decision:  {'': 20, 'CHANGES_REQUESTED': 1}
my_review:        {'none': 12, 'commented': 7, 'approved': 2}
```

`APPROVED` on none of the twenty-one, including the two the owner had approved by hand. The train
below asks for `approved` in `matches`, and `matches` gates before `steps` — so nothing claimed
anything, no step ran, no `flag` fired, and even the catch-all `wait:` that exists to make silence
audible never evaluated. Nothing to see, by construction.

So the two facts are now separate, and derived separately —
`grep -n 'fn the_repository_has_a_verdict_only_when_it_asks_for_one' src/prwork/facts.rs`:

| word | question | built from |
|---|---|---|
| `approved` | has anybody approved it, and is nobody refusing? | `reviewDecision == APPROVED`, **or** your own standing approval against the current head (`my_review`/`review_is_current`); never while `CHANGES_REQUESTED` stands |
| `review-satisfied` | is the repository's own requirement in the way? | `reviewDecision` — `APPROVED` yes, `REVIEW_REQUIRED`/`CHANGES_REQUESTED` no, **null: there is no requirement, so nothing is in the way** |

Keeping `review-satisfied` matters as much as fixing `approved`. On a protected repo, `reviewDecision`
folds in CODEOWNERS, the required-approvals count and rules skein has no API to read; a train that
merged on approvals it can count would be refused by GitHub, and a refused action is a stop somebody
has to clear. So the train asks for both: people have said yes, **and** the repository is not
holding the door.

**What skein still cannot see.** `prq::Pr` carries the repository's verdict and *your* last review,
and nobody else's — so on a repo with no review requirement, a third party's approval is invisible
and `approved` reads false. That under-reports rather than over-reports (it holds a PR back rather
than shipping one), and it closes when the queue surfaces the standing approvals it already fetches
in `latestReviews`.

### Stacks: what the base rule does, and what is not verified

**Derived.** The train only ever touches a PR whose base is the trunk (the repository's default
branch). A stacked child's base is its parent's *branch* — merging it would merge into the parent
branch, not ship it — so `base:trunk` in `matches` keeps children out entirely, and a merge off the
trunk is refused at the act whatever the file says
(`grep -n 'fn instead_of_merging_off_the_trunk' src/workflow/evaluate.rs`;
`grep -n 'fn a_stacked_child_is_kept_out_by_its_matches' src/prwork/rows.rs`). And there is no
stack-specific code: `grep -rn "restack\|retarget\|update_base\|--onto" src/` finds only prose and
test names, and no request anywhere retargets a pull request's base.

**Not verified**, and previously asserted here as fact — this section used to end *"The stack merges
bottom-up, prefix-first, with zero stack-specific machinery"*, with no citation, no command and no
measurement, in a document whose first rule is derive-don't-assert. Three things it has to survive
and nobody has checked it against:

- **Children are excluded, not queued.** `base:trunk` in `matches` is a claim rule, so every
  stacked child is off the train until its parent lands — reported by the 2026-08-26 audit as all
  18 of the stacked PRs on the owner's fleet, which is a count worth re-running rather than
  quoting. The claim above describes what happens after each parent merges, one sweep at a time;
  nothing has watched a stack do it.
- **GitHub's retarget is not a rebase.** When a parent merges, GitHub moves a child's base ref to
  the trunk. It does not move the child's commits, and the documented merge here is
  `merge:squash+delete` — so the parent's work lands on the trunk as one new commit while the
  child's branch still carries the originals. What the child's diff, `mergeable` and
  `mergeStateStatus` then say is exactly the untested part. The train's `behind → update-branch:rebase`
  step is the plausible answer and has not been shown to be one.
- **A flag is durable.** `Act::Flag` writes a stop that only a person clears
  (`grep -n 'fn clear' src/prwork/rows.rs`). So whatever the answer to the previous point is, if it is
  "conflict", a 17-deep stack asks for 17 presses rather than one.

Until somebody runs a stack through and writes down what happened, read this section as: the base
rule keeps children safely out, and what the train does with them afterwards is a plan.

### What was added to the vocabulary

Four conditions and one workflow property, all answerable from the queue skein already fetches:

| word | meaning |
|---|---|
| `review-satisfied` | the repository's own review requirement is met, or it asks for none. See above — this is `reviewDecision`, and `approved` is not |
| `behind` | GitHub's `mergeStateStatus` is `BEHIND` — the base has commits this branch lacks |
| `current` | known **not** behind. `UNKNOWN` satisfies neither, same discipline as `mergeable` |
| `base:trunk` | the PR's base ref is the repository's default branch |
| `"serial": true` | on a workflow: one at a time per **(repo, workflow)** — not per repo; order this workflow's carrying PRs oldest-first (lowest number); only the first one without a stop acts. A stopped PR is passed over — that is the "skip and move ahead" |

### How a skip reaches the owner

A failure (`flag:`, or an action GitHub refused) writes a stop, exactly as before — and the stops
now travel on the counts poll to a **banner row** in the cockpit (`#trainban`, a block row like the
cover banner, never an overlay): the repo's name and the skipped PR numbers with their reasons.
Clicking the repo opens its review queue. Clearing the stop puts the PR back in line.

### The train, written down

`~/.skein/workflows.json` — steps are guards over live state, first match fires, one per
evaluation; see the top of this document for why:

```json
{ "workflow": [ {
  "name": "merge-train",
  "serial": true,
  "matches": ["ready", "approved", "review-satisfied", "base:trunk"],
  "steps": [
    { "when": ["changes-requested"], "do": "flag:changes were requested — resolve them to rejoin the train" },
    { "when": ["not-mergeable"],     "do": "flag:conflicts with the base — resolve the conflict to rejoin the train" },
    { "when": ["behind"],            "do": "update-branch:rebase" },
    { "when": ["checks:failing"],    "do": "flag:CI failed — fix it and clear this stop to rejoin the train" },
    { "when": ["no-label:ci-queue"], "do": "add-label:ci-queue" },
    { "when": ["label:ci-queue", "checks:pending"], "do": "wait:CI is running" },
    { "when": ["label:ci-queue", "checks:passing", "mergeable", "current"], "do": "merge:squash+delete" },
    { "when": [],                    "do": "wait:waiting for GitHub to catch up" }
  ] } ] }
```

Step order is load-bearing. `behind → rebase` sits **before** `checks:failing → flag`, so a stale
red run on an old head gets its rebase (and a fresh CI run) before it can stop anything. The merge
step requires `current` explicitly: GitHub reports `mergeable: true` for a branch that is merely
behind (no conflict), and without `current` the train would merge code CI never tested against the
current trunk. And the rebase-dismisses-approval finding above still governs: on a repository that
dismisses stale approvals, the train's own rebase costs the approval, the PR stops carrying the
workflow (its `approved` match fails), and the train moves on — it rejoins, oldest-first, when
somebody re-approves. That stall is the branch-protection setting working, not a train defect.

`matches` asks for `approved` **and** `review-satisfied` for the reason above: the first is people,
the second is branch protection, and a train that merges wants both. One rule bridges them, and it
is the reason the first step can fire at all — a reviewer requesting changes makes both conditions
false, so on the plain reading the PR would leave the train one pass before its own
`flag:changes were requested` step could run. It does not
(`grep -n 'fn the_reviewer_said_no_instead' src/workflow/evaluate.rs`, SKEIN-247): a `matches` that asked for
review keeps the pull request when the answer is no, because asking somebody a question does not
stop being your question when you dislike the answer.

The kill switch is unchanged: workflows as a whole run only with `pr_workflows` on (Settings, or
`$SKEIN_PR_WORKFLOWS=on`), and `flag`/stops halt a single PR until a person clears it.
