# TODO

Work that is known, wanted, and not done. Ordered by what hurts most, not by effort.

Anything with a diagnosis attached has had it verified — the lead is the useful part, so it is kept
with the item rather than rediscovered.

---

## Broken now

### The installer condemns the fleet it just built — **fixed in the README, 2026-08-28**

Kept because the mechanism is not guessable and the same foot-gun is still live for anyone who
reaches into the fleet by hand.

sandboxd auto-stops a sandbox roughly 30s after a session **disconnects** from it. Not idleness:
a sandbox nothing has ever attached to runs indefinitely. `sbx exec` is a session, so the install's
third line — `sbx exec -i skein-fleet bash < bootstrap.sh` — arms the timer as it finishes, and the
fleet stops about 35 seconds after the install "succeeds". Nothing on the host restarts it, because
in-fleet skein has no host process at all.

Measured against a throwaway sandbox, three minutes per phase:

| phase | result |
|---|---|
| created, never attached | ran the whole three minutes |
| one `sbx exec … true`, returned 09:53:48 | last seen running 09:54:21, stopped by 09:54:27 |
| `sbx run -d` | ran the whole three minutes |

The fix is two lines after the bootstrap — `sbx stop`, then `sbx run -d` — which returns the
sandbox to the never-attached state permanently. **There is no setting to turn this off**:
`sbx daemon` exposes only `log-level`, `restart`, `start`, `status`, `stop`, and `sbx policy` is
network rules. Undocumented, too — Docker's own docs say a sandbox "does not stop or remove the
sandbox VM" when a session ends, which is the opposite of what it does.

**Why this only appeared in-fleet, and it is not the reason it first looked like.** The first
theory here was that host-driven skein had been poking `sbx` every 30s through the board's disk
measurement and that moving in-fleet removed the poke. That is wrong, and the disproof is in this
tree: `agent_target` reads the *recorded verified* port (`fleet-agent.port`, `59461` on this
fleet), so `Place::bytes` took every call over HTTP through `via_agent` and never reached
`bytes_via_sbx`. Host-driven skein with the agent made no `sbx` calls either. It survived because
HTTP to a published port is not a session, so nothing ever armed the timer — the sandbox sat in
the never-attached state for its whole life. The in-fleet install is the first thing that attaches
and leaves.

**The general shape, for the eleventh time:** true on the host, silently false in-fleet. Here the
thing that was true was not any line of skein's code — it was that nobody had ever needed to
`sbx exec` into the fleet, because skein was outside it.

### A rustup shim answered the toolchain gate — **fixed, 2026-08-29**

Kept because the gate looked right, and reading it will not show why it was not.

The install cloned, started the build, and stopped on rustup's own words:

```
error: rustup could not choose a version of cargo to run, because one wasn't specified
explicitly, and no default is configured.
```

The gate was `command -v cargo`. Three lines above it, `bootstrap.sh` points `RUSTUP_HOME` at the
private toolchain under the fleet root — which is empty until the gate fills it. From that line
onward, "a cargo is on the PATH" and "a cargo here can build" are different facts: **every rustup
shim keeps answering `command -v` while resolving against a rustup home with no default in it.**
Two shims do it. The image's `~/.cargo/bin/cargo`, and the one an interrupted earlier run of the
bootstrap left under `$CARGO_HOME/bin` — which is the state that cannot be got out of by running
the install again, because rustup-init that finds a rustup to update leaves the toolchains alone
and never honours `--default-toolchain`.

Three lines, and each is load-bearing (each was reverted, and the test fails):

* the gate runs `cargo --version` instead of asking `command -v`;
* `hash -r` after the install, because bash remembers where it found `cargo` and rustup has just
  written a better one *earlier* on the PATH — without it the shell keeps running the shim and the
  install appears not to have happened;
* `rustup default stable` when cargo still cannot run, which is the half-installed state's repair.

Plus a refusal before the clone that names the private toolchain, since a working `~/.cargo` is
exactly what makes this confusing.

`fleet::tests::a_rustup_shim_that_cannot_choose_a_toolchain_is_not_a_cargo` has both states, and
in both of them the cargo that can build is written by the block under test and by nothing else.

**The general shape, for the twelfth time:** true on the host, silently false in-fleet. Here the
thing that was true was `command -v cargo` — right on any machine that has not just redirected
`RUSTUP_HOME` out from under it.

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

**And the restart is only half of it — the other half is now fixed, 2026-08-29.** A stop is
survivable in principle: `/boxes` is the sandbox's own disk and it persists, so the whole install
is still there afterwards. What was not survivable is that *nothing put the door back*. Measured
inside the live fleet:

```
$ ps -p 1 -o comm=          tini
$ ls -d /run/systemd/system  (absent)
$ command -v systemctl cron crond   (nothing)
```

No init to hook. So every restart produced the same picture — `uptime` two minutes, everything
installed and intact, no tmux session, no doorway, :7878 unbound, the host's port mapping
connecting to nothing — and the only cure written down was to re-run the installer: a fetch, a
build and a minute, to redo four lines that were already right.

Those four lines are now `bootstrap.sh`'s `start-door.sh`, installed beside the binaries, so the
cure is `sbx exec -i skein-fleet /boxes/.skein/start-door.sh` and costs a second. It works with an
empty environment — it reads the volume from the `skein-home` marker rather than `$HOME` — which is
deliberate: **it is the piece any durable answer needs**, because whatever eventually runs at
sandbox start has to run something, and there must not be two versions of it.
`fleet::tests::the_door_is_a_file_the_install_runs_rather_than_a_passage_of_the_install` runs the
file with nothing set and then executes the supervisor it hands tmux.

What is still open is the mechanism that runs it at start. The candidates, and what decides:

