# skein — architecture

The destination design. Revised 2026-08-20 against three independent reviews (architecture,
security, delivery), each of which found real defects in the first draft. Where a claim below
contradicts something skein does today, it is because the current behaviour was checked and found
to be the better answer — not because it was overlooked.

Companion documents: `docs/delivery.md` (sequence, migration, landmines) and `docs/parity.md` (the
audited capability inventory). The root `ARCHITECTURE.md` describes today's system and is **stale**
— it still describes ratatui, Svelte and per-box microVM kernels. It should be retired, not trusted.

Organised primitives-first: §2 states the five things skein is built from, §3 shows every feature as
a composition. **A feature that cannot be written as a composition means the primitive set is wrong**,
and the correct response is to fix the primitive set — not to add a mechanism beside it.

---

## 1. What skein is

> **A board of boxes. Each box is a coding agent working on a branch. The board's job is to tell you
> which one needs you.**

One person uses one cockpit. That is a decision, not an assumption: "needs you" has a single meaning,
credentials belong to one identity, and whoever approves a privileged operation is that same person.
Multi-user moves the queue, the credential model and the approval model together, so it is a
different design rather than a later feature.

---

## 2. The primitives

Five primitives. Two nouns, one reach, two verbs — and **State has four kinds**, not two.

| | idempotent / self-healing | lossy / one-shot |
|---|---|---|
| **durable** | State: *declared* (sole writer, locked) · *artifact* (privileged-written, read-only to the reader) | State: *recorded* (many writers) · *requested* (untrusted writer, never authoritative) |
| **observed** | Signal: *level* | Signal: *edge* |
| **acting** | Operation | Act |
| **reaching** | Source | |

### 2.1 State — what is written down

Durable, on the volume (§5). **Four kinds, with different rules** — treating them alike is a defect
the first draft shipped, and each rule below is one the code had to learn:

**Declared** — what the user told skein. Repo remotes, box identities, config, branch choices.
Small, versioned, **sole writer**, guarded by a lock or an owning process.

**Requested** — written by an **untrusted party**, read by an approving side, **never authoritative**,
and **never in the same directory as the artifact it produces**. The package queue and the git-write
queue are both this. Giving them no rules is the category error behind §8.4, and it has a live exploit
today (below).

**Artifact** — written by an approving or privileged side, **read** by an untrusted one. A box's git
token is this: the box must read it, and must never be able to write it. Bound **read-only**; it is
the output side of a *requested* decision, and conflating the two is what §9.5.8 exists to prevent.

**Recorded** — what skein and the boxes write as they work. Conversations, transcripts, journals,
hook logs, per-box status, mailbox messages. **Many writers by design** — every box writes these
continuously — so the rule is one file per writer, or a documented lock protocol, never "one writer".

Today's code already needs this discipline (`flock` on `.sandboxes.lock`, per-message `.lock` files
in the mailbox) and today's code also violates it where it matters most: `config.json` and
`repos.json` are unsynchronised read-modify-write with no locking, so two cockpit tabs saving
settings is silent last-write-wins. Declared state gets a lock or an owner. That is not optional.

**Every durable file carries `"schema": N`, and the volume carries a `VERSION`.** The volume is the
only survivor of a fleet rebuild, so a volume written by version N *will* routinely be opened by
N+k. skein refuses to open a volume newer than it understands, with a printable recipe (§11 law 1
applies to skein itself). There are zero version fields in the codebase today and exactly one
migration, which fires on *unparseable* as well as absent — so a corrupt file silently re-migrates.
Retrofitting this after users have volumes is what kills a project in year two.

### 2.2 Signal — what can be observed

Every signal declares six things. The declaration is part of its definition, not documentation:

| | |
|---|---|
| **subject** | what it is about — a **box, a module, a pull request, or the fleet**. **Keyed by subject, never by observer.** |
| **kind** | `level` or `edge` |
| **source** | which Source produced it (§2.3) |
| **observed_at** | freshness is never implicit |
| **cost** | in which budget, and how much (§10) |
| **cadence** | interval, staleness threshold, and the Gate contract below |

**Level** signals are re-readable and self-healing. Missing one observation costs nothing.
**Edge** signals are events and are **lossy** — a hook that never fired, a process that died before
writing.

> **An edge-triggered latch with incomplete edge coverage cannot recover. A state nobody clears is
> shown forever.**

Therefore: **no displayed state may rest on an edge alone *without saying so*.**

The absolute form was wrong and the shipped code is right. `fuse_status` has four rules and three
display an edge with no level behind it: no level observation at all, an unrecognised screen, and a
newer edge carrying an outcome. Each is correct — showing nothing would be worse, and the third is
the fresher observation winning. What makes it honest is **provenance, rendered**: `hooks only`,
`screen lost`, `screen unread`. A runtime with no screen grammar runs on edges by design, and the
display says so. The law is about disclosure, not grounding.

And the keying rule is its
own bug: turn state keyed by box but written by session let any helper process overwrite the agent's
state, producing seventeen spurious `ended` events in 114 seconds.

The subject being open is what lets one mechanism carry things that otherwise need bespoke features:
a module's standing note and a diff's contract signals are signals *about a module*, and a review
request is a signal *about a pull request*. Assuming the subject was always a box is what made those
look like separate machinery.

**A signal's value is five-valued, not two**: `value | none | stale | unreadable | unsupported`.
"Could not observe" is not "observed absent", and collapsing them is what drives spurious action.
`none` — no observer was ever started, so reattach — is not `stale`, which is observations having
stopped; the shipped code distinguishes them and an earlier draft of this document did not.

**Fusion, defined** — because §13 mandates a scenario matrix for it and an earlier draft never said
what it was. Given a level observation `(screen, t_level)` and an edge `(status, t_edge)`:

1. no level observation → the edge
2. level is `Unknown` → the edge
3. `t_edge > t_level + 1` **and** the edge is an *outcome* (blocked, needs-input, needs-decision,
   error, ended, done, waiting) → the edge; the fresher observation wins. The `+1` is a deliberate
   one-second tie-break, not a `>`.
4. level is `Dead` **and** the edge is `done` → `done` — a human-set outcome a screen cannot
   contradict. The edge wins here *without* being newer, which is why this is its own rule.
5. otherwise → the level, mapped to a displayed state

Rule 3's outcome list is load-bearing: a *progress* edge must never override a level, or a stale hook
re-latches the bug this primitive exists to prevent.

**Where the disclosure law is currently violated, and it is rule 3.** Provenance is a *separate*
function from fusion — it reports `hooks only`, `screen lost`, `screen unread` on its own conditions.
Rules 1, 2 and 4 land on one of those, so they disclose. **Rule 3 does not**: the screen is fresh and
readable, provenance returns empty, the badge renders nothing, and the board shows a state derived
solely from an edge while saying so nowhere. An earlier revision cited provenance as the reason the
relaxed law was safe; that is true of three rules and false of the one it leaned on. **Rule 3 needs a
provenance value of its own** — the law is right, and the code does not yet meet it here.

**The Gate contract.** Every signal's observation is mediated by a gate that provides: single-flight
(concurrent askers share one observation), serve-stale-refresh-behind (the last good value is
returned immediately while a refresh runs), exponential backoff on failure, a sticky last-good value
so a wedged source does not blank the board, and **`invalidate`** — the one property that couples
acting to observing. Without it the board serves the pre-Act value and "an Act emits an edge" is not
enough; resize already depends on it. This exists today in `util.rs` and is load-bearing
— check-then-act previously gave every browser tab its own subprocess every tick, and a board refresh
walked four gates in series and took 31 seconds. Cadence without a gate is not a cadence.

### 2.3 Source — how a subject is reached

A Source is a way of observing and acting on something. Four, and each declares its cost and its
failure modes:

| source | reaches |
|---|---|
| `enter` | a box's namespace, via nsenter |
| `socket` | a box's tmux server, without entering |
| `file` | the volume, and paths visible in the fleet |
| `http` | GitHub, and the warden |

Each carries the modes it can support — **not** a 4×3 matrix, because `http`×pty and `file`×pty are
meaningless and asserting a clean product invites someone to implement the empty cells:

| | exec | stream | pty |
|---|---|---|---|
| `enter` | ✓ | ✓ | ✓ |
| `socket` | ✓ | | ✓ |
| `file` | | ✓ | |
| `http` | ✓ | ✓ | |

`stream` exists because "execute" does not cover stdin or stdout as bytes, up to gigabytes, never
buffered whole.

Source exists as a primitive because the first draft promoted `enter` to one and that was wrong twice
over. The cheapest and most-used signal in the whole system reaches a box **by socket, without
nsenter** — so "nothing reaches a box except through `enter`" broke on day one. And `github` is a
world-facing transport that was left outside the primitive set entirely. With Source, the law becomes
enforceable: **no code reaches anything except through a Source.**

`enter` has two load-bearing details, learned from a real box: the user and mount namespaces must be
joined **together** — joining mount alone is refused — and credentials must be preserved, or
`setgroups` fails for an unprivileged caller.

### 2.4 Operation — an idempotent intent

| | |
|---|---|
| **desired** | the declared state that should hold |
| **check** | a **tri-state** level signal: `satisfied \| unsatisfied \| unknown` |
| **recipe** | the exact command, with environment. Always present, always printable |
| **doer** | performing it. **Optional** |
| **requires** | prerequisite operations — operations form a **DAG**, not a list |
| **class** | `idempotent` or `destructive` |
| **lease** | an in-flight attempt: id, started_at, deadline |

