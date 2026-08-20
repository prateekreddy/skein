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

Five. Two nouns, one reach, two verbs.

| | idempotent / self-healing | lossy / one-shot |
|---|---|---|
| **durable** | State: *declared* | State: *recorded* |
| **observed** | Signal: *level* | Signal: *edge* |
| **acting** | Operation | Act |
| **reaching** | Source | |

### 2.1 State — what is written down

Durable, on the volume (§5). Two kinds, with **different rules**, because treating them alike is a
defect the first draft shipped:

**Declared** — what the user told skein. Repo remotes, box identities, config, branch choices.
Small, versioned, **sole writer**, guarded by a lock or an owning process.

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

Therefore: **no displayed state may rest on an edge alone.** Edges may only *accelerate* a level
signal — arriving sooner than the next poll — never be its sole basis. And the keying rule is its
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

**The Gate contract.** Every signal's observation is mediated by a gate that provides: single-flight
(concurrent askers share one observation), serve-stale-refresh-behind (the last good value is
returned immediately while a refresh runs), exponential backoff on failure, and a sticky last-good
value so a wedged source does not blank the board. This exists today in `util.rs` and is load-bearing
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

Each carries three modes, because "execute" does not cover what the jobs need: **`exec`** (capture
output), **`stream`** (stdin or stdout as bytes, up to gigabytes, never buffered whole), **`pty`** (a
terminal).

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

### 2.6 The reconciliation cube

Three axes, because the 2×2 of the first draft mishandled four common cases:

- **declared** ∈ `absent | present | deleted`
- **desired** ∈ `running | stopped`
- **observed** ∈ `present | absent | unknown | failed(reason)`

The cases the square got wrong, each of which is a real feature today:

| situation | square said | cube says |
|---|---|---|
| a box you deliberately stopped | "an operation is owed — start it", forever | declared present, **desired stopped** — nothing owed |
| a box whose start failed | same cell, so retry forever | `observed = failed(reason)`, and the reason is shown |
| a half-completed destroy | "foreign — never touch", so skein cannot clean up its own corpse | `declared = deleted` — a tombstone, and cleanup is owed |
| sbx unreachable | "absent" | `observed = unknown` — report, drive nothing |

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
| onboarding, breakage, approval | failing checks with their recipes — three components, one language (§11.4) |
| converse, answer, interrupt, upload, takeover | **Acts** |
| launching a box | an Operation whose check is "the anchor is alive" |
| foreign detection | `declared absent ∧ observed present` |
| crash recovery | reconcile the DAG |

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
| `credentials/` | declared, `0700`, and see §9 |
| `boxes/<name>/` — launch spec, conversation, transcript, notes, overrides | recorded |
| `boxes/<name>/transitions` — retained signal values and watermarks | recorded |
| `repos/<id>/mirror` — a bare git mirror | recorded |
| `repos/<id>/store` — the shared `.claude` every box for that repo reads | recorded, many writers |
| `grants/`, `substrate/` — git-write grants and approved packages | declared |
| `audit/` — an append-only log of every approved privileged operation | recorded |

`grants/` and `substrate/` are on the volume because they exist *precisely* to outlive the sandbox:
lose them and every box re-asks for push access and for packages already approved.

**Not** on the volume: box checkouts (VM-local; measurably faster for build work and reclonable from
the mirror), namespace anchors (a live pid, meaningless across a restart), caches and build output.

The test for anything new: *if the fleet were destroyed right now, would losing this hurt?*

**The volume can fill.** Every operation that writes has a free-space precondition; `boxes/<name>/` is
garbage-collected when a box is destroyed; transcripts and the audit log rotate. Today per-box disk
limits are *displayed and never enforced*, and no code matches `ENOSPC` anywhere — the first symptom
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

## 7. The two privileged operations

**create a fleet** and **destroy a fleet**. That is the whole list.

The first draft had six. Four collapsed once "repos are clones onto the volume" was actually applied:

| was | became |
|---|---|
| mount a repo | gone — it existed only for adopt-in-place and the mounted store parent |
| publish the cockpit port | a parameter of create |
| mount the durable volume | a parameter of create |
| store the push credential | **not privileged** — on the volume it is a file write |