* **sbx's own durable startup.** skein already ships a kit whose `commands.startup` sbx runs at
  every sandbox start (`src/kit/spec.yaml`) — the same problem, already solved for boxes. The catch
  is that `--kit` looks create-time, which would mean recreating the fleet sandbox and losing every
  box checkout on its disk. Needs `sbx create --help` read properly, and whether `sbx template` can
  attach one afterwards.
* **`sbx run -d` running the supervisor.** `sbx run -d` was measured to keep a sandbox up
  indefinitely. If it takes a command, then one call both starts the door and returns the sandbox
  to the never-attached state that arms no timer — which makes the two problems one problem, and
  needs no recreate. **This is the one to check first**; it turns on whether `sbx run --help` shows
  a command argument.
* **A held session.** `sbx exec -i skein-fleet sleep infinity` in a host terminal, started before
  any other exec. Needs no unknown flags, and is a terminal somebody has to keep open.

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

`fleet.rs` reasons from *"its whole verb list is `login run ls stop rm create exec cp ports`"*, a
list quoted from memory and short of what `sbx --help` prints. The load-bearing conclusion is
*"sbx has no unpublish verb, so every mapping is permanent"*, which justifies the port-burning dance
in `ensure_fleet_agent_port`. **`sbx ports --help` does take `--unpublish`.**

The replacement claim, and it is the one the docs should make, because it is what the enumeration
was ever used for: **no verb adds a mount to an existing sandbox, and none resizes one** (`sbx
--help`; `cp` copies *into* a sandbox, it does not mount). That is still true, and it is what makes
the mount set and the resource numbers create-time decisions.

**Careful with what does *not* follow.** Unpublish does not dissolve the port-squat argument
(architecture §9.4): it withdraws the *host* end of a mapping, and the squat is a box binding the
*sandbox* end first, inside the shared network namespace, where nothing on the host side reaches.

Done in the README, and now in `docs/architecture.md` (§7.1, §7.4, §9.4, §9.5, §13a),
`docs/delivery.md`, `docs/inventory.md` §1.1, `docs/sources.toml` and this file's two other
mentions.

**Still outstanding, and it is code.** Count it rather than trusting a number written here, because
this one moves:

```sh
grep -rn unpublish src/ warden/            # the false claim, wherever it is spelled
grep -rn 'login run ls stop rm create' src/   # the nine-verb list; 0 hits means that half is done
```

At the last count that was ~20 mentions, concentrated in `src/fleet.rs`, with the rest in
`src/warden_client.rs`, `src/doorway.rs` and `src/server-doorway.py`. Two of them are worse than a
stale comment:

- **`src/warden_client.rs` puts the false claim in front of a person.** The publish prompt the
  warden shows says *"This one cannot be taken back: sbx has no unpublish"*, and a test asserts the
  prompt contains the phrase `"no unpublish"`. A confirmation dialog that overstates
  irreversibility is asking for the wrong decision, and the test pins it there.
- **`fleet.rs`'s port-burning is designed around it** — the reuse-before-create discipline, the
  generous settle window, the refusal in `ensure_server_port`. None of that is wrong to keep (a
  publish is still the privileged call, and a wrong one still hands the browser to whatever holds
  the port), but the *reason* written beside it is false, and the recovery step it says does not
  exist does.

`docs/live-check.md` repeats it once. All of these need an owner of `src/` and `warden/`.

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

**The mechanism is written, and it is used on one path but not the other.** An earlier revision of
this entry said *"nothing calls it for `fleet_memory`, `fleet_cpus` or `fleet_disk`"*. That is
false, and it was concluded without counting the callers:
`grep -n configured_field src/fleet.rs` shows `proposed_fleet_size` calling it for all three
(`src/fleet.rs:2673, 2680, 2683`), which is exactly the "sizing a new fleet" its own doc
(`config.rs`, `pub fn configured_field`) says it was written for.

The narrower claim, which is the true one and is the actual bug: **`create_argv` does not use it.**
`create_argv` (`src/fleet.rs:1387`) reads `config.fleet_memory` directly, so the line it builds
carries this build's `26g` whether or not anybody chose it — the proposal path can tell "decided"
from "fallback" and the path that actually runs the create cannot.

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

## Designed, not built

### The reviewer role has no engine — **designed and being built, 2026-08-30**

**Built so far**, and none of it can act: a fork's pull request is stood up from `refs/pull/<n>/head`
(`repos::fetch_pull_head`); a box records its `Purpose` and skein's own are grouped rather than
hidden; a box can be restarted and cannot be repurposed (`fleet::refuse_a_repurpose`, and the guard
is the placement record because a name can collide and a purpose cannot); the reviewer's conditions
and actions, with `Act::PostApproval` overridden by `instead_of_approving_what_was_not_wholly_read`
so no workflow file can spell an approval of a change it did not wholly read; and
`review::Summary::swept`, which is what makes coverage answerable at all.

§10's flags are in (`auto_review` and four beside it, off everywhere, with a `Ceiling` that fails
narrow) **and three of them now decide something**: `auto_review` and `read_prs` through
`repos::auto_review_stands`, the trigger set through `workflow::Wake`, and the author filter. Only
`auto_review_ceiling` is still a stored field nothing reads, and that is step 4's by design.

§15 **step 3a** is in: `Act::Read` is wired. A `read` step now spends a reading at the
commit the step was decided about and files it, so `ReadingCurrent` and `ReadingWhole` are
answerable on the next pass — the engine's loop closes. It posts nothing.

§15 **step 3b** is in too: every reading now runs in the pull request's own review box when one can
be opened, for the pane's button as well as the engine. `ai::Machine` carries which machine a turn
runs on beside `Turn`'s id and directory — the same fact, because a conversation opened in one place
cannot be resumed in another — and `fleet::model_call_in_box` sends the same script the sandbox gets
with no `cd`, since `place::Place` has already put it in the box's tree.

