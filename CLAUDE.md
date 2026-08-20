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

`ARCHITECTURE.md` at the root describes the current system and is **stale** — it still describes
ratatui, Svelte and per-box microVM kernels. Do not trust it.

### Two rules these documents were written to enforce

1. **Derive, do not assert.** Three rounds of review established the pattern: claims that count
   something reproduce, and prose that *summarises* code drifts. Where a claim is about the code,
   cite the file and line or give the command — never paraphrase from memory.
2. **A feature that cannot be written as a composition of the five primitives means the primitive
   set is wrong**, and the fix is the primitive set, not a mechanism beside it.
