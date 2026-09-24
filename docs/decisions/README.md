# Decisions

A record here is a question about this project that has been **settled**, written down where every
contributor, every reviewer and every agent working in a checkout can read it, and where a test or a
`file:line` can prove it wrong. Before building something, read the records it touches; before
reopening one, read why it was closed.

These used to live in agent memory, which only the boxes of one person could read and no review
could check. Memory now keeps a one-line pointer to each record, and the record is the source.

## What a record carries

Every record has the same fields, in this order:

- **Decision** — what was settled, in one or two sentences.
- **Date** — the day it was settled.
- **Decided by** — who settled it. The project owner, unless a record says otherwise.
- **Why** — the incident or the argument that settled it. The incident is the part a reader can
  argue with; a rule without one is just something to route around.
- **Rules out** — what a contributor must not do because of it.
- **Enforced at** — the `file:line`, test or command where the code holds it. Where nothing
  enforces it yet, the record says so rather than leaving the field out.

A record follows the project's first rule, *derive, do not assert*: a claim about the code cites the
file and line or gives the command. The `path:line` citations here are held to what they named when
they were written by `tools/line-cite-check.py`, which reads every file under `docs/`.

A record is changed when the decision changes, in the same commit as the code that changes it. It is
not deleted: a reversed decision says what replaced it and why.

## The records

| record | what it settles |
|---|---|
| [warden-or-prompt.md](warden-or-prompt.md) | a privileged host act is done by a warden that can, or the person is prompted |
| [one-identity.md](one-identity.md) | what skein does on GitHub it does as you, never as a second actor |
| [install-is-one-sandbox-command.md](install-is-one-sandbox-command.md) | the install is one file handed to `sbx`, with nothing built or run on the host |
| [pr-summary-calibration.md](pr-summary-calibration.md) | the depth of a pull request summary, and saying what was not read |
| [propagation-is-not-reporting.md](propagation-is-not-reporting.md) | the shared login moves on presence and is reported on liveness |
| [login-is-judged-by-its-last-refresh.md](login-is-judged-by-its-last-refresh.md) | which copy of a rotating login is the working one |
| [tests-never-reach-a-live-fleet.md](tests-never-reach-a-live-fleet.md) | a test that has not pinned its paths refuses to run |
| [parallel-agents-share-more-than-files.md](parallel-agents-share-more-than-files.md) | what agents working in one checkout share besides their files |
| [commit-before-the-gates.md](commit-before-the-gates.md) | an agent commits its work before the full gate run, not after |
| [build-output-stays-in-the-worktree.md](build-output-stays-in-the-worktree.md) | an agent's build directory lives inside its own worktree |
| [tracker-state-lags-the-code.md](tracker-state-lags-the-code.md) | the git log, not the tracker's state, says whether work is done |
| [harness-as-a-plugin.md](harness-as-a-plugin.md) | research, parked: which part of the box harness could be a Claude Code plugin |

Two more pieces of settled knowledge already live in the design documents, and are not repeated here:

- the in-fleet rewrite, its primitives and the rules binding new code: `docs/architecture.md`, from
  its opening paragraph through §2 and §12;
- how a box's turn state is read from its screen, and what a real box taught it:
  `docs/inventory.md` §9.1.
