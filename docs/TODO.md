# TODO

Work that is known, wanted, and not done. Ordered by what hurts most, not by effort.

Anything with a diagnosis attached has had it verified — the lead is the useful part, so it is kept
with the item rather than rediscovered.

---

## Broken now

### A mirror is made once, from a checkout that in-fleet does not exist

**This is why `git fetch` does nothing in a box, and it is the first thing to fix.** Surveyed on the
live fleet, 2026-08-28 — eleven directories under `$SKEIN_HOME/repos`, and *not one* has the `work`
checkout its `source_tree` names:

| repo | `work` | mirror | the mirror's `origin` |
|---|---|---|---|
| skein | missing | yes | `/Users/you/work/.../thing/skein` — a host path, not mounted |
| ERA | missing | yes | `/Users/you/work/personal/AI/ERA` — same |
| gadget-demo | missing | yes | `/Users/you/work/.../gadget-demo` — same |
| agent-memory-consolidation | missing | missing | nothing to clone from |
| bridge-a-b, chassis, lattice | yes | yes | `git@github.com:…` — no forwarded agent in here |
| mothership, r, slate, sync | mixed | missing | — |

Two separate faults, and they compound:

- `clone_mirror` prefers `source_tree` over `source` — *"the checkout when there is one … and the URL
  when there is not"*. That was right on a host, where the checkout was the only source for an
  adopted repo. In-fleet the checkout is the one thing that is never there, so the preferred branch
  is the dead one.
- `ensure_mirror` returns early on `mirror_is_made`, so an **existing** mirror's remote is never
  reconciled with the repo's `source`. skein's own `source` is already
  `https://github.com/prateekreddy/skein.git`; its mirror still pointed at the host checkout the
  mirror was made from months ago, and no fetch since has had anywhere to go.

Repaired by hand for `skein` only (`git remote set-url origin`, then a fetch that brought 21
commits). The other eight are untouched, and four of them have no remote URL recorded anywhere —
their `source` *is* the host path — so those need a person to say where the code lives.

**Watch the prune when fixing this.** `fetch_mirror` runs `git remote update --prune` against a
refspec of `+refs/*:refs/*`, so repointing a mirror at a remote that does not carry
`refs/sandboxes/*` or `refs/stash` deletes them. Doing exactly that on skein's mirror dropped four
refs; three were already in `in-fleet`, and `refs/stash` (`5c3a2fe`, a WIP from 2026-08-05) was
reachable from nothing else and had to be put back by sha. The host checkout still holds it, which
is the only reason that was survivable.

### The fleet sandbox does not stay up

Every `sbx exec` in the install session printed `Sandbox skein-fleet started successfully`, which
means it had been **stopped** each time. That is the whole of "the cockpit works and then is not
reachable a few seconds later": nothing is crash-looping, because `server-doorway.py` holds the
listening socket across a server crash and restarts the server behind it. The machine underneath
goes away.

Unconfirmed hypothesis: the fleet is a `shell` sandbox with nothing attached — `sbx create --help`
says *"Use `sbx run --name SANDBOX` to attach to the agent after creation"* — and sandboxd reaps it
as idle. A detached tmux inside does not count, because sandboxd watches the agent. The workaround
being tried is holding it with `sbx run --name skein-fleet` in a host tmux.

If that is the cause it is a hole in the in-fleet premise rather than a `bootstrap.sh` bug:
`docs/delivery.md` assumes the fleet sandbox outlives every exec. Whoever confirms it should decide
what the model does — an attach skein documents, a sandboxd setting, or something inside that keeps
the agent alive — and write the answer into `delivery.md`.

Chased first as a memory ceiling. That was wrong, and the disproof is that it stops while idle.

### Ceilings are computed from a field nobody sets

`memory_plan` derives every cgroup ceiling from `fleet_memory` and **never** from what the sandbox
actually has. `fleet_memory` defaults to a hardcoded `26g`, and nothing writes it when a person
creates the sandbox by hand. Its own comment on the reserve is the reason this matters:

> With no swap, overshooting is an instant kill rather than a slowdown, and the victim is chosen
> across the whole VM — so the cost of being wrong is a dead sandbox, not a slow one.

So on any host under 26 GB the boxes' cap never binds and the VM's own limit is hit first. Compare
`head -1 /proc/meminfo` inside against `fleet_memory`. The fix is to read the sandbox's real memory
rather than trust the field — which is also the honest answer to the resource entry above, since
`configured_field` cannot tell a decision from a fallback for a number nobody was asked.

