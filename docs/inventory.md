# What skein actually does

Derived from the code, not from recollection. Written because two rounds of review showed the same
failure: the architecture's *diagnostic* claims held up and its *prescriptive* ones kept being
falsified by details of the current system. This document is the reality the architecture must be
derived from.

Every number here is reproducible by the command beside it.

---

## 1. Privilege — there are two domains, not one

The architecture counted six privileged operations, then two, and both times said "nothing in normal
operation is privileged". **That is false**, and the reason is that only one of two domains was
counted.

### 1.1 Host privilege — needs something outside the sandbox

| operation | tool |
|---|---|
| create the fleet | `sbx create` |
| destroy the fleet | `sbx rm -f` |
| publish a port | `sbx ports --publish` — **no unpublish exists** |
| seed the fleet-wide credential | `sbx secret set -g`, token on the argv |

**And this table is host *privilege*, not host *dependency*.** `sbx exec` — the entire transport —
plus `sbx ls --json`, `sbx ports` (read), and the interactive attach paths are all host-only too. They
need no privilege, so they are not in this table, but omitting them is what makes "four" look tidy.
The rewrite deletes them (architecture §13a) rather than delegating them.

Four, and two of them dissolve: the credential seed becomes a file write once credentials live on the
volume, and port publishing folds into create **only if the cockpit is the sole port**. That is what
survives of the collapse to two — it was right *for this domain* and was wrongly stated as covering
everything.

### 1.2 Sandbox root — used constantly, in normal operation

`grep -c "sudo " src/box-session.sh` → **21**; tree-wide it is **63 lines across 8 files**. The
distinct call sites:

| when | what | where |
|---|---|---|
| **every box start** | create the box's cgroup, enable `+memory +pids` | `box-session.sh:86-87, 860-862` |
| **every box start** | write `memory.max`, `memory.high`, `pids.max` | `:871-873` |
| **every box start** | move the session into its cgroup | `:877` |
| **every server start** | write cgroup ceilings for every box — `heal_fleet` shells the launcher's `--ceilings` path, a *different* mechanism from the per-box writes above | `fleet.rs:948` → `box-session.sh:371-378` |
| cockpit "apply now" | write one box's ceilings with `sudo tee` | `fleet.rs:866-904` |
| **every box start** | replay the approved-package manifest as root (via `ensure_fleet`, not on server start) | `fleet.rs:1625-1628` |
| **every box start** | create and chown the fleet root (one caller: `ensure_fleet`) | `fleet.rs:1876` |
| every box start **and** every server start | write `/etc/docker/daemon.json` (ensure *and* heal) | `fleet.rs:1053` |
| **resize** | `tar` the whole box tree, and restore it | `fleet.rs:3171, 3205` |
| **on approval** | `apt-get install` **or `npm install -g`** the approved packages | `substrate.rs:226, 236-237` |
| **every box destroy** | `rmdir` the box's cgroup | `sandbox.rs:526` |
| **every box startup** | apt in the startup kit | `kit/skein-startup.sh` |
| takeover setup | `apt-get install` the tools a source box needs | `lib.rs:1489` |

So the honest statement is: **normal operation is full of sandbox-root work.** Cgroups on every box
start *and* every box destroy; the package manifest replayed on every **box** start (through
`ensure_fleet`, not on server start); and ceiling healing on server start through a *different*
mechanism — the launcher's ceilings path, not the per-box writes above.

### 1.3 What this means for the design

**skein performs this domain with `sudo`, as it does today.** An earlier note here said skein would
run as *root* and so supply the domain directly; root turned out to destroy the box user namespace
entirely (architecture §9.5.1), and the claim went with it. The asymmetry that matters is already
built: **skein can escalate, a box cannot** — inside its user namespace a box's `sudo` has nothing to
escalate to, which is why the in-box shim is a message rather than a boundary.

Four kinds of sandbox-root work, each wanting naming in the architecture rather than eliding:

- **resource ceilings** — cgroups, per box start and per server heal
- **package installation** — apt, on approval and on replay
- **filesystem ownership** — the fleet root, and box archive/restore
- **container runtime config** — `/etc/docker/daemon.json`, written in both the ensure and heal paths

Note the last one contains the resize mechanism, which is the next finding.

---

## 2. Resize does not do what the architecture says

`grep -rn "snapshot_box" src/ tests/` → **no production caller.** Two test call sites only.

The architecture describes resize as carrying "the delta — unpushed commits, index and worktree
patches, untracked files" and claims "that is what today's snapshot already does". It is not.

Real resize is `sudo tar -cf` of the entire `/boxes/<name>` tree and `sudo tar -xf` to restore
(`fleet.rs:3171, 3205`) — a root byte copy including `.git`, `node_modules`, `target`, the private
HOME and `/tmp`, which is why it demands 1.2× the box size free before starting.

`fleet.rs:3126-3128` records the move away from the bundle-and-patches approach deliberately: *"the
reconstruction is slower, less faithful, and it is where the fragility lives."* Note the scope —
`:3123-3125` calls that approach *"the right shape for a migration"*. It was abandoned **for
resize**, not abandoned.