**`unknown` may never drive a doer.** It may only be reported. A binary check makes "the daemon is
wedged" and "the fleet is absent" indistinguishable, and the reconciler responds by creating a fleet
that already exists. Today's `fleet_exists` returns `Option<bool>` with exactly this comment; the
first draft threw that knowledge away.

**A lease distinguishes owed from in flight.** Creating a fleet takes minutes, during which its check
fails; without a lease a reconciler fires it repeatedly.

**The failure model, which an earlier draft left to be invented twice:**

- **partial progress** is a lease heartbeat from the doer, never inferred from the check.
- **check passes but the doer errored** → the operation is satisfied; the error is recorded and shown
  once. The world is what the check says, not what the doer claims.
- **stuck versus slow** is the lease deadline — the most important distinction in this codebase,
  since a slow daemon asked again is where the gate story begins.
- **the lease lives in recorded state on the volume**, keyed by operation id, holding
  `(holder, started_at, deadline, last_heartbeat)`. A dead holder's lease expires by deadline;
  nothing else reclaims it.

**`destructive` operations are never auto-driven**, even when a doer exists and the check is
unsatisfied. Re-running a destroy is not "running it once".

What this buys, and only with all four qualifications above: crash recovery is re-running the
reconcile; partial failure leaves nothing the next reconcile cannot see; manual operation is the same
path with the doer removed; and **skein can never be blocked without saying what would unblock it,
because the check is how it knows.**

### 2.5 Act — a non-idempotent interaction

Sending a message to an agent. Answering its question. Interrupting a turn. Uploading a file.
Attaching a terminal. Taking a box over onto another runtime.

These have no `desired` and no `check`. They are streaming, unacknowledged, and doing them twice is
doing them twice. Forcing them into Operation makes "`ensure`, never `do`" a lie; leaving them
unnamed makes them grow *beside* the primitives, which is the debt §12.8 exists to prevent.

**An Act emits an edge signal as a side effect** — skein knows it delivered the keystroke, and that
knowledge is what makes an optimistic state clear correct.

### 2.6 Reconciliation

Not a cube — `desired` is meaningful only when `declared = present`, so calling it an orthogonal
axis would repeat the 2×2's error one dimension up. It is **`declared`, then `desired` where it
applies, against `observed`**: four declared-and-desired states (`absent`, `present/running`,
`present/stopped`, `deleted`) against four observed values — **sixteen**, not twenty-four. An earlier
revision said eight, which is the number of cells *removed*.

- **declared** ∈ `absent | present | deleted`
- **desired** ∈ `running | stopped` — *only when declared is present*
- **observed** ∈ `present | absent | unknown | failed(reason)`

The cases the square got wrong, each of which is a real feature today:

| situation | the 2×2 said | this says |
|---|---|---|
| a box you deliberately stopped | "an operation is owed — start it", forever | declared present, **desired stopped** — nothing owed |
| a box whose start failed | same cell, so retry forever | `observed = failed(reason)`, and the reason is shown |
| a half-completed destroy | "foreign — never touch", so skein cannot clean up its own corpse | `declared = deleted` — a tombstone, and cleanup is owed |
| sbx unreachable | "absent" | `observed = unknown` — report, drive nothing |

Two the earlier list still missed: **declared present, desired stopped, observed present** — you
stopped a box and something restarted it — is drift in the opposite direction and is owed a *stop*.
And the tombstone needs retiring or it is immortal: `declared = deleted ∧ observed = absent` is
complete, and the record goes with the box's directory.

`failed(reason)` is how a start failure is displayed without violating §2.2: the reason is an edge,
but the *cell* is a level signal, and the cell is what is rendered.

---

## 3. Features as compositions

| feature | composition |
|---|---|
| the board | level signals per subject, ranked by attention |
| "what needs me" | a predicate over signals |
| turn state | level (pane grammar) fused with edges (hooks) that only accelerate it |
| the review queue | signals whose Source is `http`, subject a pull request |
| module notes, contract signals | signals whose subject is a **module** — §11.1 |
| understanding a change | those signals, ranked, with drill-down |
| voice, notifications, the away digest | signal **transitions** — see below |
| `doctor` | every operation's check, reported, including `unknown` |
| onboarding, breakage, approval | failing checks with their recipes — three components, one language (§11.5) |
| converse, answer, interrupt | **Acts** |
| upload | an Act **with an outcome channel** — see below |
| takeover | *not* an Act: a privileged snapshot, then an Operation, then a paid signal, plus durable rollback state |
| merging a pull request | an Operation, class `destructive` — Acts have no class, and this is the one destructive verb in the review surface |
| launching a box | an Operation whose check is "the box's tmux server is alive" (§6) |
| foreign detection | `declared absent ∧ observed present` |
| crash recovery | reconcile the DAG |

**Acts need an outcome channel.** §2.5 called them "unacknowledged", and upload is the counter-example
that matters: an empty piece mid-stream terminates the stream and silently truncates the file. An
unacknowledged upload with a silent-truncation mode is the defect, not the design. So: an Act is
non-idempotent and streaming, and it **reports what it did** — it simply has no `check`, which is a
different thing from having no result.

**This table is not the whole surface.** `docs/parity.md` lists roughly ninety capabilities; the rows
here are the ones whose decomposition was in doubt. A row's absence is not a claim that a feature
decomposes — it is a claim that nobody has argued it does not. The test in the preamble applies to
any of them on demand.

**Transitions need durable state that neither noun covers**, and the first draft had nowhere to put
it. A transition requires the retained previous value per `(subject, signal)`, plus a watermark for
"what changed since I last looked". That is **recorded** state (§2.1) on the volume, written by
skein. It must be server-side: today's away digest computes the delta client-side on tab re-focus,
which is why a box that turned while you were looking elsewhere was never announced.

---

## 4. Topology

```
host
  └── warden          creates and destroys fleets. Nothing else.
        │             owns its own approval surface (§8)
        ▼
  fleet sandbox
    ├── volume        mounted at create; the only thing that persists
    ├── skein         cockpit, board, signals, operations, acts
    └── boxes         namespaces; skein reaches them by socket and by enter
```

The warden is **not optional convenience**. It exists because the two remaining privileged operations
(§7) both terminate skein, so skein cannot be the thing that performs them. Fleet lifecycle lives
outside the fleet, permanently, and that is a boundary rather than a limitation.

---

## 5. The durable volume

**The volume is the only thing that persists; everything else is reconstructible.**

| on the volume | kind |
|---|---|
| `VERSION`, `config.json`, `repos.json` | declared |
| `credentials/` | declared — protected by the **mount cover**, not by a file mode (§9.5.2) |
| `boxes/<name>/recorded/` — launch spec, conversation, transcript, notes | recorded |
| `boxes/<name>/declared/` — `privileged`, `git-scope`, `disk`, `identity` | declared — **never bound into the box** (§9.5.2) |
| `places/<name>` — the box's placement, incl. `(generation, pid, starttime)` (§9.5.1) | declared |
| `uids` — the box-uid allocation, if per-box uids are in use (§9.5.1) | declared |
| `boxes/<name>/artifacts/` — `git-tokens/` | approved artifact — bound **read-only** |
| `boxes/<name>/transitions` — retained signal values and watermarks | recorded |
| `repos/<id>/mirror` — a bare git mirror | recorded |
| `repos/<id>/store` — the shared `.claude` every box for that repo reads | recorded, many writers |
| `<queue>/requests/` — git-write and package requests | **requested** — box-writable, and the only box-writable thing under it |
| `<queue>/approved` — the artifacts, and the replayed manifest | declared — written by the approving side, owned by it, under the cover |
| *(no `audit/` — see below)* | |

The queues are on the volume because they exist *precisely* to outlive the sandbox:
lose them and every box re-asks for push access and for packages already approved. The request is **requested** state and the approval is **declared** — written elsewhere, by the
approving side (§8.4).

**The audit log is not on the volume, and it must not record only the warden.** The warden has two
capabilities, so a log scoped to it covers fleet lifecycle and **none of the approvals that actually
grant privilege** — package approval (root apt, fleet-wide, permanent), git-write grants, workshop
promotion, credential rotation. Those are decided by skein, and §5 has just argued skein cannot audit
itself. So **skein's approval decisions are appended to the warden's log through a third, non-privileged
warden capability — a sink, not a doer — and that sink is never compilable-out.** It records warden
approvals, and the volume is inside the fleet, writable by skein, which shares a sandbox with coding agents. A record of an approval must not
live where the thing being audited can reach it — so it lives **on the host, written by the warden**.
"Append-only" on a volume path would be unenforceable anyway: there is no `chattr +a` without
`CAP_LINUX_IMMUTABLE`. (For reference, there is no audit log today at all.)

**Not** on the volume: box checkouts (VM-local; measurably faster for build work and reclonable from
the mirror), caches and build output. **Not** the anchor: an earlier draft excluded "namespace
anchors" here and that was wrong, by conflating two restarts — a **sandbox** cycle invalidates an
anchor, a **skein** restart does not, and skein must still reach a running box afterwards. The anchor
record is declared state under the cover, stamped with the sandbox generation (§9.5.1).

The test for anything new: *if the fleet were destroyed right now, would losing this hurt?*

**Reclamation.** `boxes/<name>/` is garbage-collected when a box is destroyed, along with its
tombstone (§2.6); transcripts rotate;
`repos/<id>/mirror` is repacked and pruned on a schedule; a
resize archive left by a failed restore is retained deliberately and reported, never silently deleted.
An earlier draft's whole policy was "transcripts and the audit log rotate" — and the audit log is not
even on the volume.

