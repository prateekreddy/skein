# skein — project guidance

## Work tracking

**If the `sync` MCP server is configured, work on this project is tracked through it, and the
`work-tracking` skill is the discipline.** A todo list in a session's context is scratch that dies
with the context; the tracker is the record.

- `held` **first** after any restart or compaction — resuming what you already hold beats claiming
  something new, and it is the one call whose absence you cannot notice.
- `capture` the moment you notice something worth doing, *before* deciding whether to act. It
  deduplicates, so there is no threshold to clear. No description means nobody can pick it up.
- `claim` before non-trivial work. Assigning yourself in the tracker reserves nothing — only `claim`
  is atomic — so two agents that skip it both believe they own the item.
- End with `complete` (carrying evidence: a commit, a PR, what you verified) or `release`. Silence
  is not an ending.
- `decompose` to break something up, in **one** call rather than `capture` per child: a parent stops
  being claimable at its first child, so a decomposition written one call at a time is open to
  another agent before it is finished.

**The intended shape of the plan**: modules → high-level tasks → low-level tasks and bug fixes.

## The in-fleet rewrite

Work in progress on branch `in-fleet`. Four documents, and they are meant to be read together:

| document | what it is |
|---|---|
| `docs/architecture.md` | the destination design |
| `docs/inventory.md` | what skein actually does, read from the code |
| `docs/parity.md` | the acceptance gate — what the rewrite must still do |
| `docs/delivery.md` | sequence, migration, and the landmines |

`ARCHITECTURE.md` at the root is a **signpost to these four and nothing else**. It used to
describe ratatui, Svelte and per-box microVM kernels, and had to be disclaimed here; it now makes
no claim about the code, so there is nothing left in it to go stale.

### Two rules these documents were written to enforce

1. **Derive, do not assert.** Three rounds of review established the pattern: claims that count
   something reproduce, and prose that *summarises* code drifts. Where a claim is about the code,
   cite the file and line or give the command — never paraphrase from memory.
2. **A feature that cannot be written as a composition of the five primitives means the primitive
   set is wrong**, and the fix is the primitive set, not a mechanism beside it.

## Before you change anything

**Read "Before you change anything" in `CONTRIBUTING.md`** before any non-trivial change, and
before concluding that code is dead, that a test passes for the right reason, or that a design
question is still open. Seven rules live there, each one naming the incident that bought it. That
section is the source of truth for them; the `change-discipline` skill is the copy an agent loads,
and it points at the same place rather than restating it.

Every one of those rules was bought with a real failure here, and they share one shape: **not a bad
edit — a wrong premise, confidently implemented.** The four that cost the most, with the part that
is specific to working here:

- A whole fleet-migration path built against a design that had already been settled two days
  earlier, and reverted. `held`, then `search` the tracker, then read `docs/decisions/` and
  `memory/` — *before* building, and before `capture`, which is also how two duplicate items got
  written.
- Two tests that **could not fail**: one asked `tmux has-session` about a socket inside the
  directory it deletes; one compared a `$HOME`-relative list against a path outside `$HOME`. Name
  the concrete change that would make an assertion fail *before you write it*, then prove it.
- "Nothing calls this" concluded from a `grep | head` that cut before the production caller. Count
  the whole result set first.
- `git add -A` swept a running subagent's seven files into an unrelated commit. **Commit by
  explicit path whenever an agent is working in this tree**, and give parallel agents disjoint
  files — most open work touches `src/fleet/`, which makes it a serialisation point.

## After a browser run, confirm nothing leaked

```sh
node tests/ui/harness/leaks.mjs      # exit 0, and it prints the names it looked for
```

**The line this replaces was the purest example of the shape above.** It read
`ps -eo pid,args | grep -v grep | grep -cE 'ui-onboard-|skein-fleet-it-|skein-move-it-'`, every
agent ran it, and it answered `0` on a box carrying 195 matching processes — 122 of them older than
half an hour, the oldest over nine. Those three names were the fixtures of the day when it was
written; there are forty now, and a stale alternation is indistinguishable from a correct one by its
output alone. A check that cannot fail is worse than no check, because it is trusted.

So the replacement does not carry a list. It reads the names out of the call sites that create the
fixtures — `Scratch::boxes` and `Scratch::temp` in `tests/*.rs`, `mkdtempSync` and `freshFixture` in
`tests/ui/` — prints them, and **refuses to run at all when it derives none**, so a rename it stops
recognising fails loudly instead of quietly printing zero.

**And it derived the right names and then tried them in the wrong place.** Until this
was fixed the scan read `/proc/<pid>/cmdline` and nothing else, so it could only see a fixture named
in a process's ARGUMENTS — while a `skein-server` is exec'd as a bare binary path and carries its
fixture in `SKEIN_HOME` and `SKEIN_FLEET_ROOT`. It printed "nothing is running" beside a server that
had been up for seven and a half hours. It reads the environment as well now, says how many
processes would not let it, and `tests/ui/leakcheck.mjs` starts a process naming a fixture only in
its environment to keep that true. Same lesson twice: a derived pattern is only as good as the
surface it is tried against.

**And then it went red about processes a reader could see were not theirs** — the same
failure from the other side, because a check that goes red for somebody else's reason teaches people
to read past it, and the next red is read past too. It reports in two halves and only one of them
attributed what it found: the marker half said, in those words, that nothing there was this run's to
be red about, while the fixture-name half exited 1 over five rows of another lane's `rustc`, every
one nought seconds old with a live parent. **Both halves now answer one question the same way — a
process is this run's leak only if it is attributable to THIS worktree and nothing is left of the
run that made it.** Everything else is still reported, with its age and where it came from, under a
headline that says whose it is; another lane's orphan is a real leak and is named as theirs, to be
answered for where it belongs. So `exit 0` means *this* worktree is clean, and the rows printed
beneath the counts are context rather than an accusation.

**And both halves then went red over a process that was doing nothing wrong** — another
worktree's `tmux … new-session -d -s skein-server`, one second old, twice, in a tree where no suite
was running at all. Each half of the sentence above was wrong, and either alone produces that red.
**`ppid == 1` is not "its run has gone" for something daemonised on purpose**: tmux forks a server
and the launcher returns, so a healthy fixture's tmux is parentless from its first second, and an
age threshold separates nothing, because a leak is one second old in its first second too. **And a
path in an environment says where a process has BEEN as well as what it is using** — the only
mention of this tree in that process was `OLDPWD`, the breadcrumb of the `cd` another lane's agent
made on its way into its own worktree. So a parentless process is this run's leak only when nothing
else of its own fixture is still running under a live parent, and its own children do not count
(they are exactly what a stranded tmux keeps); and only when it does not also name another checkout
of this repository, which the check asks `git worktree list` rather than keeping a list of lanes,
breaking the tie on where the process is standing. One it can attribute to neither is printed under
a headline saying so and reaches no exit code, because both available guesses are a failure this
file already has a name for.

The suites also stop what they started now, on every way out including a throw and a Ctrl-C
(`quiesceOnExit`, same file). A fixture *directory* is still kept when a suite fails, because it is
the only evidence a failure leaves — but its tmux server and doorway loop go, since a kept fixture
is exactly what let one restart a python every two seconds for nine hours.
