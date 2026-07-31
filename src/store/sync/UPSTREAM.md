# Where these documents come from

The three files beside this one come from `git@github.com:prateekreddy/sync.git` — the gateway that
fronts Plane. It changes; these have to follow, or a box is drilled in a contract the server no
longer honours.

Last synced from: **`4e2b3fc`**, 2026-07-31.

This file stays in the skein repo. `ensure_probe_in` ships only the three documents into a store, so
nothing here reaches a box.

## What comes from where

| Ours | Upstream | How |
|---|---|---|
| `work-tracking.skill.md` | `skills/work-tracking/SKILL.md` | **copied verbatim** |
| `work-tracking.block.md` | `AGENTS.md` § Work tracking, `docs/onboarding.md` § Add the rules to CLAUDE.md, `server/src/mcphttp.ts` `INSTRUCTIONS` | derived — reworded, and it carries a line about the tracker being the record that upstream does not |
| `work-tracking.memory.md` | the same three | derived — the subset that must fire unprompted, in skein's memory format, with `[[…]]` links to the starter-kit memories |

The skill used to be derived here too, from `toolspec.ts`, `errors.ts`, `readiness.ts`,
`toolpolicy.ts`, `capture.ts` and `mirror.ts`. It was contributed upstream in
[sync#1](https://github.com/prateekreddy/sync/pull/1) and now lives there, next to the sources every
claim in it is derived from. That is the right home: a copy maintained here goes stale the first
time an argument name changes, and nothing tells us.

## Refreshing

```bash
git clone --depth 1 https://github.com/prateekreddy/sync.git
cp sync/skills/work-tracking/SKILL.md src/store/sync/work-tracking.skill.md
```

Then check the two derived files, which no copy can keep current:

1. **`AGENTS.md` § Work tracking** and **`server/src/mcphttp.ts` `INSTRUCTIONS`** — these are the
   always-on channels. A rule added there belongs in `work-tracking.block.md`; putting it in the
   skill instead loses it, because a skill only loads once the model already decided the topic was
   relevant. That is upstream's own argument, in `docs/architecture.md` § *Onboarding channels*.
2. **`server/src/errors.ts`** — a new code with a recovery an agent must act on unprompted may
   deserve a line in the memory.

Then update the commit above and `cargo test`: the store tests assert the three files ship, and the
UI smoke test asserts nothing here leaks a credential.

## Deliberately not tracked here

- **`skills/README.md`** upstream — install instructions for a human. Skein's installer does it.
- **OAuth sign-in** (`claude mcp login sync`). Upstream's recommended path for a human at a terminal;
  useless to skein, whose boxes are headless. Skein mints a `sync_agent_…` token and installs it as a
  header, which is upstream's documented headless route and stays supported.
- **Anything in `deploy/`.** Standing up the gateway is the operator's job, not a box's.

## If you change the skill

Change it upstream and pull it back, rather than editing the copy. A local edit is invisible to
everyone else using sync, and the next refresh silently reverts it.
