# skein

> See and steer your fleet of agent boxes. A thin control surface over one shared `sbx`
> microVM (a bwrap namespace per box) and a mounted shared store — the layer
> Conductor-likes have but that's missing from the sbx workflow. *Compose, don't
> reinvent* — see [`ARCHITECTURE.md`](ARCHITECTURE.md); the why is in
> [`VISION.md`](VISION.md).

**Status:** v0 — a live **web cockpit** (fleet board over SSE) with an **embedded
per-box terminal** (click a box → talk to that agent in the browser) + a CLI. The goal
is the web UI as the *single pane of glass*; web actions (launch/diff/merge/stop/destroy) and
a ⌘K palette are next (ARCHITECTURE.md § Roadmap).

## Build

```sh
cargo build --release        # → target/release/{skein, skein-server}
cargo test                   # units, a black-box run of the real server (tests/server.rs), and the
                             # box hook scripts driven as scripts (tests/turn_state_probe.rs)
node tests/ui/voice.mjs      # what the mouth says + when it stays quiet (no browser needed)
node tests/ui/tabs.mjs       # do your open tabs survive a reload (no browser needed)
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
sample corpus. The agent can't see your clipboard or your disk (it runs in the sandbox), so skein streams
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
- **AI enrichment (opt-in — Settings → Workflow)** — skein runs inside an `sbx run` box where `claude`
  is logged in, so it can spend *rationed* Haiku calls on the subscription with no API key: a one-line
  summary for a box with no journal (marked ✨ and labelled as model-written, never mixed in with what
  the box actually said), and a conservative safety gate on **Continue N** — a box the heuristic reads as
  a routine "shall I proceed?" is held back if the model reads it as a real decision. The gate can only
  ever *add* a hold, never grant a continue, so a flaky or absent answer errs toward asking you. Off by
  default because these calls share the fleet's rate-limit window; lazy, on demand and cached per
  turn-end when on — never a per-tick sweep. `$SKEIN_AI=on|off` overrides the setting, and `skein doctor`
  reports what would actually happen (including "on, but `claude` is not on PATH").

The **diff** is computed **inside the box**, against the remote base branch — `origin/<your base
branch>`, then `origin/main`/`origin/master` — from the merge-base to the working tree, so it shows
committed branch work and uncommitted edits together. The pane names the base it used. It has to run
in the box for the same reason Files does: host-side git answered from `~/.skein/repos/<id>/work`,
which for a clone-mode box is a different checkout on a different branch, and empty for a repo whose
host clone never got a working tree. When the box is down, the patch it wrote at its last turn end is
shown and labelled as such, so a stale diff never reads as the live tree.

Each box also gets a **Files** tab — browse its workspace and read files without leaving skein:
markdown renders (README auto-opens at the root, relative links navigate), images display inline,
everything else shows as text. It reads **the box's own tree** (`sbx exec`, path-resolved and
escape-guarded inside the box), because a clone-mode box works on its own copy: the host-side clone
is a different checkout on a different branch, and for a repo whose host clone never got a working
tree it is empty — which is how the tab could show nothing while the agent had a full tree. When the
box is down it falls back to that host clone and labels the listing `host clone` rather than
substituting one tree for the other silently.

API: `GET /api/{boxes,health,runtimes}`, `GET /api/events` (SSE), `GET /api/boxes/:name/{diff,session,narrate,ship}`,
`GET /api/boxes/:name/files?path=` + `GET /api/boxes/:name/file?path=` (the Files tab),
`GET /api/{repos,settings,mailbox}`, `POST /api/boxes/:name/{resume,stop,destroy,pr,merge,repin,upload}`,
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

## Transcript — the conversation that survives

The terminal is a *view* of a box's conversation, and the most fragile copy of it: it dies with the
browser tab, with a skein-server restart, with the tmux session, and silently with the scrollback
limit. When a box reboots, `claude --continue` restores the agent's memory from disk and the screen
comes back empty — the agent remembers everything and you can read none of it.

The **Transcript** tab reads the record instead: the JSONL the runtime itself writes inside the box
(`~/.claude/projects/…`), discovered by mtime so it works on a box whose probes were never
installed. Tail-first — these files reach tens of megabytes — with "load older" doubling the window
back to the beginning. Tool calls are summarised as `Bash(cargo test)` rather than inlined, and
tool results and thinking are left out, because the point is the conversation.

Claude only for now. Codex's rollout files have a different shape, and skein captures a runtime's
format from a real box before claiming to read it — the tab says so rather than guessing.

## Per-repo settings

Settings → Repositories is one card per repo: open it and every per-repo setting is there with a
label — the **Plane project** its boxes' tracker tokens bind to, and which **work-tracking
connection** its boxes claim from, picked from the connections you set up under Work tracking. The
connection is a picker, not a URL, because a gateway and the token that mints at it are one thing
(see below).
The card's tags say at a glance what each repo is actually configured to do. These save as you leave
a field — they live in `repos.json`, not in the settings form, so that pane has no Save button to
mislead you.

## Work tracking — a backlog the fleet claims from

Boxes each keep their intentions in their own context, which is exactly where an intention goes to
die. **Settings → Work tracking** points repos at a [`sync`](https://github.com/prateekreddy/sync)
gateway: Plane as the system of record, behind a gateway that adds the one thing Plane cannot do —
an *atomic claim*, so two boxes never work the same item. Assigning yourself in Plane reserves
nothing; both boxes read back their own name and both proceed.

The unit is a **connection**, and it is one card holding both halves:

- **Gateway URL** — `https://plane.example.com`. This is what a box talks to.
- **Plane personal token** — used **only here, on the host**, to mint each box its own tracker
  token. A box never receives this one, deliberately: a Plane token can set `assignees` directly,
  which walks straight around the claim. It is stored in `~/.skein/tokens/<id>` at mode 0600, never
  in `connections.json`, and there is no route that reads it back — the cockpit is only ever told
  *whether* one is set.

