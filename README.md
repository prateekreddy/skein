# skein

> See and steer your fleet of agent sandboxes. A thin control surface over `sbx`
> microVM boxes + a mounted shared store — the layer Conductor-likes have but
> that's missing from the sbx workflow. *Compose, don't reinvent* — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md); the why is in [`VISION.md`](VISION.md).

**Status:** v0 — a live **web cockpit** (fleet board over SSE) with an **embedded
per-box terminal** (click a box → talk to that agent in the browser) + a CLI. The goal
is the web UI as the *single pane of glass*; web actions (launch/diff/merge/stop/destroy) and
a ⌘K palette are next (ARCHITECTURE.md § Roadmap).

## Build

```sh
cargo build --release        # → target/release/{skein, skein-server}
cargo test                   # units + a black-box run of the real server (tests/server.rs)
node tests/ui/smoke.mjs      # the cockpit in a browser — run it after touching src/web/index.html
```

`cargo test` proves the API is right; the browser smoke test proves the *page* is right, which is
not the same thing. It launches the real binary against a throwaway workspace and clicks through the
tabs, asserting what is **visible** rather than what merely exists in the DOM — the Files tab once
shipped with every folder rendered and then hidden by an unrelated CSS rule, invisible to every
other check. One-time setup in `tests/ui/README.md`.

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
box to open its **embedded terminal** (xterm.js ↔ `sbx exec` ↔ a persistent per-runtime tmux session),
its **diff**, or a **Session** digest — "what happened here" assembled for free from the branch's
commits, the agent's `.skein/journal.md`, and its last message, so you can catch up without reading the
scrollback. No model tokens are spent building any of this. Each box also gets a second **Shell** tab
(`sbx exec -it <box> /bin/bash`) for running commands yourself, and **attachments** — paste, drag-and-drop,
or the 📎 button — hand the agent any file or folder: a screenshot, a PDF, a spreadsheet, a video, a whole
sample corpus. The agent can't see your clipboard or your disk (it runs in the microVM), so skein streams
each one into the box under `/tmp/skein-drop-<batch>/` (one directory per drop, so a dropped folder keeps
its structure) and pastes the in-box path — the folder's path for a folder — into the prompt for the agent
to open. Streamed, not buffered, so a large video costs the host no memory. Open sessions stay live as
**tabs** in the dock: drag them into the order you want (it survives a reload), or move between them with
`⌥1`–`⌥9` / `⌥[` `⌥]` — chords that work while you are typing in the agent, since the browser reserves
`⌘1`–`⌘9` and the agents use `⌥←`/`⌥→` for word movement. Press `?` for the full key list (it lives in
**Settings → Shortcuts**, rendered from the same table the app binds, so it can't drift).

Turn state has two halves. **Edges** are the runtime's lifecycle hooks — fast, but no runtime fires
anything when you answer a permission prompt, dismiss a dialog, interrupt a turn, or when the agent
dies, so a state nobody clears used to be shown forever. **Level** is what the box's screen says right
now: a `nice`d in-box observer samples the agent's tmux pane (0.11% of one core) and the host reads it
back, so a chip clears itself within a second or two of you answering — and states no hook can report
(a trust prompt before any session exists, an expired login, a crashed agent, a dialog dismissed with
esc) become visible at all. A blocking dialog says *which* kind it is: `decision` (a tool wants
approval), `asks` (a question), `trust?`, `sign in`. Both runtimes' screens are read from live
captures — Codex even states its blocked-ness in the terminal title (`[ ! ] Action Required`), which
clears the instant you answer either way. With no observation present, or a screen the grammar does
not recognise, turn state is exactly the edge signal it always was, so nothing regresses — and because
that fallback is otherwise invisible, a box running on hook edges alone says so: a half-filled dot on
its state pill and its tab, and `hooks only` / `screen lost` / `screen unread` in the tab's header,
each explaining what's missing and whether reattaching fixes it.

Signals come through thin runtime adapters writing the same shared contract. Claude maps
`Notification`, `TodoWrite`, and turn lifecycle hooks; Codex maps `PermissionRequest`,
`UserPromptSubmit`, `PostToolUse`, and `Stop`. Both produce the same status, task, session, diff,
telemetry, mailbox, and handoff files; skein only reads and ranks that provider-neutral data.
Codex hooks pass through one Bash/`jq` adapter that converts probe stdout into Codex's required
event-specific JSON response, so the shared probes do not acquire provider branches.

More attention helpers, all free unless noted:
- **One-click continue** — boxes paused on a trivial "shall I proceed?" get a `proceed?` chip; **▸ Continue N**
  resumes them all in one gesture using each box's native runtime, fire-and-forget. Never silent — always
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

Each box also gets a **Files** tab — browse its workspace and read files without leaving skein:
markdown renders (README auto-opens at the root, relative links navigate), images display inline,
everything else shows as text. Served from the box's host-side workspace, path-traversal hardened.

API: `GET /api/{boxes,health,runtimes}`, `GET /api/events` (SSE), `GET /api/boxes/:name/{diff,session,narrate,ship}`,
`GET /api/boxes/:name/files?path=` + `GET /api/boxes/:name/file?path=` (the Files tab),
`GET /api/{collisions,repos,settings,mailbox}`, `POST /api/boxes/:name/{resume,stop,destroy,pr,merge,repin,upload}`,
`POST /api/{resume-batch,repos,settings,mailbox,pick-path}`, `GET /api/boxes/:name/terminal` (WebSocket).
xterm.js and marked.js are vendored into the binary (served from `/vendor/`), so everything works
with no CDN — important in the firewalled sbx network.

> Loopback-only by default (`127.0.0.1:7878`); set `$SKEIN_ADDR` (e.g. `0.0.0.0:7878`) to bind
> off-loopback. The terminal WebSocket rejects unexpected `Origin`s (drive-by / DNS-rebinding
> guard) — it allows loopback, `*.ts.net`, Tailscale IP ranges, and `$SKEIN_ALLOWED_ORIGINS`.
> For remote/mobile you can either `tailscale serve` (below, keeps the loopback bind) or bind
> off-loopback and hit the box's tailnet address directly.

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

**Alternative — bind off-loopback directly.** If you'd rather skip `tailscale serve`, bind the
server to a broader interface with `$SKEIN_ADDR` and reach it at the box's own tailnet address:

```sh
SKEIN_ADDR=0.0.0.0:7878 ./target/release/skein-server   # all interfaces
# or SKEIN_ADDR=<tailnet-ip>:7878 to bind just the tailnet interface
```

Then open `http://<machine>.<tailnet>.ts.net:7878` or `http://<tailnet-ip>:7878`. The origin guard
trusts `*.ts.net` **and** Tailscale IP ranges (CGNAT `100.64.0.0/10`, `fd7a:115c:a1e0::/48`), so the
embedded terminals work over the raw tailnet address with no per-host config — the tailnet stays the
auth boundary. (A non-tailnet LAN/public IP is still rejected; list it in `$SKEIN_ALLOWED_ORIGINS`
if you really mean to expose it there.) `tailscale serve` is still preferred where you can use it —
it keeps the bind on loopback and gives you real HTTPS.

## CLI (terminal client, same core)

```sh
skein                 # = skein ls — the fleet, live boxes first
skein attach <box>    # reattach to the live provider tmux session
skein attach <box> --agent codex --handoff   # Codex takes over a Claude box
skein attach <box> --agent claude --handoff  # Claude takes over a Codex box
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

## Adding repos

skein manages a set of repos itself — you don't wire anything into the repo. Add one by URL
(skein clones it) or by local path (skein adopts it in place):

```
skein add https://github.com/org/app.git      # clones into ~/.skein/repos/app/work
skein add /path/to/checkout                    # adopts an existing local clone
skein repos                                    # list managed repos
```

…or in the cockpit: **⌘K → "Add a repo…"**. Adding a repo provisions a shared `.claude` store
from scratch (mailbox + skein's turn-state probe), installs skein's own sbx kit, and seeds the
host `gh` token into sbx so boxes can push/open PRs. Boxes are then named `<repo>-<branch>`; create
one from the **New box** dialog (which gains a repo selector once you manage more than one).

The registry lives at `~/.skein/repos.json` (override the home with `$SKEIN_HOME`). The kit is
embedded in the binary and written to `~/.skein/kit/` — no repo-side `dev-sandbox/kit` needed. The
agent runs inside a `skein-agent` tmux session, so reconnecting (attach) re-joins the **same** live
terminal instead of spawning a parallel `claude --continue`.

**Branch names with slashes just work.** Type `feat/auth` in the New box dialog: the sbx box is
named with a slug (`<repo>-feat-auth`, since sbx names can't contain `/`) while the box actually
checks out the real `feat/auth` branch.

**Settings** (⌘K → "Settings…", stored in `~/.skein/config.json`): seed/force the gh token,
default agent, base branch for PRs, confirm-before-Destroy, and an SSH key
path. Each matching `$SKEIN_*` env var still overrides the saved value for headless use.

**Git auth inside boxes.** HTTPS remotes push with no setup — the sbx proxy injects GitHub
credentials and skein also seeds the `gh` token. For SSH remotes (`git@…`/`ssh://…`), sbx forwards
your **host SSH agent** into the box (the private key stays on the host); set an SSH key path in
Settings and skein `ssh-add`s it so it's available to forward. `skein add` warns up-front if a repo's
`origin` is SSH so you can switch it to HTTPS or load the key.

## Verify — does the work actually stand up?

The board tells you who needs you. **Verify** tells you whose work compiles. Set a check command in
Settings → Workflow (`cargo test`), or override it per repo on that repo's row, and the ✓ Verify
button in a box's toolbar runs it **inside the box** (`sbx exec`, no model tokens) and keeps the
result: a green or red chip on the fleet row, the command it ran in the tooltip, and the output —
stdout and stderr together, which is where a failing suite says the useful part — one click away.

Two things it deliberately does *not* do. It never runs itself: no schedule, no turn-end trigger, no
"auto-verify" setting, because a check is a real test suite burning cores on your machine and six
boxes doing it at once is six test suites competing with your own work. And it never lets a pass
outlive its code — once the box ends another turn, the chip is struck through, because the result now
describes work that has moved on. One check runs at a time fleet-wide, and a box that is mid-turn
refuses (the check and the agent would be writing the same files).

## Claude and Codex runtimes

Choose the default runtime when adding a repo, override it when launching a box, or use the cockpit's
`↔` action to create a replacement box in another supported runtime. Each box remains single-runtime:
Skein snapshots the source, launches the target runtime's own image, and keeps the source intact as
rollback. This keeps images light and avoids cross-provider authentication inside the wrong image.

Same-provider reconnects reuse the existing `skein-agent` tmux session. If the tmux process no
longer exists, Skein runs the provider's native resume command (`claude --continue` or
`codex resume --last`) inside a new tmux session. No replacement, transcript export, or context
conversion is involved. Immediately before creating a new agent process, Skein runs that runtime's
native updater with a two-minute bound; update failures are reported but never block the installed CLI.