**The architecture prescribes returning to an abandoned mechanism and describes it as the status
quo.** Whatever resize becomes, it starts from the byte copy, and the reasons for the byte copy are
written down.

---

## 3. State: twelve writers, not one

`grep -rn "write_atomic\|fs::write" src/*.rs` by module:

```
lib.rs 104 · fleet.rs 67 · place.rs 14 · gitgate.rs 12 · signals.rs 8
moduledocs.rs 6 · mailbox.rs 5 · diff.rs 5 · repos.rs 4 · util.rs 3 · tracking.rs 3
```

**Those counts include test fixtures and are inflated roughly fourfold.** `signals.rs` and `diff.rs`
have *zero* production writes — every hit is a test. Non-test, it is ~55 sites across 21 files, and
`gitgate.rs` outranks `fleet.rs`. The argument survives; the evidence did not, and counting test
fixtures as writers is the same class of error this document exists to stop.

"Declared state has one writer" is a goal, not a description. And the split into *declared* and
*recorded* is not exhaustive: the package queue and the git-write queue are **durable, written by an
untrusted party (a box), and read by the approving side.** That is a distinct kind — **requested** — and giving it no rules is what produced the defect in §4.

---

## 4. The approval path has a TOCTOU, currently masked by a bug

Two findings that must be fixed together, because **fixing either alone is worse than fixing
neither.**

**The request never lands.** `substrate_dir()` is `/boxes/.skein/substrate`, and `/boxes/.skein` is
`--ro-bind` in every non-privileged box (`box-session.sh:947`). So a box running
`sudo apt-get install` gets a write failure — while the shim prints *"It files a request for this
fleet's owner to approve in the cockpit."* Nothing was filed. The git-write path has the same shape.
`tests/substrate_request.rs` drives the function directly, outside a box, so it cannot catch this.

**And the install trusts the requester's payload.** `substrate.rs:245` re-reads the whole request at
install time and checks `state` and name *shape* — but `packages` comes from that same re-read. The
comment above it says *"the file between them is writable by every box in the fleet."* Approve `jq`,
rewrite the file, get arbitrary names spliced into a root `apt-get`.

**The read-only bind is what currently masks the TOCTOU.** Make the queue writable to fix the first
bug and the second becomes live. The rule that fixes both: **the approving side writes the approved
artifact, and the installer reads only that** — never the requester's file.

---

## 5. The fusion function contradicts the architecture's own law

`signals.rs:528-553`. Four rules, and three display an edge with no level signal behind it:

| rule | condition | result |
|---|---|---|
| 1 | no level observation at all | the edge alone |
| 3 | edge is **more than a second** newer than the sample (`> level_ts + 1`, a deliberate tie-break) **and** is an outcome | the edge overrides the level |
| 4 | `Screen::Unknown` | the edge alone |
| 5 | screen is `Dead` **and** the edge is `done` | `done` — the edge wins *without* being newer, because a human-set outcome is not something a screen can contradict |

The architecture says "no displayed state may rest on an edge alone". The code is **right** and the
law is too absolute — rule 1 with nothing shown would be worse, and rule 3 is simply the fresher
observation winning.

What makes the code honest is the shipped provenance display: `hooks only`, `screen lost`,
`screen unread`. So the law should be:

> **No displayed state may rest on an edge alone *without saying so*.**

Which the parity gate already requires, and the first draft's absolute version forbade.

---

## 6. The module graph carried no information, and now carries most of one

`src/lib.rs:35-50` was `pub use <mod>::*` over sixteen modules, so almost every cross-module
reference went through the flat root namespace rather than a module path. `grep -rn "crate::signals::"`
from other modules returned **0** — not because nothing used it, but because everything used the
re-exports. Any module graph drawn against that code was aspiration.

Removed in `8e38964`: `grep -c 'pub use' src/lib.rs` → **0**, and every reference is now a qualified
`crate::<mod>::` path or an explicit `use crate::<mod>::…`.

The crate root followed in `6e3944b`. `wc -l src/lib.rs` → **58**, and
`grep -cE '^(pub )?(fn|struct|enum|impl) ' src/lib.rs` → **0**: what it held became `registry`,
`sbx`, `board`, `kit`, `probes`, `digest`, `handoff`, `takeover`, `sharedhome` and `cockpit`.

**The graph is now read exactly, and checked.** `python3 tools/module-check.py --graph` → **210
edges over 38 units**, from `use crate::<mod>::` and `crate::<mod>::` alone: no heuristic, comments
excluded because a doc link is not a call, and test code counted separately because a fixture
reaching across modules is not a dependency of the design. `docs/modules.toml` is the allow-list and
CI fails on an edge that is not in it.

### What the exact graph says, and it is not comfortable

`tools/module-check.py` reports **two strongly connected components**, and the larger one holds
**eighteen of the twenty-six** modules: `ai config diff digest fleet gitgate kit mailbox place probes
registry repos runtime sandbox sbx signals substrate tracking`. The second is `moduledocs prq review`.