**§7d is fixed**, and deliberately before the posts rather than after: nothing posts a verdict yet,
so the hole was theoretical, and step 4 is exactly what makes it live. `review::unasked_scope` now
asks the lane **or** `the_engine_is_still_watching` — this repo's engine is on and a trigger it asked
for has fired. The pane's background pass is untouched.

**§15 step 4 is in**, as the two verdicts and not as the survey it was planned to be.
`post-changes` and `post-approval` go out through `prwork::post_verdict`, behind
`auto_review_ceiling`; `post-findings` refuses as a vestige and `audit` is step 5. The prompt's
prohibition **stayed** — the engine takes the verdict, which is the same outcome through the
machine that carries the ceiling, the sha guard, §7c and the audit. A session posting its own
verdict would be outside all four. So no survey, and no test went vacuous.

**§13's "both on" obligation is built**, in `skein doctor` and on the repo card. It is narrower
than §13 sketched — four conditions, and the fourth is that the ceiling reaches an approval, so the
ordinary way of turning auto-review on does not trip it.

The cockpit half came with the thing that was actually blocking everything: **`auto_review` had no
surface at all.** Every guard built over the last few days was unreachable except by editing
`repos.json` by hand. The repo card now carries the engine's two switches, and the loop sentence
sits under the ceiling when it reads `approve` — the moment a person builds the loop, rather than a
banner somewhere they are not looking.

**§15 step 5 is in**, which closes the build order. `src/owed.rs` holds §8's six checks, the diff
scanner that says which ones a change fired, and the record that says which have been answered —
keyed on the sha, so an audit of one commit cannot answer for the next. `Cond::ChecksOwed` and
`Cond::ChecksSettled` are the guard; `prwork::audit_now` is the step; `review::audit_owed` is a
`Turn::Resuming` in the pull request's own reading session, which is how the audit gets the whole
reading as context without being a second reader.

Two of §8's six triggers are refused by name — "a comment naming a mechanism" and "a claim that
something is absent" are claims about English, and `owed::Check::computable` says so the way
`Wake::computable` already does for `reply`. The default set is every computable check rather than
the empty one, which is the one place this feature starts ON: nothing in it is a permission, it only
withholds a verdict.

**§10's chain is complete.** The per-PR assignment overrides `auto_review` and never `read_prs`,
through `repos::auto_review_stands_for(repo, assigned)` with `prwork::chosen_by_hand` answering the
second argument. An empty assignment name is `Carries::Excluded` and is NOT an assignment — writing
that lookup as `.is_some()` would act on exactly the pull request somebody took out of reach, which
is the per-PR flag inverted, and a test fails on it.

**And §10's `reply` trigger fires**, which was the last row of that table that could not be
computed at all. Two GraphQL fields, exactly as `Wake::Reply`'s note predicted: `submittedAt` on
`latestReviews`, and `latest: comments(last: 1)` on `reviewThreads`. `prq::Pr::replied_to` is the
rule — a thread you opened, whose last comment is somebody else's, after your latest review — and
only a sighting fires; `None` (a cut thread list, or no time for your own review) never wakes a
reading, because waking one spends money on a guess. `Wake::computable` is deleted with it: every
trigger answers now, so it would have returned `true` for all six, and `read_wake` already refuses
a word from a newer skein.

**And the steps are written down as a workflow somebody can switch on** — §9 now carries one, and
`tests/reviewer_workflow.rs` parses it OUT OF the document with the parser production uses and walks
a pull request up it. It was missing for a reason worth naming: the vocabulary was built one step at
a time, a fleet ships no default workflows (`~/.skein/workflows.json` is a file a person writes), and
so the whole engine could be complete, gated, tested and unreachable.

`read` is written LAST in that file and that is the whole trick. `next` takes the first step whose
conditions all hold, and there is deliberately no condition meaning *skein has no reading* —
`reading-current` and `reading-stale` are both three-valued and unknown satisfies neither. So the
reading is the fallback, and the specific steps take over the moment one exists at the head. Written
the other way round it is the answer for ever and no verdict is ever reached, which is the shape a
person writes on the first try — and is the sabotage that fails the test.

### Composing the steps found one that cannot fire

`post-changes` is guarded on `findings-blocking`, which is the right way to write it. But
`prwork::facts_of_in` sets `findings_blocking: None` unconditionally and always has — deliberately,
and its own comment says why: *"the findings are on GitHub — the reading posts its own review and
skein keeps no copy"* (§5). `Cond::FindingsBlocking` holds only on `Some(true)`, so in production
that step never fires.

**Stated plainly: the engine can approve unattended and cannot refuse.** That is the asymmetry §13
records the argument about, arrived at from the other end — not a policy anybody chose, but a gap in
what skein knows about its own reading. `Act::PostChanges` is not unreachable in general (guard it
on `label:blocked` and it fires today); what cannot be reached is the intended guard. It also makes
`auto_review_ceiling: changes` a setting with nothing under it.

The shape of the fix is small and the decision is not: a field on `review::Summary` beside `swept`,
answered by the same second turn that already accounts for coverage, saying whether what it found
must block. **When skein refuses a pull request on its own is the owner's call**, so it is recorded
here rather than taken. `tests/reviewer_workflow.rs` asserts the gap, so the day the field exists
the test fails and says the workflow can be trusted with a refusal.

**Nothing in `docs/pr-review.md` is left unbuilt, and §15 step 3's four-step check has now been
run on a real fleet** — 2026-09-03, `acme/thing` #1011, in-fleet skein at `8d0a8a2`.