tmux is deliberately invisible: its status bar is disabled, mouse/copy scrolling is enabled, and
pane history is enlarged. Codex is launched with its documented `--no-alt-screen` option so browser
wheel scrolling moves through conversation output instead of cycling prompt history. Claude feeds
its native status-line JSON into Skein's renderer; Codex maps the live `token_count` data behind
`/status` into the same schema. The cockpit shows the same CTX/5H/7D/cost/model footer for both,
refreshing every 30 seconds and omitting unavailable segments. An explicit Codex `/statusline`
choice disables Skein's adapted footer.

Native transcripts are provider-specific and are not converted. A takeover preserves unpushed commits,
the staged and unstaged tree, untracked files, branch, shared memory, skills, and user hooks. It also
injects a durable brief containing the active task, last outcome, journal, diff, changed files, and a
bounded Markdown export of the source conversation. The export provides continuity, but only the source
provider can natively resume its original session.

Skein installs `jq` and mandates `tmux` as the minimal box substrate. Provider-neutral probe scripts,
handoffs, and immutable takeover snapshots live once in the mounted shared store; Skein never installs
both large agent CLIs into every box. `GET /api/health` exposes dependency/hook failures.

## Shared working files

Every managed Claude and Codex box exposes the repo's durable working-data directory at
`/home/agent/shared` (`$HOME/shared`). Its canonical host location is `<repo-store>/shared-home/`,
inside the same project-scoped mount used for memory and mailbox. Writes are visible live in every
box for that repo; concurrent edits use ordinary filesystem/last-writer-wins semantics.

