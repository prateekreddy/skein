# What agents working in one checkout share besides their files

**Decision.** Giving parallel agents disjoint file sets is necessary and not sufficient. Whoever
splits work across agents in one checkout also accounts for everything else they share — the git
index, the gate tools, the tracker's lease holder, commit shas, the machine's CPU, each other's
worktrees and running processes — and writes the rules for them into every agent's brief.

**Date.** 2026-09-08 to 2026-09-19, one incident at a time.

**Decided by.** Settled by the incidents below.

**Why, one shared thing at a time.**

- **The git index.** A path on the command line commits that path's working-tree state, whatever
  another agent has left in it, and going first does not avoid the sweep — it only decides whose
  name the other's work lands under. The fix is to remove the need for the shared file, not to take
  turns. `CONTRIBUTING.md:509` (rule 5) is the rule in full.
- **Commit shas.** A sha cited in a tracker completion is a promise not to rewrite it. Twelve commits
  were once rebased after agents had cited them. A commit that cannot be pushed from this
  environment — anything under `.github/workflows/` needs a token scope the fleet does not have —
  goes at the tip from the start. `CONTRIBUTING.md:540`.
- **The tracker's lease holder.** Every agent in one box authenticates as one holder, so `held`,
  the call to make first after a restart, returns other agents' live leases as well as your own, and
  `capture` infers false relations from them. Name each agent's items in its brief, so it has
  something to check `held` against.
- **The gate tools.** A sabotage left in a gate's ledger for ninety seconds failed that gate for
  every agent in the tree. Measure and sabotage in a throwaway clone
  (`git clone --no-hardlinks --shared . <scratch>`), never in a tree other agents are using.
- **What a verification is reading.** A red gate in a tree where agents are live is not evidence:
  `prose-check` went red and green seconds apart there, its count moving while another agent edited
  under it, so a push is verified in a clean clone at the exact sha. And a changed md5 proves a
  file changed, not that the named plant applied: a two-part plant whose first half silently
  matched nothing made a colleague's correct work look broken, and a test binary built from
  sabotaged source minutes earlier made broken work look correct. Assert on the artefact, once per
  part (`CONTRIBUTING.md:495`, rule 3).
- **Another agent's worktree.** Two agents sabotaging one file in one worktree overwrite each other's
  restores, and each one's md5 check looks correct against its own snapshot. Editing a worktree while
  its gate run is live moves the tree under the run and voids it by construction — that is what
  `tools/gates.sh` exit 3 is for. Verify a lane's work after it hands back, or after the merge.
- **The machine's CPU.** Lanes that each end with the full gate suite converge: three suites at once
  put load at 43 on 11 cores, almost all of it duplicated, because the gate run over the combined
  tree is the one that decides a push. A lane runs the gates its change can affect, and the combined
  tree is gated at integration.
- **Each other's processes.** `pkill -f` on a pattern kills every lane's matching process:
  `pkill -f tools/gates.sh` once took out two other lanes' runs. Record the PID you start and kill
  only that PID and its descendants, walking `ps -eo pid,ppid` first and killing deepest-first, with
  any `while` loop that respawns a fixture server killed before the server. Killing only the test
  binary orphans its fixture crew: one kill left 33 processes. And killing an agent's background run
  wakes the agent, which starts another: send the stand-down message first, saying the kill is
  deliberate, and let it drain.
- **Test fixtures in shared temp space.** Fixture directory names carry no lane id. While any lane is
  running, delete only merged lanes' worktrees, your own build directories, and what is older than
  the oldest live lane's start.

**And the brief is the only channel.** A prohibition left out of one brief is the one that gets
violated: "never a broad `pkill -f`" was in three of four briefs, and the fourth lane is the one that
ran it. Three more things belong in every brief because each cost an attempt:

- Name the command, not the gate. A lane told "run the test gate" ran the target it had just
  written, and the red was in a lib test outside its file set. Paste `tools/gates.sh --list`
  verbatim rather than choosing which gates are relevant; relevance is the judgement that cannot be
  made in advance.
- `residue-check` in every brief that touches prose, comments or commit messages. Tracker text is
  not residue-checked and the repository is, so a banned string copied from an item reaches the tree
  unless the lane runs it.
- Once a lane's branch is merged into an integration branch, the lane adds a follow-up commit and
  never amends, or the next merge conflicts with its own earlier copy.

Two traps in the guards themselves, both met: `/proc/<pid>/cmdline` is NUL-separated, so a `grep`
for a string with a space in it never matches; and `PPID` is read-only in bash, so assigning it fails
and the guard then reads the wrong parent. Guards must fail closed.

**Rules out.** Launching parallel agents with disjoint files as the only rule; editing, sabotaging
or killing inside another agent's worktree or processes; a brief that relies on the repository's
guidance reaching the agent instead of stating the rule.

**Enforced at.** `CONTRIBUTING.md:509` and `CONTRIBUTING.md:540` hold the index and sha rules. The
rest is enforced by nothing but the brief, which is why the brief carries it.
