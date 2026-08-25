# Workflows on a pull request

What a workflow is, and the two facts about GitHub that decide its shape. Written before the engine,
because one of those facts changes what the feature can honestly promise.

The request, in the owner's words:

> once the PR where I am an author is approved, then apply a specific tag that enabled CI and then
> let the CI be completed, if not successful flag so that we can fix it. If successful and mergeble
> merge automatically and delete the branch. If not mergable for say if the base branch moved, then
> rebase without losing the approvals (github has a way to do this) and then do the same, let the CI
> complete then once done merge it and delete branch.

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
  keyed by `train.flow` (`grep -n 'let fronts' src/prwork.rs`), and then gates each pull request on
  it (`grep -n 'flow.serial && fronts.get' src/prwork.rs`). Two
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
`grep -n 'anchors the two acts that can carry it' src/prwork.rs`.

| act | carries the head skein decided on? | where |
|---|---|---|
| `update:rebase` / `update:merge` | **yes** — `expectedHeadOid` | `grep -n 'expectedHeadOid' src/prwork.rs` |
| `merge:*` | **yes** — `sha` | `grep -n '"sha": head_sha' src/prwork.rs` |
| `add-label:*` | **no** | `grep -n 'fn add_label' src/prwork.rs` — POSTs to `/repos/{slug}/issues/{number}/labels`, body `{ "labels": [label] }`, no head |
| `remove-label:*` | **no** | `grep -n 'fn remove_label' src/prwork.rs` — DELETEs `/repos/{slug}/issues/{number}/labels/{label}`, no head |

The two that do not are not an oversight and not fixable here: **GitHub's issue-labels API accepts
no head parameter at all**, on either verb. There is nothing to send.

What that costs, stated plainly rather than left implied: `add-label:ci-queue` is the step that
*starts CI*, and it is one of the two unanchored ones. So a push that lands between skein reading
the queue and skein applying the label starts a CI run against a head skein has never seen. The
train does not merge on it — `merge:*` re-checks with `sha` and GitHub answers 409 if the branch
moved — so the failure mode is a wasted CI run and a front that has to go round again, not a merge
of unreviewed code. The anchor is on the acts where being wrong would ship something.

### Stacks need no stack model

The one rule that matters: **the train only ever touches a PR whose base is the trunk** (the
repository's default branch). A stacked child's base is its parent's *branch* — merging it would
merge into the parent branch, not ship it — so `base:trunk` in `matches` keeps children out
entirely. When the bottom PR merges and its branch is deleted, GitHub retargets the child onto the
trunk; the next sweep sees an ordinary trunk-based PR, oldest in line. The stack merges bottom-up,
prefix-first, with zero stack-specific machinery.

### What was added to the vocabulary

Three conditions and one workflow property, all answerable from the queue skein already fetches:

| word | meaning |
|---|---|
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
  "matches": ["ready", "approved", "base:trunk"],
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

The kill switch is unchanged: workflows as a whole run only with `pr_workflows` on (Settings, or
`$SKEIN_PR_WORKFLOWS=on`), and `flag`/stops halt a single PR until a person clears it.
