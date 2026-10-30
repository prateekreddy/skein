# skein's plugin is a Claude Code mod

**Decision.** skein's box plugin, `src/plugin`, carries one Claude Code hooks module beside its
command hooks, its monitor and its `skein-mcp` server, all loaded by the one `--plugin-dir` the
launcher already passes. The module is `src/plugin/hooks/register.tsx`, which composes one file per
feature; the values it keeps for a session are typed once, in `src/plugin/types/index.d.ts`. The
command hooks stay: they are the baseline a Codex box and a CLI without mods still get, and a
command hook is replaced by its mod version only once that version is proven, one at a time.

**Date.** 2026-10-30.

**Decided by.** The project owner: "go ahead, let's make skein amazing", for SKEIN-1358's phases 0,
1 and 2, with each phase-3 feature brought back with its exact rules before it is built.

**Why.** skein sees a box from outside. Command hooks leave files, the cockpit reads transcripts
after the fact, mail waits until someone types, and rules live in prompts. A hooks module runs
inside the engine: it hears every event as it happens, can refuse or rewrite one, draws (a pane, a
band above the prompt, the status line, a toast), and acts (`$.prompt.submit` wakes an idle session,
`$.clock` timers, `$.tool.register`). The API is the plugin-authoring skill's `reference.md` and the
running build's `claude-code.d.ts`.

[harness-as-a-plugin.md](harness-as-a-plugin.md) parked this on two blockers. Both are answered:

1. *A box in tmux must not wait on a keypress.* Measured: an interactive Claude Code 2.1.314 in tmux,
   started with the box's own line (`claude --name … --plugin-dir …`), the plugin bound read-only,
   loads the module with no question and no screen. Its first screen is the same with and without
   the module, and `/skein` answers. The channels confirmation that parked the research belongs to
   development channels, which this does not use.
2. *Codex gets nothing.* It still gets nothing, and so the rule above: the shared command hooks stay
   until a mod version is proven, and a feature a Codex box needs is reached through an entry point
   both can call (below).

**How it was measured (SKEIN-1359).** Every run used a throwaway `$HOME` and `$CLAUDE_CODE_TMPDIR`,
no credentials, a fake API key, `bwrap --unshare-net` so the only network was loopback, a local
stand-in for the Messages API on loopback that answers one `Bash` call and then `done`, the plugin
bound read-only as a box binds it, and a stand-in `resources.json` asking for a hold. The real
`~/.claude` and the live fleet were not touched.

- *One plugin carries both.* `hooks/hooks.json` holds `hooks` and `modules` side by side; the engine
  takes one module per plugin. On 2.1.314, interactive and under `-p`, the module loaded and
  answered `/skein`, `SessionStart`, `UserPromptSubmit` and `PreToolUse` all ran
  `bin/skein-resources` (the hold reached the model as the `Bash` result), the `skein` MCP server
  connected and its tools were in the model's request, and the interactive footer counted the
  monitor running.
- *An old CLI runs the rest.* Claude Code 2.1.221 knows nothing of hooks modules: its binary holds
  no string about them. With the module in place it loaded the plugin with no error (`Errors: 0`),
  ran all three command hooks, held the `Bash` call, connected the MCP server, and drew a first
  screen identical to the one it draws for the plugin without `modules`. `/skein` is an unknown
  command there. Its own `claude plugin validate` passes the plugin.
- *A CLI whose API is older than the module's.* 2.1.272 has hooks modules, but its scan refused the
  module (`update` from `claude-code` did not exist yet) and said so in the debug log; everything
  else ran.
- *The account's switch.* Whether a session loads the hooks modules of plugins a person installed is
  a feature flag, `tengu_plugin_hooks_modules`. On 2.1.314 its default with nothing fetched is on,
  so an offline run loads the module. Seeded off in the flag cache, the module is left out with a
  debug line, the rest of the plugin runs, an interactive session shows no question, and a `-p` run
  says so once on stderr, never on stdout. `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1` did not turn it
  back on in 2.1.314. On 2.1.272 the default was off and that variable did turn it on, for `-p`.
- *Headless.* So skein's headless start sets `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1` on both halves of
  its `claude --print` (`headless_resume` in `src/runtime.rs`), where a build that asks for it gets
  it. On 2.1.314 `claude -p /skein` answers with or without it.
- *Read-only.* A session started from a folder it watches lays the build's types into
  `.claude-plugin/types/`. In a box that folder is read-only, the write fails with a debug line, and
  nothing else changes.

**The shape a feature takes.** Later lanes build to this:

- One file per feature in `src/plugin/hooks/`, exporting `register(on: On): void`. `register.tsx`
  imports it under the feature's name and calls it in one line. A direct call, never a loop over a
  list: `claude plugin validate` reads the module's source and refuses a call it cannot trace.
