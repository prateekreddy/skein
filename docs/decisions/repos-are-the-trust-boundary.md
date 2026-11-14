# Repos are the trust boundary

**Decision.** skein's security model has three levels.

1. **A hard wall around the person's machine, their credentials, and skein itself.** No box reaches
   the host, the fleet's credentials, or what skein runs and is built from.
2. **A light wall between repos.** A box of one repo cannot read or change another repo's checkout,
   store, memory, `~/shared` or tokens, and cannot plant anything another repo's boxes run.
   Messaging between boxes (the mailbox, Claude messaging) stays open, and skein may share
   read-only content it installed itself across the whole fleet.
3. **Within one repo, a single trust domain.** The repo's boxes share freely. skein guards only
   against accidents there: each box keeps its own checkout and its own build output.

**Date.** 2026-10-29.

**Decided by.** The project owner, on SKEIN-1350. Amended by the owner on 2026-10-31 (SKEIN-1426):
each repo's caches move to its tools area on the fleet's disk. Amended again on 2026-11-06
(SKEIN-1373): a repo's kinds decide which caches it gets; its shared compiler cache waits.

**Why.** The launcher bound the sandbox's `~/.cargo`, `~/.rustup` and `~/.npm` read-write into
every box, whatever repo it belonged to. They were kept on the grounds that nothing on a box's
`PATH` points into them. That was wrong: `npx` installs a package under `~/.npm/_npx` and runs it
from there on later calls, and cargo compiles whatever is in `~/.cargo/registry/src` without
checking it again, build scripts included. So a box of one repo could plant code a box of another
repo ran, which is the shape SKEIN-963 removed for `~/.local/bin`.

Taking the caches away from every box was not the answer either. Sharing downloads is worth
something, and the boxes of one repo already share a store, memory and `~/shared`, so a wall
between them buys little. The wall that matters is between repos, and that is where the model
puts it.

**What follows.**

