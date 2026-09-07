# Delivering the rewrite

Companion to `docs/architecture.md`, which is the destination. This is how to get there without
destroying a working tool on the way.

**`docs/live-check.md` is the other companion**: what no test on a developer's machine can answer,
because it needs a real sandbox — the door, the cover over the mounted volume, the disk figures, the
warden's record, and the dry run to read before the merge train is switched on. Everything landed
here is proven against the fake-sbx harness and against real `bwrap` where the question was a mount;
that page is the residue, written as commands with what a failure would mean.

**On the commit hashes below.** They are how this plan says a step landed, so they have to resolve:
`git cat-file -e <hash>` is the check, and it is not enough on its own — an object can survive in a
local database while being reachable from no branch, which is what a clone gets — so the check is
`git merge-base --is-ancestor <hash> HEAD`. Every hash across `docs/` was re-derived against `HEAD`
on 2026-09-06, because the repository's history had been rewritten and **all forty-four of them** had
stopped resolving in a fresh clone — while thirty still answered `git cat-file` here, as unreachable
objects a `git gc` would take. Rewrite the history again and the same thing happens, silently.
Commit subjects are stable where shas are not, so
`git log --oneline --all --grep='<subject>'` is how to re-derive one.

## 1. The measurement that should govern the plan

**Measured 2026-09-06: 826 commits since 2026-06-28 — 365 are `fix:`, 266 are `feat:`.** Fifty-eight
percent of the *conventional-commit* work is fixing what was already built — 365 of 631; against all
826 it is 44%. The commands, because the numbers move with every commit and a number nobody can
reproduce is the thing this plan is arguing against:

```sh
git rev-list --count HEAD                   # 826
git log --format=%s | grep -c '^fix'        # 365
git log --format=%s | grep -c '^feat'       # 266
```

Count subjects, not `--grep='^fix'`: `^` anchors at any line start in the body, so `git log
--grep='^fix' --oneline | wc -l` answers 375 and inflates it by ten. The ratio is what governs the
plan and it has been stable — it was 44% against all commits when this was first written at 376, and
it is 44% at 826. The denominator is named because the number invites a challenge that would
discredit the rest, and the fix titles are not polish:

> *a box could read the fleet agent's token, and be root in the sandbox* · *close the ways a
> credential outlived the decision to withdraw it* · *a logged-out box can no longer log out the
> fleet* · *a published port reaches the sandbox's address, not its loopback* · *one unreadable
> directory blanked every box's disk usage*

A big-bang rewrite reproduces the 266 features and rediscovers most of the 365 fixes. The plan below
exists to avoid paying twice for knowledge already bought.

## 2. The only point of no return

**Moving skein inside the sandbox.** Everything else in the architecture is reachable incrementally,
on the current codebase, with the fleet working throughout.

That single move invalidates six things at once, of which the first draft admitted one:

| invalidated | admitted? |
|---|---|
| the credential boundary | yes |
| API authentication — skein now shares a netns with coding agents | no |
| `pick-path` — the native host file picker has no display in-fleet | no |
| the host ssh-agent, which sbx forwards into boxes | no |
| `sbx` availability — it is host-only | no |
| the review queue's host-side credential path | no |

## 3. The sequence

Each step is independently valuable and independently revertible.

**1 — The durable volume, on the current codebase. Done.** The highest-value idea in the architecture
and it needs no rewrite. `$SKEIN_HOME` is **already** a single relocatable root, so the move itself is
close to a mount and an environment variable. The work is in four things none of which is the move —
all four below, plus `skein migrate` and a `VERSION` (`f3d648b`), which refuses a volume it does not
understand rather than half-reading it. **What no test here can establish** is the step's own claim:
that a fleet can be destroyed, recreated and remounted with nothing lost. That needs a live fleet.

- **the mount split.** The API token is safe today *because* `~/.skein/repos` and `~/.skein/boxes` are
  bind-mounted into boxes while `~/.skein` itself is not — "checked, not assumed". Mounting a volume
  root whole puts `credentials/`, `api-token`, `github-pats/` and `tokens/` inside every box's reach
  on the shared uid. The cover is an **inversion derived per box** — tmpfs the state
  root, bind back what this box needs — not a list of things to hide (architecture §9.5.2).
  **Done** (`4b6f3ae`, `a989aed`, `fb40b77`, `36562fc`): the cover is derived per box; declared state
  is not under any mount at all; the volume root and its credentials are stated as a property over a
  *walk of the whole volume*, so a secret written tomorrow at a path nobody listed is private without
  anybody listing it; and a repo pointed at the volume (`skein add --store ~/.skein`, or `/`) is
  refused rather than mounted. The host's working checkout left the sandbox entirely.
- **`places/` holds the box anchors**, which are volume state but **declared** and under the cover,
  stamped with the sandbox generation (architecture §9.5.1). Moving them without the stamp is how a
  rebuilt fleet re-enters a recycled pid. **Done**: the record carries `(generation, pid, starttime)`
  and a crossing is refused when any of the three disagrees — the generation guards a sandbox cycle,
  `starttime` guards pid reuse within one.
- **`repos/<id>/work` is a working checkout**, not a mirror, and `diff.rs`, `moduledocs.rs` and
  `codeowners.rs` read it directly. Repointing them is budgeted here, not assumed away.
  **Done** (`3dac3a9`, `a50aa77`, `9da8725`, `36562fc`): `repos/<id>/mirror` is a bare mirror and is
  what boxes clone from; `codeowners` takes a reader and `moduledocs` reads `repos::Tree`
  (`git show HEAD:<path>`); `diff` had already stopped, when box diffs moved inside the box. The
  trap this bullet does not name, and the one that cost the most to see: **a mirror can never supply
  a gitignored file**, so `shared-paths.txt` — the `.env` and the `CLAUDE.md` a project keeps out of
  git — is not a mirror question at all. Those come from the repo's *source tree*, which is now
  copied into the store on the host, and the checkout is no longer mounted into the sandbox. A repo
  registered from a URL has no source tree at all (`44cd8b6`), so `repos/<id>/` holds a mirror and a
  store and nothing else.