**The volume can fill.** Every operation that writes has a free-space precondition; `boxes/<name>/` is
garbage-collected when a box is destroyed; transcripts and the audit log rotate. Today per-box disk
limits are *displayed and never enforced*, and no code *handles* `ENOSPC` — it appears twice, in comments — the first symptom
is another box's build failing. A check for "the volume is mounted" without one for "the volume has
room" repeats that.

---

## 6. Boxes and repos

A box is an **identity** — name, repo, branch, conversation — that outlives every process. It **runs**
as a bwrap namespace anchored by its tmux server, because the launcher double-forks away and its pid
names a corpse while the box runs happily.

> **box alive ⇔ tmux server alive ⇔ namespace joinable**

**A repo is a remote.** Adding one clones a bare mirror onto the volume; a box clones its checkout
from the mirror onto VM-local disk. Crucially, **a local filesystem path is a valid remote** — so a
repo with no server anywhere still works: skein fetches from your path.

What is lost, precisely: **uncommitted work in your host checkout is not visible to boxes.** You
commit — not push — and skein fetches. That is a smaller loss than "local repos stop working", which
is what the first draft claimed.

Two consequences to state rather than discover:

- **In-fleet skein cannot reach host paths.** A local-path remote works host-driven; in-fleet, the
  mirror must be seeded at import or the repo must live on the volume. A real asymmetry between the
  two deployments.
- **Three host-side features read the working checkout directly** — `diff`, `moduledocs`,
  `codeowners`. They repoint at the mirror. That is a refactor, not a deletion, and it is budgeted
  in `docs/delivery.md`.

---

## 7. Privilege — two domains

Derived from `docs/inventory.md` §1, which counted call sites rather than operations. Two earlier
drafts said "six operations", then "two", and both added "nothing in normal operation is privileged".
That was false: it counted one domain and forgot the other.

### 7.1 Host privilege — needs something outside the sandbox

| operation | today | after |
|---|---|---|
| create the fleet | `sbx create` | **warden** |
| destroy the fleet | `sbx rm -f` | **warden** |
| publish a port | `sbx ports --publish` | folds into create — §7.4 |
| seed the fleet credential | `sbx secret set -g`, token on the argv | dissolves: a file write on the volume |

**Two, after the two that dissolve.** This is the collapse an earlier draft claimed for everything;
it is true here and only here.

`sbx` is the substrate and is named deliberately, because its quirks are load-bearing: **there is no
unpublish**, so a mapping outlives the sandbox and is still reported while refusing connections; and
`sbx create` prompts before mounting host directories, which is why fleet creation carries a
microVM-sized budget rather than an action timeout.

### 7.2 Sandbox root — used constantly, in normal operation

`grep -c "sudo " src/box-session.sh` → **21**; tree-wide, 63 lines across 8 files. Four kinds:

| kind | when |
|---|---|
| **resource ceilings** — create the box's cgroup, write `memory.max`/`high`/`pids.max`, move the session in; and `rmdir` it on destroy | every box start and every box destroy; plus server start, through `heal_fleet` → the launcher's ceilings path, which is a *different mechanism* from the per-box writes |
| **package installation** — `apt-get` **and `npm install -g`** (whose root prefix is unverified — §9.5.4) | on approval; replaying the approved manifest on **every box start** (via `ensure_fleet`), not every server start; plus the takeover-tools installer and the box startup kit |
| **filesystem ownership** — create and chown the fleet root; `tar` a box out and back | every box start (fleet root); resize |
| **container runtime config** — write `/etc/docker/daemon.json` | every box start *and* every server start (it runs in both the ensure and the heal paths) |

Four kinds, not three. And note `npm install -g` is not a spelling variant of `apt-get`: it writes
into the toolchain that is shared read-write with every box, which is what makes it violate §9.5.4.

**skein performs this domain with `sudo`, exactly as it does today** — and that is unchanged by the
privilege split (§9.5.1), which is about separating skein from *boxes*, not about giving skein root.
An earlier revision claimed root "supplies this whole domain directly"; root turned out to be
self-defeating for a different reason (§9.5.1), and the claim went with it.

The asymmetry that matters is already built and is the thing to preserve: **skein can escalate;
a box cannot.** Inside a box the `sudo` shim has nothing to escalate to — the box is in an
unprivileged user namespace mapping only its own uid — so the shim is a *message*, not a boundary,
and the boundary is the namespace. The split gives skein a uid of its own so that a box cannot reach
skein's files or signal it; neither side's relationship to `sudo` changes.

### 7.3 Resize is a byte copy, and that was a decision

`snapshot_box` — a git bundle plus two patches plus an ignored-file sweep — **has no production
caller.** Real resize is `sudo tar` of the whole box tree and `tar -xf` back, which is why it demands
1.2× the box size free before starting. `fleet.rs:3122` records the move away from reconstruction:
*"the reconstruction is slower, less faithful, and it is where the fragility lives."*

An earlier draft called resize "a composition carrying a small delta … what today's snapshot already
does". It prescribed a regression and described it as the status quo. **Resize starts from the byte
copy.** It is destroy + create around a `tar`, it is `destructive` (§2.4), and it needs a doer that
can outlive skein (§7.5).

Also unstated before: the fleet's **mount set is a function of registered repos**, so adding a repo
after create makes its store unreachable and skein's own remedy is a rebuild. That is a third trigger
for resize wearing another name — and it disappears once repos are clones on the volume (§6), which
is a reason for §6 beyond the ones already given.

### 7.4 Port publishing folds into create — conditionally

Only because the cockpit is the sole port. Today publishing is a **recurring, self-healing host
operation** (`ensure_fleet_agent_port` → `heal_fleet` on every server start), and it exists in that
shape because sbx cannot unpublish, so a failed attempt is permanent. Host-side skein escapes it only
by binding loopback.

**If skein ever needs a second port in-fleet, it inherits that machinery whole, including the
cannot-withdraw trap.** The fold is a consequence of the one-port decision, not an independent
simplification.

### 7.5 The two operations that terminate their own reconciler

Create and destroy both kill skein — create because it does not exist yet, destroy because it will
not afterwards. So **fleet lifecycle cannot live inside the fleet**, permanently. That is the
warden's reason to exist (§8), and it is a boundary rather than a limitation.

Package approval is different and must not be confused with it: it needs sandbox root, not host
privilege, so the warden is not involved — but the *authority* question is real, and §8.4 answers it.

## 8. The host warden

A small host service owning fleet create and destroy. Two capability modules.

### 8.1 Its approval surface is its own, on the host

The first draft made the warden's approval card the same component as the cockpit's check card. The
cockpit is served by skein, inside the fleet, beside coding agents. That closes a loop: box →
compromise skein → skein asserts "a human approved" → warden runs a privileged host command.

So: **the warden renders and confirms its own approvals, on the host, outside the fleet.** It never
trusts an `approved` field on the wire. Requests carry an operation id it echoes into the approval
text, so what you see is what will run.

Generalised, because this is the shape of the whole class: **approval is a fact the approving side
writes, never a field the requester supplies.** The current package queue is a box-writable JSON file
carrying a `state` field; whether that is safe depends entirely on the host re-deriving approval
rather than trusting the field. It must re-derive.

### 8.2 Requests are at-most-once

A timeout on `destroy-fleet` means exactly "did it happen or not?". The first draft congratulated
itself on deleting the transport-failure-versus-command-failure distinction; that distinction is a
**safety property**, not redundancy, and moving to HTTP relocates the hazard rather than removing it.
Operation ids, and the warden answers a repeated id with the original outcome — which means the
warden has **durable state of its own**, on the host: an outcome store keyed by operation id, with a
retention window (an id older than the window is answered "unknown", never re-executed). It is a
module in §14 and a thing to back up, not an implementation detail.

### 8.3 Capabilities are compiled

Each capability is its own module; a warden that does not need one is built without it. Not gated,
not configured — absent. A runtime check falls to a bug in the check; absent code falls to nothing.

Only the **doer** is removable. Recipes and checks live in skein, always compiled, never privileged —
they are needed precisely when the doer is absent.

**Four endpoints, of which two are removable.** The removable ones are the doers — `create` and
`destroy` — and the default build ships both.

The other two are neither privileged nor optional, so they are **never compilable-out**:

- **the audit sink** (§5): skein's own approval decisions append to the host-side log, because skein
  cannot audit itself.
- **fleet observation**: `fleet_exists` today is `sbx ls`, which is **host-only** — so in-fleet skein
  cannot run the check that gates its own first run. That check becomes a `Source: http` call to the
  warden. It reads; it decides nothing; §8.5's one-outstanding-request rule does not apply to it.

This is the exception to §12.10, and it is stated rather than left implicit: a capability that
*performs* something may be left unbuilt; one that only *reports* may not, or the design loses the
ability to see and to account for itself. An earlier draft shipped only `create` — but
resize is destroy + create (§7.3), so a create-only warden cannot resize, which is the commonest
lifecycle operation after create, and cannot retire a tombstone (§2.6). Making `destroy` optional
made `resize` optional by accident.

What compile-time removal is *for* here is a warden built for a machine that should never destroy a
fleet — a shared or long-lived host. That is a deliberate choice someone makes, not the default.

The capability set is derived from what is linked, not read from config. But skein must not extend
*trust* on the strength of an advertised list — a malicious endpoint advertises whatever makes skein
offer a button. Advertisement decides what skein *offers*; it never decides what skein *believes*.

