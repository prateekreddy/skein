# A repo's store is `store/data`, and nothing a box writes is reached through a `.claude` path

## Plan (written before the code, SKEIN-1330)

**The name.** `$SKEIN_HOME/repos/<id>/store/.claude` becomes `$SKEIN_HOME/repos/<id>/store/data`.
`store/shared-home` (SKEIN-1324) stays where it is, beside the store, so the two are siblings
under `store/`, and nothing under `store/` is named `.claude`. `data` and not a dotted name: the
whole point is that no path component is one Claude Code reserves, and a dotted name invites the
next guard.

**What the empirical runs settled before any code** (claude 2.1.310, `claude -p` against fixtures
of both layouts, scripts in the lane's scratchpad):

- A path is judged twice, as written and as resolved, and **either one** under a `.claude`
  directory makes it "a sensitive file". So renaming the store is not enough on its own: a write
  through the checkout's `.claude/memory` link to `store/data/memory` was still refused as
  sensitive, in auto mode and in acceptEdits with the store added as a directory. A link
  *outside* `.claude` to the same target was allowed in acceptEdits once the target was an added
  directory.
- The same holds for Claude's own memory directory: with `~/.claude/projects/<slug>/memory` linked
  to the store (the memory bridge), acceptEdits refused a memory write as sensitive for the old
  **and** the new store. Auto mode allowed both. But Claude Code reads an `autoMemoryDirectory`
  setting, honoured from a trusted folder's `.claude/settings.local.json`, and with it set to
  `store/data/memory` a memory write went through in default and acceptEdits mode with no link and
  no prompt, and `MEMORY.md` from there was in context.
- Claude's Edit writes a temporary file and renames it over the target, so a hard link to a
  surfaced file breaks on the first edit. Onto a bind-mounted file the rename fails with `EBUSY`,
  Claude falls back to writing in place, and the edit lands in the bound file.
- `.claude/mailbox` is not a Claude Code directory: its teammate mailbox is
  `~/.claude/teams/<team>/inboxes/<name>.json`. There is no clash, and no reason to link the
  store's `mailbox/` into the checkout either.
- `.codex/memory` in a checkout is not Codex's memory. codex-cli 0.164's binary has no
  `.codex/memory` string at all; its memories are a store in the Codex home (`memories_1.sqlite`,
  the `{{ memory_root }}` folder, "a path in the Codex memories store … the current Codex home").

**Every reader of the store path.** `grep -rn 'store/.claude\|join(".claude")\|\.claude' src
tests tools docs` is 877 lines at `e53d762`; nearly all are `~/.claude` (Claude's own home) or the
checkout's `.claude`. The ones that name or derive the store's location are:

| where | what it does with the path |
|---|---|
| `src/repos/add.rs` | mints `repos/<id>/store/.claude` for a new repo, and says it in `--store`'s refusal |
| `src/kit.rs` `shared_home_beside` | the host's shape rule for `store/shared-home` |
| `src/probe/box-self.sh` `skein_shared_home`, `skein_box_store` | the box's shape rule, and which store a box reports into |
| `src/box-session.sh` (bind-back) | binds `store/shared-home` back into a covered box by the store's shape |
| `src/kit/skein-startup.sh` | finds a store by `*/store/.claude` globs, last-resort sibling, and links its entries into the checkout's `.claude` |
| `src/store/sandbox-bootstrap.sh` | memory bridge, surfaced files, the `.codex/memory` exception |
| `src/sbx.rs` `repo_workspace` | tells a per-VM box's store from its workspace by a `/.claude` suffix |
| `src/health/report.rs` | a fix line spelling `repos/*/store/.claude/...` |
| `src/store/agent-guide.sh` | points the agent at `.claude/skein/SHARED-HOME.md` |
| `src/web/app/shell.html`, `src/repos/record.rs`, `src/registry.rs`, store `README.md` | words |

Everything else reaches the store through `repos.json`'s `store` value or `$SKEIN_BOX_STORE`, so
moving the value moves them.

**The migration.** Host-side, at server start (beside `adopt_fleet_base_branch`), under the
repo-list lock: for each repo whose `store` has the managed shape `/…/repos/<id>/store/.claude`,
rename the directory to `store/data` and rewrite the `store` value, both or neither. The same rules
as SKEIN-1324's move: only the old one there → `rename`; only the new one → already done; both and
the old one empty → remove the empty one; both with files → refused, naming both, nothing moves and
the record keeps the old path; a link or a non-directory at either place → refused and never
followed. A rename, never a copy. A store of any other shape (`skein add --store`) is the person's
own directory and is not renamed — it is an open question below.

**A box that has not restarted.** A covered box keeps working and loses nothing: its store is a
bind mount, and a bind follows the directory, not the name, so its writes land in the renamed
directory. An uncovered box (the workshop, a privileged box) sees the volume itself, so the old
path is gone from under it; `skein_box_store` follows the move (`store/.claude` gone and
`store/data` a store) so its hooks, mailbox and status keep working, and the bootstrap repoints its
memory bridge at its next session start. What needs a restart in every box is what the launcher
and the kit set up: the `store/data` bind, the surfaced-file binds, the checkout's links, and the
settings below. The kit and the bootstrap say "restart this box" where they find the old layout.

**The checkout's `.claude`.** Linked into it from the store: `skein` (every hook finds the store
through it) and the entries Claude Code itself reads from a project `.claude` — `skills`,
`commands`, `agents`, `output-styles`, `rules` — and nothing else. Memory is `autoMemoryDirectory`
in the box's own `.claude/settings.local.json`, pointing at `store/data/memory`; the mailbox is
reached by `mailbox.sh`; `tasks/`, `handoffs/` and the rest are skein's runtime. A link an earlier
start made for any other entry, to the old or the new store, is removed (the link only).

**Settings the kit writes**, into `.claude/settings.local.json`, the box's own file:
`autoMemoryDirectory` (unless the person set their own), and `$HOME/shared` in
`permissions.additionalDirectories`, so acceptEdits mode can write `~/shared` — with the store's
`skills/` beside it where the checkout links them, which the re-run below showed was needed.

**Codex.** Its workspace sandbox cannot write outside the checkout, so the kit adds the resolved
shared home and the store's `mailbox/` to `sandbox_workspace_write.writable_roots` in
`~/.codex/config.toml`, in a line skein owns and replaces, never over a list the person wrote.
`.codex/memory` stops being a named exception to the shared-paths shape rule.

**Surfaced files.** A `shared-paths.txt` entry was a FILE link into `shared-rw/`, and Claude refuses
to edit through a file link at all. The launcher now binds each entry's copy over its place in
the checkout, with the bootstrap's shape and containment rules, and the bootstrap leaves a bound
entry alone. Where there is no bind yet (a box that has not restarted, a copy that appeared after
the start) the link stays as the fallback, readable, and the bootstrap says to restart.

**The guide.** The pointer to `.claude/skein/SHARED-HOME.md` was a read outside the box's working
directories, which prompts. The two facts it added go into the agent guide itself.

## The record

**Decision.** A repo's store is `$SKEIN_HOME/repos/<id>/store/data`, and skein moves a store at
`store/.claude` there once, at server start, record and directory together. Nothing a box writes is
reached through a path with a `.claude` component: memory is the store's `memory/` through
`autoMemoryDirectory`, `~/shared` is beside the store, the checkout's `.claude` links only `skein/`
and what Claude Code reads there, and a surfaced file is bound into the checkout rather than linked.

**Date.** 2026-10-26.

**Decided by.** The project owner (SKEIN-1330, under SKEIN-1329's sweep of 2026-10-26).

**Why.** Claude Code asks before a write to anything under a `.claude` directory, judging the path
as written and as resolved, in auto mode, with `--add-dir` and with allow rules alike, and it will
not write through a link to a file at all. Each row below is one `claude -p` write (claude 2.1.310,
haiku), the "before" column from sweep A's fixtures of the old layout and the "after" column from
the same calls against the new one, as the kit and launcher now set a box up:

| what the box writes | before (store at `store/.claude`) | after (store at `store/data`) |
|---|---|---|
| a memory note, acceptEdits | refused: "a sensitive file" (`.claude/memory` link, and the `~/.claude/projects/<slug>/memory` bridge) | written, into `store/data/memory` |
| a memory note, auto | refused through `.claude/memory`; written through the bridge | written |
| `~/shared/<file>`, acceptEdits | refused: "a sensitive file" (`store/.claude/shared-home`) | written |
| `~/shared/<file>`, auto | refused | written |
| the mailbox, by redirect, auto | refused: `.claude/mailbox` "a sensitive file" | written, into `store/data/mailbox` |
| a skill, auto | refused through `.claude/skills` | written when the store's `skills/` was an added directory; **since the owner's answer of 2026-10-26 it is not one**, so writing a shared skill asks, because a skill reaches every box of the repo |
| a skill through `.claude/skills/…`, acceptEdits | refused | **still asks**: Claude Code guards its own `.claude/skills` path as written; at the store's own path it is written |
| a surfaced `rw` file, an edit | refused: "it is a symbolic link" | edited in place, into `shared-rw/` (bound) |
| `.codex/memory` through `shared-paths.txt` | surfaced, as "Codex's memory", and refused as sensitive | not surfaced: not Codex's memory |

**Rules out.**

- Naming any directory skein creates for a box to write `.claude`, or linking one under a `.claude`
  path, whatever it is linked to.
- Linking a store entry into a checkout's `.claude` that Claude Code does not itself read there.
- Surfacing a file into a checkout as a link to a file.
- Copying a store to move it, merging two, or following a link at either name.
- A named exception to the shared-paths shape rule that nothing reads.

**Enforced at.**

- `kit::move_store_off_dot_claude` and `repos::move_stores_off_dot_claude` (`src/kit.rs`,
  `src/repos/list.rs`), run from `src/bin/skein-server/main.rs`: tests
  `an_old_store_is_moved_to_data_once`, `two_stores_with_files_are_refused_and_nothing_moves`,
  `a_link_at_either_store_is_refused_and_other_stores_are_not_renamed` and
  `stores_move_off_dot_claude_and_their_records_follow`. New repos get the name from
  `kit::STORE_NAME` (`src/repos/add.rs`).
- `skein_box_store` and `skein_shared_home` (`src/probe/box-self.sh`): test
  `a_box_follows_its_store_to_its_new_name`.
- The kit (`src/kit/skein-startup.sh`): tests
  `the_checkout_links_only_what_claude_code_reads_from_the_store`,
  `the_box_settings_point_memory_at_the_store_and_allow_the_shared_home` and
  `codex_may_write_the_shared_home_and_the_mailbox` (`src/kit.rs`).
- The launcher's surfaced-file binds and the shared-home bind by either name
  (`src/box-session.sh`): bwrap test
  `a_covered_box_writes_its_store_at_data_and_edits_a_surfaced_file_in_place`
  (`tests/isolation_bwrap/boxself.rs`); the bootstrap's half, test
  `nothing_under_a_hidden_directory_is_surfaced_and_a_bound_entry_is_left_alone`.
- The empirical table above is not a gate: Claude Code's guards are its own and change with it.
  The scripts that produced it are the lane's, and re-running them is the check.

**Open.**

- A store kept elsewhere with `skein add --store` at a path ending in `.claude` (a
  `skein-shared/.claude`): **decided 2026-10-26, rename it automatically** (SKEIN-1345), below.
  Where it is its own mount in the fleet sandbox, which is every such store outside skein's volume,
  the rename cannot happen from inside, and how the host-side half is done is still the owner's
  call: today the refusal tells the person how to do it by hand.
- A skill written through `.claude/skills/…` in acceptEdits still asks, by Claude Code's own rule.
- A box already running keeps its settings, links and binds until it restarts; nothing tells the
  person which boxes have not, beyond each box's own start messages.

## A store put at `<dir>/.claude` with `--store` (SKEIN-1345)

**Decision.** The owner, 2026-10-26: rename it automatically. At server start, beside the managed
move and under the same repo-list lock, a store whose last component is `.claude` and which is not
of the managed shape moves to `<dir>/data`, record and directory together.

**The rules**, the managed move's with one difference: `<dir>/data` is in a directory of the
person's, so it may be a folder of theirs rather than a store. skein takes it for the store only
when it holds skein's own `skein/` directory. So an old store that is gone moves the record only
onto a `data` that is a store, an empty old store is removed only beside one, and `ensure_store`
makes the old store again when `data` is not one. Both holding anything: refused, naming both. A
link at either name: refused and never followed. A rename, never a copy.

**The shared home and the box's shape rules need nothing more.** Such a store keeps its shared
home inside itself (`<store>/shared-home`, `kit::shared_home_dir`, `skein_shared_home`), so it moves
with the store, and the launcher's bind of `store/shared-home` beside the store is for the managed
shape only. A covered box's store is a bind, which follows the directory. An uncovered box follows
the move through `_skein_moved_store`, which answers `<dir>/data` for any `<dir>/.claude` that is
gone, and its caller takes that only when it is a store.

**What it cannot do from inside the fleet.** `fleet::fleet_mounts` mounts a `--store` store by its
own path, so in the fleet sandbox, where the server runs, the store is a mount point, and a mount
point cannot be renamed (`EBUSY`, measured with bwrap and with `unshare -rm`). There the move is
refused with what to do on the host instead: stop the fleet, rename the directory, change the repo's
`store` in `repos.json`, and create the fleet again, since sbx fixes mounts at create. A store under
a directory the sandbox already mounts (skein's own volume) is renamed in place. Tests:
`a_store_put_at_a_dot_claude_path_moves_to_data_beside_it` and
`a_store_mounted_under_its_dot_claude_name_says_what_to_do_instead` (`src/kit.rs`).
