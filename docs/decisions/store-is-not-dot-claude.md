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
`permissions.additionalDirectories`, so default and acceptEdits mode can write `~/shared`.

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
