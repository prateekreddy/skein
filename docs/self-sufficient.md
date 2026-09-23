# skein as a self-sufficient product — architecture & plan

> Status: PROPOSAL (awaiting approval). Goal: a user gives skein a **repo URL** and **GitHub
> auth**, and gets a working, observable, steerable fleet — with the repo implementing *nothing*
> for skein's sake. Repo-agnostic with native Claude and Codex runtime adapters.

## Guiding principle

A box is just **an `sbx --clone` sandbox running an agent**. skein makes it legible and steerable
using only the host's `sbx` / `git` / `gh` and its own injected probe. If a capability requires the
*repo* to ship a script or hook for skein to function, that's a coupling bug — pull it into skein.

## Signal ownership (the boundary)

| signal / capability        | who it's for        | how skein gets it (target)                              | owner   |
|----------------------------|---------------------|---------------------------------------------------------|---------|
| live / stopped / stale     | cockpit             | **`sbx ls`** (running state), not hook `lastSeen`        | skein   |
| which boxes exist          | cockpit             | enumerate from `sbx ls`; branch/dir via host `git`      | skein   |
| diff (badge + full)        | cockpit             | host-side `git` — **already done** (`diff::read_diffstat_file`) | skein   |
| session digest             | cockpit             | host-side: commits + journal (+ last message)           | skein   |
| working/waiting/needs-input| cockpit             | **injected** turn-state probe (per-agent adapter)       | skein   |
| current task               | cockpit             | injected probe (per-agent adapter)                      | skein   |
| registration of a box      | fleet contract      | skein writes it on launch/adopt                         | skein   |
| commit-guard / slice-gate / cost-gate | the project | repo's own hooks (untouched)                         | repo    |
| dep install / app env (`--kit`, .env) | the project | optional repo provisioning, auto-applied if present  | repo    |
| cross-box memory + mailbox | fleet (shared brain)| skein-provisioned shared store                          | shared  |

## What skein absorbs from `setup-sandbox.sh` / `store-template`

The current launch line is the map of what's generic vs project:

```
sbx create --clone --kit "$kit" --name "thing-$1" claude . "$shared"
           └clone┘ └─project──┘ └──skein owns───┘ └agent┘   └skein owns┘
```

