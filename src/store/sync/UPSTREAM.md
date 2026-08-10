# Where these documents come from

The files beside this one come from `git@github.com:prateekreddy/sync.git` — the gateway that
fronts Plane. It changes; these have to follow, or a box is drilled in a contract the server no
longer honours.

Last synced from: **`6e3f703`**, 2026-08-10.

This file stays in the skein repo. `ensure_probe_in` ships only the documents into a store, so
nothing here reaches a box.

## Upstream ships a plugin now, and it changes what this copy is for

Since `6e3f703` the sync repo is its own single-plugin marketplace (`plugin marketplace add
prateekreddy/sync`, `plugin install sync@sync`). The plugin carries the MCP server, the skill, a
lease **monitor**, and session **hooks** — none of which skein has any other source for.

`sync-install.sh` installs it, and still registers the gateway itself with a minted
`sync_agent_…` token. Both, deliberately:

- The plugin registers `sync` over **OAuth**, which opens a browser. Upstream's own onboarding says
  headless agents cannot do that and points them at the token route, which stays supported. A box is
  headless.
- A hand-added `sync` entry **wins** over a plugin's, and the plugin's is skipped with a note. That
  is upstream's documented behaviour and here it is the behaviour we want: the box authenticates
  with its own token and still gets the monitor, the hooks and the skill.

The hooks and the monitor do not depend on which entry won — the hooks read the session id from
their own stdin, and the monitor authenticates with a capability URL harvested from `claim`'s result.

**So the vendored skill below is now the Codex fallback, not the primary.** Plugins are a Claude Code
feature; a Codex box gets the server from a TOML block and would otherwise get no skill at all.
`sync-install.sh` skips the copy when the plugin is installed and there is no Codex on the box.

## What comes from where

| Ours | Upstream | How |
|---|---|---|
| `work-tracking.skill.md` | `plugin/skills/work-tracking/SKILL.md` | **copied verbatim** |
| `work-tracking.organising.md` | `plugin/skills/work-tracking/organising.md` | **copied verbatim** |
| `work-tracking.troubleshooting.md` | `plugin/skills/work-tracking/troubleshooting.md` | **copied verbatim** |
| `work-tracking.block.md` | `AGENTS.md` § Work tracking, `docs/onboarding.md` § Add the rules to CLAUDE.md, `server/src/mcphttp.ts` `INSTRUCTIONS` | derived — reworded, and it carries a line about the tracker being the record that upstream does not |
| `work-tracking.memory.md` | the same three | derived — the subset that must fire unprompted, in skein's memory format, with `[[…]]` links to the starter-kit memories |

**The block is not a convenience.** Upstream's own account of its channels (`docs/architecture.md`
§ *Onboarding channels*) puts `CLAUDE.md` / `AGENTS.md` as the only one that fires *before* an agent
has listed a single tool — and notes the gateway cannot ship it, because it is per-repo. Skein is
what ships it. That makes the block skein's load-bearing contribution rather than a copy of
something the server already sends, and it is why it gets its own guard
(`the_always_on_block_names_every_tool_upstreams_own_rules_do`) rather than riding on the skill's.

The skill used to be derived here too, from `toolspec.ts`, `errors.ts`, `readiness.ts`,
`toolpolicy.ts`, `capture.ts` and `mirror.ts`. It was contributed upstream in
[sync#1](https://github.com/prateekreddy/sync/pull/1) and now lives there, next to the sources every
claim in it is derived from. That is the right home: a copy maintained here goes stale the first
time an argument name changes, and nothing tells us.

## The submodule, and why the copy still exists

`upstream/sync` is the real repo, pinned. It is **not** what ships: `include_str!` runs at build
time and sync is private, so a build that read from it would fail for anyone without access, and
skein does not get to stop compiling over a work-tracking document.

So the copy is what ships and the submodule is what it is checked against.
`the_shipped_skill_is_upstreams_verbatim` fails the moment the two disagree — that is the whole
point of vendoring being safe here. In a checkout without `--recursive` the test says so and passes;
the copy is complete on its own.

## Refreshing

```bash
git submodule update --remote upstream/sync
R=upstream/sync/plugin/skills/work-tracking
cp $R/SKILL.md           src/store/sync/work-tracking.skill.md
cp $R/organising.md      src/store/sync/work-tracking.organising.md
cp $R/troubleshooting.md src/store/sync/work-tracking.troubleshooting.md
cargo test the_shipped_skill                    # green means the copies match the pin
git add upstream/sync src/store/sync            # the bumped pin is part of the commit
```

All three, always. `SKILL.md` links to the other two by name, so refreshing the entry point alone
leaves a box following a link to an older revision — current on its face and stale one hop in.

Then check the two derived files, which no copy can keep current:

1. **`server/src/toolspec.ts` descriptions** — the first place to look, and the one that is easy to
   miss because it is code rather than a document. Upstream now treats tool descriptions as the
   channel that cannot be skipped: they arrive as a set from `tools/list`, and a model cannot call a
   tool it was never told about. This is not theoretical — `capture`'s description told agents to
   decompose one child at a time long after `decompose` existed, and that is the form agents used.
   A rule can change here with `AGENTS.md` and `INSTRUCTIONS` both untouched.
2. **`AGENTS.md` § Work tracking** and **`server/src/mcphttp.ts` `INSTRUCTIONS`** — a rule added to
   either belongs in `work-tracking.block.md`; putting it in the skill instead loses it, because a
   skill only loads once the model already decided the topic was relevant. Note `INSTRUCTIONS` is
   *not* guaranteed: MCP `2026-07-28` drops the `initialize` handshake and makes `server/discover`
   optional, so upstream's rule is that nothing load-bearing may live there alone.
3. **`server/src/errors.ts`** — a new code with a recovery an agent must act on unprompted may
   deserve a line in the memory. Refusal messages are an unconditional channel too, arriving at the
   exact moment a rule is broken.

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