---

### 8.4 The approving side writes the artifact, not a flag

§8.1's rule needs its sharper half, because the current code has the weaker one and a live defect to
show for it.

`substrate.rs:245` re-reads the whole request at install time and checks `state` and the *shape* of
the package names — but `packages` comes from that same re-read, and the comment above it says the
file "is writable by every box in the fleet". Approve `jq`, rewrite the file, get arbitrary names in
a root `apt-get`. And the result is recorded into a manifest replayed as root on every fleet ensure,
so one window buys permanent root execution.

**Three steps, not two.** Bind the artifact, make the request path **per box**
(`requests/<box>/`, bound into that box only), *then* unmask. The per-box cover machinery already
exists, so this costs nothing if it is decided now and cannot be fixed in place later.

Making the queue writable to fix the masking bug makes the TOCTOU
live — and once *every* box can write the queue, each can also overwrite, delete or flip the state of
every *other* box's requests. So: **artifact binding first, then unmask the queue.**

**Both queues are masked the same way today, so neither is exploitable as things stand** — an
earlier draft said the git-write one was. And the shim does not claim to have filed a request; what it
does is tell the agent to file one by running the command that just failed, which is its own kind of
misleading.

The masking: `/boxes/.skein` is read-only in every non-privileged box,
so the request never lands at all — while the shim prints that it filed one. **Fixing either alone is
worse than fixing neither.**

> **Approval writes the approved artifact to a location only the approving side can write. The actor
> reads only that. The requester's file is input, never state** (§2.1, *requested*).

That covers the payload, not just the boolean — which is the difference between the rule as stated
before and the rule that would have prevented this.

**And the rule as stated is necessary but not sufficient.** `gitgate::decide` *already* implements it
— it never trusts a `state` field and writes a `Grant` to a host-side file the refresher alone reads —
and it is exploitable today, because the artifact is written from a **re-read by id after the human
clicks**:

1. a box files a request naming a repo you own;
2. the cockpit renders it, you read it, you approve;
3. the handler re-reads the file **by id** — and the box rewrote it in between;
4. a grant is written for the other repo, and a real installation token lands in the box.

The window is render→click: **human-scale, seconds to minutes**, where the substrate window is
machine-scale. So moving the hazard from approve→install to render→click made it *larger*.

> **An operation id correlates a decision to a request. It must never be what the decision re-reads
> by.** The approving side **keeps the bytes it rendered and acts on those**. It does not re-open the
> file; a digest check is the fallback for a design that still does.

Four ways to implement that and still be wrong:

- **there are three reads on the package path, not two** — the decision re-reads by id, and the
  install re-reads by id *again*. Closing render→click leaves click→install open. Both need the same
  binding.
- **digesting the wrong bytes.** The queue is read through a JSON normaliser before it is parsed, so a
  digest over the parsed value — or over the normaliser's output — is a digest of something the
  requester can steer. It must be over the **file bytes, read once**, and that same read must be what
  was rendered.
- **the artifact must carry every field the actor uses.** The git grant is built from the re-read's
  *box* as well as its repo, so the swap is not only "another repo" — it is "put a live token in a box
  of my choosing". Box, repo and expiry all travel in the artifact.
- **the requester's file must stop being a state machine.** The decision is currently written back
  into the box's own file by id, so a box can drop the state change or resurrect a request. Once the
  artifact is authoritative, the requester's copy is a message, not a record.

And the artifact carries the **resolved argv**, not a name list: if the actor re-splices names into a
command, the splice is still the trust boundary.

**One more the shape check misses.** The package-name whitelist admits `/`, and both `apt-get install
./x.deb` and `npm install -g /path/pkg` install a *local* package and run its maintainer or lifecycle
scripts as root — then persist into the replayed manifest, so one approval becomes root execution at
every future launch. The whitelist's own comment claims the rejected set covers "the only two that
matter"; there is a third.

Two more that follow, and neither is an implementation detail:

- **the request channel and the approval channel are different paths, and §5 must lay them out that
  way.** The requester writes the first and cannot write the second. Covering the whole of
  `substrate/` would enshrine today's masking bug as a requirement — requests would never land, while
  the shim still says one was filed. The layout: **`substrate/requests/` box-writable and the only
  box-writable thing under it; `substrate/approved` and the replayed manifest owned by the approving side
  (skein, not root — R1) and under the cover.** Same for the git-write queue. The code already gets this right and splits them; the
  document previously merged them into one line.
- **an operation id is correlation, not content.** The warden must render **the resolved arguments it
  will itself execute**, derived from its own parse — never display text the requester supplied.
  Stated the other way round in an earlier draft, which made it an obstacle sold as a boundary.

### 8.5 Flooding

A compromised skein controls *what* is proposed and *when*. One outstanding request at a time, a rate
limit, and the timeout §11.5 already names.

## 9. The trust model

Rewritten twice. The first draft asserted a namespace escape was "the only way through", which was
false. The second draft over-corrected: it named the tmux socket as a box-to-box code path, and the
source had already closed that. Both errors came from reading the `bwrap` exec without reading the
isolation block eighty lines above it.

### 9.1 What a box actually shares

A box is isolated by **two** namespaces — mount and user (`src/box-session.sh:1210-1214`). It
**shares** with everything else in the sandbox:

- **network** — no `--unshare-net`. Any port bound in the sandbox is reachable from every box.
- **PID** — no `--unshare-pid`, deliberately: *"the pid recorded below has to be the pid skein sees
  from outside, or nsenter has nothing to address."*
- **IPC, UTS, cgroup**, and **uid** — every box is uid 1000.

**Files, however, are covered** (`src/box-session.sh:943-956`, and asserted by
`tests/git_write_request.rs`). A `--tmpfs` goes over the fleet root and over the box-state parent,
then only *this* box's root and state are bound back, with `.skein` read-only. So one box cannot read
another's checkout, conversation or tokens — and cannot reach another's tmux socket, which lives
under the covered root.

**And there is a third path, out of the sandbox entirely, that the file cover does not reach.**
`fleet_mounts()` mounts `~/.skein/repos` — every repo's `store` **and**, for an adopted repo, the
host's own working checkout — into the sandbox. The box cover tmpfses only `/boxes` and
`~/.skein/boxes`, so `~/.skein/repos` is **read-write from every box**. Two consequences, and the
second is the worst thing in this document:

- **across repos, the file boundary does not hold.** A box working on one repo reads and writes
  another repo's store, launch specs and status.
- **it is a box → host code-execution path.** skein runs git against those trees *on the host* —
  `git -C <repo.work> log …` for module notes, `git -C <repo.work> pull --ff-only` on a repo pull. A
  box that writes `<repo.work>/.git/config` with `core.fsmonitor` (or a pager, or an alias) gets
  execution **as the host user** at the next host-side git call. In-fleet it becomes execution as
  skein's uid, which defeats the privilege split as well.

> **This is live today, and it is why §9.5.2's cover must be derived per box rather than listed.**

**That cover list is an enumeration, and it must grow with every new shared path.** It covers exactly
two parents today. It does not cover `/run`, and the launcher itself records that
`/run/user/1000/cc-socks/` is box-visible.

### 9.2 The three real box-to-box paths

Named because §9.1's cover makes the *file* axis safe and it is easy to stop there.

**1 — Shared writable toolchains.** `share_paths=(".local" ".cargo" ".rustup" ".npm")`
(`src/box-session.sh:465`) are bound read-write from the sandbox's real `$HOME` into every box, so
boxes share one toolchain and one build cache. **`~/.local/bin/claude` is the agent binary every
other box executes on next start.** Any box can overwrite it. This is stronger than any socket path:
it is persistent, it survives restarts, and it needs no live target.

> **No shared writable path may contain anything another box executes.**

**2 — Cross-box agent messaging, by design — and kept.** `~/.claude/sessions` is deliberately shared, the inbox
sockets live in the sandbox-wide `/run/user/1000/cc-socks/`, and every box's settings are seeded with
`crossSessionInbound: "accept"` so messages are delivered rather than held for approval. Every box is
addressable by name. So any box can drive any other box's agent with text of its choosing.

Decided: this **stays on**, because it is a real feature and boxes are already one trust domain — the
change is that the architecture states it rather than implying files are the only axis of separation.
**Control flows between boxes even though files do not.**

**3 — The workshop box.** `SKEIN_BOX_PRIVILEGED=1` skips the entire isolation block and leaves the
fleet-agent token readable — and that token runs a script as root at fleet scope. It is a per-box
cockpit toggle, so **one switch grants a box fleet root**, reaching every other box's tokens,
conversations and the credential helper.

**Kept, deliberately** — it is how skein is developed, in a box that can see the fleet it belongs to.
So it is the boundary's one intentional escape hatch, and §9.5.9 makes its terms explicit rather than
leaving them to be discovered.

> **The security boundary is the fleet sandbox. Inside it, boxes are separated by files but not by
> control.**

### 9.3 Credential poisoning, not only theft

The agent login flows **both ways**: seeded down into each box, and flowed back up on refresh, with
the winner chosen by `expiresAt` **read out of the file itself** (`better_login`, `merge_login`).

So a rogue box writes a credentials file with a far-future expiry and an attacker-chosen token. It
wins the comparison, is copied up to the canonical copy, and **every box started afterwards seeds
from it.** Self-asserted freshness, no signature, fleet-wide propagation. Theft is the obvious risk;
poisoning is the sharper one.