- **Launch** (`sbx create --clone …` + tmux attach) → skein constructs and runs it.
- **Shared store** (`$shared`) → skein provisions the minimal skein-needed store itself
  (empty `sandboxes.json`, `mailbox/`, skein's own hooks). Repo's project store-template layers on
  top *only if present*.
- **`claude`** is the sbx **agent** positional → this is the per-agent seam (`claude` | `codex`).
- **`--kit`** (dep/tool provisioning) and `.env` seeding stay the repo's, optional.

## Components to build in skein

1. **Launcher** — `sbx create --clone --name <box> [--kit <opt>] <agent> . <store>`, then
   `sbx exec` into the provider's persistent tmux session, from
   `{repo (url|path), branch, agent}`. Replaces `SKEIN_LAUNCH_CMD` → `setup-sandbox.sh`.
2. **Store provisioner** — create the sibling `.claude` store with registry + mailbox + skein hooks;
   detect & apply an optional repo store-template/kit.
3. **Liveness from sbx** — `fleet_running()` parses `sbx ls`; `state()` uses running state for
   live/stopped, falling back to `lastSeen` only if `sbx` is unavailable.
4. **Turn-state adapters** — Skein owns the provider-neutral probe contract. Claude maps
   Notification/TodoWrite lifecycle events; Codex maps PermissionRequest/UserPromptSubmit/PostToolUse
   and Stop. Both write the same status/task/session/diff/telemetry files. Claude's wiring is merged
   into project settings and Codex's generated user-level hooks are installed by the kit. A single
   Codex Bash/`jq` wrapper translates plain probe context into the event-specific hook JSON envelope;
   provider-neutral scripts remain unchanged.
   The original Claude path uses `box-status.sh` + `box-task.sh` (embedded via
   `include_str!`); installs them into the box and **additively merges** the
   `UserPromptSubmit`/`Stop`/`Notification`/`PostToolUse` entries into the box's `settings.json`
   (preserving the project's own hooks). Idempotent. (Considered alt: host-side transcript tail —
   deferred; keeps boxes untouched but is fragile + per-agent.)
5. **Session digest host-side** — assemble from commits + journal; drop `box-session.sh` dependency.
6. **Registration owned by skein** — write `{name, branch, dir, started, lastSeen}` on launch/adopt;
   enumerate via `sbx ls`. Stop requiring `sandbox-bootstrap.sh` to register for skein's sake.
7. **Auth propagation** — use the host's `gh` token to clone the URL, open/merge PRs, and seed each
   box's git credential (the `sbx secret … github` / proxy path) so the agent can fetch/push.

## Repo cleanup (gadget-demo)

- Remove the skein-only hooks now owned by skein: `box-status.sh`, `box-task.sh`, `box-diff.sh`,
  `box-session.sh` + their `settings.json` wiring. (On `master` these are already absent — so master
  becomes the canonical *project-only* store-template, the desired end state. The removal lands on
  `feat/dev-sandbox-tooling` where they currently live.)
- `setup-sandbox.sh` stays for manual / `--direct` human onboarding, but is no longer required for
  skein; trim its skein-oriented prose.
- Reconcile the `master` ↔ `feat/dev-sandbox-tooling` divergence.

## Phasing (each phase independently verifiable)

- **Phase 1 — sbx-sourced liveness. ✅ DONE.** `fleet_liveness()` parses `sbx ls --json` (NDJSON or
  array/object, varied key casing), `Sandbox::state_with()` makes a running box `live` regardless of
  `lastSeen` (turn-status still wins) and a stopped box stale; wired into `load_views` + CLI `ls`.
  Overridable via `SKEIN_LS_CMD`; falls back to `lastSeen` when sbx can't be consulted. Unit-tested;
  needs host verification of the real `sbx ls --json` shape.
- **Phase 2 — skein-owned launch. ✅ DONE (launch); store-provisioning deferred.** When
  `$SKEIN_LAUNCH_CMD` is unset, `launch_command` builds `sbx create --clone [--kit $SKEIN_KIT]
  --name <name> <$SKEIN_AGENT|claude> . <store>`, followed by the persistent agent attach, so the
  repo needs no launch script. Branch isn't passed: the box's bootstrap derives it from
  the name (`thing-<branch>` → checkout `<branch>`). `$SKEIN_LAUNCH_CMD` still overrides (fallback).
  Unit-tested; **needs host verification** (no sbx here). Pinned the per-runtime seam via `$SKEIN_AGENT`.
  REMAINING: provisioning a store *from scratch* (for fresh/any-repo setups with no existing
  `.claude`) — currently relies on the kit + an existing store; folds into Phase 4 (URL clone) and
  Phase 3 (skein ships its own kit that also installs the turn-state probe).
- **Phase 3 — turn-state probe. ✅ DONE (Claude adapter); needs host verification.** skein ships
  `box-status.sh`/`box-task.sh` (embedded via `include_str!`) and `ensure_probe_in()` installs them into
  the shared store's `skein/bin/` + **additively merges** the `UserPromptSubmit`/`Stop`/`Notification`/
  `PostToolUse(TodoWrite)` hooks into the store's `settings.json` (idempotent; the repo's own hooks
  preserved). The probe writes turn-state to `<store>/status/<vmid>.json`; `current_status()` reads it,
  wired into `load_views` (registry `status` kept only as a transitional fallback). `ensure_probe_all`
  runs at server startup. Fixes "don't see working vs waiting". Pure logic unit-tested
  (`settings_with_probe_*`); the actual hook firing needs sbx (host). NOTE: a running box only picks
  up the hooks on its *next* session start — works for newly-created boxes. With this, the registry's
  last job (turn-state) is covered by skein, so `sandboxes.json` can be dropped from skein's reads
  (left as fallback for now). Caveat: `setup-sandbox.sh --sync` would overwrite the store
  `settings.json`; skein re-adds on next startup (idempotent).