**resize is a composition**: destroy + create, carrying the delta. Not "nothing to copy" — the
checkout is reclonable but its *uncommitted* work is not, so resize preserves unpushed commits, index
and worktree patches, untracked files and deliberately-swept ignored files. That is small and fast,
and it is what today's snapshot already does. Getting this wrong destroys a week of someone's work;
the current code refuses rather than warns in four separate places, and that instinct is correct.

**Nothing in normal operation is privileged.** Not commands in boxes, not terminals, not signals, not
the review queue, not the cockpit, not storing credentials.

One caveat the first draft got wrong: **package approval is not fleet lifecycle but does need root in
the sandbox.** A box asks for `apt`/`npm`, its owner approves, and the approval is remembered in a
manifest replayed into every future launch. It belongs to the fleet's own root, not the host's, and
it is an approval system rather than a setting.

---

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
Operation ids, and the warden answers a repeated id with the original outcome.

### 8.3 Capabilities are compiled

Each capability is its own module; a warden that does not need one is built without it. Not gated,
not configured — absent. A runtime check falls to a bug in the check; absent code falls to nothing.

Only the **doer** is removable. Recipes and checks live in skein, always compiled, never privileged —
they are needed precisely when the doer is absent.

Two capabilities, so the default build ships **create** and leaves **destroy** opt-in. A default that
can do nothing makes first run worse, which is what the first draft's "default has no capabilities"
did.

The capability set is derived from what is linked, not read from config. But skein must not extend
*trust* on the strength of an advertised list — a malicious endpoint advertises whatever makes skein
offer a button. Advertisement decides what skein *offers*; it never decides what skein *believes*.

---

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

**2 — Cross-box agent messaging, by design.** `~/.claude/sessions` is deliberately shared, the inbox
sockets live in the sandbox-wide `/run/user/1000/cc-socks/`, and every box's settings are seeded with
`crossSessionInbound: "accept"` so messages are delivered rather than held for approval. Every box is
addressable by name. So any box can drive any other box's agent with text of its choosing. That is a
feature, and it is also a trust fact: **control flows between boxes even though files do not.**

**3 — The workshop box.** `SKEIN_BOX_PRIVILEGED=1` skips the entire isolation block and leaves the
fleet-agent token readable — and that token runs a script as root at fleet scope. It is a per-box
cockpit toggle, so **one switch grants a box fleet root**, reaching every other box's tokens,
conversations and the credential helper.

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

### 9.4 What moving skein inside costs

Relative to host-side skein, co-residence gives a box: the ability to signal or kill skein (shared
PID namespace, same uid); network reach to skein's own API; reach to the warden over the gateway,
indistinguishable from skein by address or uid; and credentials inside the blast radius.

### 9.5 Requirements

Independent unless stated. The privilege split (R1) is done first by decision, not because the others
wait on it — an earlier draft claimed they did, and that was wrong.

1. **skein runs as root inside the fleet sandbox; boxes remain unprivileged.** Root *in the sandbox*
   is bounded by the VM: not root on the host, and boxes still cannot sudo. It closes signal/kill,
   and gives skein files no box can read whatever the mount view.

   Chosen over per-box uid mapping because **the attach mechanism survives unchanged** — the tmux
   socket is `0700 uid 1000` and root can open it, where a neighbouring uid would need a shared group
   and a `0770` socket. Root in the initial user namespace also has CAP_SYS_ADMIN over descendants,
   so `nsenter` needs no `newuidmap`.

   Per-box subuid mapping remains the stronger second step: it separates boxes from *each other* by
   uid, which is the axis §9.2 shows is open.

2. **The privileged subtrees of the volume are outside every box's mount view, and the cover list is
   a tested enumeration.** Without this every box sees `credentials/`, `grants/` and `audit/`
   read-write as uid 1000, and §5's `0700` means nothing under one fleet-wide uid. **A mount cover,
   not a file mode, is what protects them.**
3. **skein's control API is a root-owned filesystem socket under that cover, never a TCP port**, and
   the cockpit's HTTP auth token is a root-owned file. Boxes may still *connect* to the cockpit port
   — shared netns makes that unavoidable — and cannot authenticate.
4. **No shared writable path contains an executable another box runs** (§9.2.1). Either the shared
   toolchains become read-only with a per-box overlay for writes, or they stop being shared.