- **no lock on `config.json`/`repos.json`.** Adding schema versions without a writer discipline
  versions the corruption. **Done** (`d7ac7bb`): the read moved *inside* the lock —
  `update_config`/`update_repos` — because an atomic write makes each write whole and does nothing
  about two writers. The test that proves it has to **count**: a version where each thread writes its
  own distinct field passes against the unlocked code, which is how the first one did.

A caveat on schema versions: per-box status and pane JSON are written by **shell probes generated from
the binary**, so versioning those couples probe version to volume schema to binary version. That is
not a serde attribute and should be scoped deliberately.

**2 — Extract `state`, `source`, `signal` and `operation` as modules in the current binary.**
`signals.rs` is already most of the way there, `util.rs` already implements the gate contract, and
the `ensure_*` functions already exist — sixteen when this step was planned, and **twenty `pub` plus
the private `ensure_source_takeover_tools`** (itself a sandbox-root apt install) on 2026-09-06. The
Operation primitive names something the codebase does, and it has kept naming more of it. Reproduce
with `grep -rhoE "pub(\(crate\))? fn ensure_[a-z_]+" src/*.rs | sort -u`; `inventory.md` §8 lists
them by privilege domain. `doctor` becomes "every check, reported" with no UI change.

**Two structural obstacles hit on day one**, and neither was optional: `lib.rs` re-exported sixteen
modules with `pub use *`, so the module graph carried no information about real edges; and there is a
live `place ↔ fleet` cycle.

Both are dealt with except the cycle. The façade is gone (`8da8c5c`) — that was the first task of
this step rather than a tidy-up after it — and the crate-root catch-all it exposed went with it
(`3f82bb4`): `src/lib.rs` is module declarations and nothing else — 78 lines on 2026-09-06 (`wc -l
src/lib.rs`) — and the ten modules its contents became are real nodes in a graph that
`python3 tools/module-check.py` prints the size of on every run — **308 edges over 58 units**, same
date. This paragraph said 58 lines and 417 edges; both were true when it was written and neither is
now, which is why the commands are here and the numbers are dated. The cycle remains, and dissolves
with the transport in step 4 rather than needing work of its own.

**3 — Build the warden, and route create/destroy through it from *host* skein.** Both callers
exercised before anything moves — which was the whole argument for having a warden.
**Done**, one commit per clause: a separate `warden/` crate with an outcome store (`a2e004a`), four
endpoints — two doers behind Cargo features, two reporting endpoints with no feature at all
(`1960493`) — a `/dev/tty` approval surface (`9826937`), and §8.5's doorway (`7e582d2`).
`ensure_fleet`'s create and `resize_fleet`'s destroy go through `warden_client` (`5d99e9b`);
`python3 tools/source-check.py --show` shows `fleet`'s `sbx` spellings down from five to **three**,
and they are the two `ports` calls — `existing_forwards` reads a mapping, `publish_forward` makes one
— and the interactive login in `login_argv`.

**This is an operational change and not only an internal one: a host with no warden running cannot
create or resize a fleet.** Deliberately — an unreachable warden does not fall back to running `sbx`
here, because that fallback would be taken on exactly the day something was wrong. The failure names
the fix and gives the line to run by hand.

Three things this step found that the plan did not have. The **create environment** was going to be
lost in the move (`DOCKER_SANDBOXES_ROOT_SIZE` is the difference between a 20 GB fleet and a 200 GB
one), so it travels with the request and is rendered in the approval. The **Source law could not see
any of it**: skein spawns `sbx` through `run_capture_for`, not `Command::new`, so the checker had
been reporting `fleet` as reaching nothing while it ran the fleet create — fixed in `4bc1196`. And
`bin/skein`'s `sbx` spelling does **not** go away with this step, because it is `attach`.

What step 3 does not close, stated where it will be looked for: before the uid split (4b), any
process at the host uid can reach the warden's file descriptors, so today's approval guarantee is
against a *box* rather than against a compromised skein on the same machine (§8.1).

**4 — The mount cover, then the uid split, then move in.** In that order, and the first two are gates
rather than follow-ups (architecture §9.5).

**4a — the cover. Done** (`07721bd`, `4b6f3ae`) — SKEIN-3. `tmpfs` the state root and bind back what
a box needs, *derived per box*: `src/box-session.sh` covers the fleet root and the box-state parent,
then binds back this box's own root and its own state read-only, and — the part a rule over
`~/.skein` could never have reached — covers **every host path the sandbox mounts** and binds back
only the two this box is entitled to. An inversion rather than a hide-list, so a mount skein starts
making later is covered the day it appears with nobody remembering to add it.

What that closed, and each was reachable read-write until it landed: every other repo's store, every
other repo's work tree on the host, and this box's own work tree at all. `~/.skein/repos` had been
mounted into the sandbox uncovered.

**4b — the uid split.** skein on its own uid, boxes on theirs, every crossing through
`sudo -u <box uid>` — for the launcher and for `nsenter` alike. Do not attempt this as "skein runs as
root" (no user namespace is created at all) or as "skein runs as another uid" (every `setns` is
EPERM); architecture §9.5.1 has the derivation.