### An upgrade cannot change the supervisor's environment

`bootstrap.sh` sends `SIGUSR1` when a session already exists, so anything baked into the supervise
string — `$SKEIN_HOME`, `$SKEIN_IN_FLEET`, the port, the doorway's argv — is ignored on every
upgrade. It cost two manual `tmux kill-server` steps in the install session. The script should
recreate the session when that string has changed, rather than signal the one carrying the old one.

### The API token is plaintext at rest

Analysis done, not built. `tests/isolation_bwrap.rs` (SKEIN-219) records that on a volume-mounted
fleet — this deployment — `credentials/`, `github-pats/` and `api-token` were once a `cat` away from
every box; the control is a mount-*ordering* rule in `box-session.sh`, guarded by a test that
**skips itself** when bwrap cannot make a user namespace.

Agreed design: store the digest only; migration hashes the existing plaintext in place and unlinks
it so nobody is locked out; browser sessions survive restarts because the digest is stable; rotation
is host-initiated, which an in-sandbox actor cannot originate because there is no `sbx` in there.
`sha256` is already in the tree.

Be honest about the limit: this raises a silent **read** to a loud **write**. Anything with genuine
full sandbox access is past every boundary skein has, and the neighbouring credentials must stay
usable plaintext regardless.

The token used during the install session was pasted into a chat transcript and should be rotated.

### Nine repos still point at somewhere a box cannot reach

Five are `git@github.com:…`, and `repos.rs` says SSH works only if the host agent is forwarded with
the key loaded, calling HTTPS *"the no-setup path (proxy-injected creds)"*. Four more are adopted
from host paths that are not mounted. Same survey as the mirror entry above.

**`skein add` with an existing id replaces the entry, it does not edit it** — `store`, `agent`,
`plane_project`, `read_prs` and `review_queue` are all reset unless passed. Read `repos.json` first
and carry them across.

### `sbx`'s verb list is quoted from memory, and one conclusion drawn from it is wrong

`fleet.rs` reasons from *"its whole verb list is `login run ls stop rm create exec cp ports`"*;
`sbx --help` shows fifteen more. The load-bearing conclusion is *"sbx has no unpublish verb, so
every mapping is permanent"*, which justifies the port-burning dance in `ensure_fleet_agent_port`.
`sbx ports --help` does take `--unpublish`. Corrected in the README only — the claim is still made
in this file twice (under "Leaked port mappings" and under the served-cockpit entry),
in `docs/inventory.md`, and in `src/fleet.rs`.

### The fleet's memory, CPUs and disk are chosen by silence, and cannot be changed afterwards

Every other unfixable-at-create decision has a surface: mounts get a README section, the create line
is put in front of a person before it runs. The three resource numbers get nothing, and they are the
**least** revisitable of the lot — sbx has no resize, so changing one destroys the sandbox and with
it every box's VM-local checkout (`create_env`'s doc comment, and `Config::fleet_disk`'s).

What defaults today, verified:

| value | when skein builds the line | when the README's line is pasted |
|---|---|---|
| memory | hardcoded `"26g"` (`config::default_fleet_memory`) | sbx: half the host, capped at 32 GiB |
| CPUs | `host_cpus_less_one()` (`fleet.rs:2644`) | sbx: **every** core |
| disk | absent ⇒ no `DOCKER_SANDBOXES_ROOT_SIZE` | sbx: 20 GB |

`"26g"` is an overcommit on any host under 26 GB, and `config.rs:391` already says so about itself:
*"`fleet_memory` reads back `26g` on a machine nobody has ever configured, because that is this
build's default, and a proposal that deferred to it would propose a number chosen for a different
laptop."*

**The mechanism is already written and unused.** `config::configured_field()` (`config.rs:396`)
exists to tell "somebody decided this" from "this build's fallback", and its own doc says it was
written for sizing a new fleet. Nothing calls it for `fleet_memory`, `fleet_cpus` or `fleet_disk`.

The fix asked for is that no resource nobody chose ever reaches a create: `create_argv` always names
`-m` and `--cpus` and `create_env` always names the disk, and where `configured_field` says nobody
has decided, the surface refuses to render a runnable line and says which three numbers are wanted
instead. Note the in-fleet wrinkle — `available_parallelism()` inside the sandbox reports the
sandbox's cores, not the host's, so in-fleet skein cannot propose host numbers at all and must ask.

