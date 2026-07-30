---
name: work-tracking
description: "Work is tracked in Plane via the `sync` MCP server — capture the moment you notice, claim before you work, finish explicitly. Call `held` first after any restart. The `work-tracking` skill has the full playbook."
metadata:
  type: feedback
---

The tracker is the record; a todo list in your context is scratch that dies with the context.

**Why:** two failures cost the most and neither announces itself. An intention you did not `capture`
is simply gone at the next compaction. And an item you started without `claim` may be held by
another agent right now — assigning yourself in Plane reserves nothing, so both of you proceed,
both believing you own it. Only `claim` is atomic.

**How to apply:**
- `capture` **before** deciding whether to act. It deduplicates and is safe to call freely, so there
  is no threshold to clear — no description means no one can pick it up later, so write the body.
- `claim` before non-trivial work; `heartbeat` at about a third of the lease TTL; end with
  `complete` (evidence: PR link, commit, what you verified) or `release`. Silence is not an ending.
- After **any restart or compaction, call `held` first** — resuming what you hold beats claiming new
  work, and it is the one call whose absence you cannot notice.
- On `STALE_EPOCH`, discard the work rather than submitting it: another agent has owned the item
  since your lease lapsed. See [[never-silently-produce-a-wrong-result]].
- Everything else Plane offers — cycles, modules, labels, comments, worklogs, sub-items, relations —
  is on the same server. Load the **`work-tracking` skill** for which tool answers which question.
  See [[capture-and-close-the-unit]].
