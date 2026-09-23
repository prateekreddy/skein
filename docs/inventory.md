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
| publish a port | `sbx ports --publish`; withdraw it with `sbx ports --unpublish` (`sbx ports --help`). An earlier revision of this row said **no unpublish exists**, quoting a nine-verb list recalled from memory — it is false, and the port-burning dance it justified in `ensure_fleet_agent_port` was built on it |
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

Measured 2026-09-06. `grep -c "sudo " src/box-session.sh` → **27**; tree-wide (`grep -rn "sudo "
src/ | wc -l`) it is **85 lines across 11 files** (`grep -rc "sudo " src/ | grep -v ':0$'` names
them: `box-session.sh` 27, `fleet.rs` 40, `substrate.rs` 5, `kit/skein-startup.sh` 4, `sandbox.rs` 2,
`web/index.html` 2, and one each in `bin/skein.rs`, `fleet-agent.py`, `place.rs`, `takeover.rs`,
`util.rs`).

**That grep counts comments, and four of those files are comments only** — `place.rs`,
`util.rs`, `fleet-agent.py` and both hits in `web/index.html` are prose *about* the sudo path, not a
call. `grep -n "sudo " <file> | grep -vE ':\s*(#|//|\*)'` separates them: 16 of `box-session.sh`'s 27
and 28 of `fleet.rs`'s 40 are code. The table below is the call sites, which is the claim this
section actually makes; the totals are here so the section can be re-derived, and they have moved
every time anyone has checked. The distinct call sites:

**Cited by enclosing function, not by line.** Every line number this table gave had drifted — one
pointed at `lib.rs:1489` in a file that is now 78 lines long — and a citation nobody can follow
reads the same as a call site that quietly disappeared. `grep -n '<fn>' <file>` finds each one
wherever it moves next.

| when | what | where |
|---|---|---|
| **every box start** | create the box's cgroup, enable `+memory +pids +cpu` on the parent | `box-session.sh`, `ensure_container_cgroup` |
| **every box start** | enable `+memory +pids` on the box's own cgroup root, then write `memory.max`, `memory.high`, `pids.max` | `box-session.sh`, `merge_login` |
| **every box start** | move the session into its cgroup (`cgroup.procs`) | `box-session.sh`, `merge_login` |
| **every server start** | write cgroup ceilings for every box — `heal_fleet` shells the launcher's `--ceilings` path, a *different* mechanism from the per-box writes above | `fleet/limits.rs`, `apply_box_limits` → `box-session.sh`, `apply_fleet_ceilings` |
| cockpit "apply now" | write one box's ceilings with `sudo tee` | `fleet/limits.rs`, `apply_box_limits` |
| **every box start** | replay the approved-package manifest as root (via `ensure_fleet`, not on server start) | `fleet/substrate.rs`, `ensure_substrate` → `substrate::approved_packages` |
| **every box start** | create and chown the fleet root (one caller: `ensure_fleet`) | `fleet/create.rs`, `ensure_fleet_root` |
| every box start **and** every server start | write `/etc/docker/daemon.json` (ensure *and* heal) | `fleet/containers.rs`, `install_docker_config` |
| **resize** | `tar` the whole box tree, and restore it | `fleet/resize.rs`, `archive_script` and `restore_script` |
| **on approval** | `apt-get install` **or `npm install -g`** the approved packages | `substrate.rs`, `install_script` |
| **every box destroy** | `rmdir` the box's cgroup | `sandbox.rs`, `stop_box_inner` |
| **every box startup** | apt in the startup kit | `kit/skein-startup.sh` |
| takeover setup | `apt-get install` the tools a source box needs | `takeover.rs`, `ensure_source_takeover_tools` (it was cited as `lib.rs:1489`; that code moved out of the crate root in `a8edef1`) |

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
(`grep -n 'fn archive_script\|fn restore_script' src/fleet/resize.rs` — cited by name because the line
numbers this entry used to give drifted by more than two thousand lines) — a root byte copy
including `.git`, `node_modules`, `target`, the private HOME and `/tmp`, which is why it demands
1.2× the box size free before starting.

