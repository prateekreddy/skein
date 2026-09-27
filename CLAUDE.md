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

## The design

The design is in four documents under `docs/`. [`ARCHITECTURE.md`](ARCHITECTURE.md) is the one
place that lists them and says in what order to read them; it makes no claim about the code, so
there is nothing in it to go stale.

Two standing rules, each stated once, and this is a pointer to each:

1. **Derive, do not assert** — at the top of [`CONTRIBUTING.md`](CONTRIBUTING.md).
2. **A feature that cannot be written as a composition of the five primitives means the primitive
   set is wrong** — at the top of [`docs/architecture.md`](docs/architecture.md).

## Before you change anything

**Read "Before you change anything" in `CONTRIBUTING.md`** before any non-trivial change, and
before concluding that code is dead, that a test passes for the right reason, or that a design
question is still open. Seven rules live there, each one naming the incident that bought it, and
that section is their only source. The `change-discipline` skill is the copy an agent loads: a
one-line index into the same section, not a second statement of it. For the tracker half of rule 1,
`held` and `search` come first, as "Work tracking" above says.

## After a browser run, confirm nothing leaked

```sh
node tests/ui/harness/leaks.mjs      # exit 0, and it prints the names it looked for
```

Exit 0 means this worktree is clean. What it looks for, how it decides a process is this run's, and
the four ways it was once wrong are in [`tests/ui/README.md`](tests/ui/README.md).
