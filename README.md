# skein

> See and steer your fleet of agent sandboxes. A thin control surface over `sbx`
> microVM boxes + the shared `.claude` store — the layer Conductor-likes have but
> that's missing from the sbx workflow. *Compose, don't reinvent* — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md); the why is in [`VISION.md`](VISION.md).

**Status:** v0 — a live **web cockpit** (fleet board over SSE) with an **embedded
per-box terminal** (click a box → talk to that agent in the browser) + a CLI. The goal
is the web UI as the *single pane of glass*; web actions (launch/diff/merge/stop/destroy) and
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

A self-contained dark page (no build step) that live-updates over SSE. The fleet is an **attention
inbox**: boxes sort "who needs you first" (a decision-blocked box, then a turn that ended on a
question, then work in flight), and each row carries a one-line **headline** — the prompt it's
blocked on, or the gist of its last message — with a chip from the **fork-detector**
(`decision` / `asks` / `proceed?`) so a real fork stands out from a rote "shall I proceed?". Click any
box to open its **embedded terminal** (xterm.js ↔ a server-side PTY running `sbx run --name <box>`),
its **diff**, or a **Session** digest — "what happened here" assembled for free from the branch's
commits, the agent's `.skein/journal.md`, and its last message, so you can catch up without reading the
scrollback. No model tokens are spent building any of this. Each box also gets a second **Shell** tab
(`sbx exec -it <box> /bin/bash`) for running commands yourself, and **pasting an image** into a terminal
uploads it into the box (the agent can't see your clipboard — it runs in the microVM) and types the
in-box path in for the agent to read.

The signals come from each box's Claude Code hooks (`Stop` / `Notification` → `box-status.sh`,
`box-diff.sh`, `box-session.sh`; `PostToolUse` on `TodoWrite` → `box-task.sh`) writing the shared
store; skein only reads and ranks them.

More attention helpers, all free unless noted:
- **One-click continue** — boxes paused on a trivial "shall I proceed?" get a `proceed?` chip; **▸ Continue N**
  resumes them all in one gesture (headless `claude --continue`, fire-and-forget). Never silent — always
  your click — and a real decision or a permission prompt is never auto-resumed.
- **Peripheral preview** — every row shows what its box is *doing right now* — the in-progress TodoWrite
  item the agent reports (`box-task.sh`), or its journal's `next …` line as a free fallback — so you can
  see what the other tabs are working on without switching to them. The same signal upgrades a tier-0
  `needs-input` row, whose raw notification text is just a generic "waiting for your input", to say what
  the box was actually working on.
- **Away digest** — step away and come back and skein shows "while you were away": who now needs you, who
  finished, who made progress.
- **Collision radar** — ⚠ flags files that two or more boxes have both changed, to reconcile before merge.
- **AI enrichment (opt-in, `SKEIN_AI=on`)** — skein runs inside an `sbx run` box where `claude` is logged in,
  so it can spend *rationed* Haiku calls on the subscription: a one-line digest for boxes with no journal,
  and a conservative safety gate on **Continue N** (a box is held back unless the model says it's routine).
  Off by default because it shares the fleet's rate-limit window; lazy and cached when on.

API: `GET /api/boxes`, `GET /api/events` (SSE), `GET /api/boxes/:name/diff`,
`GET /api/boxes/:name/session`, `GET /api/boxes/:name/narrate`, `GET /api/collisions`,
`POST /api/boxes/:name/resume`, `POST /api/resume-batch`, `POST /api/boxes/:name/stop` (sbx stop),
`POST /api/boxes/:name/destroy` (sbx rm), `GET /api/boxes/:name/terminal` (WebSocket).
xterm.js is vendored into the binary (served from `/vendor/`), so the terminal works with no CDN —
important in the firewalled sbx network.

> Loopback-only by default (`127.0.0.1:7878`); set `$SKEIN_ADDR` to change the bind.
> The terminal WebSocket rejects unexpected `Origin`s (drive-by / DNS-rebinding guard) — it
> allows loopback, `*.ts.net`, and `$SKEIN_ALLOWED_ORIGINS`. For remote/mobile use
> `tailscale serve` (below) rather than exposing the port directly.

### Remote access (Tailscale)

Keep skein bound to loopback and let Tailscale carry the tailnet → loopback hop. The tailnet
is the auth boundary: only your WireGuard-authenticated devices can reach it, with no public
surface — so no app-level token is needed.

```sh
# on the host running skein-server:
tailscale serve --bg 7878          # serve https://<machine>.<tailnet>.ts.net → 127.0.0.1:7878
```

Open `https://<machine>.<tailnet>.ts.net` from any device in the tailnet (incl. the phone via
the Tailscale app). The `.ts.net` origin is allowed by the WebSocket guard automatically; for a
different reverse proxy, list its host in `$SKEIN_ALLOWED_ORIGINS`.

**On a phone:** the layout goes fullscreen per box; an on-screen key bar supplies the keys a soft
keyboard lacks (`esc`, `tab`, a sticky `ctrl`, `^C`, arrows), the terminal resizes to stay above
the keyboard, and **‹ boxes** returns to the fleet without ending the session.

- **Shared/team tailnet:** restrict *which* users/devices can reach the port with a Tailscale
  **ACL** — that's the access control.
- **`tailscale funnel` (public internet):** removes the tailnet boundary, so don't use it for
  the terminal without adding an app-level auth token first (not currently implemented).

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

State prefers the explicit status a box's hooks report (`needs-input` / `waiting` / `working` /
`done`); with no report it falls back to `live` (<2m) / `idle` (<30m) / `stale` from `lastSeen`.