Interim, done: the README's install line now names all three explicitly with a table of what each
one costs if omitted.


### A second server on :7879 opens no boxes and never gets agent v2 — unexplained

Reported: two `skein-server` instances on one host, 7878 working and 7879 unable to open any box,
with its transport never reaching agent v2. "Everything seems broken."

**Not yet diagnosed, and the two symptoms probably have one cause.** Box creation does not touch
GitHub auth at all, so the credential work is unlikely to explain it; what explains both at once is
the two servers not sharing state or not being the same build.

Three candidates, cheapest first, each with the command that settles it — run for **both** pids:

1. **Different `$SKEIN_HOME`.** Then 7879 has its own empty `repos.json` (so every launch is
   refused: a box belongs to a registered repo) and its own missing `fleet-agent.port` (so
   `agent_target()` is `None` and the transport is `sbx exec` whatever the fleet is doing). One
   divergence, both symptoms.
   `tr '\0' '\n' < /proc/<pid>/environ | grep -E '^(SKEIN_|HOME=|PWD=)'`
2. **Different build.** `skein_exe()` resolves the `skein` beside the running `skein-server`, so two
   checkouts are two binaries. An older one predates `fleet_agent` defaulting to true and would show
   the transport as *off* rather than *not answering* — which is the difference worth reading on the
   gauge strip.
   `ls -l /proc/<pid>/exe` and compare the two paths and mtimes.
3. **Different user.** `~/.skein` and sbx's own state are both per-user.
   `ps -o user= -p <pid>`

Then `skein doctor` in each server's environment: the sandbox line, the transport line and the
github-token line are each one sentence and between them cover all three.

**One real regression to rule out while we are here.** Removing the `gh` dependency also removed
`gh auth token` as a credential source. A fleet whose *only* GitHub credential was `gh auth login`
now has none, so its review queue stopped working today — the queue says so rather than showing an
empty list, but it is still a working setup that this broke. If that is what happened here, the fix
is a last-resort fallback: use `gh auth token` when it exists and nothing else is configured, which
keeps `gh` optional without punishing the people who already had it.


### The fleet agent still is not installed — the cause is now testable

`fleet-agent.py` in the sandbox is still the Aug 7 v1 and nothing answers on 8317, across two
server restarts, while the launcher beside it is rewritten on every box start. In `ensure_fleet` the
agent install runs *immediately before* the launcher install and is non-fatal, so "launcher fresh,
agent stale" is the signature of the agent step failing or being skipped.

The leading explanation, now fixed but not yet confirmed as the cause: `load_config` silently
discarded a `config.json` it could not parse and returned defaults, in which `fleet_agent` was false —
so `heal_fleet_agent` returned on its first line with no message, while the file said `true` and was
right.

That default is now **true**, which removes this whole family of causes rather than only reporting
it: an unreadable config, an absent one, and a partial write all leave the transport wanted. If the
agent is still not installed after this ships, the cause is downstream of the setting and the
server's `skein: the in-sandbox agent is not serving (…)` line names it.

**Settle it after the next deploy**: `skein doctor` now prints the parse error above the settings
line, and the board's transport row shows `settings / unreadable` in place of `off`. If neither
appears and the transport row is amber, the config was fine and the failure is downstream — the
server's `skein: the in-sandbox agent is not serving (…)` line names it.

### Codex is parked, deliberately

`sync-install.sh` still writes Codex's `[mcp_servers.sync]` TOML when a token exists, but nothing
about the Codex path has been verified since the plugin took over, and the vendored skill it depends
on is now skipped on any box where the plugin installed. Revisit when Codex is actually used —
including whether the token is still worth minting at all.

### The installed sync plugin is 0.2.0, whose lease monitor keeps nothing alive

**Not an upstream report any more — upstream fixed it.** `plugin/bin/sync-monitor` on `main` now
resolves through a shared `sync-paths.sh` (`sync_session_id` → `CLAUDE_CODE_SESSION_ID`), and its
comment describes the same defect in the same terms: it "used to read CLAUDE_SESSION_ID — a variable
Claude Code does not set — and fall back to `default.watch`, so it polled a file nothing ever writes
and kept nothing alive, quietly, forever."

