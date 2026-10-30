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

**Decided by.** The project owner, on SKEIN-1350.

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

- Download caches are a repo's. Each repo has one, at `repos/<id>/store/cache` beside its store,
  shared by that repo's boxes for cargo's registry and git downloads, npm's cache with `_npx` in
  it, pip's and uv's caches, and pnpm's store. The launcher binds it back through the cover, the way
  it binds `store/shared-home`, and points each tool at it. A command skein runs into a box by a
  crossing, rather than in its session, takes the same values from the box's session (SKEIN-1354),
  so a crossing's cargo and npm use the repo's cache too, and a box the launcher gave no cache keeps
  its own in its crossings as well. A box of another repo cannot see it. A
  repo whose store is kept elsewhere by `skein add --store` has none, for the reason
  [`shared-home-outside-claude.md`](shared-home-outside-claude.md) gives for its shared files: no
  box can see beside such a store. Its boxes keep their own caches.
- cargo shares its whole `CARGO_HOME`, not only `registry` and `git`. Measured on 2026-10-29 with
  two bwrap namespaces at once: cargo, npm with npx, pip, uv and pnpm each finished every round on
  one shared cache. With only `registry` and `git` shared, cargo's download lock (the file
  `.package-cache` at the top of `CARGO_HOME`) stayed per box, and two boxes fetching one git
  dependency at once failed in two rounds of three.
- Build output stays per box: `target/`, `node_modules`, and the read-only fleet toolchain at
  `~/.cargo/bin`.
- The repo's cache counts toward the repo, not toward any box's disk allowance. It is cleaned from
  the repo's card, only while none of the repo's boxes runs, because every one of them reads it.

**Rules out.**

- Binding anything from the sandbox's home read-write into a box. That list, `share_paths`, is gone.
- A cache, or any other writable thing, shared between boxes of different repos.
- A fleet-wide copy any box can write. Fleet-wide content is skein's to install and read-only.

**Enforced at.**

- The sandbox's home: bwrap test `the_sandboxs_cargo_and_npm_reach_no_box`
  (`tests/isolation_bwrap/homes.rs`), and `fleet::shared_into_every_box`, which fails a test if a
  `share_paths` list comes back.
- The repo cache: bwrap test `a_repos_boxes_share_its_cache_and_another_repos_box_sees_none_of_it`
  (`tests/isolation_bwrap/repocache.rs`); its bind is `src/box-session.sh:2665` and the tools are
  pointed at it at `src/box-session.sh:3729`.
- A crossing points the tools where the session does: fleet tests
  `a_crossing_into_a_box_uses_its_repos_download_cache` and
  `a_crossing_into_a_box_with_no_repo_cache_carries_none_of_its_names`
  (`tests/fleet_launch/environment.rs`); the names are read out of the launcher by
  `place::session_cache_names`, pinned by
  `the_names_a_crossing_takes_for_the_repo_cache_are_the_session_blocks` (`src/place/crossing.rs`).
- The cache is made only as a real directory: test
  `a_managed_store_gets_a_repo_cache_and_a_link_there_is_refused` (`src/kit.rs`).
- Cleaning: tests `a_repo_cache_is_cleaned_only_with_all_its_boxes_stopped` and
  `no_clean_follows_a_link_into_or_out_of_the_repo_cache` (`src/fleet/clean.rs`).
- The other walls of levels 1 and 2 are the rows of [`docs/threat-model.md`](../threat-model.md).
