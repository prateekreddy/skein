# An agent commits its work before the full gate run

**Decision.** An agent working in its own worktree commits its change, by explicit path, before it
runs `tools/gates.sh`, and amends that commit if the gates find something. It does not gate first and
commit what passed.

**Date.** 2026-09-22.

**Decided by.** Settled by the incident below.

**Why.** A full gate run takes 15 to 20 minutes and reliably outlives an agent's cycle. Three lanes in
a row handed back mid-run with their work uncommitted, so the only record of the change was a dirty
worktree, and resuming one twice still did not land it. The instinct to gate first puts the longest
step before the only step that makes the work survivable: a lane that stops after committing has
lost nothing, and one that stops before has lost everything but the diff on disk.

It also makes the gate receipt name a commit — `at <sha>` rather than `at <sha> + N uncommitted
change(s)` — which is the form `tools/gates.sh --verify` can check against a tree that has not moved.

The amend is only for a commit nothing sits on and nothing has cited (`CONTRIBUTING.md:551`); after
that, fix with a new commit.

**Rules out.**

- Running the full gates on uncommitted work at the end of a lane.
- Taking the gates off the lane instead: that removes its ability to correct itself.

**Enforced at.** Nothing mechanical. `tools/gates.sh` exits 3 when the tree changes during a run
(`tools/gates.sh:1229`), and `--verify` checks a receipt against its run (`tools/gates.sh:715`), but
neither can tell whether the work was committed first. The lane brief carries the rule.
