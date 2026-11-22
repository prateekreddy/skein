# The merge train and the AI review engine were removed

**Decision.** skein no longer merges, labels, rebases or reviews pull requests on its own. The merge
train and its workflows are deleted. The AI review engine is out of core, and nothing does review
work unattended. A person reads, comments, approves, requests changes and merges from the cockpit's
review pane, and an agent in a box reviews with its own tools.

**Date.** 2026-11-22.

**Decided by.** The project owner, in the November 2026 checkpoint: "Review engine and merge train
are not used. Review engine may be needed later but must be rethought, as a separate module outside
core." The principle behind it: review and merging work best when the AI is given control rather
than a deterministic system, and unnecessary structure around AI adds friction.

**To recover it.** The last commit containing all of it is `595d9dfc`. Read it there, for example
`git show 595d9dfc:docs/pr-review.md`, or `git worktree add <path> 595d9dfc`. It is a reference for
the rethink and is not meant to be restored as it was.

## What it did

- `src/workflow/`: a vocabulary of conditions and steps, read from a per-repository workflow file.
- `src/prwork/`: the engine that ran those workflows on a sweep. It labelled, rebased, approved,
  merged and deleted branches, kept a journal, and recorded stops and per-PR assignments. A workflow
  could be a train, landing one pull request at a time.
- `src/bin/skein-server/workflows.rs`: its routes, behind the cockpit's train panel and workflow
  editor.
- `src/review/`: the AI reading of a pull request. It wrote a summary and a review in one
  conversation over one diff, cached by head commit, under a daily budget and per-repo read-ahead,
  and answered the composer's ask and draft buttons.
- `src/reviewbox.rs`: a box per pull request for that reading, and its teardown.
- `src/owed.rs`: the checks a repository owed a reviewer, fired by the diff and answered per commit.
- Designs: `docs/pr-review.md` and `docs/pr-workflow.md`.

## What stayed, and why

- The review pane: the queue, stacks, a pull request's conversation and change view, verdicts, line
  notes, set-aside and undo. None of it depended on the engine.
- The merge button. `merge_by_hand` moved to `src/prq/handmerge.rs` with both its guards: the head
  the reader saw, and the refusal to land a stacked child on its parent's branch.
- GitHub access and tokens, in `src/gitgate` and `src/prq`.
- Standing module notes (`src/moduledocs.rs`). A person presses for one, so it is not unattended.
  `review_summaries` and `review_model` still govern it, and keep their names so existing configs
  still apply.

## What is left behind on disk

Old `summaries/`, `pr-triggers.json`, workflow files and removed `config.json` or `repos.json` keys
are ignored. Nothing reads or deletes them.

**Rules out.** Adding a sweep, a scheduled review or an automatic merge back into core. If review
automation returns, it is a separate module that core does not depend on.
