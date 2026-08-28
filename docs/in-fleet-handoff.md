# In-fleet install: what broke, what is fixed, what is still open

Written at the end of a session that took the `in-fleet` install from "does not start" to "starts,
but the sandbox will not stay up". Everything below is either a commit on `in-fleet` or an open
item with the evidence attached. **The last section is the one that matters most** — it is the only
thing still stopping the fleet, and it is not a skein bug.

The audience is whoever picks this up next, including a skein box working on this repo.

## The pattern, because it is the useful part

Ten fixes, one shape: **something that was true when skein ran on the host, silently false when it
runs inside the fleet.** Not one bad edit among them — each was a correct implementation of a
premise that had quietly stopped holding.

| what was assumed | why it held before | how it failed in-fleet |
|---|---|---|
| the fleet root can be created | skein ran as a user who had already been given one | `/boxes` is at the filesystem root; `mkdir` denied |
| the image can build Rust | the host had a toolchain | the `shell` image has no `cc`, and rustc links through it |
| `tmux` exists | the host had it | the image does not, and the cockpit's supervisor is a tmux session |
| `$HOME/.skein` is the volume | on a host it is | sbx mounts the volume at its **host** path and gives the sandbox its own `$HOME` |
| the deployment is in the environment | the host process exported it | the supervisor passes it to the server and to nothing else |
| `sbx` is reachable | the host had it on `$PATH` | there is no `sbx` inside a sandbox |
| the `skein` CLI is beside the server | `cargo build` puts both in one directory | the install built only `--bin skein-server` |
| the fleet sandbox is "another sandbox" | from the host it is | in-fleet it is the machine skein is standing on |

**A test that inherits the developer's environment cannot see any of these.** Every one was found
by building the fixture's `$PATH`, `$HOME` or `mountinfo` from nothing instead of inheriting it.
That technique is the transferable part: `fleet::tests::the_image_is_given_everything_the_install_runs_before_it_runs_it`
symlinks the five real binaries the script uses and nothing else, and
`the_servers_home_is_the_mounted_volume_and_not_the_sandboxs_own` sets `$HOME` to a decoy and
asserts the **decoy is absent** — asserting the volume is present would have passed with both.

## Fixed, in order

All on `in-fleet`, each with a test that fails when the fix is reverted.

| commit | what it was |
|---|---|
| `ae5eba8` | `bootstrap.sh` could not create `/boxes` — two bare `mkdir: Permission denied` lines and nothing else. `fleet::ensure_fleet_root` had escalated for this since long before the script existed, but it runs from a binary the script's job is to build. |
| `a7a3275` | No C compiler in the image. rustc links through `cc`, so the build downloaded every crate and then failed `libc`, `proc-macro2`, `quote`. Not a `-sys` dependency — skein has none on Linux. |
| `1bd3af2` | No `tmux`. The build finished, then `bash: line 227: tmux: command not found`, with the binary installed and nothing serving it. Checked twice on purpose: in the apt list, and at the door where `has-session` swallows its own stderr and would report a missing tmux as a bash line number. |
| `bad3387` | `sbx create` takes `-p/--publish` (read off its `--help`), so the fourth install line was never needed. A step a person runs separately is a step a person skips, and skipping it leaves a fleet that looks installed and serves nothing the browser can reach. |
| `b394671` | `$SKEIN_HOME` defaulted to `$HOME/.skein` — right on a host, wrong inside, and wrong in the way that costs most: **it works**. The server minted its token and wrote `repos.json` into the container's own home, which is not the volume and does not survive the sandbox. Now discovered from `mountinfo`, and **ambiguity refuses** rather than guesses. |
| `1c6ec29` | `unreachable_from_fleet` refused skein's *own* sandbox. The refusal is right for a legacy per-VM box; most of `fleet.rs` addresses the fleet sandbox itself through `own_sandbox`, so every box start printed the refusal and provisioned nothing. Also: **nothing in the tree ever set `SKEIN_IN_FLEET`** — a grep found `deployment.rs`, one test, and now the supervisor. |
| `c9b7600` | Docs only. The create line named no resources, so sbx chose: every host core, half the host's memory, a 20 GB disk. All three are fixed for the life of the sandbox — the least revisitable decisions in the install, and the only ones nothing asks about. |
| `7e70829` | The install built `--bin skein-server` alone, but `sandbox::skein_exe` resolves `skein` as **the sibling of the running executable**. So `launch_command` produced a bare `skein start …` and every box start would have died on `sh: skein: command not found` — the exact failure that function's doc comment describes and believed it had closed. |
| `41a89bc` | `skein repos` said "no repos yet" about a fleet whose cockpit was showing nine. Nothing passes `$SKEIN_HOME` to the CLI, so every invocation read the container home and answered confidently wrong — worse than an error, because it reads as data loss. `bootstrap.sh` now records the volume it discovered; `config::skein_home()` reads it. |
| `ec94c33` | `skein start` reached for `sbx`. Two faults: the deployment declaration reaches only the server, and `ensure_fleet` asked `sbx ls` whether the sandbox it is *inside* exists — `sbx.rs` correctly refuses that in-fleet, and `ensure_fleet` turned the refusal into "cannot tell whether the fleet sandbox exists". |

### Two design boundaries that were respected rather than edited

- `deployment.rs` forbids itself from touching the filesystem, enforced by
  `where_skein_runs_is_decided_by_one_variable_and_nothing_else`. The first cut of `ec94c33` put a
  `read_to_string` there and broke it. The boundary is right: a module that sniffs its surroundings
  guesses, and the costly guess is a host deciding it is in the fleet. The read moved to
  `bin/skein.rs`, which carries the installer's declaration **into** the variable that still decides.