**Both of the owner's questions were answered on 2026-09-03 and both are built.**

The `[REPLIED]` LANE is in, and NOT as a fifth `prq::Lane`: the pane's top level groups by
`cockpit/src/move.mjs`'s `moveOf` rather than by the lane, deliberately and with SKEIN-300/302 cited
on the line that says so, and a new `Lane` variant would have rippled through
`review::worth_reading` to change a display. `answered` fires on `replied_to_me == Some(true)` only,
sits above `decided` (which is the feature — a decided pull request is `theirs` by definition and
stays there however much its author answers you) and below `archived`.

The per-PR TRIGGER set is in as `repos::triggers_for`, with the same three states the workflow
assignment already had: no entry means the repo's set governs, a list means these instead, and the
EMPTY list means *wake on nothing* — the state that cannot be said any other way, and the one the
test is really about. Both consumers ask the same function, so the engine's scope
(`review::the_engine_is_still_watching`) and the engine's refusal
(`prwork::no_trigger_of_this_repos_fired`) cannot come to disagree about which words apply.
`POST /api/repos/:id/review/:number/triggers` sets it. **There is no page surface**, declared as
such in the route-caller list: what a trigger override should look like on a row is a design
question and this build did not invent one.

**And the refusal gap is closed** — §7b's other half. `facts_of_in` wrote `findings_blocking: None`
unconditionally because the findings live on GitHub and skein keeps no copy (§5). What made it
answerable was not access: the SWEEP — the turn that already accounts for what the reading covered
— is now asked whether what it raised must block, and the answer is recorded against the sha in
`review::Summary::findings_block`, exactly as `owed_triggered` is. `Act::PostChanges` is reachable
by its intended guard for the first time, and only on `Some(true)`: the two-stage path runs no
sweep, a sweep that did not finish said nothing, and an answer that would not parse is not an
answer — every one of those stays `None`, which is what keeps a refusal from being posted off a
fact nobody looked up.

**One thing worth knowing before the first repo is switched on.** A verdict is posted by
`prq::submit_review_with_comments`, which looks up `prq::host_token` itself rather than taking the
token `prwork::perform` was handed. They cannot diverge today — `sweep` sources its token from the
same function — but every other act in `perform` takes the token as an argument, and this one does
not. Worth making uniform the day anything gives the tick a different credential.

#### What 3b needed, and what it cost

**Verified on a real fleet, 2026-09-03** — and it could be, because skein now runs INSIDE the
sandbox: a box is made by the server this cockpit is served from, so `sbx` on the host is no longer
what the box path needs. `acme/thing` #1011, `read_prs` on for the check and off again after.

| step | evidence |
|---|---|
| a box appears as skein's own | `gadget-demo-pr-1011`; `place::shared_record` says `"purpose": "review"`, which is the guard `refuse_a_repurpose` reads |
| it stands at the change | tree moved `2ac9fda8` → `c57865f2`, the head GitHub reports for #1011 |
| the reading names files | `yours: ["tools/hooks/pre-commit"]`, `swept: true`, and a line about the mechanism — the gate scanning sibling files for `#[path]` declarations — not a diff restated |
| round two resumes | the SAME transcript, `c5ca0ae3….jsonl` at the box's own slug, 302,447 → 338,908 bytes; no second conversation and no second box; round two's prompt is 1,477 chars opening *"You have already read this change in this session — do not read it again from scratch"*, against round one's 8,290 |

**Where the transcript actually lives, because looking in the obvious place says the opposite.**
`/boxes/<box>/home/.claude/projects` is an empty MOUNTPOINT — `box-session.sh` binds `.claude/projects`
from `$SKEIN_HOME/boxes/<box>/claude-projects/`, and that is where the conversation is. Reading the
mountpoint concludes that no conversation was opened and that a resume is impossible, which is the
opposite of what is true.

**And one cost the check exposed.** Every reading calls `reviewbox::open_at` → `start_box`, which
adopts the tree and session (`already has a checkout; keeping it`) and then provisions anyway —
`start_box_inner` provisions unconditionally. Round two paid a full provisioning pass before its
model call could start. That was 240s until the tracker wiring was detached; it is seconds now, so
the fix absorbs most of it, but the redundant pass is still there and is its own question.

**One thing the design was wrong about, in the useful direction.** §11's fourth gap was "no box
starts with an instruction", and it proposed the handoff brief for round one. The gap dissolved: the
box is not asked to do anything. It is where the reading's model call runs, and the reading sends
the prompt it has always sent.

**And one dependency the gate refused.** `reviewbox` reached for `prq::pr_is_open`, which put the
module that destroys boxes inside the `{prq, review}` cycle. `close_finished` now takes the answer
rather than the asker, and the server asks — which also made it testable for the first time. Worth
recording as a shape: the edge that was hard to justify was the same one making the code impossible
to prove.

#### What 3b needed, read from the code rather than from §11

Worth writing down, because §11 was drafted before any of it was traced and two of its assumptions
were wrong in useful directions.

* **The dispatch seam already exists.** `ai::claude_in_turn` has two destinations — a local
  `Command` and `fleet::model_call_in_sandbox`, which builds a script and runs it through
  `own_sandbox(&sandbox).attempt(...)`. A review box is a third: `place_of(<box>)` is the same
  `Place` type with the same `attempt`. So 3b is a destination, not a new mechanism.
* **`fleet::model_runs_here` is the fact that changes.** Today it is false host-driven, and
  `review::stand_the_change_up` returns `Standing::Nothing` — no checkout, so the prompt falls back
  to the truncated diff. In a box the checkout is the box's own tree and that branch goes away,
  which is §11's whole point stated as a code path.