~~**Do not start 4b until the anchor moves.**~~ **The anchor has moved** (`eccda3b`, `48efd31`) —
SKEIN-4. It had been read from a file inside the box's own writable root, so a box picked the
namespace skein landed in; a placed box is addressed by its record now, under the cover, and the
record names the box's tmux server with the sandbox boot it belongs to. Everything else in 4b was
downstream of an address it trusts, and now there is one.

**What 4b waits on instead, and it is not a smaller thing: 4c.** The split is skein-on-one-uid and
boxes-on-another, and today skein's control plane runs on the *host* while crossings go through `sbx
exec`, which is the sandbox's root. There is no uid to split from until skein is a process inside the
sandbox. Building the `sudo -u` plumbing before that means writing it for a deployment that does not
exist and cannot be exercised — so SKEIN-35 stays unclaimed until 4c, and the two gates below are the
work that gets there.

Budget the sudoers policy as the security-critical
artifact it is, and one extra `exec` per crossing. Crossings are launch, attach, upload, diff, takeover **and the
tmux control operations** — the socket is a crossing too, and its sockets are `0700` per box. The
board stays off that path only because liveness moves from probing each socket to reading the anchor
pid, which §6 already licenses.

If 4b slips, what remains exposed is a denial of service against the control plane, which the
supervisor restarts. That is a materially different risk from what 4a closes, which is why they are
ordered rather than bundled.

**4b′ — the rest of §9.5.** Eleven requirements exist and an earlier version of this page sequenced
two. The other nine are the security backlog, and they are where a decomposition starts rather than
where it discovers a hole:

| requirement | shape of the work | notes |
|---|---|---|
| ~~R3 control API on a covered socket~~ **dropped, deliberately** | the port stays; §9.5 R3 names what that leaves open | the owner's call: a browser cannot open a socket, and every way of keeping a working URL cost more than the exposure |
| R4 no shared writable executable path | read-only toolchains with a per-box overlay | **user-visible** — one box's `cargo install` stops reaching the others, and the shared build cache goes. In `docs/parity.md` §7. |
| ~~R5 warden secret under the cover~~ **done** | the warden mints it, skein reads it, checked before routing | fails closed: a warden that cannot read its own copy refuses everything and says which failure it is |
| R6 audit log, warden-written, host-side | new: the sink endpoint, and skein reporting into it | never compilable-out |
| R7 credentials never win on self-asserted freshness | ~~replace the expiry comparison~~ — **done**: the comparison could not be fixed, the *direction* was | The expiry is a field inside a file a box writes, and a box legitimately holds the refresh token — so nothing it can produce honestly it cannot also produce dishonestly, and no field in that file is evidence about it. The fleet's login now flows **down only**; a box's reaches the fleet solely when the fleet has none, where there is nothing to poison. Cost, stated: a token refreshed in a box no longer improves the fleet's copy, which ages until `skein login`. |
| R8 no privileged actor follows a box-influenced path | the resize archive, `git-tokens/`, `disk`/`identity`, and the anchor | the largest of the nine; several distinct sites |
| ~~R9 workshop toggle states its terms~~ **done** | the switch and the per-start banner name the same grants — every box's files, fleet scope, the fleet agent token — and say the mount cover is off for it | the last of those is the one a person cannot discover by using the box, and it is what R8's two guards lean on |
| ~~R10 cross-box messaging renders provenance~~ **done** | two directories, not a field: the shared store's `mailbox/` is writable from every box, a box's own `inbox/` under its state is bound read-only into it | §9.2.2 is kept, so this is the mitigation. A box's message still arrives — it arrives *saying* it is a box's |
| ~~R11 `/run` covered, or its exposure stated~~ **both** | `/run/user/<uid>` and `/run/secrets` are private tmpfs per box; `$SSH_AUTH_SOCK` was already covered | `/run/docker.sock` is left reachable **deliberately** — skein configures the daemon for box workloads — and §9.5 R11 states what that grants, because it bounds the mount cover and the R8 guards alike |

R5, R7 and R9 are small. R4 is a product decision as much as a security one. R8 is a cluster, not an
item.

**Which of these gate 4c**, since 4c is the point of no return and the rest is a backlog:

| must land before 4c | may follow |
|---|---|
| ~~**R3**~~ — **decided: the port stays** (SKEIN-76, owner's call). It was never a transport swap — a browser cannot open a filesystem socket, so it decided how a person reaches their own board, and every alternative cost either a proxy the browser then depends on or a tunnel before the first page load. §9.5 R3 records the three residual exposures; the one that mattered is port squatting, which the socket would have closed for free. **Not a gate any more**, so 4c is unblocked | R4 (a product decision, and the exposure is unchanged by the move) |
| ~~**R5**~~ — **done**: the warden checks a shared secret before it routes, and refuses everything if it cannot read its own copy. 4a supplied the mechanism — a file under the cover is readable by skein and unreachable from every box. The narrow bind still stands beside it; the secret is what survives the bind widening at 4c | R9, R10, R11 |
| **R6** — skein cannot audit itself once it shares a sandbox with the agents | |
| ~~R7~~ — **done**, and not by fixing the comparison: the fleet's login flows **down only**, so there is no field a box asserts that anything trusts | |
| ~~**R8**~~ — **done** (SKEIN-79 and its three children). Four sites, and the plan was wrong about which were open: the resize *restore* was already safe and its **create** was not; `git-tokens` was safe where the token is written and not where the directory is made; the settings were closed in the library and open in the cockpit's panel; the anchor was closed and is now a test rather than a sentence | |

**So 4c is gated on nothing.** R6 and R8 both landed; R3 was decided against and R5 before them.
What remains before the move is the move — and the one thing it must carry that is not a
requirement of its own is SKEIN-77, the listening socket opened before any box exists (**done**,
with SKEIN-105 supplying the end that opens it at create and holds it across the doorway's own
restarts).

~~**So 4c is gated on two: R6 and R8.**~~ It was five when this table was written, then two, and is
now **none** — R3 was decided against (the port stays, §9.5 R3), R5 landed, R7 turned out to be
answerable by changing the direction credentials flow rather than by trusting a better field, and R6
and R8 are done.

One thing 4c must carry that is not a requirement of its own: **the cockpit's listening socket is
opened before any box exists and inherited across restarts.** Keeping the TCP port left port
squatting open (§9.4), and a port that is never free is what closes it. It belongs here rather than
in its own item because there is nothing to hang it on until there is a fleet start. **Done**
(SKEIN-77, SKEIN-105): §9.4's squatting bullet reads closed rather than half.

**4c — the move**, with host-driven mode still working one environment variable away. **Started**,
and written down: SKEIN-101 with eight children, one per row of §2's table plus the mechanics.

The variable exists (`SKEIN_IN_FLEET`, `src/deployment.rs`), and what it decides is **enumerated**:
the module carries `CONSULTED_BY` — eight units at this writing — and a test fails the build when a
unit starts branching on the deployment without being added to it, or stays listed after it stops.
It began as "reported by `skein doctor` and read nowhere else", deliberately, so the seam existed
before anything leant on it. The move lands one change at a time, and "what does this flag change
so far" has to stay answerable for that to mean anything.

Declared rather than detected, because every detector anybody would write — is `/run/sandbox` there,
is `sbx` on `$PATH` — is a guess about somebody else's machine, and the two wrong answers are not
symmetric. A fleet process that thinks it is on the host runs `sbx`, fails, and says so. A host
process that thinks it is in the fleet stops reaching a fleet only it can reach, and the symptom is a
fleet that appears to have no boxes.

**Five of §2's six rows now have their answer, and one of them was not a row at all.** The file picker
went from both boards rather than being made deployment-dependent (SKEIN-106) — the answer had
already shipped at `/v2`, and a button that is there and does nothing is how somebody concludes skein
is broken. The credential turned out to be two-thirds already built (SKEIN-107): a GitHub App and
per-repo tokens are skein's own and flow down only, and only the account token is the host's — so the
work was stopping skein *claiming* one it cannot have, since that label is what the checklist reads
as "boxes can push". And **the forwarded ssh-agent is not invalidated by the move** (SKEIN-108): the
forward is `sbx create`'s, from the host into the sandbox, so it is in the same place whether skein
is beside it or outside it. What does not travel is the key *file*, which is a host path — so
`ensure_ssh_key` refuses with where to run `ssh-add` rather than failing on a missing file, which
reads as a mistyped path.

**Every host-only call is answered** (SKEIN-104), and they did not all want the same answer. Two
stop existing: in-fleet the agent's port is not published at all — it is on loopback at the port it
listens on, and publishing would forward a port to the machine skein stands on — and `skein login`
runs its command directly rather than through `sbx exec -it`. Two answer as far as they can: `sbx ls`
asks about the host's machine and returns the `None` callers already fall back to the registry on,
with the *reason* recorded so the board does not report a broken sbx for a deployment where its
absence is correct; and a missing `sbx` stops being a fault, since a banner red for a correct state
hands somebody something they cannot clear. One refuses: the GitHub secret is host-side on both
halves — `gh auth token` reads the host's login, `sbx secret set` writes the host's keyring — and it
says so rather than returning `Ok(())`, because seeding is how boxes get a credential and a quiet
success is a 403 inside a box some minutes later.

That work found a hole in the Source law itself. Splitting the login into a `(program, argv)` tuple
put `"sbx"` on its own line, `fleet`'s count fell from two to one, and **the call it stopped counting
was the one that still runs `sbx`**. The pattern now allows whitespace after the paren — and with it
the checker saw, for the first time, `sbx ports … --publish`: the more privileged of the two `ports`
calls, since a publish creates a host mapping while the read only reports one. (It was justified here
as "sbx has no unpublish verb and every mapping is permanent" — `sbx ports --help` takes
`--unpublish`, so the mapping is not permanent; the *reach* is still the privileged one, and it is
what the Source law is about.) A law a reformat can repeal is not one.

**The crossing is the first thing that leans on it** (SKEIN-103). A crossing has two hops and only
the first depends on where skein runs: `sbx exec [flags] <sandbox>` from a host, nothing at all from
inside, since skein is already there and `sbx` is host-only. `Place::reach` is that hop and the only
place it is decided; `enter()` — the `nsenter` into the box — is unchanged in both, which is why this
is a hop removed rather than a transport rewritten.

Two things it found rather than planned. **A box whose sandbox is its own cannot be reached from
inside the fleet's**: `SandboxItself` has no second hop, so dropping the first as well runs the command
in *skein's* sandbox — a different machine with the same paths on it. It refuses in-band, the way an
unplaced box already does. And **the cockpit's terminal stopped naming `sbx`**: it spawned a literal
`CommandBuilder::new("sbx")`, which cannot be told the deployment changed the program, so
`interactive_argv` now returns the whole argv including argv[0]. `bin/skein-server` came off the
`sbx` row of `docs/sources.toml` as a result.

**The two rows nobody had to build turned out to be already answered, one with a stated residue.**
API authentication survived the shared netns before the move reached it: the API takes a token
(`src/apiauth.rs` — measured from a box first, `curl` to the cockpit answered 200 with no
credential), connecting is rate-limited pre-auth (`src/knock.rs`), and the socket handover below is
what answers the one attack the token cannot (§9.4's squat). And the review queue's credential path
mostly rides the volume already: `prq::host_credential` resolves `$GH_TOKEN`, then the read token
and any write PAT — both `gitgate` files under the volume, which travels — and only its *last
resort* is host-only (`gh`'s keyring). A host whose sole credential was that keyring loses the queue
in-fleet, and `host_token`'s refusal already names the three ways to hand it one.

**The mechanics landed (SKEIN-109), and each piece was found rather than invented.** The binary is
carried, not embedded — a binary cannot `include_str!` itself — as the sibling of the running
executable or whatever `$SKEIN_SERVER_BINARY` names, refused without an ELF header so a mac host's
own build fails as a sentence naming the musl cross-target instead of as a start bug later. The
install is the launcher's own stdin trick, and the trick already carries megabytes: a body up to 1
GiB rides the agent's chunked `/write`, and past it (or with no agent) `sbx exec -i` streams from a
thread with no ceiling — verified byte-for-byte at 1 MiB against the fake sbx, no new chunking. The
socket is opened by a **doorway** (`src/server-doorway.py`), not by the server: it binds the
cockpit's port, then fork-and-execs `skein-server` behind descriptor 3 in systemd's spelling, which
`src/doorway.rs` (SKEIN-77) already validates from the inheriting side — so a server crash restarts
the server under a door that never closed, and `SKEIN_LISTEN_INHERITED_ONLY=1` turns a start that
lost its descriptor into a refusal rather than a re-run of the race. The port is published **last**,
once something holds it, reusing mappings before making them for the reason the agent's port does.
`skein fleet-serve` is the sequence end to end; a `skein-server` run on the host sets none of this
and is unchanged.

**The door opens at fleet *create*, not at serve** (SKEIN-105), which is the moment that actually
closes the race: `ensure_fleet` opens it before it installs the launcher — the thing without which
no box in that sandbox can exist — so there is never an interval in which a box and a free cockpit
port coexist. The doorway holds the port with *nothing* behind it until a binary arrives, which is
what makes that ordering possible at all. It is reported rather than fatal there, and the reason is
read off `box-session.sh` rather than chosen: a box without python3 still starts (it loses shared
logins), so refusing every launch on a fleet whose image has no python would be a larger outage than
an exposure that needs a published mapping before it is reachable at all.

The other half of SKEIN-105 was the doorway surviving **its own** restart, and it took four things,
each of which was a way the port became free again. A re-serve **reloads** rather than stops and
starts: `SIGUSR1` makes the doorway `exec` itself across the same descriptor, so upgrading a live
fleet never closes the listener — proved by reading `/proc/<pid>/fd/3` before and after, same pid
and same socket inode, in `tests/fleet_move.rs`. A killed doorway takes its server with it
(`PR_SET_PDEATHSIG`), because the server inherited the listener and an orphan holding it is a wedge
rather than a window: nothing can re-bind. The supervisor's delay became **conditional**, so a
doorway that had been working is replaced in the time python takes to start — measured at ~20ms
against the 2s it slept before — while one that cannot start at all still backs off. And the host
mapping is published only when the **doorway** holds the port, read off the pid it stamps: a
squatter accepts a TCP connect exactly as the doorway does, so a connect-only judgement is how the
browser and its token get handed to a box. `sbx ports --unpublish` is not the answer to it: it
withdraws the *host* end of the mapping, and by the time anyone knows to run it the token has
already been handed over — and the bind that was stolen is at the sandbox end, inside the shared
namespace, where nothing on the host side reaches.

**And the move's one create-time difference — mounting the volume — is covered, not granted.**
The server needs the volume mounted; 4a's inversion covers "every host path the sandbox mounts",
but it used to skip any mount that was an *ancestor* of its own covers, because a tmpfs over
`~/.skein` written after the `~/.skein/boxes/<box>` binds throws them away. The volume root is
exactly that ancestor, so mounting it handed every box `credentials/`, `api-token`, `github-pats/`
and `tokens/` — step 1's exposure, back — and `fleet_serve_mounts` refused unless
`--uncovered-volume` said the derivation had been read (R9's shape).

Ordering closed it (SKEIN-219). `box-session.sh` now tmpfses every ancestor mount **before** the
fleet root and the state parent are bound back; bwrap resolves each `--bind` source against the
original filesystem, so those binds still land through the cover. Enumeration was never needed —
the ancestor is derived from `$SKEIN_FLEET_MOUNTS`, so a volume mounted somewhere new is covered
the day it appears. Proved rather than argued:
`tests/isolation_bwrap.rs::a_box_on_a_mounted_volume_cannot_read_the_fleets_credentials` builds a
volume-shaped fleet, runs real bwrap, and reads those three paths back as `gone` while the box's
own store, state, git token and checkout still answer. The flag is gone with the exposure; a fleet
that serves is a fleet whose boxes still cannot read its credentials.
 This is where
the six items in §2 get answered, with a fallback available while answering them.

**5 — The cockpit.** Orthogonal, and it can start on day one — with three things named rather than
assumed, because "the API is a stable seam" is true of transport and false of semantics:

- **some semantics are client-side, not all.** The server already computes `tier` (a six-level
  who-needs-me ranking), `pause`, `headline`, `task`, `blocked_kind`, `hook_health`, `screen_health`,
  `scoped` and `diff`. What lives in the page is `GROUPS`, `NEEDS_YOU`, `labelOf`, the away deltas and
  provenance *rendering*. The conclusion holds; the earlier evidence for it did not. A `/v2`
  cockpit re-derives them and drifts from `/` unless they move server-side — which the architecture
  requires anyway for transitions, and which means new endpoints, i.e. not purely "against the
  existing server".
- **box creation is not a route.** It happens over the WebSocket, via `?launch=<branch>` on the
  terminal endpoint. Port the REST API and you lose box creation.
- **~~the asset layer is compile-time~~ — built, so this no longer gates anything.** It was: four
  vendored scripts as four `include_str!` constants and four handlers, with nothing for a bundle
  whose file *names* carry content hashes to be registered as. `src/assets.rs` is the answer and its
  own doc opens by saying which shape it replaced — one route (`any_asset`), a table generated from
  a directory by `build.rs`, no code per file, five `/vendor/*` names still resolvable by their old
  paths. Embedded by default, because skein is one binary that cannot be half-upgraded; overridable
  from `$SKEIN_COCKPIT_ASSETS`, which is what makes a stylesheet change a reload rather than a
  `cargo build`. **The "do it first" this bullet demanded has been done**, and a `/v2` bundle can be
  served without touching the binary. Treating "ground-up surfaces" and "new topology" as one project is the single biggest
avoidable risk in the plan.

**What runs in parallel from the start:** the component library, the GitHub module (already
self-contained since `gh` was dropped), and the warden's two removable capability modules.

**The cost of incremental**: two placement shapes and the `sbx exec` fallback survive one more cycle
— the very things the architecture wants deleted. That is real. It is smaller than a six-month branch
against a codebase taking ~200 commits a month.

## 4. Migration

Existing users have live fleets, boxes holding unpushed work, and configured state. This is what
breaks, and none of it was in the first draft.

### 4.1 Lost silently unless explicitly carried

- **Unpushed work in every box.** Checkouts are VM-local, held nowhere on the host.
- **Every box's conversation**, keyed by a cwd-derived slug — so *any* change to box root layout
  orphans all of it. There is already one repair in the codebase for exactly this.
- **Every box's HOME**: `.claude.json`, `.codex`, `.gitconfig`, per-box MCP registration.
- **Secrets with no second copy**: `tokens/`, `github-pats/`, the read token, the App key, the API
  token, and `fleet-home/` — the agent logins copied out of the sandbox.
- **Grants and approvals as facts**: git-write grants and the approved-package manifest exist
  *because* the sandbox-side queues die with the VM. Drop them and every box re-asks.
- **Per-box overrides**: privileged, tracking, git-scope, disk, identity.
- **Journals, diffs, mailbox, handoffs** — deliberately not deleted on box destroy, because for a
  clone-mode box they are the only durable record.
- **The repo's shared `.claude` store** — and stores adopted from outside the state root via
  `--store`, which must **not** be relocated.
- **`review/<repo-id>/`** — the PR archive, standing module notes and the summary cache. Parity lists
  all three as capabilities.
- **`connections.json`, `repos.json`, `config.json`** themselves, `places/`, `starts/<name>.err`, and
  local commits or branches in `repos/<id>/work`.

### 4.1a What must *not* be carried

Equally important and easier to get wrong. `fleet-agent.token` and `fleet-agent.port` are
**instance-scoped**: the migration re-mints them rather than copying, or "no machine-global secret" is
untrue on day one. Same for the seeded `sbx secret` and its `gh-secret-seeded` marker — the secret
outlives the store that recorded it, so losing only the marker means a keyring prompt at every server
start.

### 4.1b Files named after the fleet sandbox, which belong to no box

A file keyed on the *sandbox's* name is not a box's anything, and a fresh fleet must not inherit
them or start making more. Both halves are measured, on this host, 2026-08-25:

```
find /Users/you/.skein/repos/*/store/.claude \
     \( -name 'skein-fleet' -o -name 'skein-fleet.*' \) | wc -l          # 36, in 5 stores