- `ensure_mirror` runs *before* a repo is registered, so a repo whose boxes cannot clone is never
  written. This is why a hand-written `repos.json` is the wrong repair: the file is an index, and
  `add_repo` also builds the mirror, the store and the kit.

## Open

### 1. The fleet sandbox will not stay up — the live blocker

Every `sbx exec` in the session printed `Sandbox skein-fleet started successfully`, meaning it was
**stopped** each time. That is the whole of "the cockpit works and then is not accessible a few
seconds later": nothing is crash-looping — `server-doorway.py` holds the listening socket across a
server crash and restarts the server behind it, so a crashing server does not free the port. The
machine underneath goes away.

Working hypothesis, unconfirmed: the fleet is a `shell` sandbox with nothing attached, `sbx create
--help` says *"Use `sbx run --name SANDBOX` to attach to the agent after creation"*, and sandboxd
reaps it as idle. A detached tmux inside does not count — sandboxd watches the agent.

Workaround being tried: hold it with `sbx run --name skein-fleet` in a host tmux.

**If that is the cause, it is a hole in the in-fleet premise, not a `bootstrap.sh` bug.**
`docs/delivery.md` assumes the fleet sandbox outlives every exec. Whoever confirms it should decide
what the model does about it — an attach skein documents, an `sbx daemon` setting, or something in
the sandbox that keeps the agent alive — and write the answer into `delivery.md`.

Earlier in the session the same symptom was chased as a memory ceiling. That was wrong, and the
disproof is that it stops while idle. The memory item below is still real, just not this.

### 2. Ceilings are computed from a config field nobody set

`memory_plan()` (`fleet.rs:1551`) derives every cgroup ceiling from `config.fleet_memory`, **never
from what the sandbox actually has**. `fleet_memory` defaults to a hardcoded `"26g"` and nothing
writes it when a person creates the sandbox by hand. Its own comment on the reserve:

> "With no swap, overshooting is an instant kill rather than a slowdown, and the victim is chosen
> across the whole VM — so the cost of being wrong is a dead sandbox, not a slow one."

So on any host with less than 26 GB, the boxes' cap never binds and the VM's own limit is hit
first. Check `head -1 /proc/meminfo` inside against `fleet_memory`. The fix is to read the
sandbox's real memory rather than trust the field.

### 3. Resources are chosen by silence — filed in `docs/TODO.md`

Memory, CPUs and disk are fixed for the life of the sandbox and nothing asks. `config::configured_field()`
exists precisely to tell a decision from a fallback, was written for sizing a new fleet, and is
called for none of the three. Note the wrinkle: in-fleet skein **cannot** propose host numbers,
because `available_parallelism()` inside the sandbox reports the sandbox's cores.

### 4. The reload branch cannot change the supervisor's environment

`bootstrap.sh` sends `SIGUSR1` when a session already exists, so anything baked into the supervise
string — `$SKEIN_HOME`, `$SKEIN_IN_FLEET`, the port, the doorway's argv — is ignored on every
upgrade. It cost two manual `tmux kill-server` steps this session. The script should recreate the
session when that string has changed.

### 5. The token is plaintext at rest

Analysis done, not built. `tests/isolation_bwrap.rs:307` (SKEIN-219) records that on a
volume-mounted fleet — this deployment — `credentials/`, `github-pats/` and `api-token` were once
"a `cat` away" from every box; the control is a mount-*ordering* rule in `box-session.sh`, guarded
by a test that **skips itself** when bwrap cannot make a user namespace.

Agreed design: store `SHA-256(token)` only; migration hashes the existing plaintext in place and
unlinks it so nobody is locked out; browser sessions survive restarts because the hash is stable;
rotation is host-initiated (`sbx exec … skein token --new`), which an in-sandbox actor cannot
originate because there is no `sbx` in there. `util::sha256` is already in the tree (moved there
for this, then reverted with the rest — it currently lives at `ai.rs:643`).

**Be honest about the limit**: this raises a silent **read** to a loud **write**. Anything with
genuine full sandbox access is past every boundary skein has, and the neighbouring credentials must
stay usable plaintext regardless.

### 6. Nine repos to move to HTTPS remotes

Five are `git@github.com:…`. `repos.rs:437` says SSH works only if the host agent is forwarded with
the key loaded, and calls HTTPS "the no-setup path (proxy-injected creds)". Four more are adopted
from host paths that are not mounted.

**`skein add` with an existing id replaces the entry, it does not edit it** — `store`, `agent`,
`plane_project`, `read_prs` and `review_queue` are all reset unless passed. Read `repos.json` first
and carry them across.

### 7. `sbx`'s verb list is quoted from memory in several places

`fleet.rs` reasons from "its whole verb list is `login run ls stop rm create exec cp ports`" —
`sbx --help` shows fifteen more. One conclusion drawn from it is load-bearing: *"sbx has no
unpublish verb, so every mapping is permanent"* justifies the port-burning dance in
`ensure_fleet_agent_port`. The README's own next sentence said `--unpublish` takes one back. Check
`sbx ports --help`; if unpublish exists, that subsystem is solving a problem that no longer does.
Fixed in the README only.

## Environment notes for whoever works in this tree

- **`cargo`'s `target/` on the virtiofs mount went incoherent mid-session** — a directory that both
  existed and could not be created (`File exists (os error 17)` after a successful `rm -rf`). The
  same mount silently corrupted `bootstrap.sh` during a backup-and-restore. Building to a
  sandbox-local `CARGO_TARGET_DIR` avoids it.
- `cargo test --tests` has one **pre-existing** failure,
  `docker_watchdog::a_shield_that_did_not_take_is_reported_as_not_taken`, confirmed failing at HEAD
  before any of this session's changes.
- Lib suite: 869 passing.
- The API token used during the session was pasted into a chat transcript and should be rotated.
