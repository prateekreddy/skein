# skein

> See and steer your fleet of agent sandboxes. A thin control surface over `sbx`
> microVM boxes + the shared `.claude` store — the layer Conductor-likes have but
> that's missing from the sbx workflow. *Compose, don't reinvent* — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md); the why is in [`VISION.md`](VISION.md).

**Status:** v0 — a live **web cockpit** (read-only fleet board over SSE) + a CLI. The
goal is the web UI as the *single pane of glass*; web actions and an embedded per-box
terminal are next (ARCHITECTURE.md § Roadmap).

## Build

```sh
cargo build --release        # → target/release/{skein, skein-server}
```

## Web cockpit (the primary surface)

```sh
SKEIN_REGISTRY=<…>/skein-shared/.claude/sandboxes.json \
  ./target/release/skein-server          # → http://127.0.0.1:7878
```

A self-contained dark page (no build step) that live-updates over SSE. Today it shows the
fleet; Phase 1 adds launch / diff / merge / archive and an embedded terminal so you talk
to each agent in the browser. API: `GET /api/boxes`, `GET /api/events` (SSE).

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