What is left is a **version** problem, and it is live. The plugin installed in these boxes is
**0.2.0** (marketplace at `392d6ab`), whose `sync-monitor` still has
`WATCH_FILE="$TOKEN_DIR/${CLAUDE_SESSION_ID:-default}.watch"` and no `sync_session_id` at all. So the
lease keepalive in every box does nothing, silently — the failure its own README warns about, "a
guard nobody knows is disabled". Upstream knows: `sync-monitor` calls out 0.2.0 by name as predating
the fix, and `8fac188` makes a monitor left behind by an update stop rather than pretend.

**Done.** This box updated to 0.4.7 (marketplace `c573fd1`) and `sync-install.sh` now refreshes the
marketplace on every start, so no box can freeze on a version again — the marker goes back to
answering only "does this box have the plugin", which is the one question it should ever have
answered. Every other box picks it up on its next start.

The vendored skill needed nothing: the sixteen commits between `e113f18` and `c573fd1` are all
server, lease and monitor logic (reconciliation about to return every finished item to the pool, one
410 blinding the monitor for 17 minutes, a credential that aged out reading as a takeover) and
`plugin/skills/` is byte-identical across the range. So the submodule pin can be bumped whenever
convenient without re-vendoring or touching the always-on block.

### Shared login: the poisoning is fixed, the recovery is on box-restart cadence

The cause was that credentials synced by mtime alone, and a logout leaves a *newer* file than the
login it replaced — so one logged-out box propagated its emptiness to the sandbox and from there to
everything else. Fixed: a file only competes if it carries a login.

Two more of the same shape, found by surveying the live fleet on 2026-08-10 and now fixed:

- **the host side never got that fix.** `sync_fleet_login` tested "the sandbox has a file", and a
  husk is a file — so a logged-out sandbox overwrote the copy in `fleet-home`, which exists solely
  so a rebuild can restore it. The decision is now `login_move`, tested rather than inline.
- **mtime was still the tiebreak between two real logins**, and it answers the wrong question: it
  says when a file was *written*, not which credential is better. A box that starts rewrites its own
  copy, so it holds the newer mtime whether or not its token is the older one. Ordering is now by
  `expiresAt`, which can only ever prefer the longer-lived credential; mtime remains the tiebreak for
  shapes that record no expiry (codex `auth.json`).

The survey itself is the useful artefact — nine boxes held **nine distinct token pairs**, so the
fleet does not share a live credential in steady state and is not meant to: each box diverges the
moment it first refreshes, from one seeded login. What was broken was four boxes that had not
started since their token lapsed (`bridge-a-b-master` a husk from Aug 4, `chassis-statement-
parsing` and `example-box-7` expired ~Aug 7, `example-box-1` Aug 9) — the restart cadence below, not the
propagation rule.

What remains is a lag rather than a fault. Healing happens when a box **starts**, so the fleet
recovers as boxes restart and not before. If a login is needed sooner than that, restarting any
logged-in box pushes its credentials up and every later start picks them up.