* **`resume_box` covers round one.** `claude --continue --print … || claude --print …` falls back to
  a fresh conversation, and the exec path `cd`s to the box's recorded tree over a host-bound
  `~/.claude/projects`. So §11's "no box starts with an instruction" needs no handoff brief:
  round one and round N are the same call.
* **The return channel is the store, and it has a convention to copy.** A box's bwrap binds exactly
  two host-shared read-write paths: `$SKEIN_BOX_STORE` (= `repo.store`) and the two conversation
  directories. Everything else the fleet mounts is tmpfs'd. So the reading cannot be written to
  `skein_home()/review/...` from inside a box, and the artifact goes at `<store>/<kind>/<box>.json`
  — `signals.rs`'s own rule, with `signals::signal_is_ours` checking the file names its writer, and
  `kit::ensure_store` needing the new directory added.
* **What a box cannot reach, and must therefore be handed or left on the host**: the fleet-wide
  spend ledger, the summary and `read-tried` caches under `skein_home`, CODEOWNERS via the bare
  mirror, the host GitHub token, and `ai`'s in-process refusal memo.
* **Teardown is built** — `src/reviewbox.rs`, and it landed before anything can create a box, which
  is the same rule as the kill switch on the author side. `close_finished` runs from the queue's
  housekeeping pass beside `review::prune`, destroys only on `prq::pr_is_open` answering *closed*,
  and is a no-op until the first review box exists. `reviewbox::AT_ONCE` is the standing cap.

**What is left of 3b is the dispatch itself**, and it forks on a question worth deciding before any
code: `review::summarise` is shared by the engine and by the pane's "read it" button. Running the
reading in a review box for BOTH is faithful to `review.rs`'s own rule that there is no second
reader; running it in a box only for the engine gives two reading paths, which is the thing that
module fought hardest to avoid. The first is the bigger change and the right one, and it means the
pane's button starts a box too.

Also unproven and worth saying: **`reviewbox::open_at` and `close_finished`'s side effects cannot be
exercised here.** Both need `sbx`, which is not on this machine (`start_box`, `destroy_box`). What
IS tested is every decision they are built on — the name round trip, which boxes are ours to end,
the rule that only *closed* ends one, and the cap — and each of those was proven by sabotage. The
side-effecting halves are deliberately thin for that reason.

**A test I wrote and deleted in the same increment, recorded because the shape recurs.**
`a_fleet_with_no_sandbox_holds_no_review_boxes` asserted that `reviewbox::theirs` returns nothing
when no fleet sandbox is configured. It passed — and it went on passing with the guard it was
testing removed, which is how it was caught. `config::load_config` substitutes the default fleet
name for an empty one (`src/config.rs`, "nobody chose it"), so `place::fleet_sandbox` cannot answer
with nothing and the state the test described is unreachable. The guard stays, documented as
unreachable, because what is downstream of that list destroys boxes; the test went, because a test
that cannot fail is worse than no test. Third time this exact shape has appeared in this repo.

**Two things step 3a left behind, both small.** `READINGS_PER_SWEEP` is 1, chosen from the 120s
tick and a reading taking most of a minute; if the reviewer is ever used on a busy repo that number
wants measuring rather than reasoning about. And a reading that fails the same way for ever is an
unending `Wait` on a row — visible, with its reason, but nothing escalates it the way
`a_wait_that_will_not_end_on_its_own` escalates a stalled train front.

The design as it was first written follows.

`docs/pr-review.md`. The owner's ask was that skein's automated mode work like the fully-automated
PR review cycle `gadget-demo-repo-archaeology` ran in one session, with manual mode as the same
mechanism behind checkboxes. That box was interviewed and its answers are in the design.

The finding that reframed it: skein's automation is entirely **author-side**, and everything the
box did was **reviewer-side**. So this is a second vocabulary over `workflow.rs`, not a second
engine, and most of the parts already exist — the queue, the staged reading, the `(number,
head_sha)` cache, the posting with re-anchoring.

The two things that are genuinely new, and the two the box corrected an earlier draft on:

* **the sha joins the program counter.** Reading and posting are seconds apart in a session and
  polls apart in an engine, so a memoryless evaluator would post a review of tree A anchored to
  tree B. The reading records its sha; the post's guard compares it to the head.
* **the adapter is where a lying source is corrected, never the step model.** A stale `APPROVED`
  is not a memory bug — `reviewDecision` answers "does an approval exist", so re-deriving it every
  poll is confidently wrong every poll.

Decided 2026-08-30: unattended approvals yes, §7c's whole-pass rule stands as correctness rather
than a gate, and the undoability asymmetry is accepted. §10 is the flag layering — four switches
already exist (`pr_workflows`, `read_prs`, `review_queue`, the per-PR workflow assignment) and three
are new. Still open: which repos start with it on.

The question that came back with the agreement was the useful part — *when does a pass not cover the
whole file set?* — because the answer makes §7c load-bearing rather than theoretical. The reading is
byte-capped at 40 KB / 140 KB / 300 KB, `truncate_diff` already cuts at a file boundary and names
what fell off, and `Reading::cut` carries that to the pane. The cached `Summary` does not record it,
so nothing can currently ask whether a pass was whole. That is the one field the rule needs.


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

51957–51959 on `skein-fleet`, from before the agent bound `0.0.0.0`. Harmless while they sit there:
the reuse logic skips them because they never answer. **They are also removable** — `sbx ports
--unpublish` exists (see the entry above; this line used to say it did not), so this is a tidy
somebody can actually do rather than a permanent scar.

### `copy_guest_file` still uses `sbx exec`

Deliberate — it is a download, and `/exec` would buffer a multi-GB bundle in memory at both ends.
Revisit only if a streaming download endpoint earns its keep.

