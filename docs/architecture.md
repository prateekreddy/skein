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
| **subject** | what it is about. **Keyed by subject, never by observer.** |
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

**A signal's value is four-valued, not two**: `value | stale | unreadable | unsupported`.
"Could not observe" is not "observed absent", and collapsing them is what drives spurious action.

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
| the review queue | signals whose Source is `http` rather than `enter`/`socket` |
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

The first draft asserted that a namespace escape was "the only way through". That is false, and the
correction is the most important change in this revision.

### 9.1 What a box actually shares

Verified in `src/box-session.sh:1206-1214`. A box is isolated by **two** namespaces — mount and user.
It **shares** with every other box and with anything else in the sandbox:

- **network** — no `--unshare-net`. Any localhost port in the sandbox is reachable from every box.
- **PID** — no `--unshare-pid`, and deliberately: *"the pid recorded below has to be the pid skein
  sees from outside, or nsenter has nothing to anchor on."*
- **IPC, UTS, cgroup**, and `/sys/fs/cgroup` is writable.
- **uid** — every box is uid 1000, as is everything else.

And one thing that is not a hole but an addressing scheme: **a box's tmux socket sits outside its
private mounts by design**, so skein can list, attach and kill without entering. Since every box sees
`/boxes/*` read-write as uid 1000, **any box can run code in any other box** via that socket. The
launcher says so plainly: *"Other boxes here are not a security boundary."*

> **The security boundary is the fleet sandbox. Boxes are isolated from each other's files, not from
> each other.**

### 9.2 What moving skein inside costs

Stated because it must not be discovered later. Relative to host-side skein, co-residence gives a box:

- the ability to **signal or kill skein** (shared PID namespace, same uid)
- **network reach to skein's own API** (shared netns) — a mount boundary protects files, not ports
- **reach to the warden over the gateway**, indistinguishable from skein by address or uid
- **credentials inside the blast radius**, since they move onto the volume

### 9.3 What hardening co-residence requires

Chosen over moving skein back out, because that would give back the reason for doing this. These are
requirements, and each is a project rather than a paragraph:

1. **skein's control API is a root-owned filesystem socket, never a TCP port**, and the cockpit's
   HTTP auth token is a root-owned file. Boxes may still *connect* to the cockpit port — shared netns
   makes that unavoidable — but cannot authenticate. That mechanism already exists precisely because
   a box once reached the host cockpit.
2. **skein runs as root inside the fleet sandbox; boxes remain unprivileged.** This is the
   privilege split, and it is done **first** — everything else here depends on it.

   Root *in the sandbox* is bounded by the VM: it is not root on the host, and boxes still cannot
   sudo. It closes signal/kill (an unprivileged process cannot signal root), gives skein files
   boxes cannot read regardless of mount view, and puts the cgroup plane out of reach (requirement
   4) at no extra cost.

   It is chosen over per-box uid mapping because **the attach mechanism survives unchanged**. That
   was the blocker: the tmux socket is `0700 uid 1000`, so skein on a neighbouring uid could not
   open it, and the launcher had already rejected per-box uids for exactly that measured reason.
   Root can open it, and root in the initial user namespace has CAP_SYS_ADMIN over descendant
   namespaces, so `nsenter` needs no `newuidmap` — which is not installed, though `/etc/subuid` is
   configured.

   The cost, stated: a skein bug reachable from a box yields root in the sandbox rather than uid
   1000. Small, because a skein compromise is already total — it holds the tokens and drives every
   box.

   **Per-box subuid mapping is the second step, not the first.** It is strictly stronger, because it
   isolates boxes *from each other* by uid rather than only from skein — but it needs the `uidmap`
   package in the fleet image, an `unshare` + `newuidmap` sequence before bwrap (unprivileged bwrap
   can map only its own uid), and it does not close the tmux socket path on its own.
3. **The warden authenticates a caller skein can prove to be and a box cannot.** Requirement 2 is its
   prerequisite and supplies it: a secret at `0600 root` inside the sandbox.
4. **The cgroup control plane is outside every box's write reach.** Today a fleet-scoped box can
   raise `memory.max`, defeating the ceilings §10 depends on.
5. **The audit log is append-only and written by the approving side.**

### 9.4 The agent's own credential cannot be scoped

GitHub tokens can be scoped per repo, short-lived, and revoked — that part is real and well built.

The agent's Claude/Codex OAuth login is different: it must be **inside the box** for the agent to run
at all, it is seeded down into every box and flows back up on refresh, and it is the credential a
rogue agent most wants. There is no scoping primitive for it from any provider.

So it is carved out explicitly rather than covered by a claim that does not hold: **the agent login is
fleet-shared and unscopable.** The only real defence is a proxy that injects it outside the box's
reach, and that is unbuilt. Saying "boxes get scoped short-lived tokens" without this carve-out is an
obstacle sold as a property.

---

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
| **review** — read a diff, comment back | often | diff and comment in one place |
| **recover** — something broke | rare, high-stakes | the failing check and the command that fixes it |
| **set up** — add a repo, make a box | rare | one action, no configuration exercise |

**Attention is the scarce resource, not screen space.**

### 11.1 One queue, many sources

The board is a **queue, not a dashboard**, ranked by who needs you; a box that needs nothing is
recessive. Because "needs you" is a predicate over signals and the review queue is a signal source, a
pull request awaiting review belongs in the same queue as a box awaiting an answer — they are the
same thing to the user, and separate today only because they were built separately.

**The queue groups by repo, and grouping is not cosmetic.** Today's board has collapsible per-repo
sections with counts, persisted collapse state, and per-group pull and new-box actions. At two repos
that is decoration; at eight it *is* the board. A flat three-section list is a different product, and
the first draft's mock-up quietly chose it.

### 11.2 Three states most tools botch

- **Nothing needs you.** Say so, plainly and calmly. A dashboard that looks the same whether or not
  anything is wrong has failed at its only job.
- **You were away.** Continuity across absence is a requirement, not a nicety, and it needs
  server-side transitions (§3) rather than a client-side delta on tab focus.
- **Setup is incomplete.** Failing checks at the top of the same queue — but see §11.5, because the
  cockpit is not where a new user starts.

### 11.3 The laws

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

### 11.4 The blocked state is three components, not one

Onboarding needs *sequence and prerequisites*; breakage needs *what changed and when it last passed*;
approval needs *what will happen, who asked, and a timeout* — and lives on the host (§8.1). They share
a visual language and two primitives (the recipe block, the live check pip). They are not one card
with eleven optional props.

### 11.5 First run is the CLI

The blocked state renders in the cockpit; the cockpit needs the fleet created and the port published
— privileged operations. So **the first-run surface is `skein doctor` in a terminal**, and check cards
must render as text. Law 6 is preserved: it is still the blocked state, just not in a browser.

Because operations form a DAG (§2.4), **a failing prerequisite collapses its dependents**: a missing
fleet shows one card, not five, which is what makes law 7 achievable.

### 11.6 Identity and build

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

- **Per-box subuid isolation** (§9.3.2, second step). Boxes are still isolated from each other only
  by mount namespace and convention; the tmux socket remains a cross-box code path until this lands.
- **The agent-credential proxy** (§9.4). Unbuilt, and the only real defence for the credential that
  matters most.
- **Multiple fleets on one host.** The volume makes it clean; the cockpit port and the warden's
  addressing both assume one.
- **API authentication in-fleet.** Today it is one shared bearer token, and its own comment says
  *"not a login — one shared secret"*. It exists because a box reached the host cockpit. In-fleet it
  matters more, and §9.3.1 changes its shape rather than answering it.