`box_archive`'s doc comment records the move away from the bundle-and-patches approach deliberately:
*"the reconstruction is slower, less faithful, and it is where the fragility lives."* Note the scope
— the same comment calls that approach *"the right shape for a migration"*. It was abandoned **for
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

## 4. The approval path had a TOCTOU, masked by a second bug — both closed

**Closed, and kept here because the rule it produced is what §3 is arguing for.** This section
described two live findings when it was written; both were fixed in the order the rule demanded, and
what follows is what was wrong and what makes it stay fixed.

**The request never landed.** `substrate_dir()` is `/boxes/.skein/substrate`, and `/boxes/.skein` was
`--ro-bind` in every non-privileged box, with nothing bound back over it. So a box running
`sudo apt-get install` got a write failure — while the shim printed *"It files a request for this
fleet's owner to approve in the cockpit."* Nothing was filed. The git-write path had the same shape.
`tests/substrate_request.rs` drives the function directly, outside a box, so it could not catch this.

**And the install trusted the requester's payload.** `substrate::install` re-read the whole request at
install time and checked `state` and name *shape* — but `packages` came from that same re-read, over
a file "writable by every box in the fleet". Approve `jq`, rewrite the file, get arbitrary names
spliced into a root `apt-get`.

**The read-only bind was what masked the TOCTOU**, which is why they had to be fixed together and in
one order: unmasking the queue first would have made the TOCTOU live. The rule that fixed both:
**the approving side writes the approved artifact, and the installer reads only that** — never the
requester's file. Architecture §8.4 has the derivation and the three steps.

Both halves are readable in the code now, and neither is a line number:

- `grep -n 'requests/\$box\|for asking in substrate gitgate' src/box-session.sh` — the queue is
  unmasked **per box**, `requests/<box>/` bound writable into that box alone, so a request lands and
  no box can rewrite another's.
- `grep -n 'fn install' src/substrate.rs` and `grep -n 'rendered' src/gitgate/decide.rs` — the decision is
  made on the bytes the cockpit rendered, and `install` reads the host-side artifact through
  `decision_or_why`. Neither consults the queue. `substrate::decided_over` makes the host's decision
  win over the box's copy, so a box that rewrites its request after approval changes neither what is
  shown nor what runs.

---

## 5. The fusion function contradicts the architecture's own law

`fuse_status` in `src/signals.rs` (`grep -n 'fn fuse_status' src/signals.rs`). Four rules, and
three display an edge with no level signal behind it:

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

Removed in `9d1ddf5`: `grep -cE '^ *pub use' src/lib.rs` → **0**, and every reference is now a
qualified `crate::<mod>::` path or an explicit `use crate::<mod>::…`. (The loose `grep -c 'pub use'`
this line used to run now returns **2**, both of them *comments* (`grep -n 'pub use' src/lib.rs`) that
explain the removal. A check that counts the prose about itself is the one that goes stale
silently.)

The crate root followed in `a8edef1`. `wc -l src/lib.rs` → **78** (2026-09-06), and
`grep -cE '^(pub )?(fn|struct|enum|impl) ' src/lib.rs` → **0**: what it held became `registry`,
`sbx`, `board`, `kit`, `probes`, `digest`, `handoff`, `takeover`, `sharedhome` and `cockpit`. The
length is the throwaway number here and the zero is the claim.

**The graph is now read exactly, and checked.** `python3 tools/module-check.py` → **308 edges over
58 units** on 2026-09-06 (it was 210 over 38 when this was written and 276 over 54 a fortnight
later; the crate has grown, and the count is a snapshot that moves with every module added), from
`use crate::<mod>::` and `crate::<mod>::` alone: no heuristic, comments
excluded because a doc link is not a call, and test code counted separately because a fixture
reaching across modules is not a dependency of the design. `docs/modules.toml` is the allow-list and
CI fails on an edge that is not in it.

### What the exact graph says, and it is not comfortable