> **A credential never wins on freshness it asserts about itself.**

### 9.4 What moving skein inside costs — and the cockpit port, answered here

The single most important consequence of co-residence, stated where it arises rather than deferred:
**the cockpit's HTTP port is reachable from every box, and there is nothing to be done about that** —
one network namespace. This is not hypothetical. `apiauth.rs` documents the attack already happening
against *host-side* skein: a box reached `host.docker.internal:7878/api/fleet/git-grants` and got a
200, followed by a three-step defeat of the git gate. In-fleet, that host becomes `localhost`.

The answer is not to hide the port. It is that **connecting is not authenticating**: the bearer token
is a file under the volume's mount cover (§9.5.2, §9.5.3), owned by skein's uid (§9.5.1), so a box can
open the socket and get nothing. The mechanism already exists — it was built *because* of that
incident, and it fails closed when the token cannot be read.

That answers the box→skein direction and **only** that direction. Five things it does not cover, each of
which needs a requirement rather than an inference:

- **the auth-off switch.** `SKEIN_NO_API_AUTH` voids all of this, and the server already prints that
  anything reaching the port drives the fleet, boxes included. It exists for a fleet whose owner has
  some other boundary; **in-fleet there is no such boundary, so the switch is refused.**
- **the token has a second copy.** It is printed as `?t=…` on stdout at every start, and in-fleet
  stdout lands in a log, a tmux scrollback or a supervisor capture inside the sandbox. Covering the
  file does nothing for that.
- **port squatting, the reverse direction.** Shared netns plus no-unpublish (§7.4) means the mapping
  outlives skein — so **a box that binds the cockpit port before skein starts becomes the cockpit**,
  and the browser hands it the token on the first request. A distinct uid stops SO_REUSEPORT theft
  from a live listener; it does not stop an empty port at sandbox start.
- **pre-auth connection exhaustion.** The gate runs after accept, and §10.1's cap is post-auth. A box
  gets a free denial of the control plane, and therefore of the approval surface, with no credential.
- **box-authored content rendered inside the authenticated cockpit.** The file viewer parses markdown
  from a box's tree into the page. Raw HTML is escaped, but the link renderer is not overridden, so a
  `javascript:` href survives — and one click in a rendered README is same-origin script with the
  cookie attached. The code still justifies its choice by "the unauthenticated cockpit API", an
  assumption the bearer token retired. **Sanitising box-authored content is part of the boundary**,
  not a rendering detail.

Relative to host-side skein, co-residence gives a box: the ability to signal or kill skein (shared
PID namespace, same uid); network reach to skein's own API; reach to the warden over the gateway,
indistinguishable from skein by address or uid; and credentials inside the blast radius.

### 9.5 Requirements

Independent unless stated, and **requirement 2 (the cover) is done before requirement 1 (the split)**
— see the accounting under R1, and `docs/delivery.md` §3 step 4. An earlier draft ordered them the
other way and a still earlier one claimed the rest waited on the split; neither was right.

1. **skein runs as its own uid; boxes run as theirs; skein becomes a box to reach it.**

   ### Why this is hard — two constraints that must hold at once

   **(a) The box's user namespace exists only because `bwrap` is invoked unprivileged.**
   `grep -c 'unshare-user' src/box-session.sh` → **0**. It is never requested; bwrap creates one
   because it must, to obtain mount capability without privilege. The launcher says so: *"only this
   box's uid is mapped into its user namespace… setuid cannot grant uid 0 inside a namespace you
   created yourself."*

   **(b) `setns` into a user namespace needs CAP_SYS_ADMIN *in that namespace*.** An unprivileged
   caller has it only when its **euid equals the namespace owner's** — the euid of whoever created
   it.

   Together these rule out both obvious answers:

   | attempt | fails because |
   |---|---|
   | **skein runs as root** | the whole launch chain is root, so (a) gives no user namespace at all: every box is uid 0 under `--dev-bind / /`, with CAP_SYS_ADMIN over the sandbox and a walk out of its own mount namespace. It destroys the one boundary §9.1 confirms holds. It also silently breaks every bind derived from `$HOME`, which as root is `/root`. |
   | **skein simply runs as a different uid** | (b) makes every `setns` **EPERM** — terminal, takeover, provisioning, diff, upload. The design would specify something that cannot run. |

   ### Where each side sits

   Three facts the mechanism depends on, none of them optional:

   - **skein stays in the sandbox's initial user namespace.** The box's namespace must be a *direct
     child* for the euid-equals-owner rule to apply, so skein must never be wrapped in a `bwrap` of
     its own.
   - **The cover is applied by the launcher, inside the box** — which is what the launcher already
     does. It is not something skein wraps around itself, and §9.5.5's secret is protected because
     the *box* cannot see it, not because the launcher cannot.
   - **The `exec` is mandatory, not a cost.** Joining a user namespace is refused for a multithreaded
     caller, so an in-process `setns` from the threaded server is impossible regardless of
     credentials.

   ### The objection this overturns

   The launcher already argued against uid separation, and measured it: *"Separate uids were the
   obvious answer and are the wrong one here, measured rather than assumed… a box running as its own
   uid would leave the cockpit unable to attach to any box in the fleet."* That objection is correct
   about a skein that stays itself. It is answered — not waved away — by skein *becoming* the box for
   the crossing, which is what the next section is.

   ### The mechanism

   **Every crossing goes through `sudo -u <box uid>`.** skein does not enter a box *as skein*; it
   becomes that box's uid for the duration.

   - **launch**: `sudo -u <box uid> <launcher>` → bwrap runs unprivileged as that uid → constraint
     (a) satisfied, and the namespace owner is the box's uid.
   - **entry**: `sudo -u <box uid> nsenter …` → the caller's euid now equals the owner → constraint
     (b) satisfied.
   - **sandbox-root work** (cgroups, apt, fleet root, archive) — plain `sudo`, exactly as today.

   Three things fall out that are worth stating, because an earlier draft got each of them wrong:

   - **No shared group, and no group at all.** The tmux socket is `0700` owned by the box's uid, and
     skein arrives *as* that uid, so it opens normally. An earlier draft proposed per-box groups with
     `0770` sockets; that scheme also had a lifecycle bug (supplementary groups are fixed at process
     start, so skein could not join the group of a box created after it started) and a shared group
     would have let box A reach box B's socket — restoring the cross-box control path the split
     exists to remove. All of it goes away.
   - **No `newuidmap` on the critical path.** It is needed only to map *several* uids inside one
     namespace, which this does not do. Its absence is not a blocker.
   - **The launcher does not need "splitting".** It is already split — its privileged prologue is the
     `sudo` prefix on the cgroup work, and `bwrap` already runs unprivileged. What changes is *which
     uid invokes it*, and the `$HOME` it carries.

   ### What it costs

   One extra `exec` per box operation. Box operations are not on the hot path — the per-tick signal
   is a file read, not a crossing — so this is launch, attach, upload, diff and takeover, not the
   board.

   And a **sudoers rule — a tripwire, not a boundary, and it must be labelled as one.** §7.2 keeps
   broad `sudo` for skein's sandbox-root work, and a process with broad `sudo` can already become any
   uid, so the narrow rule below constrains nobody who has defeated skein. It is worth writing anyway
   — it documents intent and catches mistakes — but calling it the boundary would be an obstacle sold
   as one. Narrowing it into a real boundary means enumerating §7.2's sudo (cgroup writes, apt, npm,
   `tar`, `chown`, the docker config), which is a separate decision this document does not take.

   The rule: skein's uid may run *the launcher*, *`nsenter`*
   and *`tmux`* as the box uids, and nothing else. Written wider than that, it is an escalation. A box
   never reaches this policy: inside its user namespace `sudo` has nothing to escalate to, which is
   why the in-box shim is a message rather than a boundary.

   ### The address must not come from the box

   R1 crosses into `/proc/<anchor>/ns/user`, and **today the anchor is read out of a file the box can
   write**: `read_anchor` `cat`s the pidfile under the box's own root, which is bound read-write. A
   box writes a *sibling's* tmux server pid into its own anchor, and skein's next provisioning, diff,
   upload or takeover for that box executes **in the sibling's namespace**. A confused deputy walking
   straight through §9.1's file cover — and §9.5.8's rule names it, while §9.5.8's enumerated list
   missed it.

   > **The anchor is reported to skein over the channel skein opened, never read from a path the box
   > can write.** skein spawns the launcher; the launcher asks its own tmux server for the server pid
   > and writes it to **stdout**; skein records it. The pidfile in the box's tree may remain for the
   > box's own use, and skein must never read it.

   Three consequences, each of which the channel alone does not give:

   **(a) Adoption must re-launch.** Today `start_box_inner` reads the anchor *unconditionally* —
   including on the branch that keeps an already-live session, where **no launcher is spawned at
   all**. "The launcher reports it" never reaches that path, so the confused deputy survives there.
   Either skein re-launches rather than adopting, or it re-derives the anchor from something it owns.
   Adopting a session and trusting the box's file for its address is the one thing that must not
   happen.

   **(b) It must survive a skein restart — and `places/` is where it already lives.** Boxes outlive
   skein, so on reconnect the launcher is gone; the existing placement record exists for exactly this
   reason. §5's "namespace anchors are not on the volume" was **wrong**, and it was wrong by
   conflating two different restarts: a **sandbox** cycle invalidates an anchor, a **skein** restart
   does not. So `places/` is **declared state under the cover** (§9.5.2), and it carries a
   **sandbox-generation stamp** — a boot id — so that everything recorded before a sandbox cycle is
   discarded rather than re-entered.

   **(c) A bare pid is not an identity.** Pids recycle within a generation, so the record is
   **`(pid, starttime)`** and every use re-reads the start time and compares. A mismatch means *the
   box is gone* — never "enter this instead". Generation guards the sandbox cycle; start time guards
   recycling inside one.

   **And the report must come from a binary the box cannot shadow.** It is produced today by an
   unqualified `tmux` inside a **login shell**, whose PATH includes `~/.local/bin` — which is shared
   read-write with every box (§9.2.1). So the anchor's integrity would silently depend on R4. Invoke
   it by **absolute path**, with a fixed PATH.

   This has to be settled before R1 is built, because every other part of R1 is downstream of an
   address it trusts.

   **`tmux` is in that list because the `socket` Source is a crossing too**, and an earlier draft
   missed it: a box's tmux socket is `0700` owned by the box, and §2.3 is explicit that the cheapest
   signal reaches it *without* `nsenter`. So `has-session`, `attach`, `new-session` and `kill-server`
   all become `sudo -u`.

   **One more per-tick reader is uid-dependent, and it is not a crossing.** The disk sweep walks
   every box tree every 30s, and each box's private HOME is `0700`. Under the split that walk runs as
   skein over box-owned directories with errors suppressed, so it would **silently under-report**
   rather than fail — and the code's own comment records that this silence "was invisible for a long
   time because the row chip stays silent below 80%". Either the sweep runs per box as that box, or
   boxes report their own usage. It must not stay a suppressed-error walk.

   **And liveness moves off the socket.** Today the board's liveness sweep opens every box's
   `session.sock` each tick — which under the split would put the board on the crossing path. It does
   not need to: §6 defines liveness as *the tmux server is alive*, and the anchor **is** that server,
   so the level signal is a `/proc` read. Cheaper than the probe it replaces, and uid-independent.

   **The cgroup prologue moves to skein whole — and the launcher does not join at all.** Under
   `sudo -u` the launcher runs as the box's uid, so its `sudo` cgroup writes would need every box uid
   to hold root, which is the opposite of the point.

   An earlier draft proposed chowning `cgroup.procs` to the box uid so the launcher could still join
   itself. **That does not work**: cgroup-v2 delegation containment requires the writer to have write
   access to the *common ancestor's* `cgroup.procs` as well as the destination's, and the ancestor is
   root-owned. The observable result would be the launcher's existing `could-not-join-cgroup` path —
   every box silently unbounded. Chowning the ancestor instead would let any box move processes
   between box cgroups, which is worse than the problem.

   **Membership is inherited across `fork`, `exec` and setuid**, so skein writes the pid of the shell
   that is about to `exec sudo -u <box uid> <launcher>`, and the launcher, bwrap, tmux and the agent
   all inherit it. No chown, no delegation, no second policy.

   And the prologue moves **whole**: the fleet-scope work in it — the container cgroup and the fleet
   ceilings — loses root under `sudo -u` too, and the launcher's own comment says what that costs,
   naming the container cgroup as *"the one thing in the fleet nothing bounds"*. And `nsenter` keeps `--preserve-credentials` (§2.3) — without it `setgroups` fails.

   **Per-box uids are the stronger form and are *not* free.** They
   collide with two things this design keeps: the per-user socket directory that carries cross-box
   messaging (§9.2.2) is `0700` per uid, and the shared toolchains (§9.2.1) are owned by the single
   uid today. They also need a uid allocation record — **declared state on the volume, and §5 must
   list it** — a chown of every existing box tree at migration, and a decision about the uid a box
   sees *inside* its own namespace, which is 1000 today.

   **And they are gated on the credential path**, which is what makes them a redesign rather than an
   argument to `sudo -u`: the agent login is seeded by copying uid 1000's `0600` credentials into the
   box and flows *back* into that same canonical copy on refresh; the shared toolchains are
   uid-1000-owned and boxes install into them; and the cross-box session directory (§9.2.2, kept)
   lives in uid 1000's home. **Per-box logins (§9.6's second unbuilt defence) are a prerequisite of
   per-box uids**, not an independent improvement.

   ### What the split actually buys — and what it does not

   Honest accounting, because an earlier draft credited the uid split with things the **mount cover**
   (requirement 2) provides on its own:

   | threat | closed by |
   |---|---|
   | a box reads skein's credentials, token or state | **the cover** |
   | a box impersonates skein to the warden | **the cover** (a secret it cannot read) |
   | a box writes skein's declared state | **the cover** |
   | a box signals or kills skein | **the uid split** |
   | a box that defeats its mount namespace is skein's peer | **the uid split** |

   So the cover carries most of the value and is cheap; the split is defence in depth and costs a
   `sudo` hop per crossing plus a sudoers policy. **Do the cover first.** If the split slips, what
   remains exposed is a denial of service against the control plane — which a supervisor restarts —
   rather than a disclosure.