The two halves are one thing because a token minted with PAT *A* is only valid at the gateway *A*
authenticates to. So a repo **picks a whole connection** rather than naming a URL: add as many as
you have backlogs (a self-hosted Plane alongside the shared one, a second product in its own
instance), then choose one per repo. **Not tracked** is a first-class choice, not a blank field, and
◇ Track work only appears on a box whose own repo has a usable connection — a button that can only
fail is worse than no button. Removing a connection is refused while a repo still selects it, by
name, because silently untracking three repos is a bigger edit than the click asked for.

A host set up before connections existed is migrated on first read: the old `~/.skein/plane-token`
and gateway setting become a connection, each repo that had its own gateway URL becomes another, and
every repo keeps pointing at the one it was already using. That includes copying the single PAT to
each of them — which is exactly what skein was doing before, right or wrong — so a fleet that tracked
work yesterday still does today. If one of those is a different Plane, its token is one field away.

Give a repo its Plane project on its row in Settings → Repositories (paste the project URL; skein
reads the uuid out of it), then open a box and press **◇ Track work**. The host mints that box its
own token, writes it into the box over stdin at `~/.config/sync/env`, and registers the `sync` MCP
server there for whichever runtime the box runs. Nothing triggers it: it spends a network round
trip and creates a real credential, so it is always your click.

**Existing boxes work too, and nothing of theirs is overwritten.** The installer rides the *store* —
mounted live into every box for the repo — rather than the kit, because a kit only reaches boxes
created after it changed. On a box that has been running for weeks, every write is an append or a
create, never a replace: the Work tracking section is appended to `CLAUDE.md` only if it isn't
already there; the memory and the skill are copied only if absent; one line is appended to
`MEMORY.md`; and a hand-written `[mcp_servers.…]` in Codex's config is left exactly as it was. Then
it stamps itself and stops — the box owns all of it from that point, including deleting the parts it
doesn't want, and a later start will not put them back. The single exception is the box's own `sync`
MCP registration, which a re-apply deliberately replaces: that is how a rotated token gets in.

