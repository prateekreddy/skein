# skein as a self-sufficient product — architecture & plan

> Status: PROPOSAL (awaiting approval). Goal: a user gives skein a **repo URL** and **GitHub
> auth**, and gets a working, observable, steerable fleet — with the repo implementing *nothing*
> for skein's sake. Works toward repo-agnostic *and* agent-agnostic (Claude now, Codex later).

## Guiding principle

A box is just **an `sbx --clone` sandbox running an agent**. skein makes it legible and steerable
using only the host's `sbx` / `git` / `gh` and its own injected probe. If a capability requires the
*repo* to ship a script or hook for skein to function, that's a coupling bug — pull it into skein.

## Signal ownership (the boundary)

| signal / capability        | who it's for        | how skein gets it (target)                              | owner   |
|----------------------------|---------------------|---------------------------------------------------------|---------|
| live / stopped / stale     | cockpit             | **`sbx ls`** (running state), not hook `lastSeen`        | skein   |
| which boxes exist          | cockpit             | enumerate from `sbx ls`; branch/dir via host `git`      | skein   |
| diff (badge + full)        | cockpit             | host-side `git` — **already done** (`host_diffstat`)    | skein   |
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
sbx run --clone --kit "$kit" --name "thing-$1" claude . "$shared"
        └clone┘ └─project──┘ └──skein owns───┘ └agent┘   └skein owns┘
```

- **Launch** (`sbx run --clone …`) → skein constructs and runs it.
- **Shared store** (`$shared`) → skein provisions the minimal skein-needed store itself
  (empty `sandboxes.json`, `mailbox/`, skein's own hooks). Repo's project store-template layers on
  top *only if present*.
- **`claude`** is the sbx **agent** positional → this is the per-agent seam (`claude` | `codex`).
- **`--kit`** (dep/tool provisioning) and `.env` seeding stay the repo's, optional.

## Components to build in skein

1. **Launcher** — `sbx run --clone --name <box> [--kit <opt>] <agent> . <store>`, from
   `{repo (url|path), branch, agent}`. Replaces `SKEIN_LAUNCH_CMD` → `setup-sandbox.sh`.
2. **Store provisioner** — create the sibling `.claude` store with registry + mailbox + skein hooks;
   detect & apply an optional repo store-template/kit.
3. **Liveness from sbx** — `fleet_running()` parses `sbx ls`; `state()` uses running state for
   live/stopped, falling back to `lastSeen` only if `sbx` is unavailable.
4. **Turn-state adapter (Claude)** — skein owns `box-status.sh` + `box-task.sh` (embedded via
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
- **Phase 2 — skein-owned launch + minimal store provisioning.** skein constructs `sbx run` and
  provisions the store; repo no longer needs `setup-sandbox.sh` for skein.
- **Phase 3 — Claude turn-state adapter.** Embed + inject `box-status.sh`/`box-task.sh`, additive
  settings merge. Fixes "don't see working even when it is."
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

- **Phase 4 — registration + auth + URL clone.** Delivers "repo URL + gh auth → fleet". With
  enumeration done, the remaining registry role is the turn-state datum; once Phase 3's skein-owned
  probe writes that to a skein file, `sandboxes.json` can be dropped from skein's read path entirely.
  (`cmd_ls` in the CLI still lists from the registry — minor follow-up to share `load_views`'s source.)
- **Phase 5 — repo cleanup + branch reconcile + docs.**

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
