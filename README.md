# skein

> See and steer your fleet of agent sandboxes. A thin control surface over `sbx`
> microVM boxes + the shared `.claude` store — the layer Conductor-likes have but
> that's missing from the sbx workflow. *Compose, don't reinvent* — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md); the why is in [`VISION.md`](VISION.md).

**Status:** v0 bootstrap — a status view + attach. The live TUI, real status, and
review/merge are on the roadmap (ARCHITECTURE.md § Roadmap).

## Build

```sh
cargo build --release        # → target/release/skein
# optional: install onto PATH
cargo install --path .
```

## Use

```sh
skein                 # = skein ls — the fleet, live boxes first
skein ls
skein attach <box>    # reconnect (runs: sbx run --name <box>)
skein version
skein help
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