Worth considering, not yet done: heal from the whole fleet rather than only from the sandbox's copy
— on start, if neither this box nor the sandbox carries a login, take the newest one that does from
`/boxes/*/home/.claude/.credentials.json`. It would repair everything from a single box start
instead of needing the *right* box to restart. It reads other boxes' private homes, which is
consistent with the stated model (boxes are isolated from each other's state, not their identity)
but is still a widening, so it wants a decision rather than a commit.

The `mcpOAuth` half is fixed: the sync now merges the login keys instead of copying the file, so a
box keeps its own per-repo MCP grants and never receives another repo's.

---

## Voice — the rest of it

The mouth and the ear are both built. What is not:

### Try it for a day and cut what annoys

Two defects are already fixed and worth knowing about when judging the rest: `waiting` was missing
from both voice paths, and the mouth only ever spoke at the *instant* of a change while you were
away — so a box that turned while you were looking at the board was never announced at all.
`tests/ui/voice.mjs` covers both.

The verb set is a guess until it is used: `yes / continue / next / show me / open / what needs me /
continue all`, plus `tell it …`. Expect the ordering of `voiceTarget` to be the part that feels
wrong first — it prefers the box the mouth last named, then the selected row, then the only one
owed an answer.

Deliberately absent, and worth keeping absent unless the daily use argues otherwise: stop, merge,
ship, destroy. A confirmation is not the answer for those, because the point of the ear is that you
are not looking at the screen.

### Settle whether on-device recognition actually engages

A ten-minute probe page: `SpeechRecognition.available({langs:['en-US'], processLocally:true})`, then
`install()`. Decides whether voice works with the network down. Chrome has an open macOS bug in this
path; Safari routes through Apple's engine. Does not change the design either way.

### Intent fallback for anything the grammar misses

Transcript + board state → Haiku via the existing `claude_oneshot` in `ai.rs` → `{action, box, args}`.
Pennies a day. Must feel like a distinct, slower path so the latency reads as deliberate.

---

## Cross-session messaging — one bind away from working

Claude Code v2.1.224+ ships `SendMessage` / `ListAgents` between sessions. Measured inside a fleet
box on 2026-08-09:

- the **transport is already shared**: `/run/user/1000/cc-socks/` holds every box's inbox socket,
  four of them visible from inside one box;
- the **discovery registry is not**: each session registers in `~/.claude/sessions/<pid>.json`, and
  `~/.claude` is bound private per box — so `ListAgents` reports "No reachable agents" while the
  sockets it would use are sitting right there.

Sharing `~/.claude/sessions/` alone (not the rest of `~/.claude`, which holds credentials and
history) would give every box in the fleet native box-to-box messaging with no skein code. That is
close to what the cross-box mailbox already does, so the question is whether it replaces the mailbox,
complements it, or is not worth the extra shared surface.

If taken up: pass `--name "$SKEIN_BOX"` when launching, or every box derives the same name from its
`tree` directory. And check `crossSessionInbound` — boxes running with permission prompts bypassed
hold each other's messages for approval by default, which would look like silent loss.

Docs: <https://code.claude.com/docs/en/cross-session-messaging>

---

## The HTTP transport

### It has not yet faced a real stall

The only test that matters for it. Next time the daemon hangs:

```sh
curl -s http://127.0.0.1:$(cat ~/.skein/fleet-agent.port)/health
```

An answer means the VM is alive and the daemon path is stuck — which is evidence for
docker/sbx-releases#163, currently resting on inference.

### Leaked port mappings

51957–51959 on `skein-fleet`, from before the agent bound `0.0.0.0`. Harmless: sbx has no unpublish
verb, and the reuse logic skips them because they never answer.

### `copy_guest_file` still uses `sbx exec`

Deliberate — it is a download, and `/exec` would buffer a multi-GB bundle in memory at both ends.
Revisit only if a streaming download endpoint earns its keep.

---

## Migration and cleanup

### Retire the per-VM box model — **done**

The fallback that carried it was one line: `place_of` answered "a sandbox named after the box" for any
name with no placement record. That was skein's original model, and it outlived it as a *guess* — any
name at all resolved to a `Place`, so a box whose start had failed, and a sandbox skein never created,
were both addressed as though skein owned them. `place_of` now returns `None`, and every caller says
what it means by an unplaced box.

Removed with it: `skein migrate` / `skein recover` (migration existed only to move a per-VM box in,
and `resize_fleet` carries fleet boxes by its own copy-out), the two `sbx create` paths, the
`$SKEIN_KIT`/`$SKEIN_STORE` single-repo launch mode, `persistent_launch_command`,
`resolve_under_repo`, `launch_store`, `remint_tracker_token`, `write_restore_launch_spec`, and the
`transcript_is_vm_local` parameter that only a migration ever set true.

**The stranding this was blocked on is now real, and deliberately accepted.** A per-VM box on disk is
no longer reachable through skein, and there is no `skein migrate` to rescue it. It is not lost — the
sandbox still exists and `sbx exec -it <name> bash -l` still enters it — and the cockpit says exactly
that when you try to open one. Copy anything wanted out with `sbx`, then `sbx rm` it.

A note that survives the removal: a legacy box had real root; a fleet box cannot (see the sudo shim in
`box-session.sh`). What a fleet box has instead is a way to *ask* — the shim files a package request
its owner approves in the cockpit, and the install serves the whole fleet. Narrower than "no root"
suggests, but not the same capability and never will be.

### Sandboxes skein did not create

Related, and fixed alongside: `load_views` takes every name from `sbx ls`, which cannot say which
sandboxes are skein's. Unrelated `sbx` boxes therefore appeared on the board as rows with no branch,
no signals and nothing that worked — a first run on a machine with a couple of them looked like a
fleet full of broken boxes. They now carry `foreign: true`, are hidden by default, and are shown by
the `foreign:` filter term; the empty board offers a link to it when any exist.

### The docs still describe one sandbox per box

**Done for `README.md` and `ARCHITECTURE.md`.** The latter mattered more than a tidy-up: its "hard
constraint" section asserted boxes were separate microVM kernels and ruled out SQLite across boxes
on that basis. They share one kernel now, so the constraint was forbidding a design that is
available — the file-based mailbox stays, but on the honest grounds (mail must outlive a box that
never read it), not a kernel boundary that no longer exists.

`docs/self-sufficient.md` still describes the old `sbx create --clone` per-box launcher. It reads as
a design note about how the launcher was derived rather than as current reference, so it wants a
decision — update it, or mark it historical — rather than an edit.

### Merge `modules-and-shared-sandbox` into `master`

### `agent-memory-consolidation` has no `origin` remote

### The cockpit's white background has never been diagnosed

---

## Verification gaps

### `cfg!(test)` is false in `tests/`, so process-global gates leak between integration tests

The library an integration test links was built without `cfg(test)`, so every "no gate under test"
escape inside `src/` is inactive there. A warm gate then serves one test the previous test's answer
— immediately, while refreshing behind the caller, which is the behaviour that stops the board
blanking and is worth keeping.

`fleet_liveness` is handled (`forget_fleet_liveness`). The other gates — `FLEET_GATE`, `DISK_GATE`,
resources — have the same exposure the moment an integration test touches them.

### The browser smoke test cannot run in a box

`tests/ui/smoke.mjs` needs chromium's system libraries, and a box has no working `sudo` to install
them — the shim above explains why. It downloads but will not launch. Run it on the host after any
`src/web/index.html` change.

There is now a way through this, untried at the time of writing: a box can *ask* for those libraries
(`sudo apt-get install libnss3 …` files a request; the cockpit's package panel approves it) and the
install lands in the sandbox for every box. If that works, `smoke.mjs` becomes runnable in a box and
this entry can be closed rather than worked around. Worth doing deliberately, because it is also the
first real exercise of the request path end to end. Note the browsers themselves are **not** the
problem — `~/.cache/ms-playwright` re-downloads with `npx playwright install chromium` and was
deleted once already to reclaim a full disk.

This is not academic: it is why the voice shipped with `waiting` missing from both of its paths and
spoke nothing while boxes sat waiting. `tests/ui/voice.mjs` closes that particular hole by lifting
the pure sentence-building functions out of the page and running them in plain node, which works in
a box — but everything about the page that needs a *browser* is still only covered on the host.
Anything testable without one belongs in the node test, precisely because that is the one that gets
run where the code is written.

### Stopping a served cockpit — **done**, and the verb is `skein fleet-serve --stop`

A flag on the verb it undoes rather than a new top-level one, because `skein stop` already means
"stop a box"; a second top-level stop meaning something else would be the ambiguity, not the fix.

The interesting half was what "stop" must *not* do. `fleet::stop_server` ends the tmux session,
which ends the doorway — and a doorway that lets go of the port reopens exactly the hole it exists
to close, because `sbx` has no unpublish verb and the host mapping outlives whatever holds the
port. So `fleet::stop_serving` takes the *server* away and leaves the door standing, using a state
the doorway already has rather than a mechanism beside it: with nothing executable at
`server_path()` it holds the socket and waits. That is the create-time state, so a stop returns
the fleet to a shape it has already been in.

`stop_server` is still called only by tests, and correctly so: it is a teardown, and the only
thing that should want it is a fleet being destroyed — which takes the sandbox with it anyway.

The leaks it hid, for the record — **two of them, and only one was a supervisor.** Both
supervisors said `while true`, so a fleet deleted out from under either left a bash restarting a
python script that no longer existed, twice a second, for ever: 105 doorway loops and 1 agent loop
were alive on one box. They go through `fleet::supervised` now, which ends when the script it
restarts is gone — named by `fleet::a_supervisor_stops_when_the_script_it_restarts_is_gone` and,
end to end against real tmux, by
`fleet_move::a_supervisor_whose_fleet_is_gone_stops_rather_than_restarting_for_ever`.

Beside them sat **126 orphaned box sessions**, which that fix does not touch and which were found
only by counting what was left after it. A box outlives the skein that started it by design, so
killing the server does not end one and deleting its socket does not either — tmux holds the open
file and the session sits idle for ever. `tests/ui/onboarding.mjs` kills its boxes' tmux servers
before it deletes their fleet; measured at one leaked session per run before, none after.