5. **The warden authenticates with a secret under the cover of requirement 2.** A mount cover hides
   it regardless of uid; cross-userns `/proc` access is already denied, so a box cannot lift it out of
   skein's memory either.
6. **The audit log is written by the warden, on the host, on a path no box's mount view includes.**
   "Append-only" is unenforceable on a path a uid-1000 box can reach — there is no `chattr +a`
   without `CAP_LINUX_IMMUTABLE`.
7. **Credentials are compared on evidence skein controls, never on a field the file asserts** (§9.3).
8. **The workshop toggle states what it grants.** It is fleet root, and it is the boundary's only
   deliberate in-fleet escape hatch.

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

Correcting the first draft: in-fleet mode does **not** make observation cheaper, because host cost was
already zero by design. What it does is put skein's web server, SSE fan-out, git operations and
GitHub polling **inside the fleet's memory reservation** — the reservation whose summing is the entire
reason the one-VM design exists. Every byte skein takes is a byte a box cannot have. That is the real
cost and nobody had costed it.

**Delivery cost is a budget too.** The event stream today re-sends every box every two seconds with no
deltas, no bounded channel, no lag counter and no connection cap. It scales as boxes × transitions ×
clients and belongs in the table.

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
7. **The first screen has exactly one action**, which requires prerequisite collapsing (§11.5).
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
4. **No displayed state from an edge alone.**
5. **Every signal is keyed by its subject, not its observer.**
6. **Every signal declares its budget, its cadence and its staleness threshold.**
7. **Everything a human might do by hand has a printable recipe.**
8. **A new feature is a composition of primitives — or it adds a primitive deliberately and amends
   this document.**
9. **Trust-boundary capabilities are modules that can be left unbuilt**, and they never reference one
   another.
10. **Approval is written by the approving side, never supplied by the requester.**
11. **Every durable file carries a schema version, and skein refuses a volume it does not understand.**

---

## 13. Testing

- **Every check has a test that it fails when the thing is absent.** A check that passes
  unconditionally is worse than no check: it makes a broken system report as healthy.
- **Every check has a test for `unknown`** — that an unreachable source reports rather than drives.
- **Signal fusion is a scenario matrix**, including every missing-edge case.
- **Anything the UI computes is a pure function tested in node.** The build step (§11.6) is what makes
  this possible.
- **Browser tests run in a box.** Correcting the first draft: this was fixed, and
  `tests/ui/README.md` names the libraries Playwright's own list omits.
- **The warden is built and tested three ways**: empty, each capability alone, both.
- **Screen grammars are verified against a real box**, never a clean-room one — a bare tmux session
  has no configured statusline, a short pane and no scrollback, which hides exactly the defects that
  matter.

---

## 14. Modules

| module | owns | depends on |
|---|---|---|
| `state` | the volume: declared and recorded, schema and locks | — |
| `source` | `enter`, `socket`, `file`, `http`; exec/stream/pty | — |
| `signal` | kinds, freshness, budgets, cadence, gates, fusion | `source` |
| `operation` | desired, tri-state check, recipe, doer, DAG, leases | `state`, `signal` |
| `act` | streaming interactions; emits edges | `source`, `signal` |
| `warden` | client and protocol | `operation` |
| `box` | identity and lifecycle | `state`, `operation`, `act` |
| `server` | cockpit, API, event stream, transitions | everything |
| `cli` | `skein` — a **client of the server**, not a second writer | `server` |

`state` and `source` depend on nothing. `source` never depends on `operation` — reaching a subject
must never require privilege. The CLI is a client rather than a peer, because two processes doing
unsynchronised read-modify-write on the same declared state is today's silent last-write-wins.

---

## 15. Open

- **Per-box subuid isolation** (§9.5.1, second step). Files are covered; control is not (§9.2).
- **Whether cross-box agent messaging stays on by default.** It is seeded to `accept` in every box,
  and it is the channel that carries control between boxes.
- **The agent-credential proxy** (§9.4). Unbuilt, and the only real defence for the credential that
  matters most.
- **Multiple fleets on one host.** The volume makes it clean; the cockpit port and the warden's
  addressing both assume one.
- **API authentication in-fleet.** Today it is one shared bearer token, and its own comment says
  *"not a login — one shared secret"*. It exists because a box reached the host cockpit. In-fleet it
  matters more, and §9.3.1 changes its shape rather than answering it.
