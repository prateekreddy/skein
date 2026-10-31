# A repo's shared working files live beside its store's `.claude`, not inside it

**Decision.** For a store skein manages, `$SKEIN_HOME/repos/<id>/store/.claude`, the directory every
box links `$HOME/shared` to is `$SKEIN_HOME/repos/<id>/store/shared-home`: a sibling of `.claude`,
not `store/.claude/shared-home`. skein moves an existing one across once, when it next prepares the
store. It refuses rather than merges when both places hold files, and it never follows a link at
either place. A store of any other shape, which only `skein add --store` makes, keeps its shared
files inside itself, as before.

**Since SKEIN-1330** the store itself is no longer named `.claude`: it is `store/data`, beside
`store/shared-home` (`docs/decisions/store-is-not-dot-claude.md`). The rule above holds for both
names, so a box started before the store moved, which still has `store/.claude` bound, finds the
same shared home.

**Date.** 2026-10-25.

**Decided by.** The project owner, from his report of the prompts (SKEIN-1324).

**Why.** Every write to `~/shared` in a box asked for permission: *"Claude requested permissions to
edit …/store/.claude/shared-home/<file> which is a sensitive file"*. Claude Code treats any path
inside a `.claude` directory as its own configuration. It judges the path a link resolves to, so
allowing `/home/agent/shared` did not stop the prompt. Working data is not agent configuration, and
it had no reason to be under that name.

How it got there is history, not a decision:

- `708518c` (2026-06-29) created each repo's store as `repos/<id>/store/.claude`, because the store
  *was* each checkout's `.claude`: the kit linked one to the other.
- `641d3b3` (2026-07-14) put `shared-home/` inside that directory to reuse a directory that was
  already mounted and already shared between a repo's boxes. Nothing written at the time weighed
  the `.claude` name.
- Since then the store has stopped being the checkout's `.claude`. SKEIN-1053 gave every box a
  `.claude` directory of its own and stopped linking the whole store there. SKEIN-1083 (open) moves
  runtime records out of the `.claude` store. This move goes the same way.

**What a box can see, checked rather than assumed.** The plan assumed that a box sees the whole
`~/.skein` volume, so the sibling would need no new mount. That is true only of an **uncovered**
box. The workshop box, a privileged box, a box skein cannot match to a repo, or a box started by a
launcher older than the mount set all see the fleet's mounts as they are. A **covered** box (the
normal case: matched to a repo, not privileged) does not. The launcher puts a tmpfs over every
fleet mount, `~/.skein/repos` among them (`src/box-session.sh:2403`). It then binds back only the
store it is given, `SKEIN_BOX_STORE`, which is the `.claude` directory (`src/box-session.sh:2411`).
So `store/shared-home` sat under the tmpfs. A box's mountinfo still names the volume, because
the bind of `store/.claude` comes from it, and that is likely where "the whole volume" was read
from. So the launcher now binds the sibling back too, beside the store
(`src/box-session.sh:2420`), only for the managed shape and never through a link. A box already
running keeps the namespace it started with, so it gets `~/shared` back only after a restart. Until
then `shared-home.sh` refuses and says to restart. It does not make the directory, because a
directory made under the tmpfs would lose its files when the box stops.

**Why only the managed shape.** `fleet::fleet_mounts` mounts a store kept elsewhere by its own path
and nothing beside it, so a sibling of that store is unreachable from any box. The store's parent
is also a directory of the person's, not skein's. Moving those stores would need a mount per repo
and a sandbox recreate. That question is open, and nothing here settles it.

The rule is decided from the path's shape, `/<…>/repos/<id>/store/.claude`, on both sides. The
box cannot know `$SKEIN_HOME`, and a host that answered differently for the same path would leave
the box waiting for a directory the host never made. The cost is one edge case: a `--store` path of
exactly that shape outside `~/.skein` is treated as managed. Its sibling is not mounted into the
fleet, so its boxes refuse to link `$HOME/shared` and say why.

**Rules out.**

- Putting anything a box works with under a `.claude` path, where every write to it prompts.
- Making the shared home from inside a box. The host makes it, and the launcher binds it.
- Merging two shared homes, or following a link at either one, when moving between them.
- Linking `$HOME/shared` past files still at the old place, which would hide them.

**Enforced at.**

- `kit::shared_home_dir` and `kit::ensure_shared_home` (`src/kit.rs`): where the files live, and the
  move. Tests `an_old_shared_home_is_moved_beside_the_store_once`,
  `two_shared_homes_with_files_are_refused_and_nothing_moves` and
  `a_link_at_either_shared_home_is_refused_and_not_followed`.
- `skein_shared_home` (`src/probe/box-self.sh`) and `src/store/shared-home.sh`: the box's half. Test
  `shared_home_links_beside_the_store_and_repairs_an_old_link` (`src/kit.rs`), which also checks
  that no part of the link's target is a `.claude` directory.
- The bind beside `SKEIN_BOX_STORE` (`src/box-session.sh:2420`). The bwrap test
  `a_box_whose_repo_tracks_claude_never_reports_into_its_checkout`
  (`tests/isolation_bwrap/boxself.rs`) runs a covered box and checks that `$HOME/shared` and an
  import both land in the sibling. The fleet test
  `a_box_lives_and_dies_inside_the_fleet_sandbox` (`tests/fleet_launch/lifecycle.rs`) checks the
  same link on a box it started and provisioned.
- The threat model's store row and its SKEIN-1308 note (`docs/threat-model.md`). The held
  whole-store fix on `fix/iso2-1308-held` has to cover `store/shared-home` now as well as
  `store/.claude`.
