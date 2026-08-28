# Generalization: skein for any repo, many at once

> Design doc. Turn skein from a thing-specific reader of one shared store into a repo-agnostic
> product that can **set up any repo** and **drive several repos' fleets at once**. Decided with the
> user 2026-06-29: multi-repo cockpit · build alongside thing (leave it working) · skein owns the code.

## The forcing constraint (why the store is a sibling)

`sbx` mounts land at their **absolute host path** with no `src:dst` remap. A store kept *inside* the
repo would be mounted into the `--clone` target and break the in-container `git clone`. So the shared
`.claude` store is a **sibling outside the repo** (thing: `<dir>/skein-shared/.claude`), mounted into
every box and linked to `<clone>/.claude` by the kit's startup hook. **Any generic store must also be a
sibling.** (An in-repo `.skein/` is the one layout that cannot work.)

## From one store to many projects

Today skein resolves a single `sandboxes.json` and shows its boxes. Generalization introduces a
first-class **project**:

```
Project { name, store }      // store = the sibling dir holding sandboxes.json + sessions/ diffs/ mailbox/
```

- **Projects config:** `$SKEIN_PROJECTS` → default `~/.config/skein/projects.json`:
  `[{ "name": "thing", "store": "/…/skein-shared/.claude" }, …]`
- **Back-compat (thing keeps working with zero config):** when the config is absent/empty, synthesize
  ONE project from the legacy single-store resolution (`$SKEIN_REGISTRY` / `$SKEIN_STORE` /
  `$SKEIN_SHARED` / `<git>/../skein-shared/.claude`). So existing setups are just "the default project."
- `skein init <repo>` registers a project here (and scaffolds its store — below).

## Box identity across projects

Box names can collide across repos (two repos with a `feature-x` box). So identity becomes
**(project, name)**, not `name` alone:

- `BoxView` gains `project`.
- Per-box HTTP routes gain the project: `/api/projects/:proj/boxes/:name/…`.
- Every lib fn that currently resolves the single store via `store_dir()`/`locate_registry()`
  (`read_diff`, `session_signal`, `session_digest`, `resume_box`, `archive_box`, `changed_files`,
  mailbox, …) takes the **project's store** instead. This is the largest slice (slice 2).
- `valid_name` still guards both `proj` and `name` (no `/`, `..`, NUL).

## Setup machinery (skein-owned, generic)

Lift the dev-sandbox subsystem out of thing into `skein/sandbox/` (skein repo), thing-free:

- **generic kit** (`sandbox/kit/spec.yaml`) — link the sibling store → run bootstrap; no `thing`
  name, store path from env/arg.
- **generic store-template** (`sandbox/store-template/`) — ONLY skein's engine:
  `sandbox-bootstrap.sh`, `box-status.sh`, `box-diff.sh`, `box-session.sh`, `mailbox.sh`,
  `statusline-command.sh`, `settings.json`. None of thing's app skills/gates or `.env` seeding.
- **`skein init <repo>`** — scaffold the sibling store from the template, register the project in the
  projects config, and emit the launch command:
  `sbx create --clone --kit <skein-kit> --name <prefix><branch> claude . <store>`, followed by the
  persistent `sbx exec` + tmux attach
- **Configurable naming:** box-name prefix per project (default = derived from repo dir, e.g. repo
  name). The prefix is a contract: `setup` names `--name <prefix><branch>`, `sandbox-bootstrap.sh`
  recovers the branch by stripping it, and skein groups/labels by it. Store the prefix in the project
  config so all three agree.

## Slice plan (each builds green + commits; thing stays working throughout)

1. **Project model + config + back-compat** (lib): `Project`, `load_projects()` with the
   single-store fallback. Tests. *(no behavior change yet)*
2. **Thread (project, name) through the store layer** (lib): per-box fns take a resolved store;
   `load_views` aggregates across projects and tags `BoxView.project`. The big internal refactor.
3. **API + web**: project in routes; inbox groups by project; a repo filter/switcher.
4. **`skein init` + generic kit + generic store-template** under `skein/sandbox/`.
5. **Docs/naming** cleanup; README rewrite around "any repo."

## Non-goals / assumptions

- Boxes are Claude Code agents (skein is built around Claude Code hooks/signals).
- Not auto-migrating thing's dev-sandbox; thing is just one project skein reads.
- Multi-agent (non-claude) runners are out of scope.
