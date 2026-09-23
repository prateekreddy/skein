# skein

> See and steer your fleet of agent boxes. A thin control surface over one shared `sbx`
> microVM (a bwrap namespace per box) and a mounted shared store — the layer
> Conductor-likes have but that's missing from the sbx workflow. *Compose, don't
> reinvent* — see [`docs/architecture.md`](docs/architecture.md); the why is in
> [`VISION.md`](VISION.md). [`ARCHITECTURE.md`](ARCHITECTURE.md) is the signpost to all four
> design documents.

**Status:** v0 — a live **web cockpit** (fleet board over SSE) with an **embedded
per-box terminal** (click a box → talk to that agent in the browser) + a CLI. The goal is the
web UI as the *single pane of glass*. What is being built next, in order, is
[`docs/delivery.md`](docs/delivery.md) — the sequence lives there rather than here, because a
roadmap copied into a README is a second place to update and the copy is what goes stale.

[![ci](https://github.com/prateekreddy/skein/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/prateekreddy/skein/actions/workflows/ci.yml?query=branch%3Amaster)

Pinned to `master`, deliberately: the archive branches carry old code and an old workflow, so an
unpinned badge would report their permanent red as this project's state.

## Getting started

**You need:** [`sbx`](https://docs.docker.com/ai/sandboxes/) (Docker Sandboxes) on your `PATH` with
Docker running, and `git`. macOS or Linux. `jq` inside the sandbox is installed for you.

skein does **not** need [`gh`](https://cli.github.com). It reads GitHub over the API with a token
you have already given it — see [One credential](docs/operating.md#configuration) — so there is no CLI to install and
nothing to authenticate. `curl` carries those calls and is on every macOS and ordinary Linux.

```sh
curl -fsSL https://raw.githubusercontent.com/prateekreddy/skein/HEAD/bootstrap.sh -o bootstrap.sh
DOCKER_SANDBOXES_ROOT_SIZE=60g \
  sbx create --name skein-fleet -m 26g --cpus 7 -p 7878:7878 shell "$HOME/.skein"
sbx exec -i skein-fleet env SKEIN_FLEET_MEMORY=26g SKEIN_FLEET_CPUS=7 bash < bootstrap.sh
sbx stop skein-fleet
sbx run -d --name skein-fleet --kit "$HOME/.skein/fleet-kit"
```

**`--kit` on that last line is what makes the fleet survive a restart.** The sandbox has pid 1
`tini` and no init — no systemd, no cron, no `systemctl` — so nothing in it puts the cockpit back
after a stop, and every restart came back with the whole install intact on disk and nothing serving.
sbx's `commands.startup` runs at every sandbox start and is the only hook this sandbox has; the kit
`bootstrap.sh` just wrote is one command, `start-door.sh`, which is the same file a person runs by
hand. Safe to attach on a create as well as a re-attach — it does nothing at all until the fleet is
bootstrapped, so ordering it after the install is a convenience rather than a requirement.


**Those last two lines are not tidying up — without them the fleet stops about 35 seconds later
and stays stopped.** sandboxd auto-stops a sandbox once a session has *disconnected* from it, and
`sbx exec` is a session. So the install's own third line condemns the fleet it just built. A
sandbox nothing has ever attached to is not affected, which is why `sbx create` on its own is
fine — and why a detached start puts it back into that state for good.

Measured on 2026-08-28 against a throwaway sandbox, three minutes per phase:

| what was done | what happened |
|---|---|
| created, never attached | ran for the whole three minutes |
| one `sbx exec … true` | **stopped 33-39s after the exec returned** |
| `sbx run -d` | ran for the whole three minutes |

**So the rule, and it outlives the install: any `sbx exec` into the fleet stops it about 35 seconds
after it returns.** Reaching in to look at something is what kills it, which is a poor thing to
learn by accident. If you do exec in, put it back with the same two lines. Nothing else is needed —
no session to hold open, nothing running on the host — because skein drives the fleet over its
published ports, and a port is not a session. There is no setting for this: `sbx daemon` offers
only `log-level`, `restart`, `start`, `status` and `stop`, and `sbx policy` is network rules.

**The memory and CPUs are named twice on purpose, and the install refuses without them.** Once for
`sbx`, which is the only thing that can set them, and once for `bootstrap.sh`, which is the only
thing that can check them. Omit the flags on the `create` and sbx does not complain — it takes half
your host's memory and **every** one of its cores, decided by nobody, permanently. So the second
mention is not a repetition: `bootstrap.sh` compares what you claimed against what the sandbox
actually got, and stops before it builds anything if they disagree. Forget the `-m` and the two
disagree, which is the point.

If you leave the declaration off, the install refuses and tells you what the sandbox you just made
actually has — so you can approve those numbers by re-running with them, or destroy it and create it
again. It cannot tell you what your *host* has; from inside the sandbox that is not visible. Once
stated, the numbers are recorded and an upgrade does not ask again — but it does re-check, so a
fleet rebuilt at a different size is caught rather than carried forward. `skein doctor` reports
whether anybody ever chose them.

**Pick those three numbers for your machine before you paste that.** Memory, CPUs and disk are
**fixed for the life of the sandbox** — sbx has no resize, so changing one means destroying the
sandbox and building a new one, which discards every box's working tree (checkouts live on
VM-local disk, deliberately: see [Getting started](#getting-started) on mounts). They are the least
revisitable decisions in the install, and the only ones nothing asks you about.

| flag | what it is | if you leave it out |
|---|---|---|
| `-m 26g` | memory **shared by every box**, not one reservation each | sbx takes half your host, capped at 32 GiB |
| `--cpus 7` | cores the fleet may use; leave at least one for the host | sbx takes **every** core, and your machine stutters while the fleet compiles |
| `DOCKER_SANDBOXES_ROOT_SIZE=60g` | one shared disk for every box's checkout and `target/` | sbx gives 20 GB, which eight boxes have exhausted with two `target/` directories |

Too low on memory and a single `cargo build` takes the whole fleet down with it; too high and the
create fails on a host that does not have it. `sysctl -n hw.memsize hw.ncpu` on macOS, `nproc` and
`free -g` on Linux.

**Nothing is built or run on the host.** You download one file and hand it to `sbx`; the sandbox
clones skein, builds it with a toolchain of its own, and starts the cockpit. There is no binary to
install, no service to keep running, and no Rust on your machine — `bootstrap.sh` is the whole
install, it is ordinary shell, and it is worth reading before you run it. The first build takes
minutes, and that cost is the point: what runs is what was published.

**The `-p` is the cockpit's port**, and it is on the `create` because that is the one thing a
sandbox cannot do for itself and `sbx create` takes the flag. It used to be a fourth line run by
hand, which is a step that can be skipped — and skipping it leaves a fleet that looks installed,
serves nothing the browser can reach, and says so nowhere. **Mappings are fixed at create the same
way mounts are**, so a sandbox made without it needs `sbx ports skein-fleet --publish 7878:7878`
once; that is also the repair if the cockpit is ever alive inside the sandbox but unreachable from
the browser. `sbx ports skein-fleet` on its own lists what is already mapped.

**If you already have repos whose stores live outside `~/.skein`, the second line is not enough.**
Every directory a box must see is named on the `create`, and **sbx fixes mounts at creation** — no
verb adds one later (`sbx --help`; `cp` copies into a sandbox, it does not mount). A store you point
elsewhere (`skein add <git-url> --store …`) lives wherever you keep it, so it has to be on that line
or its boxes come up with no store, which reads as a broken box rather than a missing mount.

The line above is right for a first install, where every repo will live under `~/.skein/repos`. For
any other case, do not assemble it by hand — **`skein doctor` prints the exact one** for what you
have registered, under `create line`. It is the same text the cockpit would put in front of you
before running, so the two cannot disagree:

```sh
DOCKER_SANDBOXES_ROOT_SIZE=60g \
  sbx create --name skein-fleet -m 26g --cpus 7 -p 7878:7878 shell \
  "$HOME/.skein" "$HOME/work/some-repo" "$HOME/elsewhere/another"
```

**If the sandbox dies**, re-run all three. `sbx create` on a name that exists is refused rather than
destructive, and the bootstrap is idempotent — it fetches instead of cloning and reloads the cockpit
across its own socket rather than restarting it. There is deliberately no `skein` on the host to
repair a fleet with, so these lines are also the repair.

Then, in the cockpit: sign in once (every box inherits it), add a repo, and press **+ box**.

**Open the URL it prints**, not `127.0.0.1:7878` on its own — it carries the fleet's token
(`http://127.0.0.1:7878/?t=…`) and your browser keeps it in a cookie, so it is a one-time step. A
page opened without it says so and tells you where the token lives; see [Opening the
cockpit](#opening-the-cockpit).

Then press **+ box**, name a branch, and an agent starts working on it. The board's own first-run
checklist tracks what is left (sbx answering, an agent signed in, a repo added), and `skein doctor`
diagnoses the environment if anything looks wrong.

**Creating and destroying the sandbox stays yours.** It is the most privileged thing in skein, and
skein inside the fleet cannot do it at all — there is no skein until the fleet exists. So the create
above is a line you ran, and a later resize or recreate is a line the cockpit **shows you to run**,
with what it is for and what declining costs. `skein-warden` is the optional other half of that: run
it on the host and the cockpit asks it instead of asking you, still putting the command to a person
before it runs. Without a warden nothing is blocked — you are simply the one who pastes the line.

**Don't skip signing in.** It authenticates the agent runtime once inside the shared sandbox, and
every box inherits that session. Without it each box comes up sitting at a login prompt, does
nothing, and shows `sign in` on the board — the single most common way a first run goes quiet.
Claude and Codex are separate sign-ins and both can be signed in.

## Web cockpit (the primary surface)

```sh
./target/release/skein-server          # → http://127.0.0.1:7878
```

*(The [install above](#getting-started) is already serving this — the kit's `start-door.sh` starts it
at every sandbox start, so the cockpit is up before you type anything. The raw command is the host
route, against a checkout you have [built](#build) yourself.)*

*(`SKEIN_REGISTRY=…` is the pre-`skein add` single-repo path; see [Registry
resolution](docs/operating.md#registry-resolution-first-match-wins). Managed repos need none of it.)*

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
- **AI enrichment (opt-in — Settings → Boxes)** — skein runs inside an `sbx run` box where `claude`
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

API, the part you would reach for: `GET /api/{boxes,health,runtimes}`, `GET /api/events` (SSE),
`GET /api/boxes/:name/{diff,session,narrate}`, `GET /api/boxes/:name/files?path=` +
`GET /api/boxes/:name/file?path=` (the Files tab), `GET /api/{repos,settings,mailbox}`,
`POST /api/boxes/:name/{resume,stop,destroy,repin,upload}`,
`POST /api/{resume-batch,repos,settings,mailbox}`, `GET /api/boxes/:name/terminal` (WebSocket).
That is a selection and not the set. Every route is declared in one `Router` in
`src/bin/skein-server/main.rs`, so `.route(` in that file is the list, and it is the only version of it
that cannot go stale — this paragraph used to name four routes (`ship`, `pr`, `merge`, `pick-path`)
that went with the box-level PR tools and the path picker, and nothing noticed.
xterm.js and marked.js are vendored into the binary (served from `/vendor/`), so everything works
with no CDN — important in the firewalled sbx network.

> Loopback-only by default (`127.0.0.1:7878`); set `$SKEIN_ADDR` (e.g. `0.0.0.0:7878`) to bind
> off-loopback. The terminal WebSocket rejects unexpected `Origin`s (drive-by / DNS-rebinding
> guard) — it allows loopback, `*.ts.net`, Tailscale IP ranges, and `$SKEIN_ALLOWED_ORIGINS`.
> For remote/mobile you can either `tailscale serve` (below, keeps the loopback bind) or bind
> off-loopback and hit the box's tailnet address directly.

### Sandboxes skein did not create

The board's list of boxes comes from `sbx ls`, which reports every sandbox on the machine and cannot
say which of them are skein's. So an `sbx` box you made yourself — or a box from a skein old enough to
give each one its own microVM — appears with no branch, no signals and nothing that works.

Those are **hidden by default** and shown by typing `foreign:` in the board filter. skein cannot
attach to one, read its work, or manage it: there is no placement record, no store it provisioned, and
no tmux session it owns. Reach one directly with `sbx exec -it <name> bash -l`, or hand it to skein by
registering its repo with `skein add` and creating the box from the cockpit.

The per-VM model itself is gone: every box lives in one shared sandbox, because a microVM reserves
its memory whether the box is working or idle and those reservations sum.

### What one box can see of another

Each box gets a mount namespace holding its own directories and the fleet root's scripts, and
nothing else — the directories that hold every box are covered, so a box created later is hidden
too. Process-level isolation was already there: `/proc/<pid>/{root,cwd,environ,maps}` of another
box is denied, because each box is its own user namespace. What leaked was the filesystem, and that
is what this closes.

Existing boxes migrate by restarting. Nothing moves on disk and no ownership changes — a running box
keeps the namespace it was given, and gets the new one at its next start.

**The workshop box.** One box can opt out, under *box settings → workshop box*: it sees every box's
files and keeps the fleet agent's token, which is what makes it usable for debugging and extending
skein itself. Off by default, per box, and it announces itself on its own terminal at every start —
a box that can read every other box's credentials should never be one you have to look up.

### Opening the cockpit

The API needs the fleet's token. On startup the server prints the URL that carries it:

```
skein-server → http://127.0.0.1:7878/?t=<token>
```

Open that once and the browser keeps a `HttpOnly` cookie; after that plain `http://127.0.0.1:7878`
works. The token lives at `~/.skein/api-token` (0600, generated on first run), so a script can use
`Authorization: Bearer $(cat ~/.skein/api-token)`.

**Why it exists.** Loopback is not the boundary it looks like. A box in the fleet reaches the host
at `host.docker.internal:7878` — measured, not assumed — so before this, any agent could approve its
own write-access request, or un-scope its own box, and collect a real GitHub token for a repo it was
never meant to touch. Static assets and the page itself are still served to anyone; everything that
reads or changes state is not. `SKEIN_NO_API_AUTH=1` turns it off if you have another boundary —
outside the fleet. Inside one, it is refused: the cockpit serves nothing but the reason why.

### Remote access (Tailscale)

Keep skein bound to loopback and let Tailscale carry the tailnet → loopback hop. The tailnet is a
second boundary on top of the token: only your WireGuard-authenticated devices can reach it, with no
public surface.

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
  BOX           STATE  BRANCH           SEEN     DIR
● my-feature    live   feat/my-feature  12s ago  ~/work/.../gadget-demo
● bugfix-login  idle   fix/login        8m ago   ~/work/.../gadget-demo
○ thing-export  stale  export           9h ago   ~/work/.../gadget-demo

3 boxes
```

Every column is as wide as its own widest value (`pad`, in `src/bin/skein.rs`), so a longer box name
or branch moves everything to the right of it. The block above is those three rows put through that
rule rather than a table drawn by hand — the one it replaces had been edited by hand until the
`stale` row no longer lined up with the two above it, which is the one thing a sample of aligned
output cannot afford to get wrong.

State prefers the explicit status a box's hooks report (`needs-input` / `waiting` / `working` /
`done`); with no report it falls back to `live` (<2m) / `idle` (<30m) / `stale` from `lastSeen`.

## Build

**You do not need this to run skein** — the [install above](#getting-started) builds it inside the
sandbox. This is the developer route, for working on skein itself.

```sh
cargo build --release --workspace   # → target/release/{skein, skein-server, skein-warden}
                             # `--workspace`: a plain `cargo build` makes the first two only, and
                             # the fleet cannot be created or resized without the third
cargo test --workspace       # units, a black-box run of the real server (tests/server/), and the
                             # box hook scripts driven as scripts (tests/turn_state_probe.rs)
node tests/ui/voice.mjs      # what the mouth says + when it stays quiet (no browser needed)
node tests/ui/tabs.mjs       # do your open tabs survive a reload (no browser needed)
node tests/ui/smoke.mjs      # the cockpit in a browser — run it after touching src/web/index.html
                             # (`cargo test` runs every suite in `BROWSER_SUITES` too, and says so
                             #  when Playwright's chromium is not installed)
```

**Some tests do not run on macOS**, and the suite says which. They drive shell scripts skein
installs *into a box* — `sed -i` with no argument, `readlink -f`, `sort -z`,
`tar --ignore-failed-read`, `/proc/<pid>/stat` — and every one of those spellings is the correct one
where the script actually runs, which is a `bwrap` namespace inside a Linux sandbox.

The list is `GATED` in `tests/platform_gates.rs`, each name with the reason it cannot run elsewhere.
No count is written here on purpose: the last one said eight while `GATED` held eighteen, and a
number in prose that the code can answer is a number that drifts. `cargo test` on a Mac prints the
count and the names from `GATED` itself, and
`every_platform_gated_test_is_declared_with_its_reason` fails the build if a test is gated without
being written down — in both directions, so the list cannot silently outlive the tests either. For
the whole suite, run `cargo test` inside a box.

`cargo test` proves the API is right; the browser smoke test proves the *page* is right, which is
not the same thing. It launches the real binary against a throwaway workspace and clicks through the
tabs, asserting what is **visible** rather than what merely exists in the DOM — the Files tab once
shipped with every folder rendered and then hidden by an unrelated CSS rule, invisible to every
other check. One-time setup in `tests/ui/README.md`.

## Where to go next

Everything above is the front door — install skein, open the cockpit, drive a box from the browser
or from the terminal. Everything else it does lives one document over, in
[`docs/operating.md`](docs/operating.md), because *is this for me* and *how do I point this repo at
a backlog* are questions asked months apart, and the first should not have to scroll past the
second to reach its answer.

Go there to:

* **manage repos** — `skein add`, why a repo is a remote and a local path is refused, the per-repo
  settings card, and the registry resolution order the pre-`skein add` single-repo path still uses;
* **give the fleet a backlog it claims from** — a [`sync`](https://github.com/prateekreddy/sync)
  gateway in front of Plane, why an atomic claim is the one thing Plane cannot do, and what a token
  per box buys that assigning yourself does not;
* **choose what credential a box holds** — the three GitHub paths, none of them a default, and the
  measured retraction about what scoping a box does *not* narrow;
* **decide what the fleet may install** — a box cannot `sudo`, so an `apt-get` inside one becomes a
  request you approve once, for every box;
* **read a conversation the terminal has lost**, hand a box from Claude to Codex, or carry durable
  working files from one box to the next;
* **look a knob up** — the `SKEIN_*` knob table, the `.env` that saves you retyping it, how skein
  sizes a fleet, and how it reaches one at all.

It closes with the two things worth reading before you change skein itself: what skein owns of the
`sbx` setup and what it only reads, and why no request handler may run blocking work inline — the
invariant the embedded terminal's responsiveness rests on.

## Contributing, security, and the licence

* [`CONTRIBUTING.md`](CONTRIBUTING.md) — what you can run without a fleet, the gates your change
  has to pass, and the commit-message voice, which is distinctive enough that it is worth reading
  twenty of them before writing one.
* [`SECURITY.md`](SECURITY.md) — how to report a vulnerability privately, and what counts as one in
  a tool whose whole job is to sandbox agents. Please read the scope section: the line between a
  vulnerability and the product working as designed is not in the usual place here.
* [`docs/threat-model.md`](docs/threat-model.md) — what a box can and cannot reach, in one table,
  with the test behind each row.
* [`CHANGELOG.md`](CHANGELOG.md) — what skein does today and how it got there, every entry traced
  to a commit.

skein is dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE), at your option.
