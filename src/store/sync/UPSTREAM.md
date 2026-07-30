# Where these documents come from

The three files beside this one are **derived**, not authored here. Upstream is
`git@github.com:prateekreddy/sync.git` — the gateway that fronts Plane. It changes; these have to
follow, or a box is drilled in a contract the server no longer honours.

Last checked against: **`2f38a86`** — *fix(mirror): drop provenance Plane's own byline already
shows*, 2026-07-30.

This file stays in the skein repo. `ensure_probe_in` ships only the three documents into a store, so
nothing here reaches a box.

## What is derived from what

| Ours | Upstream source of truth |
|---|---|
| `work-tracking.block.md` | `AGENTS.md` § Work tracking, `docs/onboarding.md` § Add the rules to CLAUDE.md, `server/src/mcphttp.ts` `INSTRUCTIONS` |
| `work-tracking.memory.md` | the same three, reduced to what must fire unprompted |
| `work-tracking.skill.md` | `server/src/toolspec.ts` (tool contracts, TTL bounds), `readiness.ts` (the gate, capability routing, claim ordering), `toolpolicy.ts` (what is refused), `errors.ts` (codes and recovery), `capture.ts` (dedup and parent adoption), `mirror.ts` (lag, expiry flagging), `README.md` § The tool surface |

Upstream ships **no skill**, and says so on purpose: `docs/architecture.md` § *Onboarding channels*
argues that rules which always apply belong in `AGENTS.md` and the MCP handshake, never in something
that loads only when the model judges it relevant. That argument is right and the split here honours
it — the block carries the rules, the skill carries Plane's surface, which genuinely is on-demand
reference. Do not migrate the three rules into the skill.

## Refreshing

```bash
git clone --depth 1 https://github.com/prateekreddy/sync.git
```

Then diff intent, not prose — the wording here is deliberately ours:

1. **`server/src/toolspec.ts`** — argument names, defaults, bounds, and any new coordination tool.
   This is the one file that makes the skill wrong rather than merely stale.
2. **`server/src/errors.ts`** — a new code, or changed recovery guidance, means a new table row.
3. **`server/src/readiness.ts`** — `BLOCKING_LABELS` and the screen reasons are quoted almost
   verbatim in the skill's readiness list.
4. **`server/src/toolpolicy.ts`** — the `DESTRUCTIVE` set is listed by name in the skill.
5. **`server/src/mcphttp.ts` `INSTRUCTIONS`** and **`AGENTS.md`** — if a rule was added to the
   always-on channel, it belongs in the block. Adding it to the skill instead loses it.
6. **`README.md` § Known gaps** — a gap that closes (a GitHub webhook verifying `complete`, say)
   usually changes what an agent should do, not just what the server does.

Then update the commit above, and `cargo test` — the store tests assert the three files ship, and the
UI smoke test asserts nothing here leaks a credential.

## Deliberately not tracked here

- **OAuth sign-in** (`claude mcp login sync`). Upstream's recommended path for a human at a terminal;
  useless to skein, whose boxes are headless. Skein mints a `sync_agent_…` token and installs it as a
  header, which is upstream's documented headless route and stays supported.
- **Anything in `deploy/`.** Standing up the gateway is the operator's job, not a box's.
