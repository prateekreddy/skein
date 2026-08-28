# skein — project guidance

## Work tracking

**All work on this project is tracked in Plane, via the `sync` MCP server.** The project is
**Skein** (`SKEIN`), id `5b00388b-e528-4770-94f5-c0e9dccfcfb5`. A todo list in a session's context
is scratch that dies with the context; the tracker is the record.

- `held` **first** after any restart or compaction — resuming what you already hold beats claiming
  something new, and it is the one call whose absence you cannot notice.
- `capture` the moment you notice something worth doing, *before* deciding whether to act. It
  deduplicates, so there is no threshold to clear. No description means nobody can pick it up.
- `claim` before non-trivial work. Assigning yourself in Plane reserves nothing — only `claim` is
  atomic — so two agents that skip it both believe they own the item.
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

`ARCHITECTURE.md` at the root is a **signpost to these four and nothing else** (SKEIN-222). It used
to describe ratatui, Svelte and per-box microVM kernels, and had to be disclaimed here; it now makes
no claim about the code, so there is nothing left in it to go stale.

### Two rules these documents were written to enforce

1. **Derive, do not assert.** Three rounds of review established the pattern: claims that count
   something reproduce, and prose that *summarises* code drifts. Where a claim is about the code,
   cite the file and line or give the command — never paraphrase from memory.
2. **A feature that cannot be written as a composition of the five primitives means the primitive
   set is wrong**, and the fix is the primitive set, not a mechanism beside it.

## Before you change anything

**Invoke the `change-discipline` skill** (`.claude/skills/change-discipline/`) before any
non-trivial change, and before concluding that code is dead, that a test passes for the right
reason, or that a design question is still open.

Every rule in it was bought with a real failure here, and they share one shape: **not a bad edit —
a wrong premise, confidently implemented.** The four that cost the most:

- A whole fleet-migration path built against a design that had already been settled two days
  earlier (SKEIN-312), and reverted. `held`, then `search` the tracker, then read `memory/` —
  *before* building, and before `capture`, which is also how two duplicate items got written.
- Two tests that **could not fail**: one asked `tmux has-session` about a socket inside the
  directory it deletes; one compared a `$HOME`-relative list against a path outside `$HOME`. Name
  the concrete change that would make an assertion fail *before you write it*, then prove it.
- "Nothing calls this" concluded from a `grep | head` that cut before the production caller. Count
  the whole result set first.
- `git add -A` swept a running subagent's seven files into an unrelated commit. **Commit by
  explicit path whenever an agent is working in this tree**, and give parallel agents disjoint
  files — most open work touches `src/fleet.rs`, which makes it a serialisation point.