## Registry resolution (first match wins)

1. `$SKEIN_REGISTRY` — full path to `sandboxes.json`
2. `$SKEIN_SHARED/sandboxes.json`
3. `<git-toplevel>/../skein-shared/.claude/sandboxes.json`

## Configuration

`skein doctor` reports the resolved registry, bind address, and whether `sbx`/`git`/`gh`
are present — run it first if something looks off. All knobs are environment variables — set
them inline, or drop them in a **`.env`** (loaded automatically at startup from the cwd upward;
real env vars still win). Copy [`.env.example`](.env.example) to `.env` and you can just run
`skein` / `skein-server` with no prefix:

| var | what | default |
|-----|------|---------|
| `SKEIN_REGISTRY` | full path to `sandboxes.json` | (see resolution above) |
| `SKEIN_SHARED` | shared store dir (`/sandboxes.json` appended) | — |
| `SKEIN_ADDR` | server bind address | `127.0.0.1:7878` |
| `SKEIN_ALLOWED_ORIGINS` | extra WS origins to allow (comma-sep hosts); loopback + `*.ts.net` always allowed | — |
| `SKEIN_SELF` | this box's vmid (kept `live` when its `lastSeen` is quiet) | `$SANDBOX_VM_ID` |
| `SKEIN_REPO` | dir to run `git`/`gh` in (PRs, checks, host-side diffs) **and to launch/attach from** — so relative `*_CMD` paths resolve here | cwd |
| `SKEIN_BASE` | base branch for `gh pr create` / merge | repo default |
| `SKEIN_LAUNCH_CMD` | launch-a-box template — `{branch}` substituted; relative to `$SKEIN_REPO` (e.g. `dev-sandbox/setup-sandbox.sh {branch}`) | `setup-sandbox.sh <branch>` |
| `SKEIN_ATTACH_CMD` | agent-terminal attach — `{name}`/`{dir}` substituted | `sbx run --name {name} -- --continue` |
| `SKEIN_SHELL_CMD` | shell-terminal command (the **Shell** tab) — `{name}`/`{dir}` substituted | `sbx exec -it {name} /bin/bash` |
| `SKEIN_PR_CMD` | open-PR template — `{branch}`/`{name}` substituted | `gh pr create --head <branch> --fill` |
| `SKEIN_STOP_CMD` | **Stop** — `{name}` substituted; halts the sandbox to free compute (resume via attach). Non-destructive | `sbx stop {name}` |
| `SKEIN_DESTROY_CMD` | **Destroy** — `{name}` substituted; kills & removes the sandbox (clone mode: unpushed commits lost). Legacy fallback `SKEIN_ARCHIVE_CMD` | `sbx rm -f {name}` |
| `SKEIN_RESUME_CMD` | one-click "continue" template — `{name}`/`{prompt}` substituted | `sbx run --name {name} -- --continue --print {prompt}` |
| `SKEIN_AI` | opt into rationed Haiku enrichment (narrator + Continue safety gate) | off |
| `SKEIN_AI_MODEL` | model for AI calls when `SKEIN_AI` is on | `claude-haiku-4-5` |
| `SKEIN_CLAUDE_BIN` | path to the `claude` CLI (for AI calls) | `claude` |

> The `*_CMD` templates run via `sh -c`; values you substitute are shell-quoted, but only
> point them at trusted commands.

## How it fits the sbx setup

skein **reads** the shared store the sandboxes already maintain (`sandboxes.json`,
`mailbox/`) and **drives** `sbx` / `git` / `gh`. It owns no state the bootstrap owns,
and degrades gracefully when `sbx` isn't on PATH (e.g. read-only `ls` from anywhere
with `$SKEIN_REGISTRY` set).
