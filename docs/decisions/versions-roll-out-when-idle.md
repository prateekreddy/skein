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
- Restarting a box onto a rollout while it is working, waiting on a person, or in a state skein
  cannot read; and restarting without reading its state again at that moment.
- Removing a version automatically, or removing one a placed box records, the current one, or the
  one kept for rollback.
- Treating a box's restart ask as anything but a request: skein acts on it only for a box that is
  behind and idle.
- Running anything of a package outside the fill namespace; the upstream check reads the npm
  registry's metadata only.

**Enforced at.** Tests `a_rolled_out_cli_is_read_only_in_every_box`,
`an_idle_box_moves_onto_a_rollout_and_a_working_box_does_not`,
`a_repos_rust_toolchain_file_selects_its_toolchain` and `the_clean_refuses_a_version_in_use`
(`tests/fleet_launch/versions.rs`); `only_an_idle_box_behind_on_a_version_is_restarted` and
`an_untested_cli_is_not_rolled_out_and_the_last_one_is_kept` (`src/fleet/versions.rs`);
`idle_is_a_finished_turn_with_nothing_open_and_nothing_running` (`src/fleet/idle.rs`). Retiring
`skein update-agents` is not done yet: it still exists, and still writes the sandbox's own prefix,
which a box runs only for a CLI with nothing rolled out.