---

## Migration and cleanup

### Retire the per-VM box model — **the fallback is gone; the plumbing around it now is too**

Half of this entry was true for a while and read as all of it. What `place_of` no longer does is
below and still correct. What it did not mention, and what has since been done (SKEIN-477):

- `Where::OwnSandbox` was only ever the FLEET's own sandbox by then — every production caller passed
  a fleet name — while its name and doc still said "one sbx sandbox per box, skein's original
  model". Renamed to `Where::SandboxItself`, with no behaviour change; the test written to stop
  somebody deleting the variant is still there and still passes.
- `Place::unreachable_from_fleet` is kept, because without it both hops vanish and a command runs in
  skein's own sandbox against other people's files at the same paths. Its condition is now stated as
  the invariant it actually enforces — *the sandbox addressed is not the one this process stands in*
  — rather than as a claim about legacy boxes.
- `stop_box`/`destroy_box` no longer fall back to `sbx stop <box>` / `sbx rm -f <box>`. They refuse
  with what `absent_box_reason` says. The `$SKEIN_STOP_CMD`/`$SKEIN_DESTROY_CMD` hooks stay: they are
  test seams, and `tracking` uses one.
- `delist_box` was a live bug rather than residue — it read the single legacy registry, errored on a
  fleet install, and the `?` skipped the per-box cleanup below it, leaking four files per destroy.
  It uses `store_for_box` now, with the cleanup ahead of the registry write so a registry failure
  cannot skip it.

**Still open**, deliberately: the `own_sandbox` *function* rename is ~55 mechanical call sites
(SKEIN-482), and `board::load_views`'s sbx branch is a product decision, not a cleanup (SKEIN-484) —
removing it makes an unnamed fleet show a blank board rather than sbx's list, which is a different
answer, not a tidier one.

The original entry follows, and its account of the fallback is unchanged:


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

### `preparing_a_checkout_starts_from_the_remote_base_and_never_reuses_a_tree` fails only in the suite

Pre-existing, and confirmed pre-existing rather than assumed: it passes run alone and fails under
`cargo test --lib`, on a **clean tree** with nothing of the size gate applied. So it is suite order,
not a regression from the change that found it.

    cargo test --lib preparing_a_checkout                    # passes
    cargo test --lib                                         # fails, 868 passed / 1 failed

It asserts on a path built from `fleet_root()`, which reads `$SKEIN_FLEET_ROOT` and falls back to
`/boxes` — so the failure is another test's `set_var` still standing when this one runs. That makes
it the same family as the entry below, and the same warning: a test whose answer depends on what ran
before it is a test that will one day pass for the wrong reason instead of failing.

### Two single-test failures nobody has ever seen the panic for

Measured 2026-08-30 rather than guessed at, because it was called a known flake once on a single
data point and that was wrong. **2 failures in 9 full `cargo test --tests` runs — and 0 in the last
5.** It passes every time the suite is run alone.

The correlation, which is the useful part: **both failures happened while another process was
building against the same `CARGO_TARGET_DIR`** (a parallel agent compiling). Every run after those
processes stopped has passed, including four consecutive ones taken specifically to try to
reproduce it.

Ruled out, so nobody re-does it: it is not a local env race — that binary contains no `set_var` and
no `set_current_dir` in any of its 47 tests — and it is not the code under test moving, since
`src/box-session.sh`, where the shim lives, has not been touched.

**The panic is now captured**, on the sixth sighting, and it narrows this a lot:

```
tests/git_write_request.rs:352 — the shim changed what `git --version` prints
  left:  ""
  right: "git version 2.53.0\n"
```

**The shim produced nothing at all.** So it is not printing something extra and not diverging on an
exit code — it did not run. That is the copy-then-exec: the test copies the real `git` binary into
a temp directory, `chmod`s it, and executes it through the shim. `ETXTBSY` — exec of a file another
handle still has open for writing — is the classic shape, and it fits both the emptiness and the
dependence on what else is running.

**Done: the assertion now carries the shim's exit code and stderr.** It compared stdout and the
exit code and threw away the one thing that says why, so every sighting was an empty string against
a real `git --version` — which says the shim did not run and nothing about what stopped it.

That is as far as this could be taken without the failure in hand. It was chased with a diagnostic
build and would not reproduce: it fails only when the whole `--tests` set runs at once, and across
that afternoon it went from 2-in-9, to two consecutive failures, to passing again — which tracks
how busy the machine was rather than anything in the tree. **So the next person to see it gets the
cause for free, and does not have to reproduce it.** That is the point: an intermittent failure
nobody can summon has to explain itself the one time it happens.

**The next step is to capture the panic, which has never once been seen.** Three sightings were all
piped through `grep` filters that kept the `FAILED` line and dropped the assertion message, so
which of the four assertions fires is still unknown — and "the copied binary would not run",
"the shim printed something extra" and "the exit codes diverged" have three different fixes. Run
the suite in a loop keeping FULL output:

```sh
for i in $(seq 20); do cargo test --tests > /tmp/run.$i 2>&1; done
grep -l "the_git_shim.*FAILED" /tmp/run.*
```

**The second one, in `--lib`, is now named — and it has a hypothesis.** It was
`sandbox::a_shared_boxs_lifecycle_never_names_a_sandbox_after_the_box`, caught on 2026-08-31 by
writing the whole log to a file first, which is the discipline this entry exists to impose:

```
assertion `left == right` failed: the box's own tmux server IS its liveness —
sbx ls knows nothing about a shared box
  left:  None
  right: Some(Running)
```