`tools/module-check.py` reports **two strongly connected components**, and the larger one holds
**eighteen of the fifty-six** modules (`ls src/*.rs | wc -l` → 57, less `lib.rs`): `ai config diff
digest fleet gitgate kit mailbox place probes registry repos runtime sandbox sbx signals substrate
tracking`. The second is `prq review` — `moduledocs` left it when `review::context` went, and
`module-check` says so at the cycle check. The eighteen has not moved through any of this; the
denominator has, twice — it was twenty-six when this was written and fifty-two a fortnight later.
Both are recorded in `docs/modules.toml` as `[[cycle]]` entries, which is what stops a nineteenth
module joining quietly.

That is not eighteen mistakes. It is what one 7,400-line crate root looks like once it is split —
`kit` calls `probes::ensure_probe_in` while `probes` calls `kit::ensure_store`; `registry` reads
`repos` to find a box's store while `repos` reads the registry to find its boxes. Every one of those
was a call between two functions in one file, and invisible until there were two files.

The `place → fleet` edge that §14.2 named is **gone** (SKEIN-22, and `docs/modules.toml` records
it): `grep -n 'crate::fleet' src/place.rs` finds only doc-comment links now, which
`tools/module-check.py` excludes because a doc link is not a call. The knot did not change size —
`place` is still inside it through `config → runtime → repos → place`, and `fleet` still imports
`place` (`cat src/fleet/*.rs | grep -c 'crate::place'` → 58). Worth knowing before anyone spends a day on a single edge: in a
component this dense, removing one is a local tidy, not a structural change.

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

`grep -rhoE "pub(\(crate\))? fn ensure_[a-z_]+" src/*.rs | sort -u` → **twenty public**, plus one private (`ensure_source_takeover_tools`, itself a sandbox-root apt
install) for **twenty-one**, plus `heal_fleet`. (There was a second `heal_` beside it, the
transport watcher; it went with the in-sandbox agent — SKEIN-521.) skein is already written as
idempotent ensures; the Operation primitive names something the codebase does rather than importing
a pattern.

Sorted by which privilege domain they need (§1) — this is the table the architecture's §7 should have
been derived from:

| operation | domain |
|---|---|
| `ensure_fleet` | **sandbox root** (apt replay). It no longer creates anything: the create moved to `request_fleet_create`, the explicit act (SKEIN-576) |
| `request_fleet_create`, `create_fleet_operation` | **the warden**, over `http`, with the `sbx create` line printed when none answers |
| `publish_cockpit_port`, `cockpit_port_advice` | **nobody** — the cockpit's mapping is a recipe a person runs; §9.4's stamp guard decides whether it is even offered (SKEIN-576) |
| `ensure_fleet_root` | **sandbox root** (`sudo mkdir`, `chown`) |
| `ensure_substrate` | **sandbox root** (`apt-get`) |
| `ensure_fleet_door` | in-sandbox, unprivileged — the doorway that holds the cockpit port across restarts, and the server behind it |
| `ensure_box_session` | box |
| `ensure_kit`, `ensure_store`, `ensure_probe_all`, `ensure_probe_in`, `ensure_mirror`, `ensure_volume` | filesystem |
| `ensure_known_hosts`, `ensure_box_known_hosts` | credentials |
| `heal_fleet` | **sandbox root** (cgroup ceilings, by shelling the launcher's `--ceilings` path) |

Note `ensure_probe_all` deserves its own line in any design: it writes 19 scripts and merges hooks
into **every registered repo's `settings.json` on every server start**. Parity records that without
it there is no turn state at all, which makes it the highest-blast-radius write in the system.

## 9. The level signal is six values

`Screen` (`grep -n 'enum Screen' src/signals.rs`): `Busy`, `Waiting`, `Blocked(kind)`,
`Error(detail)`, `Dead`, `Unknown`.
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

And `$SKEIN_HOME` is already a single relocatable root (`config::skein_home`), so "move state onto a
volume" is closer to a mount and an env var than to a rewrite — the work is in the three exceptions
above and in adding a writer discipline, not in the move.