**⟳ Update rules delivers a correction without taking the box's config back.** Installing once and
handing off is what makes the config the box's — but it left no way to fix a rule that turned out to
be wrong, and one did: upstream moved decomposition from `capture` per child to `decompose`, and
every box already wired kept the superseded version. The button appears on a box only when this
repo's store actually holds something newer, which the host answers from two file reads without
waking a single box. One rule governs what it does: **skein never overwrites an edit it can see.**
A document still byte-identical to what skein installed is replaced; one the box changed is reported
and kept. Boxes wired up before skein recorded what it wrote are a genuine third case — stale and
edited are indistinguishable there — so they are named rather than guessed at, and shift-click takes
them once you say so.

**Every box writes to Plane as you.** The gateway keeps your PAT against each agent it mints, so
Plane's activity log shows your name, not a per-box user — `<you>/<box>` is the gateway's *holder*
string, not a Plane account. What one token per box buys is the lease: a distinct holder, so two
boxes cannot both hold the same item, plus `held` telling a box what it was in the middle of, and
revocation that retires one box without disarming the fleet. Destroying a box revokes its token
first; if the gateway can't be reached, the destroy still completes and says so on stderr, so you
know to retire that one by hand.

The box then gets `capture` / `claim` / `heartbeat` / `complete` plus Plane's own tools — cycles,
modules, labels, comments, worklogs — and the installer lays down the discipline that goes with them:
the rules in `CLAUDE.md`, a memory so they survive a fresh session, and a `work-tracking` skill for
the full surface. Those three documents are derived from the `sync` repo rather than invented here;
`src/store/sync/UPSTREAM.md` records which upstream file each claim comes from, the commit they were
last checked against, and what to re-read when the gateway moves.

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

### One disk or two

A fleet sandbox carries two disks: the root filesystem the boxes live on (`DOCKER_SANDBOXES_ROOT_SIZE`)
and a second mounted at `/var/lib/docker` (`DOCKER_SANDBOXES_DOCKER_SIZE`). Both are fixed when the
sandbox is created — sbx has no resize — so changing either destroys and recreates the VM.

Two ceilings means guessing the split in advance and rebuilding when the guess is wrong. **Docker
shares the fleet disk** (Settings) removes the guess: dockerd's data root moves to `<fleet-root>/.docker`,
on the boxes' own filesystem, and `Fleet disk` sizes the lot. One generous number instead of two exact
ones.

The trade is real and worth stating. Two disks are also two firewalls — a runaway `docker build`
fills Docker's disk and cannot touch the boxes. Measured on this fleet: the root hit 100% while
Docker's disk sat at 63% and every container kept running. Share them and one runaway takes out both.
Off by default for that reason; on when fungible space is worth more than the wall.

It takes effect at dockerd's next start, in practice the next sandbox. Turning it on moves nothing:
images and volumes on the old disk stay there, whole, and simply stop being visible to a dockerd now
reading elsewhere. Turning it off again never *removes* the setting, for the same reason — off means
"stop moving it", not "move it back onto a disk nothing has written to since".

### Asking for a system package

A box cannot install one: it is a user namespace mapping a single uid, so `sudo` inside it is
unfixable rather than unconfigured. The sandbox *around* the boxes has a working root, and one
install there serves every box in the fleet — which is exactly why it is a decision rather than
something a box does for itself.

So the install an agent typed becomes a request. Inside a box:

```
$ sudo apt-get install libnss3
skein: asked the fleet for apt (libnss3). Request 20260812-093132-560434 is pending approval.
skein: it installs for every box once approved in the cockpit; nothing is installed yet.
```

Nothing is installed, and the command still fails. The ask lands in a queue in the fleet root
(`$SKEIN_FLEET_ROOT/.skein/substrate/requests`), the cockpit's package panel badges it, and the
fleet's owner approves or denies it there. Approving installs it once, for every box, and by default
records it in `~/.skein/substrate.json` on the **host** so a rebuilt sandbox reinstalls it — untick
"remember" at approval time to install it now without recording it. Identical asks collapse into one
decision however many boxes make them, or however often an agent retries.

