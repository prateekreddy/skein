# Operating skein

[`README.md`](../README.md) installs skein, opens the cockpit and puts a box in front of you. This
document is everything after that: the repos a fleet manages, the backlog its boxes claim from, the
credential each one holds, what they may install, and every knob underneath.

It is for the person **running** a fleet rather than deciding whether to try one, so it assumes the
cockpit is already open and does not re-introduce what the README has shown you. Read it through
once and the fleet stops surprising you; past that it is a reference, and every section stands on
its own. The order is the order you meet these things in rather than a sequence you have to follow:
repos first, then what boxes do with work and credentials, then the configuration underneath all of
it.

It is not a design document. [`ARCHITECTURE.md`](../ARCHITECTURE.md) is the signpost to those, and
they answer why skein is shaped the way it is; this one answers what it does today and how to drive
it.

## Registry resolution (first match wins)

1. `$SKEIN_REGISTRY` — full path to `sandboxes.json`
2. `$SKEIN_SHARED/sandboxes.json`
3. `<git-toplevel>/../skein-shared/.claude/sandboxes.json`

## Adding repos

skein manages a set of repos itself — you don't wire anything into the repo. Add one by its git
URL; **a repo is a remote**, and a local path is refused, because skein runs inside the fleet
sandbox and cannot reach a checkout on your machine:

```
skein add https://github.com/org/app.git      # mirrors it into ~/.skein/repos/app/mirror
skein repos                                    # list managed repos
```

