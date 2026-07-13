# Skein — architecture

## Topology

```
HOST (your Mac)
├─ skein            ← this tool: TUI/CLI control surface (+ future daemon)
├─ sbx              ← isolation: one microVM per box (`--clone`)
└─ skein-shared/.claude/      ← the shared store, bind-mounted into every box
     ├─ sandboxes.json         registry (who's running, branch, lastSeen, status)
     ├─ mailbox/               cross-box messages
     └─ memory/ skills/ …      the shared brain

BOX A (microVM)   BOX B (microVM)   …      ← each a separate kernel; all mount the shared store
```

Skein runs on the **host**. It reads the shared store and drives `sbx` + `git` + `gh`. It does
**not** run inside the boxes and does **not** own state the bootstrap already owns.

## The hard constraint (why some designs are off the table)

The boxes are **separate microVM kernels** that merely bind-mount one host directory. That rules out
two tempting shortcuts:

- **No SQLite shared *across boxes* over the mount.** SQLite's WAL/locking assumes one host with
  shared memory; across VM kernels it corrupts. So the *cross-box* comms layer stays **file-based**
  (the current `mailbox.sh` / `sandboxes.json`), which is safe precisely because it avoids this.
- **No in-repo shared store.** An sbx mount lands at its absolute host path with no remap, so an
  in-repo store would collide with the `git clone` target. Hence the sibling location.

These aren't preferences; they're forced by the runtime. Skein is designed around them.

## What we reuse vs. what Skein adds

> Principle: *compose, don't reinvent.* Skein writes only the sbx-specific glue that no existing
> tool has. Everything else is an existing wheel.

| Concern | Reused (existing wheel) | Skein adds |
|---|---|---|
| Per-agent isolation | **sbx** (`--clone` microVMs) | nothing — calls the CLI |
| Shared memory / skills / hooks | **existing shared `.claude` store** | nothing — reads it |
| Cross-box messaging (now) | **existing `mailbox.sh`** (files) | a read/compose view |
| Registry of boxes | **existing `sandboxes.json`** (bootstrap-written) | reads + renders + reacts |
| Collecting each box's work | **git** (`git fetch sandbox-<box>`) + **gh** (PRs) | the per-box review/merge flow |
| TUI rendering | **ratatui + crossterm** | the fleet layout/keymap |
| Event bus + durable job queue (host) | **honker** (Rust-native crate) | job definitions + handlers |
| JSON / time parsing | **serde / serde_json / chrono** | — |

If a capability already lives in one of those, Skein calls it; it does not grow a second copy.

## Where honker fits (and doesn't)

- **Yes — the host-side orchestrator.** The daemon ⇄ TUI ⇄ (future) web ⇄ CLI are several processes
  on **one machine** — honker's sweet spot. It backs: a live **event bus** (box status changes →
  redraw), a durable **job queue** (launch / fetch-diff / merge / archive, with retries), simple
  **scheduling** and **named locks**. honker is Rust-native, so it's a direct crate dependency, not a
  binding. This replaces hand-rolling a queue/bus.
- **No — the cross-box mailbox.** See the hard constraint above: SQLite over the shared mount across
  microVMs is unsafe. Keep files there for now.
- **Future, if we want real-time cross-box comms:** route boxes → host over HTTP
  (`host.docker.internal`) into the host's honker DB. That makes honker the single source of truth
  for comms **without** SQLite-over-mount. A later phase, not v0.

## Data model

The registry (`sandboxes.json`) is the contract, written by `sandbox-bootstrap.sh`:

```json
{ "<box>": { "started": "RFC3339", "branch": "…", "dir": "…", "lastSeen": "RFC3339",
             "status": "working|waiting|done|…  (optional)" } }
```

- **`status`** is the one field Skein wants that doesn't exist yet. Today Skein *derives* a coarse
  state from `lastSeen` (live <2m, idle <30m, else stale). **Phase 2** adds a Claude `Stop`/
  `Notification` hook in the shared `settings.json` that writes `working|waiting|done` into the
  registry — turning the list into a true "who needs me?" board. (Registry writes use the existing
  `flock` discipline in the bootstrap.)

## Surface decision: web-first

We lead with a **web cockpit**, not a TUI. Reasons: diffs review far better in a browser; remote/
mobile is a tunnel away; the team can just open a URL; and it **reuses our existing stack** (axum +
Svelte + design sensibility) whereas ratatui is a stack we use nowhere else. The architecture is
**server + thin clients**, so the CLI stays and a TUI could later be another client of the same API —
choosing web closes nothing. Aesthetic is a dark developer cockpit (Linear/Vercel/Raycast), distinct
from thing's light Fluent product chrome.

> **Remote access note (checked):** Claude's *own* Remote Control is outbound **HTTPS:443 (TLS),
> not UDP** — it works through the sbx proxy (we allowed `api.anthropic.com`); its only gotcha is it
> needs claude.ai OAuth, not an API key. But it's **orthogonal to Skein-web**: Skein-web is reached
> by tunnelling *our own* server port (Tailscale/cloudflared), independent of Anthropic.

## Roadmap

- **v0 (now):** `skein` CLI (`ls` / `attach`) **+ `skein-server`** (axum) — JSON API, a live **SSE**
  fleet stream, and a self-contained **dark web board** (no build step). Read-only + live.
- **Phase 1 — web actions + embedded session + ⌘K (the single-pane milestone):** endpoints for
  launch / diff / merge / archive + a command palette; **attach opens a terminal *in the browser***
  (xterm.js over `sbx exec` into a persistent provider-specific tmux session), so you converse with each agent
  without leaving the page; **Svelte** for the syntax-highlighted diff viewer. After this, the browser
  is the only surface a dev touches day-to-day.
- **Phase 2 — real status:** a Claude `Stop`/`Notification` hook writes `status` into the registry;
  the board shows waiting / working / done instead of inferring from `lastSeen`.
- **Phase 3 — honker job queue + push:** durable launch/merge/archive jobs and an event bus; replace
  the SSE *polling* with a honker subscription (push). Optional: `coder/agentapi` per box for steering.
- **Phase 4 — remote/mobile:** tunnel the server + auth + browser push, so the fleet is on your phone.

Each phase is additive and keeps the reuse rule: pull a wheel before writing one.