This is a chokepoint and an audit trail, not a security boundary: any box can already reach the
fleet agent's token, and skein deliberately puts no wall between boxes. What it buys is that a
package changing the toolchain under every box does not get installed because one agent decided to.
Package names are validated on both sides of the wire, because a name approved here ends up on a
command line running as root.

### One repo to write, the rest to read

Every box used to hold the same GitHub credential. Measured on a live fleet, that was a user token
carrying `repo`, `admin:public_key`, `gist` and `read:org` — **460 repositories, read and write** —
plus a forwarded ssh-agent signing for anything the host's key could reach. Ten boxes, one identity.
An agent that misread a remote could push to any of them, and `admin:public_key` let a box add a key
to the account: access that outlives the sandbox and appears nowhere in skein.

With **Settings → Scope each box's GitHub access to its own repo**, a box instead gets:

* a **write** token scoped to its own repository — `contents`, `pull_requests` and `issues` write,
  valid an hour, minted by the host and placed in the box's own state directory;
* a **read-only** token covering the repos the App is installed on, also hourly;
* nothing at all for anything else, so git falls through to anonymous access — every public repo
  still clones and fetches.

`gh` holds the write token, so `gh pr create` and `gh pr comment` work against the box's own repo.
Remotes are rewritten to HTTPS with `insteadOf`, so existing `git@github.com:…` remotes keep working
untouched, and the forwarded ssh-agent socket is bound over rather than merely unset.

Unlike the package gate above, **this boundary is real**: GitHub enforces it server-side, so a box
holding a token for one repository cannot touch another whatever runs inside it. It is still not a
boundary *between* boxes — they share a uid and a PID namespace, so one box can read another's token
file. The wall is fleet→GitHub.

#### Setting it up

Create a GitHub App — no domain, no webhook, no hosting; it is a credential-minting primitive, and
the traffic is outbound only. Untick **Webhook → Active**, leave the callback blank, set permissions
to **Contents: write**, **Pull requests: write** and **Issues: write**, choose *Only on this
account*, generate a private key to `~/.skein/github-app.pem`, and install it on the repos you want
reachable. Put the App ID in Settings.

The installation list is then the only control: a repo the App is installed on is readable, and one
it is not is not. Adding one takes effect at the next refresh with nothing to re-mint.

Until an App (or a stored token, below) exists, **nothing is scoped** — the setting has no effect
and every box keeps the credential it already had. That is deliberate: scoping with no way to issue
a write token would not narrow a box's reach, it would take pushing away from every box at once.

Prefer not to run an App? Store a fine-grained PAT per repository under **Repo write access** in the
cockpit. One repository per token, enforced when stored *and* when used — a token covering three
repos is write access to three repos for whichever box receives it, because the credential helper
runs inside the box as the agent's own uid and can route but never contain. Cross-repo reads of
private repos then need an optional read-only PAT, which nothing prompts for.

#### Asking to write another repo

A push elsewhere is refused by GitHub. To ask for it, from inside a box:

```
$ /boxes/.skein/box-session.sh --request-write "$SKEIN_BOX" acme/thing "fix the shared type"
skein: asked to write acme/thing. Request 20260815-101122-4711 is pending approval in the cockpit.
```

The ask lands in `$SKEIN_FLEET_ROOT/.skein/gitgate/requests`, the cockpit's **Repo write access**
panel badges it, and approving mints a token for that repo within a tick. Grants expire after 24
hours unless the approver ticks *keep indefinitely* — unlike an approved package, which should
survive a rebuild, write access to someone else's repository usually wants to lapse. Grants are
listed with their expiry and can be revoked, which takes effect on the next refresh.

Per-box, the switch lives in **box settings**; per-launch, in the launch dialog. Both take effect at
the box's **next start**, because a credential is placed as the box comes up.

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
| `SKEIN_SSH_KEY` | path to a private SSH key skein `ssh-add`s into the host agent (sbx forwards it into boxes for SSH git push; the key never enters a box). Ignored by boxes with scoped GitHub access — the agent socket is bound over there, since it signs for every repo the key reaches | — |
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