`None` is `box_liveness` saying it **cannot tell**, not saying stopped. The test stubs `sbx` by
writing a shell script into a temp `bin/`, `chmod`ing it, prepending to `PATH`, and executing it
immediately — and it rewrites that same path twice more in the same test. **That is the `ETXTBSY`
shape**, the same one hypothesised for the git shim above: exec of a file that was being written a
moment ago. Two consecutive full runs after it were `927 passed; 0 failed`, so it is load-dependent
like the other two.

**The remedy to try is the repo's own atomic-write convention** — write to a temp name in the same
directory and `rename` into place, which is what every box probe does (`mktemp` + `mv`). A rename
replaces the directory entry, so an exec already under way keeps the old inode and a fresh exec
gets the new one; neither can see a half-written file or a busy one. Not applied yet, because it is
a fix to a cause that is inferred rather than observed, and it deserves its own proof: run the
suite in a loop until it fails, apply the change, run the same loop again.

**A fifth, in `--lib` — caught, diagnosed and fixed, 2026-08-31.**
`place::a_reply_cut_off_part_way_is_reported_rather_than_sent_again` expected the failure to say the
reply was cut off and got `fleet agent: read: Connection reset by peer (os error 104)`.

**Caught by doing what this entry has been asking for**: eight paired runs of `--lib` and `--tests`
with the WHOLE log kept to a file. It failed once in eight, and for the first time in four sightings
the panic was captured. That is the entire reason it could be diagnosed — the three earlier
sightings were all piped through greps that kept the `FAILED` line and dropped the message.

**The cause, read off the two files rather than inferred.** `send_request` writes the head and the
body as two separate `write_all` calls, so they are often two TCP segments. The fixture
`serve_badly` did a single `read` into a 64 KiB buffer and treated whatever arrived as the whole
request — so when the body landed in the second segment it was still unread when the fixture closed
the socket, and **Linux sends RST rather than FIN for a close with unread data in the receive
queue**. The client's next `read` then failed with `ECONNRESET` instead of returning `Ok(0)`.

**Production was never wrong.** `read_fault(.., unheard: false)` makes either outcome a
`Fault::heard`, so a reply that had begun arriving is never re-sent whichever way the socket ended.
Only the *sentence* differed, and only one of the two matched the assertion.

**Two fixes, and the second is the one worth copying.** `read_whole_request` drains the
`Content-Length` body before answering — which is what `src/fleet-agent.py` does, and what
`send_request`'s own comment already relied on it doing — so the close is a FIN and the outcome is
deterministic. And the assertions are **reordered**: the no-retry count is asserted BEFORE the
wording. It used to be after, so all three earlier sightings failed on the sentence without anybody
learning whether the dangerous thing — the script going out twice — had also happened.

Measured before and after, with in-module concurrency as the load: **1 failure in 40 runs** with the
body left unread, **0 in 40** with it drained.

**A fourth, named, and it fits the same shape.** 2026-08-31, `--tests`:
`slow_fleet_snapshot_does_not_starve_concurrent_requests` failed with
`GET /vendor/xterm.js to 127.0.0.1:40285 failed 3 times; last error: Connection refused` — the test
server it had just started was not accepting. Five consecutive runs of that test alone passed, and
the whole `--tests` set immediately after was 30/30. So it is the same dependence on what else is
running as the shim above: the failure is a connection refused under ~30 concurrent test binaries,
not a wrong answer. Kept here rather than filed as a test bug for that reason — three different
tests have now failed this way, and what they have in common is the machine.

### `detach_named` put a 35 KB script in tmux's argv — **fixed**, and the shape is worth keeping

Reported live, 2026-08-31: *"When I click on update, it said something like command too long or
something, though the update started."* Both halves of that sentence were true and the second one
was wrong.

`tmux new-session -d -s <name> <script>` packs the whole script into one argument, and tmux's client
sends it to its server in a single imsg — capped at `MAX_IMSGSIZE`, 16384. `update::start`'s script
embeds the whole of `bootstrap.sh` (`build_script`), which had grown to 34,916 bytes and 35,254 once
quoted. Measured, not looked up: on tmux 3.6 in this sandbox the 35 KB argument answered
`command too long` and created no session, and the same call with a short argument created one.

**The update had not started.** What made it look as if it had is a second bug, and it is the one
worth remembering: `update::start` wrote an empty log and removed the done marker *before* the
launch, and `update::running` is "the log is there and the marker is not". So a launch that never
happened left precisely the state a successful launch leaves — for ever, with an empty log and the
button disabled, and every later press answered "an update is already running" about a run that did
not exist. Confirmed on the owner's fleet: `~/.skein/update.log`, zero bytes, three minutes old, no
`update.done` beside it. Recovering it meant deleting a file by hand.

**Both fixed.** The script goes to `<fleet>/.skein/detached/<session>.sh` through `Place::write` —
the trick `install_server` already uses, whose size problem is solved there — and tmux is handed a
filename; `detach_command` takes no script at all, so it cannot grow one back. And a failed launch
now writes the reason INTO the log and marks the run done-and-failed, so the pane says what happened
instead of nothing, and the next press works.

**The general shape, which has now cost twice:** a state machine whose "in progress" is the absence
of an end marker must write that marker on every exit, including the ones that never began. The
same rule as `prwork`'s "silence is not an ending".

### The update button had never worked, and the tmux ceiling was hiding it — **all three fixed, 2026-08-31**

Reported live the same day as the entry above: *"I clicked update now but it worked while there was
no update really breaking the update permanently."* Every clause was accurate. Tracing it found two
more defects behind the one that had just been fixed, neither reachable until it was.

