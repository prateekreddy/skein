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
each repo's caches move to its tools area on the fleet's disk.

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
  whose toolchain is there never waits on it.
- cargo shares its whole `CARGO_HOME`, not only `registry` and `git`. Measured on 2026-10-29 with
  two bwrap namespaces at once: cargo, npm with npx, pip, uv and pnpm each finished every round on
  one shared cache. With only `registry` and `git` shared, cargo's download lock (the file
  `.package-cache` at the top of `CARGO_HOME`) stayed per box, and two boxes fetching one git
  dependency at once failed in two rounds of three.
- Build output stays per box: `target/`, `node_modules`, and the read-only fleet toolchain at
  `~/.cargo/bin`.
- The repo's cache counts toward the repo, not toward any box's disk allowance. The old one is
  cleaned from the repo's card, only while none of the repo's boxes runs, because every one of them
  reads it. Showing and pruning the tools area is SKEIN-1432.

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
  `src/box-session.sh:2597` and the tools are pointed at it at `src/box-session.sh:3982`. The install
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
- The other walls of levels 1 and 2 are the rows of [`docs/threat-model.md`](../threat-model.md).
