## Work tracking

Work lives in **Plane**, reached through the `sync` MCP server. That is the record; your own todo
list is scratch. Three rules:

1. **Write it down first.** The moment you notice anything worth doing — a bug, a missing test, a
   refactor, a question for a human — call `capture` *before* deciding whether to do it now. It
   deduplicates and is safe to call freely. An unwritten intention is lost the moment your context is,
   and that includes a bug someone just pointed at: being discussed is not being tracked.
2. **Claim before you work.** Never start non-trivial work on an item you have not claimed with
   `claim`. Assigning yourself in Plane reserves nothing: two agents doing it both believe they own
   the item and both proceed. Only `claim` is atomic. If it refuses, follow the error's recovery line
   rather than working around it.
3. **Finish explicitly.** End with `complete` — carrying the evidence, a PR link or commit and what
   you verified — or `release`. Going silent means the lease expires and someone redoes your work.
   Call `heartbeat` on long tasks.

After a restart, call `held` first to find out what you were in the middle of. Never take work by
editing assignees or state in Plane directly — the gateway refuses it.

To break a large item up, call `decompose` once with every child — not `capture` per child. A parent
with unfinished children is deliberately unclaimable, and that starts at the *first* child: written
one call at a time, another agent can start work under a decomposition you have not finished writing.
To do the reverse — put items that already exist under one container — call `gather`, which asks a
person before it moves anything.

Plane's own surface is on the same server: cycles, modules, labels, comments, worklogs, sub-items,
relations. Use it rather than keeping the state in your head — the **`work-tracking` skill** is the
playbook for which tool answers which question.
