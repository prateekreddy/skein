# An agent's build directory lives inside its own worktree

**Decision.** An agent that builds sets `CARGO_TARGET_DIR` inside its own worktree, and never to a
shared path such as the fleet root's `.skein` directory.

**Date.** 2026-09-09, when the build directories were found.

**Decided by.** Settled by the incident below.

**Why.** The fleet reached 88% of its disk, and 19.2 GB of it was build directories under the fleet
root's `.skein`, four days old, from agent lanes whose briefs put their build output there. Nothing
in skein makes those directories and nothing removes them: it was a convention with no owner
(`src/fleet/disk.rs:167-180`). A worktree already has the lifetime a build directory needs — it goes
when the worktree goes — and the leak happened because the convention put the cache outside the one
directory that had it. Only the privileged workshop box can write there at all, so this is an
orchestration problem, not a skein defect, and it is invisible from the boxes that might notice.

The first report of it was also wrong, in a way worth keeping: it inferred "the box is gone" from a
directory's name and printed the inference under a heading that read as a measurement, and three
tracker items were built on it before it was disproved.

**Rules out.**

- A build directory under a shared path.
- Reporting an inference from a naming convention as a measurement.

**Enforced at.** skein reports what it did not make: `src/fleet/disk.rs:165-188` names every entry
under the fleet root's `.skein` that is neither skein's own nor any box's, and deletes nothing. What
puts a build directory in the right place is the lane brief.