That is not eighteen mistakes. It is what one 7,400-line crate root looks like once it is split —
`kit` calls `probes::ensure_probe_in` while `probes` calls `kit::ensure_store`; `registry` reads
`repos` to find a box's store while `repos` reads the registry to find its boxes. Every one of those
was a call between two functions in one file, and invisible until there were two files.

The `place → fleet` edge that §14.2 named is one strand of the larger knot rather than a cycle of its
own. `place.rs:309` calls into `fleet`, and `fleet.rs:19` imports `place`.

---

## 7. What the code knows that the architecture was over-absolute about

A pattern worth naming, because it recurred:

| architecture said | code says |
|---|---|
| no displayed state from an edge alone | …without saying so (§5) |
| one writer per declared fact | twelve writers; the goal is a lock, not a count (§3) |
| nothing in normal operation is privileged | normal operation is full of sandbox-root work (§1) |
| resize carries a small delta | resize is a root byte copy, deliberately (§2) |
| boxes can run code in each other | files are covered; **control** is not |

In each case the code is right and the law needed a qualifier. The lesson for the next revision is to
derive laws **from** this document rather than asserting them and discovering the qualifier in
review.

---

## 8. The operations already exist

`grep -rhoE "pub(\(crate\))? fn ensure_[a-z_]+" src/*.rs` → **fifteen public**, plus one private (`ensure_source_takeover_tools`, itself a sandbox-root apt
install) for **sixteen**, plus `heal_fleet` and
`heal_transport`. skein is already written as idempotent ensures; the Operation primitive names
something the codebase does rather than importing a pattern.

Sorted by which privilege domain they need (§1) — this is the table the architecture's §7 should have
been derived from:

| operation | domain |
|---|---|
| `ensure_fleet` | **host** (`sbx create`) *and* **sandbox root** (apt replay) |
| `ensure_fleet_agent_port` | **host** (`sbx ports --publish`) |
| `ensure_gh_secret` | **host** (`sbx secret set -g`) — dissolves once credentials live on the volume |
| `ensure_fleet_root` | **sandbox root** (`sudo mkdir`, `chown`) |
| `ensure_substrate` | **sandbox root** (`apt-get`) |
| `ensure_fleet_agent` | in-sandbox, unprivileged — deleted by the rewrite |
| `ensure_box_session` | box |
| `ensure_kit`, `ensure_store`, `ensure_probe_all`, `ensure_probe_in` | filesystem |
| `ensure_ssh_key`, `ensure_known_hosts`, `ensure_box_known_hosts`, `ensure_agent_token` | credentials |
| `heal_fleet` | **sandbox root** (cgroup ceilings, by shelling the launcher's `--ceilings` path) |
| `heal_transport` | host (port publishing) — deleted by the rewrite |

Note `ensure_probe_all` deserves its own line in any design: it writes 19 scripts and merges hooks
into **every registered repo's `settings.json` on every server start**. Parity records that without
it there is no turn state at all, which makes it the highest-blast-radius write in the system.

## 9. The level signal is six values

`Screen` (`signals.rs:259`): `Busy`, `Waiting`, `Blocked(kind)`, `Error(detail)`, `Dead`, `Unknown`.
`Blocked` carries four kinds (permission · question · trust · auth-or-quota).

Provenance is a **five**-valued freshness, not four as the architecture says:
`"" | none | stale | unreadable | unsupported`. `none` — no observer was ever started, so reattach —
is not `stale`, which is observations having stopped. Collapsing them repeats the error the section
warns about.

## 10. What is actually in the state root

`skein_home().join(...)` across the codebase:

```
api-token  boxes  fleet-home  gh-secret-seeded  github-pats  github-read-token
kit  places  plane-token  repos  review  starts  tokens
```

plus `config.json`, `repos.json`, `connections.json`, `git-grants.json`, `substrate.json`,
`github-pats.json`, and the instance-scoped `fleet-agent.token` / `fleet-agent.port`. **This list is
what the codebase joins onto the root by name; treat it as a floor, not a census.**

Three of these must **not** move to a durable volume unchanged:

- **`places/`** holds the box anchors. These **are** volume state (architecture §5, §9.5.1) — but as
  *declared* state under the cover, stamped with the sandbox generation, so a fleet rebuild discards
  them wholesale rather than re-entering a recycled pid.
- **`fleet-agent.token` / `fleet-agent.port`** are instance-scoped. The migration must re-mint them,
  not copy them, or "no machine-global secret" is untrue on day one.
- **`repos/<id>/work`** is a working checkout, not a mirror. `diff.rs` and `moduledocs.rs` derive it
  and read it directly; `codeowners.rs` takes the path as a parameter and does not, so it is the
  cheapest of the three to repoint.

And `$SKEIN_HOME` is already a single relocatable root (`config.rs:17`), so "move state onto a
volume" is closer to a mount and an env var than to a rewrite — the work is in the three exceptions
above and in adding a writer discipline, not in the move.