**1. The script was not shell, and never had been.** `build_script` ends with a heredoc, so it ends
with a newline — and `update::start` wrapped it as `{ <build>; } > log 2>&1; printf ... > done`,
which puts the `;` at the *start* of a line. No shell parses that. Measured, bash 5.2 and dash
alike: `syntax error near unexpected token ';'`, the file rejected whole. A parse error happens
before anything runs, so the redirect was never applied and the marker line was never reached: the
run wrote **nothing**. tmux exits 0 having created the session, so `start` returned `Ok` and the
button reported success. Present since `89cf36b`, the commit that added the pane — the 35 KB
ceiling above refused the launch first, every time, so it never got far enough to be seen.

Assembling the script is now `update::run_script`, split out of `start` for one reason: so `sh -n`
can be run on the real bytes. Reaching it through `start` needs a sandbox to talk to and a build to
run, which is exactly why nothing caught this.

**2. Which jammed it permanently — the same shape as the entry above, one layer down.** The fix
recorded there covers a launch that *failed*. This one succeeded and then died, which leaves the
identical state: log present, marker absent, `running()` true for ever. `running` was a claim about
two files and not about a process, and the marker is written by the script itself — so a killed
session, a sandbox restarted mid-build, or a machine rebooted during one all do it too. Recovery was
deleting a file by hand.

`update::settle` now asks tmux whether the session is actually there and writes the failure down
once when it is not. **Only `Some(false)` ends a run**: `fleet::detached_alive` returns `None` for
could-not-ask, because that is precisely what a sandbox says while it is being restarted by the very
update being watched. Rate-limited to one question every three seconds, skipped entirely when
nothing is believed to be running, and a `LAUNCHING` shutter closes the window `start` opens between
clearing the marker and having a session.

**3. And nothing swapped the cockpit onto what it built.** `bootstrap.sh` under
`SKEIN_BOOTSTRAP_STOP_AFTER=build` installs both binaries and returns without restarting anything —
correctly, because `fleet::build_server_in_sandbox` runs the same bytes while a fleet is being
created and must not restart a server there. So an update that fetched, compiled and installed
perfectly left the **old** binary serving, the page reloaded onto it, and the revision never moved.
The button's caption already promised "restarts the cockpit" and `tailUpdate`'s reload was written
expecting it. Every update so far had been finished by hand with a `pkill`.

The swap is skein's existing one rather than a second mechanism: `SIGUSR1` to the doorway, which
re-execs across the same descriptor so the port is never free — what `start-door.sh` does when it
finds a cockpit already running. Guarded on the build's own status, and **after** the marker,
because the pane stops reading the log the moment the marker says the run ended.

**An assertion that could not fail, recorded because it nearly shipped.** To prove the signal comes
after the marker, the first version of the test asked the stand-in doorway's own signal handler
whether the marker existed yet. The sabotage that moves the signal ahead of the marker **passed
it** — the run writes the marker microseconds after `kill` returns, while the handler runs whenever
the kernel gets to it. The ordering is a property of the generated script, so it is now asserted
there, where it is decided and where it fails deterministically. The sabotage pass found this; the
author did not.

**The general shape, for the third time on this one button:** every defect here was invisible
because an earlier one failed first. A gate that refuses before the thing you are testing can run
does not prove the thing works — it proves nothing about it at all.

### A move test leaks a doorway loop that restarts itself — **fixed, 2026-08-31**

Found by the leaked-process gate on 2026-08-31: four processes under
`/var/tmp/skein-move-it-<pid>/`, a `tmux` server plus
`while [ -f .../server-doorway.py ]; do … done` and the `skein-server` it had started. The loop's
own exit condition is the presence of `server-doorway.py`, so it keeps restarting for as long as the
temp directory survives — and the directory survived the test that made it.

Cleared by hand (remove `server-doorway.py`, which ends the loop, then the exact directory — never
by glob). **The teardown now outlives the panic**: `scratch()` returns a `Scratch` guard whose
`Drop` removes the doorway script, kills the tmux server, waits a beat and removes the directory.
`Drop` runs while unwinding, so it happens whether the test passed or failed — which is the whole
defect, because every test in that file ended with `remove_dir_all` on its last line and a failing
assertion unwinds straight past it. `Staged` already did this for five of the twelve tests; the
others had nothing.

`a_test_that_panics_still_takes_its_supervisor_down` pins it, with a real panic inside
`catch_unwind` rather than a simulated one — what is under test is what `Drop` does while
unwinding, and an early return would exercise the ordinary path instead. Sabotage (a `Drop` that
returns immediately) fails it with *"a failing test left 3 supervisor process(es) alive"* and
reproduces the original leak exactly.

**One detail the clearing taught, and it is why the order in `Drop` is what it is.** Removing the
script does NOT end the loop while the doorway is still running: the `while` condition is only
evaluated between iterations, so the leak survived four seconds of the script being gone and died
only to `tmux kill-server`. Script first *and* tmux second — either alone leaves something behind. Worth doing because this is the one
gate that reports a number rather than pass/fail, so a leak that nobody clears makes every later
run's count wrong.

**That is the actual lesson here, and it is about the runner rather than the tests.** Three sightings
of one intermittent failure and one of another, and not a single panic captured, because every
invocation filtered its own output. A suite run that may fail must keep the whole log:

```sh
cargo test --lib > /tmp/run.log 2>&1; grep -E '^test .* FAILED|panicked' /tmp/run.log
```

Same family as the entry below, and as `preparing_a_checkout_…` above: a test whose answer depends
on what else is running is a test that will one day pass for the wrong reason.

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
to close, because the *sandbox-side* port goes free and any box in the shared namespace can bind it
before anything else does. (The host mapping outliving the doorway is the lesser half and is
recoverable: `sbx ports --unpublish` exists — see the sbx entry above, which this line used to
contradict. The bind is the half nothing on the host can undo.) So `fleet::stop_serving` takes the *server* away and leaves the door standing, using a state
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