find /Users/you/.skein/repos/*/store/.claude \
     \( -name 'skein-fleet' -o -name 'skein-fleet.*' \) -newermt 2026-08-05   # 2
```

**Do not carry the 36.** `skein-fleet` is `config::default_fleet_sandbox`, no box has ever been
called that, and `board::load_views` strips the name from the board — so every one of them is
already invisible from every surface and nothing reads it. They predate the `box` field in the
observation, so they name nobody: `signals::signal_is_ours` passes them (correctly — a signal that
names nobody cannot be checked), the `misfiled` badge cannot reach them, and no future attribution
work can, by construction. A file with no name in it can never be proven wrong. The only thing that
makes dropping them safe is that the name is the sandbox's; **prefix matching is not safe** —
`store/.claude/skein/launch/skein-fleetsmoke.json` is a real box's file and starts with the same
eleven characters.

**The two are the part that matters**, because they say the class is still being produced:

| file | last written | what it is |
|---|---|---|
| `gadget-demo/store/.claude/workflow-journal/skein-fleet.tsv` | 2026-08-25 18:10 | 240,952 bytes, 2,474 lines, **157 distinct session slugs fused into one file** — 3.6× the largest correctly-named sibling in the same directory |
| `gadget-demo/store/.claude/slice-gate/skein-fleet` | 2026-08-25 17:06 | the per-box gate state, shared by every box in the sandbox |

Neither is written by skein. They are the *repo's own* hooks, in that repo's store
(`.claude/hooks/wf-journal.sh` and `.claude/hooks/slice-gate.sh` **in that repo, not in this one**,
so neither path exists here to open), and both spell the box as

```sh
vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
```

which is precisely the chain SKEIN-224 removed from skein's own probes — and in a shared sandbox
every box in it answers to the same string. `wf-journal.sh`'s own comment says the shards exist "so
that boxes in the same folder each append to their own shard": the intent is per-box, the effect is
one file, and nothing anywhere reports the difference.

So the rule the recreation has to carry is not about these two files. It is that **`SKEIN_BOX` is
the identity contract and skein is the only thing that knows it.** Fixing skein's probes fixed
skein's probes. A store also holds hooks skein did not write — a repo's own, and a handoff snapshot
of them — and those keep the old chain until somebody tells them, which nothing does. A fresh fleet
regrows this on day one unless coming up includes announcing the contract to hooks skein does not
own, or refusing to run one that has not read it (SKEIN-322).

### 4.2 Breaks quietly rather than loudly

- **Every config field has a serde default**, so a format change reads as "the user chose the
  default". One partial write previously unmade a whole fleet.
- **Hook commands are baked into each repo's `.claude/settings.json`** as absolute-ish paths. There
  is already a legacy-path repair for one rename, because merging is additive and the stale entry sat
  *beside* the correct one — the box worked perfectly and announced a hook failure at every start.
  Relocating these without the same repair takes every existing box dark, silently.

### 4.3 `skein migrate`, and what it must refuse

One shot, and it must:

1. snapshot every box before anything changes;
2. copy the secret files onto the volume;
3. rewrite hook paths in every registered store, using the existing legacy-repair pattern;
4. **stop every box first** — a live box's tmux server, cgroup and bind mounts anchor to paths under
   the current root, so moving it under a running fleet breaks live sessions and orphans namespaces;
5. **verify the restore, and refuse on a failed snapshot** — naming what could not be read. The rule
   is narrower than "`--ignore-failed-read` is banned", which is false tree-wide: it is banned where
   the output is a **restore**, and permitted where the output is declared best-effort and every
   warning is captured;
6. leave the old state directory intact.

**And the case that is not `skein migrate` at all: somebody copies the volume by hand.** Asked by
the owner — "I point at the existing `.skein` folder and spin skein up, that works, right?" It does
when the folder is where it was written, and it half-worked silently when it was not. `repos.json`
holds each repo's `store`, `source`, `source_tree` and `work` as absolute paths *under the volume*,
so a `cp -a` opened at its new path went on reading and writing the **old** one — perfectly, and
invisibly, until somebody deleted the original. Reproduced before it was fixed: a byte-identical
copy reported its store under the source path and `skein repos` said nothing.

The volume now records where it was written (`written-at`, beside `VERSION`), and `ensure_volume`
compares. A marker that is wrong about **nothing** — no repos yet, or every store deliberately
elsewhere — is corrected rather than raised, because refusing over a fact with no consequence is how
a check earns the reputation that gets it switched off. A marker that is wrong about something is a
refusal naming both intents, since both are real and they want opposite things: `skein repoint` to
make this copy stand on its own, or `export SKEIN_HOME=<recorded>` to go back to the original.
`repoint` is the same rewrite step 3 above does, run without the copy — the copying already
happened, by whatever means.

An earlier draft said "refuse when any box has unpushed commits **or a dirty tree**". Every actively
worked box has a dirty tree, so that rule refuses always — and it sat *after* the snapshot, which is
the thing that makes a dirty tree survivable in the first place. The rule matching this codebase's
actual instinct is point 5: `--ignore-failed-read` is banned precisely so a copy that cannot read
everything stops rather than restoring a box short of its contents with nobody told.

## 5. Landmines

Where the current code encodes knowledge a rewrite pays for twice. Each presents as an intermittent
mystery rather than a clean failure.

**The fleet sandbox does not outlive an `sbx exec`, and this document used to assume it did.**
sandboxd auto-stops a sandbox ~30s after a session **disconnects** from it. Idleness is not the
trigger and neither is the agent exiting: a sandbox nothing has ever attached to runs indefinitely,
and the fleet's own pid 1 is a `sleep infinity` that never dies. `sbx exec` is a session, so the
in-fleet install — which is `sbx exec -i skein-fleet bash < bootstrap.sh` — arms the timer as it
finishes and the fleet stops about 35 seconds after the install reports success. Measured, three
minutes per phase: never-attached ran throughout; one `sbx exec … true` stopped it 33-39s after the
exec returned; `sbx run -d` ran throughout. The cure is `sbx stop` then `sbx run -d`, which restores
the never-attached state permanently. There is no setting — `sbx daemon` offers only `log-level`,
`restart`, `start`, `status`, `stop`.

**Why it did not bite host-driven skein, which is the part worth carrying.** Not because skein was
poking sbx: with the fleet agent configured, the agent client read the recorded verified port and
`Place::bytes` answered over HTTP, never reaching the spawn. Host-driven skein made no `sbx` calls
at all. (Both halves of that sentence are history — the agent and the deployment are deleted,
SKEIN-521 — and the lesson below is why it is kept.) It survived because **HTTP to a published port is not a session**,
so nothing ever armed the timer. The lesson generalises past this bug: the fleet is safe for as long
as it is driven over its ports, and every host-side `sbx` round trip is a small act of sabotage
scheduled 30 seconds out. Anything a rewrite adds that shells out to `sbx` against the fleet
inherits that, including a diagnostic someone adds to make this easier to debug.

**Process plumbing.** stdin must be written on its own thread — a pipe holds ~64 KB, past which
`write_all` blocks *before* the timeout starts, so this is the only reason the call has a deadline at
all. stdout must be nulled or a chatty command looks like a hang. Kill must be followed by wait or
every timed-out write leaks a zombie. `-t` corrupts binary bytes. In chunked upload an empty *body* is
legal but an empty *piece* mid-stream terminates the stream and silently truncates the file.

**The screen grammar.** Every line of it is empirical. The busy detector keyed on the end of the
status line and flipped a live box between working and waiting every two seconds. The spinner set is a
**denylist**, because the animation cycles through at least six glyphs including a plain ASCII `*`,
and an allowlist re-breaks the day a release adds a frame. `esc to interrupt` never appeared across
four minutes of continuous real work. Only the bottom of the screen counts. **The architecture is the
easy half; the grammar is the product.**

**The snapshot sweep.** Ignored files are work too — the sweep was once `--others --exclude-standard`,
so `.env` and each box's own journal were silently left behind on every migration and every resize.
The bundle must be checked for the box's own branch. **Symlinks need three rules, not one**: a box's
`.env` is often a symlink into a host mount that does not exist in the fleet, and tar preserving it
*as a symlink* is the bug — the box gets a dangling link where its config should be, which looks like
the file is there. So it is carried as a symlink **only while it points inside the tree**; pointing
outside it is carried as its **content**; already dangling it is **reported, not carried**. An earlier
draft of this page stated the bug as the rule. Not
`--ignore-failed-read`, because that restores a box short of its contents with nobody told. The agent
login must be captured *before* the destroy — measured the hard way, when a login made between two
resizes was gone after the second.

**Credentials.** `iat` backdated a minute, because GitHub rejects a future-issued token and Macs
drift. chmod **before** rename, because the generic atomic write takes the umask and leaves a window
at 0644 — and the tokens handed to boxes took the weaker path. An unparseable expiry counts as
**expired**, because the alternative fails open and turns a 24-hour grant into a forever one.

**Gating and backoff.** On failure the old code re-armed at the same interval, so a slow daemon was
asked more often than it could answer and every attempt was SIGKILLed with the guest work still
running. Check-then-act gave every browser tab its own subprocess every tick.

**Memory and cgroups — and one number that must not be copied.** The reserve is
`(1024 + total/50).min(total/2)` — about 1.5 GiB on a 26 GiB fleet. **574 MiB is the *measurement the
formula was calibrated against*, not the reserve**, and an earlier draft of this page said otherwise.
Implementing 574 MiB reproduces exactly the failure the comment exists to prevent: *"With no swap,
overshooting is an instant kill rather than a slowdown, and the victim is chosen across the whole
VM — so the cost of being wrong is a dead sandbox, not a slow one."*

`memory.high` below `memory.max` is a **per-box** rule, so an overshooting box gets slow rather than
killed. At **fleet** level the same instinct went the other way and cost the most: capping the
`docker` cgroup with `memory.high` wedged the whole fleet — `pgscan` 43,232 MiB against `pgsteal`
45 MiB, ~1,695 throttle events a second across ten of eleven cores, indefinitely, with 16 GB free —
because init, socat and dockerd share that cgroup and the stall landed on the sandbox's own service
path. **The fix was to move the containers, not to adjust the ceiling.**

The cgroup must be removed only after the tmux server is gone, or a box later given the same name
inherits the old limits.

**Terminal responsiveness.** The named regression test guards `load_views` specifically — the 1–2 s
call on the 2 s event tick — because running it inline caused typing to lag *only when the box was
idle*. It is **not** true that every library call is on a blocking-safe executor: roughly twenty
handlers still call synchronously. Stated as "every call", a rewrite would not know it has to decide
per call, which is the actual requirement.

### 5.1 Landmines the first pass missed

- **Launcher/binary version skew can take the whole fleet down.** An unrecognised ceiling value must
  be *skipped*, not evaluated: under `set -u`, arithmetic on a word aborts the shell. Measured — a
  skein sending a new ceiling spelling to sandboxes still carrying the old launcher killed every box,
  and because the launcher died before tmux, each reconnect reported a namespace error for a fleet
  that actually needed a file copied. **This is a direct constraint on the warden protocol.**
- **Half a cgroup ceiling is worse than none.** Both halves are read before either is written, because
  a `high` with no `max` above it is the throttle-forever shape.
- **Ceilings scale down to observed memory, never up** — the sandbox's memory is fixed at creation, so
  editing it without rebuilding describes a VM that does not exist.
- **`chmod` follows symlinks.** Setting permissions followed a box's `.claude` into the shared store
  and left it world-readable. Same family as the credential chmod rule above.
- **Pane files collided across the fleet** — every box's screen observer wrote one filename, so no box
  had a fresh observation. Same shape as cgroup name reuse.
- **The registry self-heals a stray leading brace**, seen in the wild and repaired on read.
- **Terminal scrollback is carried over by hand on reconnect** — tmux repaints only the visible pane,
  so without it a server restart wiped everything you had already read.

Two corrections to the sweep entry above: the untracked sweep was **added to**, not replaced — both
passes still run — and what makes the ignored pass safe is that it filters by **size, not names**
(a hand-written list of build directories is wrong for the next language), writing refusals to a
skipped-files report.

## 6. Underspecified — settle before two engineers build incompatible things

- **Signal schema**: identity/key, value type, how cost is expressed, and who enforces the
  budget. *(Fusion is no longer here — architecture §2.2 defines it, matched against the code.)*
- **Operation failure model**: partial progress, check-passes-but-doer-errored, what the reconciler
  runs on, and stuck versus slow — which the gate story says is the most important distinction here.
- **The component list versus the surface**: eight components against ~155 elements. Missing at
  minimum: settings, the tabbed session dock, the file browser with markdown and image rendering, the
  mailbox composer, two shapes of approval row, the transcript reader, the session digest, the away
  overlay, toasts, four modal dialogs, the mobile key bar, the resizable gutter.
- **Where the box checkout lives**, precisely. The conversation's storage key is derived from the
  checkout's absolute path, and a past move orphaned 25 MB of transcript.
- **The warden protocol**: transport, auth, versioning, idempotency keys. The current in-sandbox agent
  already learned this — an agent from before versions existed answers with its name alone, and that
  is protocol 1.
