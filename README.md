# skein

> See and steer your fleet of agent sandboxes. A thin control surface over `sbx`
> microVM boxes + the shared `.claude` store — the layer Conductor-likes have but
> that's missing from the sbx workflow. *Compose, don't reinvent* — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md); the why is in [`VISION.md`](VISION.md).

**Status:** v0 — a live **web cockpit** (fleet board over SSE) with an **embedded
per-box terminal** (click a box → talk to that agent in the browser) + a CLI. The goal
is the web UI as the *single pane of glass*; web actions (launch/diff/merge/archive) and
a ⌘K palette are next (ARCHITECTURE.md § Roadmap).

## Build

```sh
cargo build --release        # → target/release/{skein, skein-server}
```

## Web cockpit (the primary surface)

```sh
SKEIN_REGISTRY=<…>/skein-shared/.claude/sandboxes.json \
  ./target/release/skein-server          # → http://127.0.0.1:7878
```

A self-contained dark page (no build step) that live-updates over SSE. Click any box to
open its **embedded terminal** (xterm.js ↔ a server-side PTY running `sbx run --name
<box>`) and drive that agent without leaving the page. Next: launch / diff / merge /
archive actions and a ⌘K palette. API: `GET /api/boxes`, `GET /api/events` (SSE),
`GET /api/boxes/:name/terminal` (WebSocket). xterm.js is vendored into the binary
(served from `/vendor/`), so the terminal works with no CDN — important in the
firewalled sbx network.

> Localhost-only by default (`127.0.0.1:7878`); set `$SKEIN_ADDR` to change the bind.
> The terminal WebSocket rejects non-loopback `Origin`s (drive-by / DNS-rebinding guard).
> Remote/mobile access still needs a tunnel **and an auth token** (not yet implemented) —
> roadmap Phase 4; don't expose this port directly until then.

## CLI (terminal client, same core)

```sh
skein                 # = skein ls — the fleet, live boxes first
skein attach <box>    # reconnect (runs: sbx run --name <box>)
skein version · help
```

Example:

```
  BOX            STATE  BRANCH                    SEEN    DIR
● my-feature     live   feat/my-feature           12s ago ~/work/.../gadget-demo
● bugfix-login   idle   fix/login                 8m ago  ~/work/.../gadget-demo
○ thing-export  stale  export                    9h ago  ~/work/.../gadget-demo
```

State is `live` (<2m) / `idle` (<30m) / `stale` from `lastSeen`, until the Phase-2
status hook reports `working|waiting|done` explicitly.

## Registry resolution (first match wins)

1. `$SKEIN_REGISTRY` — full path to `sandboxes.json`
2. `$SKEIN_SHARED/sandboxes.json`
3. `<git-toplevel>/../skein-shared/.claude/sandboxes.json`

## How it fits the sbx setup

skein **reads** the shared store the sandboxes already maintain (`sandboxes.json`,
`mailbox/`) and **drives** `sbx` / `git` / `gh`. It owns no state the bootstrap owns,
and degrades gracefully when `sbx` isn't on PATH (e.g. read-only `ls` from anywhere
with `$SKEIN_REGISTRY` set).
