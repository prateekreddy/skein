# Agent CLIs and Rust toolchains are known versions, rolled out by the owner and applied when idle

**Decision.** Every agent CLI (claude-code, codex) and every Rust toolchain a box runs is a version
skein fetched into the fleet depot, checked and published read-only, and never changed afterwards.
skein fetches and tests each new CLI version on its own; nothing changes for any box until the owner
presses "Roll out". A rollout changes what the next start binds. A running box keeps the version it
started on, shows "restart to apply", and is restarted onto the new one only when it is idle: at its
first idle moment if it asked for that, otherwise after it has been idle for ten minutes. A working
box is never restarted. Rust is per repo: a repo whose tree pins a toolchain in `rust-toolchain.toml`
or `rust-toolchain` gets that toolchain, fetched once and shared read-only; every other repo gets the
fleet's default, which is rolled out like a CLI.

**Claude Code is pinned** (SKEIN-1380, the owner's option (a), "skein tests, I click"). The one
version skein's mod is checked against is written in `src/claude-code.version`, and it is also the
only claude-code version the fleet fetches and the only one "Roll out" accepts. Moving it is a
reviewed commit, whose CI checks the mod against the new build before any box can be given it. A
newer release on npm is shown and nothing more. Codex is not pinned and follows npm's newest. A
claude-code version is tested by the version check and by the mod's own `tools/plugin-check.sh`,
run with that entry's `claude` first on `PATH` and `CLAUDE_CODE_VERSION` set to it.

**TypeScript is pinned for that check** (SKEIN-1396, the owner's decisions of 2026-10-30). The
mod's check type-checks with `tsc`, and the one version is written in `src/typescript.version`,
which CI's `plugin` job reads too. skein fetches that version into the depot as its own kind,
`typescript@<version>`, and binds it read-only into the check of each claude-code entry; no box is
given it, and it is shown in the cockpit and never rolled out. A claude-code entry is not tested
until it is there, and an entry tested with another TypeScript, or with none, is tested again.

**Date.** 2026-10-30.

**Decided by.** The project owner (SKEIN-1372), with the coordinator's addition of the same day that
a box is told about a pending version and may choose its own moment.

**Why.** `skein update-agents` rewrote the npm prefix every box runs from in place, so an update
changed what running agents executed half-way through a session, with no record of which box ran
which version (the box-environment design, §3.1). skein's plugin is becoming a Claude Code mod
(SKEIN-1358), and a mod has to be tested against the exact CLI the boxes run, which needs that CLI to
be one known version rather than whatever the prefix holds today. Restarting a box mid-turn loses
work, and an agent between tasks knows better than a heuristic when it is free.

**Rules out.**

- Installing or updating a CLI or toolchain in place, anywhere a box executes from.
- Rolling out a CLI version that has not passed its tests (`fleet::versions::rollout` refuses).
- Rolling out a claude-code version other than `src/claude-code.version`, or writing that number
  anywhere else.
- Restarting a box onto a rollout while it is working, waiting on a person, or in a state skein
  cannot read; and restarting without reading its state again at that moment.
- Removing a version automatically, or removing one a placed box records, the current one, or the
  one kept for rollback.
- Treating a box's restart ask as anything but a request: skein acts on it only for a box that is
  behind and idle.
- Running anything of a package outside the fill namespace; the upstream check reads the npm
  registry's metadata only.
- Writing the TypeScript pin anywhere but `src/typescript.version`, binding TypeScript into a box,
  running its package's install scripts, or recording a claude-code test that ran without it.

**Enforced at.** Tests `a_rolled_out_cli_is_read_only_in_every_box`,
`an_idle_box_moves_onto_a_rollout_and_a_working_box_does_not`,
`a_repos_rust_toolchain_file_selects_its_toolchain` and `the_clean_refuses_a_version_in_use`
(`tests/fleet_launch/versions.rs`); `only_an_idle_box_behind_on_a_version_is_restarted` and
`an_untested_cli_is_not_rolled_out_and_the_last_one_is_kept` (`src/fleet/versions.rs`);
`idle_is_a_finished_turn_with_nothing_open_and_nothing_running` (`src/fleet/idle.rs`).
TypeScript: `the_ci_typescript_pin_is_the_fleets_pin`,
`only_typescript_and_a_plain_version_is_a_typescript_entry` and
`the_mod_check_is_given_the_pinned_tsc` (`src/fleet/versions.rs`);
`a_claude_entry_waits_for_its_typescript_and_is_retested_when_it_arrives` and the `tsc` the planted
check of `a_rolled_out_cli_is_read_only_in_every_box` runs (`tests/fleet_launch/versions.rs`); the
TypeScript row of `tests/ui/updatepane.mjs`. Retiring
`skein update-agents` is not done yet: it still exists, and still writes the sandbox's own prefix,
which a box runs only for a CLI with nothing rolled out.