Real `$HOME` remains private to each box. Skein never shares agent/auth state (`.claude`, `.codex`),
credentials (`.ssh`, `.aws`, gh config), caches, toolchains, sockets, or locks. Startup creates or
repairs the `shared` symlink but refuses to replace a real file/directory at that path, and box startup
fails visibly if the canonical mount is unavailable or unwritable.

To rescue durable files from an older box, inventory first (read-only), review, then name every
top-level entry explicitly:

```sh
skein shared import gadget-demo-feat-topic-research
skein shared import gadget-demo-feat-topic-research \
  --include CASE_PREP.md --include reference-documents --apply
```

Hidden state, workspaces/repos, symlinks, sockets/devices, credentials, dependencies, and build
outputs are excluded. Unreadable source files are skipped and reported without privilege escalation.
Apply never overwrites or merges an existing destination and records imported names plus warnings in
a receipt under `<repo-store>/skein/imports/`.

## Configuration

`skein doctor` reports the resolved registry, bind address, and whether `sbx`/`git`/`gh`
are present — run it first if something looks off. All knobs are environment variables — set
them inline, or drop them in a **`.env`** (loaded automatically at startup from the cwd upward;
real env vars still win). Copy [`.env.example`](.env.example) to `.env` and you can just run
`skein` / `skein-server` with no prefix:

| var | what | default |
|-----|------|---------|
| `SKEIN_HOME` | skein's own dir (`repos.json`, embedded `kit/`, cloned repos) | `~/.skein` |
| `SKEIN_NO_GH_SECRET` | set to skip seeding the host `gh` token into sbx (`sbx secret set -g github`) | — |
| `SKEIN_FORCE_GH_SECRET` | set to overwrite an existing sbx `github` secret with the current token (refresh on rotation) | — |
| `SKEIN_SSH_KEY` | path to a private SSH key skein `ssh-add`s into the host agent (sbx forwards it into boxes for SSH git push; the key never enters a box) | — |
| `SKEIN_REGISTRY` | full path to `sandboxes.json` | (see resolution above) |
| `SKEIN_SHARED` | shared store dir (`/sandboxes.json` appended) | — |
| `SKEIN_ADDR` | server bind address | `127.0.0.1:7878` |
| `SKEIN_ALLOWED_ORIGINS` | extra WS origins to allow (comma-sep hosts); loopback + `*.ts.net` always allowed | — |
| `SKEIN_SELF` | this box's vmid (kept `live` when its `lastSeen` is quiet) | `$SANDBOX_VM_ID` |
| `SKEIN_REPO` | dir to run `git`/`gh` in (PRs, checks, host-side diffs) **and to launch/attach from** — so relative `*_CMD` paths resolve here | cwd |
| `SKEIN_BASE` | base branch for `gh pr create` / merge | repo default |
| `SKEIN_LAUNCH_CMD` | launch-a-box template — `{branch}`/`{name}` substituted; relative to `$SKEIN_REPO`. **Optional**: unset, skein builds the launch itself (below), so the repo needs no launch script | _(native builder)_ |
| `SKEIN_KIT` | _(legacy single-repo fallback)_ sbx kit for the native launch when the box isn't in `repos.json`; managed repos use skein's own embedded kit | — |
| `SKEIN_AGENT` | sbx runtime override; must match a registered Skein runtime adapter | repo/default runtime |
| `SKEIN_STORE` | _(legacy single-repo fallback)_ store to mount when the box isn't in `repos.json` | `$SKEIN_REGISTRY`'s dir |
| `SKEIN_ATTACH_CMD` | agent-terminal attach — `{name}`/`{dir}` substituted | `sbx exec -it {name} tmux new-session -A -s skein` |
| `SKEIN_SHELL_CMD` | shell-terminal command (the **Shell** tab) — `{name}`/`{dir}` substituted | `sbx exec -it {name} tmux new-session -A -s skein-shell` |
| `SKEIN_LS_CMD` | fleet-liveness probe (run via `sh -c`); must emit the `sbx ls --json` shape. A running box shows `live` regardless of `lastSeen`; on any failure skein falls back to `lastSeen` | `sbx ls --json` |
| `SKEIN_PR_CMD` | open-PR template — `{branch}`/`{name}` substituted | `gh pr create --head <branch> --fill` |
| `SKEIN_STOP_CMD` | **Stop** — `{name}` substituted; halts the sandbox to free compute (resume via attach). Non-destructive | `sbx stop {name}` |
| `SKEIN_DESTROY_CMD` | **Destroy** — `{name}` substituted; kills & removes the sandbox (clone mode: unpushed commits lost) | `sbx rm -f {name}` |
| `SKEIN_MERGE_CMD` | **Merge** — override the whole merge command (`{name}` substituted) | `gh pr merge <branch>` |
| `SKEIN_MERGE_METHOD` | merge strategy flag passed to `gh pr merge` | `--squash` |
| `SKEIN_RESUME_CMD` | one-click "continue" template — `{name}`/`{prompt}`/`{runtime}` substituted | runtime adapter's native headless resume |
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

## Terminal responsiveness (an invariant worth guarding)

The embedded terminal shares skein-server's async runtime with everything else: the SSE
fleet stream, every JSON handler, and the WS↔PTY bridge are all tasks on the same tokio
workers. So **no request handler or stream may run blocking work inline** — anything that
shells out (`sbx`, `git`, `gh`) or touches the filesystem must go through
`tokio::task::spawn_blocking`. A single inline blocking call freezes its worker for the
whole duration and starves any terminal websocket scheduled on it: keystrokes stop echoing
until it returns.

The subtle case was the 2s fleet-snapshot tick (`load_views` — `sbx ls` + a per-box `git`,
1-2s for a busy fleet). Run inline it caused a periodic "typing lags **only when the box is
idle**" freeze — mid-stream the output flood hid the gap, but at rest a lone keystroke's
echo waited out the stall. Every `skein::` call in `skein-server.rs` is on `spawn_blocking`
for this reason; the regression is guarded by
`slow_fleet_snapshot_does_not_starve_concurrent_requests` in `tests/server.rs` (pins the
server to one worker, makes `load_views` sleep, and asserts a concurrent request isn't
blocked). Separately, each accepted connection sets `TCP_NODELAY` (our own accept loop, not
`axum::serve`) so Nagle's algorithm can't coalesce single-keystroke packets. Both matter:
the socket must be fed promptly *and* flushed promptly.