…or in the cockpit: **⌘K → "Add a repo…"**. Adding a repo provisions a shared `.claude` store
from scratch (mailbox + skein's turn-state probe) and installs skein's own sbx kit; how its boxes
push is the credential you pick under Settings → GitHub & keys (below). Boxes are then named `<repo>-<branch>`; create
one from the **New box** dialog (which gains a repo selector once you manage more than one).

The registry lives at `~/.skein/repos.json` (override the home with `$SKEIN_HOME`). The kit is
embedded in the binary and written to `~/.skein/kit/` — no repo-side `dev-sandbox/kit` needed. The
agent runs inside a `skein-agent` tmux session, so reconnecting (attach) re-joins the **same** live
terminal instead of spawning a parallel `claude --continue`.

**Branch names with slashes just work.** Type `feat/auth` in the New box dialog: the sbx box is
named with a slug (`<repo>-feat-auth`, since sbx names can't contain `/`) while the box actually
checks out the real `feat/auth` branch.

**Settings** (⌘K → "Settings…", stored in `~/.skein/config.json`): whether an unscoped box falls
back to the account token (`seed_gh_secret`, read by `box_credential`), the default agent, and
confirm-before-Destroy. Where the table below lists a matching `$SKEIN_*` variable, that variable
can hold the matching switch off, and never on. The branch a new box starts from is per repo, on its
card (`base_branch` in `repos.json`), and `fleet::base_branch` checks it against what the remote
actually has (`git ls-remote --symref`) before using it.

**Git auth inside boxes.** HTTPS remotes push with no setup. A scoped box reaches GitHub **direct**
(the GitHub hosts are in `NO_PROXY`), so git presents the per-repo token skein placed and GitHub
enforces it; a `fleet`-mode box instead keeps the sbx proxy, which injects the account credential —
see [One repo to write, the rest to read](#one-repo-to-write-the-rest-to-read), and SKEIN-548. For SSH
remotes (`git@…`/`ssh://…`), sbx forwards your **host SSH agent** into the box (the private key stays
on the host). Run `ssh-add` on the host to load a key: skein in the fleet cannot read a key file
on the host, so it takes no key path (SKEIN-947). `skein add` warns up-front if a repo's `origin`
is SSH so you can switch it to HTTPS or load the key.
Note that scoping does **not** cover SSH: `github.com:22` is reachable direct and the host agent is
also reachable at `SSH_AUTH_SOCK_GATEWAY=gateway.docker.internal:3129`, so any key in the host agent is usable by every box for
every repo it reaches (SKEIN-929) — the launcher's socket cover blanks only the box's local agent.

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
conversion is involved.

**Starting an agent reaches for nothing over the network** (SKEIN-403). It used to run the
runtime's native updater first, on every session start — 1.9–3.2 s, and it failed every time,
because the CLI is root-owned in the sandbox and a box maps only its own uid, so npm cannot write
it and the error was swallowed. `shell_and_attach_argv_differ` asserts the absence, which is the
only way anyone would notice it coming back short of timing a box. Moving the version is
`skein update-agents` (SKEIN-404) — one command, run where sudo works, printing what actually
moved (`claude: 1.2.3 -> 1.2.9`) rather than "done".

**A running agent keeps the CLI it started with**, so an install moves no box until its next
session. After one, Settings → Update lists the boxes still on the old version (SKEIN-1070): the
install records when it finished and what moved, in `agent-installs.json` under `$SKEIN_HOME`, and
the pane compares each running box's agent start time with that record — "3 boxes are still running
claude 2.1.278 and move to 2.1.280 at their next session". A box whose agent is `waiting` gets one
restart button of its own and a working box gets none; there is no "restart all" (SKEIN-1071). The
press ends the box's agent session and opens it again with the runtime's resume command, so the
conversation comes back and nothing is sent to it. The server reads the box's turn state again when
the button is pressed and refuses a box that has started working since the list was drawn. Ending
the session hangs up its terminal, which stops the agent and whatever it runs there; a process that
ignores the hangup (`nohup`) or left the session (`setsid`) keeps running.

tmux is deliberately invisible: its status bar is disabled, mouse/copy scrolling is enabled, and
pane history is enlarged. Codex is launched with its documented `--no-alt-screen` option so browser
wheel scrolling moves through conversation output instead of cycling prompt history. Claude feeds
its native status-line JSON into Skein's renderer; Codex maps the live `token_count` data behind
`/status` into the same schema. The cockpit shows the same CTX/5H/7D/cost/model footer for both,
refreshing every 30 seconds and omitting unavailable segments. An explicit Codex `/statusline`
choice disables Skein's adapted footer.

Each Claude box also keeps the prompt cache for subagents, workflows and background requests for an
hour instead of Claude Code's five minutes: the kit sets `"subagentPromptCacheTtl": "1h"` in the
box's own `.claude/settings.local.json` at every start, unless the repo's `.claude/settings.json` or
that local file already sets the key, so a repo that commits `"5m"` keeps it.

Claude Code's built-in `telemetry` plugin is off in a box unless its repo allows it. At every box
start the launcher sets `"telemetry@builtin": false` under `enabledPlugins` in the box's own
`~/.claude/settings.json`, the one settings file every Claude session in the box reads, so Claude
Code does not load that plugin and the analytics rows other built-in plugins log through it are
neither queued nor sent. That is all it turns off. Claude Code's own usage statistics still go to
Anthropic, and its feature flags still evaluate, so Remote Control, push notifications and your
plugins synced from claude.ai behave as they would anyway. To allow the plugin for a repo, set
`"anthropic_telemetry": true` on that repo in `repos.json`; its boxes pick it up at their next
start, when the launcher takes skein's `false` back out. A value somebody else wrote for that key
is left alone either way. skein's own per-turn telemetry is separate and stays in the repo's store.

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
on the boxes' own filesystem, and the next create's disk (Settings → Fleet) sizes the lot. One generous number instead of two exact
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
* nothing at all for anything else, so git falls through to whatever the network answers a request
  carrying no credential — every public repo still clones and fetches, and today more than that
  (the retraction below).

`gh` holds the write token, so `gh pr create` and `gh pr comment` work against the box's own repo.
Remotes are rewritten to HTTPS with `insteadOf`, so existing `git@github.com:…` remotes keep working
untouched, and the forwarded ssh-agent socket is bound over rather than merely unset.

Unlike the package gate above, the **token** boundary is real: GitHub enforces it server-side, so a
box acting with its own token cannot touch another repository. Nor can it read the
token another box holds — each box's mount namespace hides every other box's directories ([what one
box can see of another](../README.md#what-one-box-can-see-of-another)); the exception is the workshop box, which
opts out of that on purpose.

**The token is not the only route out of a box, so the boundary is made real by taking the box off
the proxy for GitHub.** Measured inside a live box (2026-09-06, re-measured 2026-09-15): `sbx` routes
the sandbox's HTTP through a credential-injecting proxy. Left on it, a request that carries *no*
Authorization header is answered as the account — and one carrying a deliberately invalid token, or
the box's own per-repo token, is too, because the proxy terminates TLS and replaces the credential.
So skein routes a scoped box's GitHub traffic **direct**: startup adds the GitHub hosts to `NO_PROXY`,
and git and gh then present the token this box actually holds, which GitHub enforces server-side — a
private repo the box was not granted comes back `401` rather than the account's `200`. A box in the
account mode (`Scope … off`, `SKEIN_GIT_SCOPE=fleet`) stays on the proxy on purpose: that is the
account-wide credential, honest about being account-wide.

So scoping now narrows **what a box's normal tools reach**, not only what its token can do. What it
still does not contain is a process that deliberately routes back through the proxy or reaches the
host ssh-agent gateway — a firewall-grade boundary is the sandbox's egress policy to set, not this
setting's (SKEIN-548, closed for the git/gh path; the residual is SKEIN-926/SKEIN-929).

**Which repository "its own" means.** The URL the repo was added by, or — for an entry written back
when a local path could still be registered — the `origin` on that repo's mirror. Such an entry is
not unscopable: skein's own repo was registered that way, and a mirror with a GitHub origin gets a
token like any other. A repo with no origin at all is the one case with nowhere to push: no token is
placed, and its card says so rather than offering a field that could not work.

#### Setting it up

Create a GitHub App — no domain, no webhook, no hosting; it is a credential-minting primitive, and
the traffic is outbound only. Untick **Webhook → Active**, leave the callback blank, set permissions
to **Contents: write**, **Pull requests: write** and **Issues: write**, choose *Only on this
account*, generate a private key to `~/.skein/github-app.pem`, and install it on the repos you want
reachable. Put the App ID in Settings.

The installation list is then the only control: a repo the App is installed on is readable, and one
it is not is not. Adding one takes effect at the next refresh with nothing to re-mint.

Until an App (or a stored token, below) exists, **nothing is scoped** — the setting has no effect and
every box keeps whatever fleet-wide credential you chose. That is deliberate: scoping with no way to
issue a write token would not narrow a box's reach, it would take pushing away from every box at once.

#### The three paths, all opt-in

There are exactly three ways a box gets GitHub credentials, and **none of them is a default**:

| Path | What a box holds | What it costs you |
|---|---|---|
| **GitHub App** | a write token for its own repo, an hour at a time, plus read over your installs | one App, installed where you want it readable |
| **Per-repo PAT** | a write token for that one repo — or, if you shared it, for every repo sharing it | one token per repo, rotated by hand |
| **This account's `gh` token** | your whole account, in every box | nothing to set up — and no narrowing either |

The third used to be on by default, which made the broadest of the three the one nobody chose. It also
announced itself: `gh` keeps its token in the system keyring on a modern Linux, so startup asked to
unlock your keyring — every launch — before you had said which path you wanted. It is now off until
picked, seeded once and remembered, and the first-run checklist asks for a choice rather than making
one. Turning it off changes nothing for a fleet already running on it: the secret lives in sbx's own
store, so it stays seeded and boxes keep pushing.

**A per-repo PAT can be shared, and sharing widens every box that holds it.** When you add a repo,
the dialog offers "paste a new token" or "use the token another repo has". The second adds the new
repo to that token's list in `github-pats.json` — one token, one file, so replacing it on any of
their cards reaches them all — after GitHub has said the token can push to the new repo. The cost,
which the dialog states before you confirm: a box is handed its repo's token, and a box can read
anything it holds, so the boxes of every repo sharing a token can push to all of them. That is the
owner's decision of 2026-09-29 (SKEIN-1231: "Allow sharing, with a warning"); before it a stored
token covered exactly one repository and nothing else was accepted. A pasted token still covers
one, a list naming several that was not written by a share is refused where tokens are handed out
(`src/gitgate/credentials.rs`, `WriteCredential::problem`), and a repo removed from skein, or given
a token of its own from its card, leaves the share without taking the token from the others.

`skein doctor` names which path you are on, and says so plainly when you are on none — boxes then
hold no GitHub credential of their own. For a scoped box that means what it says: with nothing placed
and the GitHub hosts in `NO_PROXY`, the box reaches GitHub direct and unauthenticated, so it clones
public repos and is refused everything else. (A `fleet`-mode box on none is the exception — it stays
on the proxy, so it still reaches GitHub as the account.) `skein doctor` also carries a reachability
line: if the sandbox's egress policy blocks GitHub, the direct path cannot connect, and it prints the
one `sbx policy allow network` command that clears it rather than silently falling back to the proxy.

Prefer not to run an App? Store a fine-grained PAT per repository under **Settings → GitHub & keys →
Without a GitHub App** (or on a repo's own card under **Repositories**). It is folded away because it
is the path that asks for more than one credential — the App asks for one key and derives every token
from it. One repository per token, enforced when stored *and* when used — a token covering three
repos is write access to three repos for whichever box receives it, because the credential helper
runs inside the box as the agent's own uid and can route but never contain. Cross-repo reads of
private repos then need an optional read-only PAT, which nothing prompts for.

#### Asking to write another repo

A push elsewhere has no credential of this box's to make it with, and the push itself files the ask:

```
$ git push origin HEAD
skein: asked to write someone-else/private. Request 20260815-101122-4711 is pending approval in the cockpit.
skein: this box holds a GitHub token for its own repository only, so nothing here grants you
someone-else/private. That is deliberate, not a misconfiguration — re-authenticating, switching
to SSH or editing the remote will not change it.
error: failed to push some refs to 'github.com:someone-else/private.git'
```

That comes from a `git` shim, and it is **the message, not the boundary**: it never blocks, it files
the ask and then runs the real git, so the push gets whatever answer it would have got without the
shim, and an agent calling the real binary directly gets the same one. Everything that is not a push
execs the real git on the shim's first line, and any surprise on the push path execs it too — the
token is what isolates, so the shim can afford to be timid. **What the shim does not do is promise a
particular outcome** — it states only that no *write* grant for that repo exists in this box, not
what the push returns. For a scoped box the push now goes direct and is bounded by the token, so a
repo the box holds nothing for is refused by GitHub itself (SKEIN-548, closed for the git path); the
shim also adds the `sbx policy allow network` hint if the direct connection is blocked outright.

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

`skein doctor` reports the resolved registry, the bind address, the cockpit URL, whether `git` and
`curl` are on `PATH`, and the warden (`cmd_doctor`, `src/bin/skein.rs`) — run it first if something
looks off. Most settings live in the cockpit, under Settings, and are saved to
`~/.skein/config.json`; the variables below are the rest, plus the few that override a setting for
headless use. Set them inline, or drop them in a **`.env`** (loaded automatically at startup from
the cwd upward; real env vars still win). Copy [`.env.example`](../.env.example) to `.env` and you
can just run `skein` / `skein-server` with no prefix:

| var | what | default |
|-----|------|---------|
| `SKEIN_HOME` | skein's own dir (`repos.json`, embedded `kit/`, cloned repos) | `~/.skein` |
| `SKEIN_REGISTRY` | full path to `sandboxes.json` | (see resolution above) |
| `SKEIN_SHARED` | shared store dir (`/sandboxes.json` appended) | — |
| `SKEIN_ADDR` | server bind address | `127.0.0.1:7878` |
| `SKEIN_ALLOWED_ORIGINS` | extra WS origins to allow (comma-sep hosts). Always allowed: loopback, any `*.ts.net` host, and any Tailscale IP — CGNAT `100.64.0.0/10` and `fd7a:115c:a1e0::/48` (`origin_ok`, `src/bin/skein-server/door.rs`) | — |
| `SKEIN_SELF` | this box's vmid (kept `live` when its `lastSeen` is quiet) | `$SANDBOX_VM_ID` |
| `SKEIN_REPO` | working directory for the tools skein runs (`git`, host-side diffs) **and to launch/attach from** — so relative `*_CMD` paths resolve here | cwd |
| `SKEIN_LAUNCH_CMD` | launch-a-box template — `{branch}`/`{name}` substituted; relative to `$SKEIN_REPO`. **Optional**: unset, skein builds the launch itself (below), so the repo needs no launch script | _(native builder)_ |
| `SKEIN_AGENT` | sbx runtime override; must match a registered Skein runtime adapter | repo/default runtime |
| `SKEIN_ATTACH_CMD` | agent-terminal attach — `{name}`/`{dir}` substituted | the box's `skein-agent` tmux session (`agent_session_name`, `src/runtime.rs`) |
| `SKEIN_SHELL_CMD` | shell-terminal command (the **Shell** tab) — `{name}`/`{dir}` substituted | the box's `skein-shell` tmux session (`shell_argv`, `src/sandbox.rs`) |
| `SKEIN_LS_CMD` | fleet-liveness probe (run via `sh -c`); must emit the `sbx ls --json` shape. A running box shows `live` regardless of `lastSeen`; on any failure skein falls back to `lastSeen` | none: inside the fleet `sbx ls` cannot be asked, and boxes are read from their placement records (`src/sbx.rs`) |
| `SKEIN_MERGE_METHOD` | merge strategy for a merge skein makes through GitHub's API: `squash`, `merge` or `rebase` | `squash` |
| `SKEIN_RESUME_CMD` | one-click "continue" template — `{name}`/`{prompt}`/`{runtime}` substituted | runtime adapter's native headless resume |
| `SKEIN_AI` | `off` holds AI enrichment off whatever Settings says; an environment variable can only turn a switch off, so `on` does nothing (Settings → Boxes switches it on) | unset |
| `SKEIN_AI_MODEL` | model for every AI call, over Settings' **Review model** | `claude-haiku-4-5` |
| `SKEIN_CLAUDE_BIN` | path to the `claude` CLI (for AI calls) | `claude` |

> The `*_CMD` templates run via `sh -c`; values you substitute are shell-quoted, but only
> point them at trusted commands.

**One credential, doing every job it can.** skein needs GitHub in two places: inside a box (push,
open a PR) and on the host (the review queue, PR diffs, merges). The box side has always been a
choice of three — a GitHub App, a per-repo PAT, or the account token. The host side used to be a
fourth: it went through the `gh` CLI, which only knows its own login, so choosing a PAT still meant
running `gh auth login` as well — and on Linux `gh` keeps that token in the system keyring, so a
queue polling every three minutes asked for a password every three minutes.

The host now talks to the API itself, and every call about a repository resolves its credential
the same way (`credential_for_repo`, `src/gitgate/credentials.rs`): the token you stored for that
repository — the one its boxes push with — then an exported `GH_TOKEN`/`GITHUB_TOKEN`, then the
read token in Settings for reads only, and only then a `gh` login if the host has one. The queue,
a verdict, a merge and the mirror of one repository therefore use the same token. Everything but
`gh` is read again on each call, so a token replaced in Settings is used at once, without a
restart; `gh` is asked once per run, because asking it can unlock a keyring. `skein doctor` says,
per repository, which one reads and which one writes.

A GitHub App is the one credential that cannot cover the host side: an installation token
authenticates an installation, not a person, so it cannot say whose review a PR is waiting on. The
queue reports that rather than listing nothing.

**Sizing the fleet.** Every box runs inside one sbx sandbox, and its memory, CPUs and disk are
fixed when that sandbox is created — sbx has no resize, so changing any of them means making a new
one. You choose them on the `sbx create` line in the README's [Getting
started](../README.md#getting-started), and its table is where the sizes and their defaults are
stated. Settings → Fleet shows what this machine has, and the lines to run on the host to make the
sandbox again at other numbers; skein cannot do that from inside the sandbox it would be destroying.

**How skein reaches a box.** skein runs inside the fleet sandbox, so it reaches a box by entering
the box's namespace directly (`Place::exec`, `src/place/mod.rs`). There is no `sbx exec` hop to fall
back from, and no transport to choose (`reach`, `src/place/argv.rs`).

## How it fits the sbx setup

skein **reads** the shared store the sandboxes already maintain (`sandboxes.json`,
`mailbox/`) and **drives** `git` and GitHub's API. It owns no state the bootstrap owns. `sbx` is
host-only, so skein inside the fleet runs none of it; what needs `sbx` is a line it shows a person
to run, and a read-only `ls` works from anywhere with `$SKEIN_REGISTRY` set.

## Terminal responsiveness (an invariant worth guarding)

The embedded terminal shares skein-server's async runtime with everything else: the SSE
fleet stream, every JSON handler, and the WS↔PTY bridge are all tasks on the same tokio
workers. So **no request handler or stream may run blocking work inline** — anything that
shells out (`git`, `curl`, a box's `tmux`) or touches the filesystem must go through
`tokio::task::spawn_blocking`. A single inline blocking call freezes its worker for the
whole duration and starves any terminal websocket scheduled on it: keystrokes stop echoing
until it returns.

The subtle case was the 2s fleet-snapshot tick (`load_views` — `sbx ls` + a per-box `git`,
1-2s for a busy fleet). Run inline it caused a periodic "typing lags **only when the box is
idle**" freeze — mid-stream the output flood hid the gap, but at rest a lone keystroke's
echo waited out the stall. Every `skein::` call in `src/bin/skein-server/` is on `spawn_blocking`
for this reason; the regression is guarded by
`slow_fleet_snapshot_does_not_starve_concurrent_requests` in `tests/server/routes.rs` (pins the
server to one worker, makes `load_views` sleep, and asserts a concurrent request isn't
blocked). Separately, each accepted connection sets `TCP_NODELAY` (our own accept loop, not
`axum::serve`) so Nagle's algorithm can't coalesce single-keystroke packets. Both matter:
the socket must be fed promptly *and* flushed promptly.
