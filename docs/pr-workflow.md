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