2. **The cover is an inversion, derived from the fleet's mount set.** Not from one root:
   `repo.work` and an adopted `repo.store` are **arbitrary host paths chosen at repo-add time**, so a
   rule written over the state root alone never reaches `/home/you/code/thing`. The launcher must be
   *given* the mount set — it has no way to learn it today — and each box gets back only its own
   repo's store. `tmpfs` the whole of the state root and bind
   back the short list a box needs — which is what `box-session.sh:943-956` already does for the
   fleet root. Enumerating what to *hide* is the wrong direction and an earlier revision froze that
   list at three names while the root holds fifteen things that matter, among them `substrate.json`
   (replayed onto a **root** `apt-get`), `api-token`, `config.json`, `repos.json`, `github-pats`,
   `tokens`, `plane-token`.

   **A mount cover, not a file mode**: `0700` means nothing under one fleet-wide uid.

   **And per-box state must split**, which is where the current design has a live exploit.
   `privileged` and `git-scope` are **declared** state — sole-writer, security-deciding — living at
   `boxes/<name>/`, a directory bound **read-write** into the box because the conversation and
   transcript live there. So today:

   ```sh
   echo 1 > ~/.skein/boxes/$SKEIN_BOX/privileged   # from inside any box
   ```

   and at its next start that box is the workshop box: every isolation bind skipped, the fleet-agent
   token readable, and that token runs scripts as root at fleet scope. `git-scope` is the same shape
   — write `fleet` and keep the account-wide token and the forwarded ssh-agent.

   > **`boxes/<name>/` splits four ways** — `declared/` (never bound in), `recorded/` (bound
   > read-write), `artifacts/` (bound **read-only**) and `transitions` (recorded, **not bound in** — skein writes it, the box has no use for it). Only binding
   > `recorded/` would remove git push, since the box's credential helper reads its own token out of
   > `artifacts/git-tokens/`.
3. **skein's control API is a filesystem socket under that cover, owned by skein's uid, never a TCP
   port**, and the cockpit's HTTP auth token is a file with the same ownership — skein mints it, so it cannot be
   root-owned. (An earlier draft said
   *root-owned*; skein is not root — R1.) Boxes may still *connect* to the cockpit port
   — shared netns makes that unavoidable — and cannot authenticate.
4. **No shared writable path contains an executable another box runs** (§9.2.1). Either the shared
   toolchains become read-only with a per-box overlay for writes, or they stop being shared.

   Two honest qualifications. This hardens **durability and blast radius, not control**: §9.2.2 is a
   shipped feature that lets one box drive another's agent directly, so R4 does not close the cheapest
   path and must not be sold as doing so. And **skein's own approved-package installer may break it**: `sudo npm install -g` writes to
   whatever prefix npm resolves under root, and nothing in this tree sets one. If that prefix is the
   shared `~/.local`, the rule is violated by the honest path before any attacker arrives — and worse,
   it is root writing through a box-writable path (requirement 8). **Check `npm config get prefix`
   under `sudo` in the fleet image before designing around either answer.**
5. **The warden authenticates with a secret under the cover of requirement 2.** A mount cover hides
   it regardless of uid; cross-userns `/proc` access is already denied, so a box cannot lift it out of
   skein's memory either.
6. **The audit log is written by the warden, on the host, on a path no box's mount view includes.**
   "Append-only" is unenforceable on a path a uid-1000 box can reach — there is no `chattr +a`
   without `CAP_LINUX_IMMUTABLE`.