- Download caches are a repo's, and so are the tools its agents install. Each repo has one tools
  area on the fleet's own disk, at `<fleet root>/.repo-tools/<repo id>`, which every box of the
  repo sees at `~/.repo-tools` (SKEIN-1431, the owner's decision of 2026-10-31 on SKEIN-1426). It
  holds every download cache under `cache/`: cargo's whole `CARGO_HOME`, npm's cache with `_npx` in
  it, pip's and uv's caches. And it holds what the tool managers install: rustup's home, `cargo
  install`, npm's global prefix, uv's pythons and tools, pipx, pyenv, nvm, fnm, volta, mise and
  `GOPATH`, with `tools/bin` on every box's `PATH` after the fleet's npm prefix, so a `claude` installed
  there never shadows the fleet's. The launcher makes
  it, binds it into the repo's boxes and no other repo's, and points each tool at it. A command skein
  runs into a box by a crossing, rather than in its session, takes the same values from the box's
  session (SKEIN-1354), so a crossing's cargo and npm use the area too, and a box the launcher gave
  no area keeps its own in its crossings as well. A box of another repo cannot see it. A repo whose
  store is kept elsewhere by `skein add --store` has none, because such a store names no repo the
  launcher can tell its boxes by. Its boxes keep their own tools and caches.
- The area is outside `.skein`, which is skein's own and read-only in every box. The caches were at
  `repos/<id>/store/cache` beside the store on the host from 2026-10-29 (SKEIN-1350); the host's
  store is virtiofs, slow for a toolchain, and the owner judged a cache there made no sense. The old
  caches are no longer used, and nothing removes them; they stay bound into the repo's boxes so
  that one can still be cleaned from inside a box.
- pnpm's store is the one cache that is each box's own, beside its checkout in the box root. pnpm
  builds `node_modules` by hard links from its store, and a hard link cannot cross from one mount to
  another, even on one file system (link(2) answers EXDEV); the area and the checkout are two
  mounts in a box, so a store in the area would be copied into every worktree in full, which is
  SKEIN-1429. In the box root it is in the same mount as every worktree under the checkout. uv still
  copies from its cache into a virtual environment for the same reason (`UV_LINK_MODE=copy`).
- An install into the area is shared by every box of the repo, and some installers are not safe
  for two processes on one home at once (rustup is not: rust-lang/rustup#988). So the area has one
  lock, `skein-lock [name] -- <command>`, an flock on a file in the area's `.locks`. The fleet's
  rustup shim takes it only while it installs a toolchain the repo pins and the area lacks; a build
  whose toolchain is there never waits on it. Every agent has the command, Codex's too: it is in the
  turn-state plugin's `bin`, bound read-only at `~/.skein-bin` on every box's PATH (SKEIN-1436).
- cargo shares its whole `CARGO_HOME`, not only `registry` and `git`. Measured on 2026-10-29 with
  two bwrap namespaces at once: cargo, npm with npx, pip, uv and pnpm each finished every round on
  one shared cache. With only `registry` and `git` shared, cargo's download lock (the file
  `.package-cache` at the top of `CARGO_HOME`) stayed per box, and two boxes fetching one git
  dependency at once failed in two rounds of three.
- A repo's caches follow its kinds (SKEIN-1373, the owner on 2026-11-06: "if it's a JS repo there's
  no point adding the Rust cache, and vice versa"). A kind is rust, node, python or go, read off
  each box's checkout as it starts (`Cargo.toml`, `package.json`, `pyproject.toml` or
  `requirements.txt`, `go.mod`, at the top or one directory down) unless the owner chose the
  repo's kinds on its card, which skein keeps in its own record of the repo and never in the
  repo's files. A kind the repo is not has its cache names left unexported, so a tool run anyway
  uses the box's own default and counts against that box; where the tool managers install is
  pointed into the area for every repo, so a cargo tool installed in a node repo still runs.
- No compiler cache yet: no box gets `RUSTC_WRAPPER`. The owner asked for one shared sccache per
  rust repo, and two things are settled first. sccache compiles in its own server, which outlives
  the heavy job that started it, keeps that job's cgroup from being removed and charges every later
  compile there, outside the queue (SKEIN-1479). And sccache 0.13 keys each entry by the target
  directory's path, so two worktrees share nothing (SKEIN-1481; measured 2026-11-06: 0 hits of
  101 between two, 101 of 101 at one path).
- Build output stays per box: `target/`, `node_modules`, and the read-only fleet toolchain at
  `~/.cargo/bin`.
- The repo's cache counts toward the repo, not toward any box's disk allowance. The old one is
  cleaned from the repo's card, only while none of the repo's boxes runs, because every one of them
  can still see it; nothing uses it, so all of it is listed as removable.
- The area does not grow for ever, and skein removes nothing in it on its own (SKEIN-1432, the
  owner's decision on SKEIN-1426). Its size is shown per repo, on the repo's card and in each box's
  `skein_top`, with no budget. The tool versions in it nobody has run for `unused_tool_days` (a
  setting, 0 turns it off) are listed, by the newest access time of their executables, in each
  box's `skein_clean`, its resource signal and the repo's card, and a box's agent may remove one,
  only while no process on the machine has a file in it open. That is checked, not locked: a box
  that starts the version in the seconds between the last check and the removal loses it mid-run,
  and the owner accepted that window (2026-10-31); the removal does not take the area's lock. And each tool's own prune runs once a
  day in each running box: `pnpm store prune` on the box's store, `uv cache prune` on the repo's
  cache from one box of the repo. cargo cleans its own caches (`cache.auto-clean-frequency`, on by
  default since cargo 1.88), and nothing skein writes turns that off.

**Rules out.**

- Binding anything from the sandbox's home read-write into a box. That list, `share_paths`, is gone.
- A cache, or any other writable thing, shared between boxes of different repos.
- A fleet-wide copy any box can write. Fleet-wide content is skein's to install and read-only.

**Enforced at.**

- The sandbox's home: bwrap test `the_sandboxs_cargo_and_npm_reach_no_box`
  (`tests/isolation_bwrap/homes.rs`), and `fleet::shared_into_every_box`, which fails a test if a
  `share_paths` list comes back.
- The repo's tools area: bwrap tests
  `a_repos_boxes_share_its_tools_area_and_another_repos_box_sees_none_of_it`,
  `a_tool_installed_into_the_area_survives_a_box_restart`,
  `the_caches_are_on_the_trees_file_system_and_pnpm_hard_links_into_a_worktree` and
  `a_link_where_the_area_should_be_is_refused` (`tests/isolation_bwrap/repocache.rs`); its bind is
  `src/box-session.sh:2641` and the tools are pointed at it at `src/box-session.sh:4095`. Which
  caches follow which kind, and no box gets a compiler wrapper: bwrap test
  `a_repos_kinds_decide_its_caches_and_no_box_gets_a_compiler_wrapper`; the kinds a box is handed:
  tests `a_checkouts_kinds_are_read_off_its_files` (`src/repos/kinds.rs`) and
  `a_box_start_carries_its_kinds_and_the_owners_choice_replaces_detection` (`src/fleet/start.rs`). The install
  lock: fleet test `two_concurrent_auto_installs_are_serialised_by_the_shim`
  (`tests/fleet_launch/versions.rs`).
- A crossing points the tools where the session does: fleet tests
  `a_crossing_into_a_box_uses_its_repos_download_cache` and
  `a_crossing_into_a_box_with_no_repo_cache_carries_none_of_its_names`
  (`tests/fleet_launch/environment.rs`); the names are read out of the launcher by
  `place::session_cache_names`, pinned by
  `the_names_a_crossing_takes_for_the_repo_cache_are_the_session_blocks` (`src/place/crossing.rs`).
- The old cache is made only as a real directory: test
  `a_managed_store_gets_a_repo_cache_and_a_link_there_is_refused` (`src/kit.rs`).
- Cleaning: tests `a_repo_cache_is_cleaned_only_with_all_its_boxes_stopped` and
  `no_clean_follows_a_link_into_or_out_of_the_repo_cache` (`src/fleet/clean.rs`).
- The area's listing and pruning: tests `an_old_version_is_listed_and_a_fresh_one_is_not`,
  `a_file_open_in_a_version_is_seen_in_that_area_only`,
  `each_box_prunes_daily_and_one_box_a_repo_prunes_uv` and
  `the_prune_script_runs_each_tools_own_prune` (`src/fleet/repotools.rs`); bwrap test
  `a_file_a_box_holds_open_marks_the_version_open_in_its_repos_area_only`
  (`tests/isolation_bwrap/repocache.rs`); the box's removal,
  `an_unused_tool_version_is_removed_only_while_nothing_has_it_open` (`tests/plugin_tools.rs`).
- `skein-lock` for every agent: bwrap test `every_agent_in_a_box_has_skein_lock_and_cannot_change_it`
  (`tests/isolation_bwrap/repocache.rs`).
- The other walls of levels 1 and 2 are the rows of [`docs/threat-model.md`](../threat-model.md).