- An event that more than one feature only watches is hooked once, bare, in `register.tsx`:
  `session.start`, `agent.spawn`, `turn.step`, `turn.complete`, `tool.call` today, and any other
  the moment a second feature needs it. That hook calls `next(e)` itself, once, then calls each
  feature's exported `on<Event>` (`onStart($, e)`, `onSpawn($, e, started)`) in the order written
  there, each passed by if it fails, so the order the features hear an event is read in one place.
  A hook that refuses or rewrites, where its place in the chain and its `next` matter, stays in its
  feature file under a real matcher of its own: a tool, a command, a component, a pane's id.
  `claude plugin validate --strict` refuses a module that hooks one event bare twice (measured on
  2.1.314: `on("session.start") is registered twice without a matcher`), and
  `only_the_module_entry_hooks_an_event_bare` in `src/runtime.rs` holds the stronger rule, that a
  feature file hooks nothing bare at all. A matcher that selects everything (`{ cwd: /^/ }`) to
  get past the first is ruled out: it hides which feature runs when.
- A value the feature keeps for the session is a `$.state` key under `skein` in
  `src/plugin/types/index.d.ts`, declared `Shaped<T>` and read and written through `atom` with a
  `shape` tag; a value's fields change, its tag changes in the same commit. Types are imported with
  `import type`, which the engine strips, so the contract need not ship to a box.
- The file is added to `PLUGIN_FILES` in `src/runtime.rs`: a box gets only what is listed there.
- Its tests are `src/plugin/tests/<feature>.test.ts`, run by `claude plugin test`. Nothing sits
  beneath the plugin in a test, so each `on` there stands for the engine and answers what the
  feature asks of `$`.
- A feature that wraps something a box can already do (clean its own disk, link the fleet's
  browsers, restart onto a new CLI after the turn) has one implementation: the box-side entry
  point. The `skein_*` MCP tool calls it for Codex and for a CLI without mods; the mod feature calls
  the same entry point through `$.process.run`, and adds what only a mod can (a `$.tool.register`
  tool, a band button, a `/skein` command, firing on `session.measure`, `turn.complete` or a timer).
  A contract test drives both doors with one input and holds them to one answer.

**Limits.**

- The API is early access and moves between releases: the declarations 2.1.310 and 2.1.314 lay
  differ in thousands of lines. So CI checks the plugin against one pinned build, and that build's
  own declarations, not a copy kept here.
- Codex boxes get nothing from the module.
- A guard in the module stops mistakes; it is not the security boundary, which stays in the
  launcher and the host. What a box sends the cockpit is *requested* state (`docs/architecture.md`
  §2.1), never authoritative.
- Whether the module loads at all is the account's `tengu_plugin_hooks_modules`, which skein cannot
  set. A box where it reads off runs exactly the plugin it ran before this record.
- The narrower plugin a fleet gets with its plugin switch off (`turn_state_plugin_dir`) carries no
  module.

**The CLI version.** CI installs one pinned Claude Code, `CLAUDE_CODE_VERSION` in
`.github/workflows/ci.yml`, and `tools/plugin-check.sh` refuses to run on any other. The boxes are not
pinned: the sandbox installs `@anthropic-ai/claude-code` once into the npm-global prefix every box
shares, and the cockpit's update installs whatever npm's newest is then
(`update_runtimes` in `src/fleet/substrate.rs`). The two numbers were equal on 2026-10-30, at
2.1.314, read from that prefix's `package.json`. Nothing holds them equal after the next update:
pinning the boxes changes what the update button does, which is the owner's call. Until then, an
update that moves the boxes is followed by moving the CI pin and running the check.

**Rules out.** A second hooks module in the plugin (the engine refuses one). Removing a command hook
because a mod version exists, before that version is proven in a box. Writing a feature's logic
twice, once in the MCP server and once in the module. A loop over a feature list in
`register.tsx`. Keeping a copy of the engine's declarations in the repository.

**Enforced at.**

- `tools/plugin-check.sh`, run by the `plugin` job in `.github/workflows/ci.yml`: `claude plugin
  validate --strict`, `claude plugin test`, an offline `claude -p /skein` that must answer, and
  `tsc` against the declarations that build lays.
- `every_hooks_module_file_ships` in `src/runtime.rs`: every file under `src/plugin/hooks/` ships,
  as it is on disk, and every value a shipped module imports ships too.
- `only_the_module_entry_hooks_an_event_bare` in `src/runtime.rs`: a feature file's every hook has
  a matcher; the shared events are hooked in `register.tsx` alone.
- `turn_state_hooks_load_whichever_way_the_switch_is_set` in `src/probes.rs`: the installed
  `hooks.json` keeps `modules` after the turn-state hooks are merged into it, and the narrow variant
  has none.
- `every_claude_the_adapter_starts_loads_skeins_plugin` in `src/runtime.rs`: both headless halves
  carry `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`.

Supersedes the parking in [harness-as-a-plugin.md](harness-as-a-plugin.md).