7. **Credentials are compared on evidence skein controls, never on a field the file asserts** (§9.3).
8. **No privileged actor reads, writes, chowns or follows a path a box can influence.** §8.4 states
   this for approvals; that was too narrow, and the general form sweeps in three more:

   - **the resize archive** is parked in the box-writable state directory, created with root `tar`
     and `chown`, and restored with root `tar -x` **preserving owners and modes**. Its name is a
     second-resolution timestamp, so it is pre-plantable as a symlink. A root actor writing through a
     path a box controls is the same class as the approval TOCTOU.
   - **`git-tokens/`** is host-*written* and box-*read* — an **artifact** (§2.1's fourth kind), bound
     **read-only**. The requirement is that a box cannot *replace the path*, not merely cannot write
     through it: the privileged refresher does `create_dir_all`, an unlink loop and a secret write
     under that directory, so a box that swaps it for a symlink gets the privileged side to unlink
     host files and drop a **live installation token** somewhere of its choosing. Note the launcher
     also creates and chmods that directory *from inside the box* today, and will start failing
     silently against a read-only bind — a line that has to move with the requirement.
   - **`disk` and `identity`** join `privileged` and `git-scope` as declared per-box files in a
     box-writable directory: a box raises its own disk allowance on a shared disk, or forges its
     committer.

9. **The workshop toggle states what it grants.** It is fleet root, and it is the boundary's only
   deliberate in-fleet escape hatch.
10. **Cross-box messaging renders provenance** (§9.2.2, kept). At minimum, inbound-from-a-box is
   distinguishable from inbound-from-you — otherwise the one channel that carries control between
   boxes is also the one with no attribution.
11. **`/run` is covered, or its exposure is stated.** §9.1 notes the cover reaches neither `/run` nor
   the per-user socket directory, and no requirement followed.

Corrected from an earlier draft: the cgroup control plane is **not** box-writable. Every cgroup write
in the launcher goes through `sudo` before `bwrap`, and the source is explicit that a write from
inside a box "is not an option at all" — the userns maps only uid 1000 and cgroupfs is root-owned.

### 9.6 The agent credential cannot be scoped

GitHub tokens can be scoped per repo, short-lived and revoked — with one caveat that belongs beside
the credit: `SKEIN_GIT_SCOPE=fleet` is an opt-out restoring the account-wide token *and* re-exposing
the forwarded ssh-agent. And the guard is the token, never the shim: *"The shim is the message, not
the boundary."*

The agent's own OAuth login is different. It must be **inside the box** for the agent to run, and no
provider offers a scoping primitive for it. So it is carved out rather than covered by a claim that
does not hold: **the agent login is fleet-shared and unscopable.**

Two defences exist and neither is built: a proxy that injects it outside the box's reach, and
**per-box logins** — which the launcher already falls back to when `python3` is absent, so the path
is not hypothetical.

## 10. Budgets

**skein must never be expensive enough to disturb development on the machine it runs on.** Measured,
not asserted: the in-box observer costs 0.11% of a core.

Cost is **not one number**. Five budgets, each with its own unit and its own enforcement:

| budget | unit | note |
|---|---|---|
| CPU | core-fraction | the observer's; the host side is already zero |
| wall-clock | ms per board refresh | gates, single-flight, backoff |
| GitHub | API units | 403 with a reset time, not a slowdown |
| model | dollars | AI summaries, narrate, the resume safety gate |
| volume I/O | writes/s | mounted writes are expensive; the observer shapes its writes for this |
| delivery | boxes × transitions × clients | the event stream — §10.1 |

Correcting the first draft: in-fleet mode does **not** make observation cheaper, because host cost was
already zero by design. What it does is put skein's web server, SSE fan-out, git operations and
GitHub polling **inside the fleet's memory reservation** — the reservation whose summing is the entire
reason the one-VM design exists. Every byte skein takes is a byte a box cannot have. That is the real
cost and nobody had costed it.

### 10.1 Delivery, and what the design owes it

The event stream today re-sends the whole fleet every two seconds: no deltas, no bounded channel, no
lag counter, no connection cap, and a missed-tick policy that bursts at a drained slow client. The
diagnosis is easy and an earlier draft stopped there. The design:

- **one producer, fanned out.** Today there is no broadcast channel at all: **every SSE client
  independently runs the fleet snapshot on the blocking pool every two seconds.** A per-client bounded
  channel presupposes a single producer, so that is the first thing to build — and it is the only
  thing that bounds the `boxes × transitions × clients` budget. The lesson is already paid for:
  check-then-act once gave every browser tab its own subprocess every tick.
- **transitions, not snapshots.** §3 already requires server-side transitions; the stream carries
  them, and a full snapshot only on connect or on request.
- **a bounded per-client channel with a lag counter.** On overflow the client is told it fell behind
  and re-syncs from a snapshot — never silently skipped.
- **a connection cap**, as the PTY path already has.

---

## 11. Surfaces

Designed from the five jobs, not ported from the current layout. `docs/parity.md` governs *what*;
this governs *how*.

| job | frequency | needs |
|---|---|---|
| **triage** — who needs me? | constant | a ranked queue, a calm empty state |
| **converse** — talk to an agent | often | terminals that never lie about freshness |
| **understand** — what did it change? | often | the *shape* of the change: modules, decomposition, drill-down |
| **recover** — something broke | rare, high-stakes | the failing check and the command that fixes it |
| **set up** — add a repo, make a box | rare | one action, no configuration exercise |

**Attention is the scarce resource, not screen space.**

### 11.1 Understanding a change is architectural, not textual

The job is **not** reading a diff. Agentic coding is good enough at writing code that line-by-line
reading is rarely where the value is; what matters is **which modules changed, how the design
decomposes now, and the ability to drill to code when something warrants it.** Mostly the code is not
read at all.

So **skein does not compete with GitHub on diff rendering.** If someone wants the text, GitHub has
it, and it is one click away. What skein owes is the layer above it:

```
web-main · warden owns fleet lifecycle           3 modules · +412 −180

  ▸ warden        NEW      the host side of create and destroy
  ▸ operation     CHANGED  checks become tri-state; doers optional
  ▸ fleet         SHRANK   lifecycle moved out

  ⚠ operation::check changed shape — 12 call sites
```

Each row opens: the module's **standing note** (what it is for), then what this change did to it, then
the files, then the code. Four levels, and most of the time you stop at the first.

Two things already in the codebase are the primitives, and they were previously filed as review
features rather than architecture ones:

- **standing module notes**, whose freshness is keyed to the commit the module was at, so a stale
  note is never used. That is a continuously-maintained description of how the system decomposes —
  exactly the artifact this job needs, and it already exists.
- **contract signals**, a mechanical scanner over the diff that escalates a change the model called
  boring. That is what fills the `⚠` line: the structural consequence a summary would miss.

Both are **signals whose subject is a module** (§2.2). No new machinery.

The consequence for scope: the diff pane and its inline comment composer are not ported. Commenting
back to an agent is an Act against a *box*, which the terminal already is.

**The pull-request queue is kept whole** — decided — and that is consistent rather than in tension
with the above. Its six actions (approve, request-changes, comment, **merge**, ask, draft) do not need
skein to render a diff: what they need is to know *what the change is*, which is what this section
supplies. So the loop is **skein tells you the shape, you act from skein, and the text is one click
away on GitHub.** Merge is an Operation of class `destructive`, not an Act (§3).

### 11.2 One queue, many sources

The board is a **queue, not a dashboard**, ranked by who needs you; a box that needs nothing is
recessive. Because "needs you" is a predicate over signals and the review queue is a signal source, a
pull request awaiting review belongs in the same queue as a box awaiting an answer — they are the
same thing to the user, and separate today only because they were built separately.

**The queue groups by repo, and grouping is not cosmetic.** Today's board has collapsible per-repo
sections with counts, persisted collapse state, and per-group pull and new-box actions. At two repos
that is decoration; at eight it *is* the board. A flat three-section list is a different product, and
the first draft's mock-up quietly chose it.

### 11.3 Three states most tools botch

- **Nothing needs you.** Say so, plainly and calmly. A dashboard that looks the same whether or not
  anything is wrong has failed at its only job.
- **You were away.** Continuity across absence is a requirement, not a nicety, and it needs
  server-side transitions (§3) rather than a client-side delta on tab focus.
- **Setup is incomplete.** Failing checks at the top of the same queue — but see §11.5, because the
  cockpit is not where a new user starts.

### 11.4 The laws

1. **Never report a problem without the action that resolves it.**
2. **Never show an observation without its freshness.** A stale signal must *look* stale.
3. **The board answers one question: who needs me.**
4. **Every Operation and every Act has a CLI form, named identically to its cockpit form.** Scoped
   deliberately: "anything doable in one is doable in the other" against ~80 capabilities was the
   largest uncosted item in the first draft.
5. **Destructive actions say what is lost** — including when the answer is "nothing".
6. **No modal onboarding.** Onboarding is the blocked state rendered well.
7. **The first screen has exactly one action**, which requires prerequisite collapsing (§11.6).
8. **Quiet by default.** Colour, badges and motion are an attention budget.

### 11.5 The blocked state is three components, not one

Onboarding needs *sequence and prerequisites*; breakage needs *what changed and when it last passed*;
approval needs *what will happen, who asked, and a timeout* — and lives on the host (§8.1). They share
a visual language and two primitives (the recipe block, the live check pip). They are not one card
with eleven optional props.

### 11.6 First run is the CLI

The blocked state renders in the cockpit; the cockpit needs the fleet created and the port published
— privileged operations. So **the first-run surface is `skein doctor` in a terminal**, and check cards
must render as text. Law 6 is preserved: it is still the blocked state, just not in a browser.

Because operations form a DAG (§2.4), **a failing prerequisite collapses its dependents**: a missing
fleet shows one card, not five, which is what makes law 7 achievable.

### 11.7 Identity and build

Fresh, and mostly monochrome: **warm means a human is needed, cool means the machine is working,
muted green means finished, grey means nothing is happening.** Learned in one glance, and it leaves
the whole colour budget for the only thing colour has to say.

Density is adaptive — dense when the fleet is large, comfortable when it is small. Same components,
two spacing scales.

**The cockpit has a build step.** The current single 6516-line `index.html` cannot support both the
pure-function testing law (§13) and a reviewed component library; a build makes both achievable and
removes the third source of truth. The component library lives in a Claude Design project and is the
source the cockpit is assembled from.

---

## 12. Rules that keep it clean

Each is a specific way this codebase has previously accumulated debt.

1. **Declared state has one writer. Recorded state has a documented protocol.** Not "one writer per
   fact", which is false for most of the volume.
2. **No dual code paths to the same outcome.**
3. **Every Operation is idempotent, or marked `destructive` and never auto-driven.**
4. **No displayed state from an edge alone *without saying so*** (§2.2 — the absolute form was wrong
   and the shipped code is right).
5. **Every signal is keyed by its subject, not its observer.**
6. **Every signal declares its budget, its cadence and its staleness threshold.**
7. **Everything a human might do by hand has a printable recipe.**
8. **A new feature is a composition of primitives — or it adds a primitive deliberately and amends
   this document.**
9. **A requester's file is input, never state. The approving side writes the approved artifact, and
   the actor reads only that** (§2.1, §8.4).
10. **Trust-boundary capabilities are modules that can be left unbuilt**, and they never reference one
   another.
11. **Every durable file carries a schema version, and skein refuses a volume it does not understand.**

---

## 13. Testing

- **Every check has a test that it fails when the thing is absent.** A check that passes
  unconditionally is worse than no check: it makes a broken system report as healthy.
- **Every check has a test for `unknown`** — that an unreachable source reports rather than drives.
- **Signal fusion is a scenario matrix**, including every missing-edge case.
- **Anything the UI computes is a pure function tested in node.** The build step (§11.7) is what makes
  this possible.
- **Browser tests run in a box.** Correcting the first draft: this was fixed, and
  `tests/ui/README.md` names the libraries Playwright's own list omits.
- **The warden is built and tested four ways**: sink-and-observation only (the minimal build — not
"empty", since two endpoints are never removable), plus each doer alone, plus both.
- **Screen grammars are verified against a real box**, never a clean-room one — a bare tmux session
  has no configured statusline, a short pane and no scrollback, which hides exactly the defects that
  matter.

---

## 13a. What the rewrite deletes

Restored, because an earlier draft removed the whole list after one entry was found wrong — which
made the other seven unauditable rather than fixing the one. **Every entry here is a mechanism.
Anything user-visible belongs in `docs/parity.md` §7 instead, and the transport *readout* is there
for exactly that reason.**

| deleted | why it can go |
|---|---|
| the in-sandbox agent and its transport | it exists to survive a host-to-guest hop that no longer happens |
| its port publishing, healing loop and backoff | same, and it is the one thing that inherits sbx's no-unpublish trap (§7.4) |
| every `sbx exec` path **and its fallback twin** | with them, the transport-failure-versus-command-failure distinction that made the pairing necessary — but see below |
| two placement shapes | one remains |
| sandbox listing as the truth about boxes | replaced by the box's own anchor (§6) |
| the machine-global secret store | with it, two fleets on one host sharing one token |
| host-absolute mount path translation | no host mounts of repos remain |
| adopt-in-place mounts | replaced by local-path remotes (§6) |

**The hazard the fallback twin guarded is not deleted, it moves.** "Did it run or not?" becomes a
timeout on a warden request, and §8.2's operation ids are what answer it there. Deleting the
distinction without carrying the safety property forward is how this becomes worse than what it
replaced.

**How they retire**: the transport and the `sbx exec` twin survive until delivery step 4, because
until skein is in the fleet there is still a hop. They are removed *with* the move, not before it and
not after — `docs/delivery.md` §3.

## 13b. Debugging skein itself

Law 1 applies to skein's own failures, and today there is no structured logging anywhere — `eprintln!`
only, no request log, no metrics. A design whose central claims are a reconciler, a set of gates and a
trust boundary needs to be able to answer:

- **which check failed, when it last passed, and what it returned** — the check history is recorded
  state, bounded and rotated.
- **which operations hold leases, and since when** (§2.4).
- **which gate is degraded and how long it has been serving stale.** `degraded()` exists and *two of
  four* production gates surface it — the fleet gate drives the health card's "showing last successful
  snapshot", and the resource gate marks the gauges stale. The disk and liveness gates surface
  nothing, and no gate reports *how long*.
- **what the warden was asked, what was approved, by whom** — the host-side audit log (§5).

`skein doctor` is the read surface for the first three; it is already every check, reported.

## 14. Modules

| module | owns | depends on |
|---|---|---|
| `state` | the volume: **declared, requested, artifact, recorded** — and the different rule each carries (sole writer / lock protocol / box-writable / read-only bind); schema; generation stamp | — |
| `source` | `enter`, `socket`, `file`, `http` | — |
| `signal` | kinds, freshness, budgets, cadence, gates, fusion | `source`, `state` |
| `operation` | desired, tri-state check, recipe, doer, DAG, leases | `state`, `signal` |
| `act` | streaming interactions; emits edges; reports outcomes | `source`, `signal`, `state` |
| `warden-client` | protocol, operation ids | `operation` |
| `warden` | the host service: capabilities, approval surface, outcome store, audit log | — (separate binary) |
| `box` | box identity and lifecycle | `state`, `operation`, `act`, `source`, `signal` |
| `fleet` | **fleet** lifecycle — create, destroy, resize as their composition | `operation`, `warden-client`, `state` |
| `grant` | **who may do what, and with which credential**: the git-write and package decisions (§8.4's three steps), the agent-login comparison (R7), the installation-token refresher and `artifacts/` (R8), the approved-artifact writes | `state`, `source`, `warden-client` |
| `mailbox` | cross-box and cross-project messaging, and its provenance (R10) | `state` |
| `github` | HTTP client, review queue, CODEOWNERS, contract signals, summary cache and its throttle | `state`, `source` |
| `probes` | the in-box probe scripts and hook merging — installed into every store, and the highest-blast-radius write in the system; owns the probe/binary compatibility contract | `state`, `source` |
| `api` | HTTP transport, auth, routes, the WebSocket | everything below |
| `stream` | the **single** event producer and its fan-out (§10.1), and transitions | `signal`, `state` |
| `cockpit` | the page, its build, and the component library (§11.7) | `api` (over the wire only) |
| `migrate` | the one-shot: snapshot, carry, rewrite hooks, refuse (see `docs/delivery.md` §4.3) | `state`, `operation`, `box` |
| `cli` | `skein` | `state`, `operation`, `act`, `signal`, `box`, `fleet`, `warden-client` |

`grant` exists because nothing owned the *deciding*. `state` owns the rules an artifact obeys;
until this row, no module owned who decides — which is where §8.4's three-step fix, R6, R7 and most of
R8 live, i.e. the heaviest security work in the plan. Two people decomposing without it would invent
two different owners.

`server` was one row owning "everything", which cannot be decomposed against — it is split into
`api`, `stream` and `cockpit`, because the single-producer fan-out and the cockpit build are separate
deliverables with separate owners. `github` and `probes` were absent while `docs/delivery.md`
schedules both as parallel work.

The `cli` row carries `act` because law §11.4.4 requires every Act to have a CLI form, and
`warden-client` because first run is `skein doctor` (§11.6) against a fleet that does not exist yet.
`fleet` exists because otherwise nothing owns the two privileged operations.

`state` and `source` depend on nothing. `source` never depends on `operation` — reaching a subject
must never require privilege. The **warden is a separate binary** with no dependency on skein's
modules; an earlier draft gave it no row while §13 required it be built four ways.

### 14.1 The CLI stays standalone, and declared state gets a lock

An earlier draft made the CLI a client of the server. That was wrong twice.

**Inverted against the code**: today the *server spawns the CLI* — the cockpit creates a box by running
`skein start … --attach` as a subprocess, resolving the sibling binary. There is no HTTP client in the
crate at all.

**Circular against §11.6**: first run is `skein doctor` in a terminal *because the cockpit needs a
fleet that does not exist yet*. A CLI that is a client cannot run before the server, and the server
cannot run before the fleet.

So the CLI keeps driving the library directly, and the problem the client idea was solving — two
processes doing unsynchronised read-modify-write on declared state — is solved where it belongs:

> **Declared state is written under a lock, by whoever holds it.** Not "one writer" as a count. Today
> `config.json` and `repos.json` are atomic-write but unlocked, so two cockpit tabs saving settings is
> silent last-write-wins; the mailbox and the sandbox registry already lock, and are the model.

### 14.2 What must be dismantled before any of this is checkable

`src/lib.rs` re-exports sixteen modules with `pub use <mod>::*`, so cross-module references go through
a flat root namespace — `grep -rn "crate::signals::"` from other modules returns **zero**, not because
nothing uses it but because everything uses the re-exports. There is also a live cycle: `place.rs`
calls into `fleet`, and `fleet.rs` imports `place`.

**Every dependency rule above is unverifiable until the façade comes off.** That makes removing it the
first task of extraction, not a tidy-up afterwards.

## 15. Open

- **Per-box subuid isolation** (§9.5.1, second step). Files are covered; control is not (§9.2).
- **Narrowing the workshop box** (§9.2.3). Kept as-is for now; whether developing skein needs *full*
  fleet root, or a named set of capabilities, is a smaller-blast-radius question worth revisiting once
  the privilege split lands.
- **The agent-credential proxy** (§9.6). Unbuilt, and the only real defence for the credential that
  matters most.
- **Multiple fleets on one host.** The volume makes it clean; the cockpit port and the warden's
  addressing both assume one.
- **API authentication in-fleet** (§9.4 answers the box→skein direction; the five gaps listed there are open). Today it is one shared bearer token, and its own comment says
  *"not a login — one shared secret"*. It exists because a box reached the host cockpit. In-fleet it
  matters more, and §9.5.3 changes its shape rather than answering it.