- **Enumerate fleet from sbx. ✅ DONE** (pulled ahead, per "remove the registry we maintain unless it
  has specific data"). `fleet_boxes()` parses the full `sbx ls --json` (name/agent/status/workspaces);
  `load_views` now sources the fleet from sbx ∪ registry, deriving `dir` from workspaces and `branch`
  host-side via git, with the registry kept only for (a) the **turn-state** datum sbx can't give and
  (b) a fallback when sbx is down / a box is direct-mode. `lookup_dir`/`branch_of` gained the same
  sbx/git fallback so diff/PR/session work for sbx-only boxes. A box now appears because sbx knows it,
  not because it registered. Audit conclusion: the *only* registry-specific datum is the agent
  turn-state (`status`); `branch`/`dir`/`diff`/`lastSeen` are all derivable from sbx + git.

  Real `sbx ls --json` schema (pinned in `parse_boxes_real_sbx_schema`):
  `{"sandboxes":[{"name","id","agent","status":"running|stopped","workspaces":[..],"ports":[..]}]}`.

- **Phase 4 — registry-independent read path. ✅ DONE.** Every read path now works with **no
  `sandboxes.json`**: the fleet list from `sbx ls`, `dir` from sbx workspaces, `branch` from host git
  (`branch_of`/`lookup_dir` fallbacks), turn-state from skein's probe (`current_status`), and
  `session_digest` rebuilt to use all of those + sbx liveness instead of requiring a registry entry.
  "Registration" is therefore moot — a box is visible because sbx knows it, not because it registered;
  skein maintains no registry of its own. The registry, if present, is only a transitional fallback.
  REMAINING (the genuine frontier, **host/sbx-dependent — not built blind**): the full *new-repo*
  onboarding "URL + gh auth → fleet" = clone the URL host-side + provision a store from scratch +
  **skein ships its own kit** (today native launch reuses the repo's `dev-sandbox/kit`) + propagate
  the gh token into the box (`sbx secret`). None of this is needed for the current local-repo fleet;
  it should be built with sbx in the loop, not guessed.

- **Phase 5 — repo cleanup. ✅ already satisfied on `master`.** `master`'s `store-template` carries no
  skein-specific hooks (`box-status`/`box-task`/`box-diff`/`box-session` live only on
  `feat/dev-sandbox-tooling`), and skein now owns those signals, so nothing on `master` is redundant.
  The `kit` + `sandbox-bootstrap.sh` stay (the kit links the store; the bootstrap checks out the
  branch from the box name — both still relied on by native launch). The bootstrap's `sandboxes.json`
  write is now unused by skein but harmless; left in place. Only leftover: drop the redundant
  `box-*.sh` from `feat/dev-sandbox-tooling` if/when that branch is reconciled.
- **Phase 5 — repo cleanup + branch reconcile + docs.**

## New-repo onboarding — design (the Phase 4 frontier)

> Goal: `skein add <url-or-path>` (+ a `gh` login) → a launchable, observable fleet for that repo,
> with the repo shipping **nothing** for skein. Today skein is implicitly single-repo (one
> `SKEIN_REPO`/`SKEIN_KIT`/`SKEIN_STORE` triple) and reuses the *repo's* `dev-sandbox/kit`. Both
> assumptions have to go.

### The model: a skein-owned repo registry

Replace the single-repo env triple with `~/.skein/repos.json` (skein's own config — distinct from the
per-box `sandboxes.json` we already dropped). One entry per repo:

```json
{ "id": "thing", "source": "https://github.com/acme/gadget-demo.git",
  "work": "~/.skein/repos/thing/work", "store": "~/.skein/repos/thing/store/.claude",
  "agent": "claude" }
```

skein reads this to (a) enumerate repos, (b) resolve a box → its repo, (c) scope launch/diff/store.
A box already maps to its repo via the `sbx ls` workspace path, so the fleet view groups for free.
Box names become `<repo>-<branch>` (the repo id is the prefix). The branch is passed to the box
**via an env var on `sbx run`** (`SKEIN_BRANCH`), not derived by stripping the name — robust for any
repo id / branch and frees us from the bootstrap's `thing-` assumption.

### What `skein add` produces, per repo

1. **A host working clone** — URL → `git clone` into `~/.skein/repos/<id>/work`; local path → use in
   place. Needed for host-side diff/branch and as the clone source.
2. **A shared store from scratch** — `ensure_store(repo)` creates `…/store/.claude` with `mailbox/`,
   `status/`, `tasks/` and runs `ensure_probe()` against it. No repo store-template required.
3. **skein's own kit** — see below. Shared across all repos, not per-repo.
4. **gh auth wired** — clone private URLs with the host token; seed each box so the agent can push.

### Keystone: skein ships its own kit (`ensure_kit()`)

Embed the kit (`spec.yaml` + a startup script) in the binary via `include_str!` exactly like the
probe, and write it to `~/.skein/kit/`. Native launch passes `--kit ~/.skein/kit` instead of the
repo's. The startup script — **brace-free**, because the sbx kit resolver rejects `${...}` it doesn't
recognise (only `WORKDIR`); use `printenv` / unbraced reads — does three things:

1. symlink the mounted store into `<clone>/.claude`,
2. `git checkout "$(printenv SKEIN_BRANCH)"`,
3. **start the agent inside a named tmux session**: `tmux new-session -A -s skein claude`.

Step 3 is also the **reconnect fix**: the live agent runs in a tmux session *from birth*, so a
reconnect re-attaches to the exact same PTY (partial command intact) instead of `claude --continue`
spawning a parallel session. Requires `tmux` in the box image — the kit `apt-get install`s it if
absent (or we document it as an image requirement).

### Attach / shell via tmux (the reconnect fix, host side)

With the kit running the agent in tmux, the host commands become:

- attach: `sbx exec -it <box> tmux new-session -A -s skein` (re-attach the live agent session)
- shell:  `sbx exec -it <box> tmux new-session -A -s skein-shell` (a separate persistent terminal)

`-A` = attach-if-exists-else-create, so it's safe before the first launch completes. Both stay
overridable via `$SKEIN_ATTACH_CMD` / `$SKEIN_SHELL_CMD`. **Note:** flipping these defaults only
helps once the kit starts the agent in tmux — until then a tmux attach would create a *second*
claude (the same bug). So attach/shell change lands together with the kit, not before.

### gh auth propagation

- Host clone: use `gh auth token` over HTTPS (the proxy/credential helper injects it).
- In the box: `sbx secret set <box> github -t "$(gh auth token)"` on launch so the agent can
  fetch/push and open PRs. (`sbx secret` is the documented per-sandbox secret path.)

**That injection is not only the host's, and left on the proxy it is why the credential alone is not
a boundary — SKEIN-548.** Measured from inside a live box on 2026-09-06/07 and again 2026-09-15:
inside the sandbox the proxy answers a request carrying *no* credential as the account, and overrides
a wrong one. So on the proxy, `sbx secret set` decides what a box **holds**, not what it can
**reach** — which is why a scoped box is routed **direct** for GitHub (`NO_PROXY`), so its tools
present the token skein placed and GitHub enforces it. The account-token bullets above are the
`fleet` mode, which keeps the proxy on purpose. See `docs/architecture.md` §9.6.

### Recommended sequencing (each a host checkpoint — all sbx-dependent, untestable in this sandbox)

- **A. Kit + tmux** — `ensure_kit()`, native launch → skein's kit, agent in tmux, attach/shell →
  tmux. Fixes reconnect **and** removes the repo-kit dependency. *Do first.*
- **B. Store-from-scratch** — `ensure_store()` so a repo with no `.claude` works.
- **C. `skein add` (local path first)** — `repos.json` + repo-scoped resolution + multi-repo fleet
  view (group rows by repo).
- **D. URL clone + gh auth** — the full "paste a URL, log in, go" onboarding.

### ✅ BUILT (A→D in one pass, 2026-06-29) — pending host verification

All four landed together (Opus, one pass). Code map:

- **Repo registry** — `Repo {id,source,work,store,agent}`, `~/.skein/repos.json`
  (`$SKEIN_HOME`-relative), `load_repos`/`save_repos`/`repo_for_box` (longest-id-prefix match)/
  `branch_from_box`. `BoxView.repo` carries the id; `load_views` fills it and uses it as a branch
  fallback. Cockpit shows a per-row repo tag only when >1 repo is managed (the attention-inbox
  grouping stays the primary axis — repo is a tag, not a regrouping).
- **`ensure_kit`** — embeds `src/kit/spec.yaml` (`include_str!`), writes `~/.skein/kit/spec.yaml`.
  The kit's `skein-startup.sh` (runs before the agent) finds the store by scanning
  `/proc/self/mountinfo` for the `skein/launch/<vmid>.json` marker — **env-free**, since `sbx run`
  has no `--env` — then `git checkout`s the recorded branch and links the store into the clone, and
  installs tmux (for the shell tab) if missing + allowed. brace-free reads only (the kit resolver
  rejects unknown `${...}`).
- **`ensure_store`** — provisions `mailbox/ status/ tasks/ skein/launch/ skein/bin/` + runs the probe.
- **Launch** — `repo_launch_command_as` builds `sbx create --clone --kit ~/.skein/kit --name
  <id>-<branch> <agent> <work> <store>`, then `sbx exec`s the agent into `skein-agent` tmux, writing
  `<store>/skein/launch/<box>.json` first. The agent positional is a **registered sbx agent name**
  (claude/codex/…; each selects its image, so it can't be a path/wrapper).
  `native_launch_command` routes repo-managed boxes here; the old `$SKEIN_REPO/$SKEIN_KIT` env path
  stays as the fallback.
- **Branch slugging** — `feat/auth` can't be a box name (sbx rejects `/`), so the *name* is a slug
  (`<repo>-feat-auth`) while the **real** branch (`feat/auth`) rides in the launch spec and is what
  the kit checks out. `slug`/`box_name` in lib; mirrored in the cockpit JS.
- **Reconnect** — creation starts the provider inside `skein-agent` tmux via `sbx exec`; every UI or
  CLI reconnect attaches to that exact live process. The provider's native resume command is used
  only if the tmux process no longer exists, avoiding parallel/forked sessions during ordinary reloads.
  `shell_argv` = `sbx exec -it <box> bash -lc '… tmux new-session -A -s skein-shell || bash -li'`
  (persistent shell tab; managed-box creation requires tmux).
- **Cross-runtime replacement** — takeover never installs a second CLI into the source image. Skein
  exports an immutable Git bundle, separate staged/unstaged patches, untracked archive, shared
  memory/skills/hooks backup, and bounded native transcript Markdown into
  `<store>/skein/handoff-snapshots/`. A new target-runtime box restores that snapshot before its mandatory
  `skein-agent` tmux starts. The old box and native transcript remain intact as rollback.
- **gh auth** — **gone.** Skein used to run `sbx secret set -g github -t "$(gh auth token)"` once,
  globally, so every box could push as the account. Both halves were the host's and the store was
  the *machine's* rather than the fleet's, so two fleets on one host shared one token; architecture
  §13a deletes it. `repos::gh_secret_seeded` still reads the marker a pre-deletion seeding left on
  the volume, which is how `gitgate::box_credential` and the first-run checklist know whether an
  unscoped box actually holds a credential. Nothing writes it.
- **SSH auth** — sbx forwards the host ssh-agent into boxes (key stays on host). A person runs
  `ssh-add` on the host. skein takes no key path, because in the fleet it cannot read a key file
  on the host (SKEIN-947). `ssh_remote_warning` flags an SSH `origin` at add-time, with the
  HTTPS-switch command.
- **Settings** — `~/.skein/config.json` (`Config`): whether boxes push as the account, default
  agent, base branch, confirm_destroy. `GET/POST /api/settings`; cockpit Settings modal;
  each has a `$SKEIN_*` env override.
- **Surfaces** — CLI `skein add <url|path> [--id] [--agent]` + `skein repos`; HTTP `GET/POST
  /api/repos`, `/api/settings`; cockpit "Add a repo…" + "Settings…" palette, repo selector in the
  new-box dialog, per-row repo tag when >1 repo. Server startup: `ensure_probe_all` + `ensure_kit`.
  `skein doctor` reports repos/kit/settings/ssh-agent + host
  notes.

**Confirmed against the host this round (was: open assumptions):**
- `sbx create` can provision the selected registered agent image without attaching; Skein then uses
  `sbx exec` for its own persistent tmux process rather than asking `sbx run` to fork an agent session.
- gh secret `-f` overwrites; "already exists" without `-f` is benign.
- SSH works inside boxes via host **agent forwarding** (per the sbx Credentials docs) — no in-box key
  needed; skein just loads the key into the host agent.

**Still needs host verification:**
1. **Required tool install** — the kit installs jq + tmux and fails creation if either remains absent.
2. **Mount visibility** — kit assumes the store mount shows in `/proc/self/mountinfo` and the clone is
   `WORKSPACE_DIR` (both hold for the thing kit); sibling-path + `$SKEIN_STORE` fallbacks exist.
3. **`--kit` accepts a directory** of this shape, and `sbx create --clone --kit … <agent> <work> <store>`
   mounts `<store>` at its host path so the marker scan finds it.

## Open items / risks

- **`sbx ls` output format** is unconfirmed (no `sbx` or docs in the build sandbox). Phase 1 needs it.
  Plan: prefer a JSON/quiet format if `sbx ls` offers one, parse defensively, make the command
  overridable (`SKEIN_LS_CMD`), and degrade to `lastSeen` on any parse/exec failure. Confirm the
  exact format on the host before finalizing the parser.
- **No `sbx` in the build sandbox** → launch/inject/liveness can't be end-to-end tested here; cover
  the pure logic (command construction, settings merge, `ls` parse) with unit tests and verify the
  live paths on the Mac host.
- **`settings.json` additive merge** must never drop the project's existing hooks → jq-style deep
  merge with explicit tests.
- **Provisioning seam** — a bare clone has no deps. Default UX = agent installs on first run; honor an
  optional repo `.skein/setup` / `--kit`. Never required.
