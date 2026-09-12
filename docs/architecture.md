# skein — architecture

The destination design. Revised 2026-08-20 against three independent reviews (architecture,
security, delivery), each of which found real defects in the first draft. Where a claim below
contradicts something skein does today, it is because the current behaviour was checked and found
to be the better answer — not because it was overlooked.

Companion documents: `docs/delivery.md` (sequence, migration, landmines) and `docs/parity.md` (the
audited capability inventory). The root `ARCHITECTURE.md` no longer describes anything: it was
retired to a signpost pointing here (SKEIN-222), after the version that described ratatui, Svelte and
per-box microVM kernels had to be disclaimed in `CLAUDE.md`.

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
| **subject** | what it is about — a **box, a module, a pull request, the fleet, or the machine**. **Keyed by subject, never by observer.** |
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

The third case had no disclosure at all until it was given one, and its absence is instructive: the
other two are visible *because the observer is broken*, so the screen-health flag names them for
free. A newer edge wins with the observer perfectly healthy — the flag is empty precisely because
the screen is being read — so the row showed an edge-derived state and said nothing anywhere.
`fuse_status` returns a provenance of its own now (`StatusFrom::EdgeAheadOfScreen`) and the row
carries an `unconfirmed` caveat, which clears when the next sample confirms or corrects it. It is
not a fault: showing the event at once is the whole point of the rule. It is a state nothing had
yet seen.

And the keying rule is its
own bug: turn state keyed by box but written by session let any helper process overwrite the agent's
state, producing seventeen spurious `ended` events in 114 seconds.

**The machine is a fifth subject, and it is not "fleet" stretched.** "What fleets are on this machine"
is a question somebody running more than one asks, and it cannot be about *a* fleet: the answer
includes fleets this skein does not own and could not reach. The other four are all things skein
manages; this one is the ground they stand on. Nothing on the board carries it — a machine's other
fleets are not the board's business, which is why the sandbox listing left the tick — and it is named
because a subject a signal can have and the type cannot express is one that arrives as an
unclassified call.

The subject being open is what lets one mechanism carry things that otherwise need bespoke features:
a module's standing note and a diff's contract signals are signals *about a module*, and a review
request is a signal *about a pull request*. Assuming the subject was always a box is what made those
look like separate machinery.

**`source` and "which copy" are two axes, and one word for both was a bug waiting to happen.** A
signal reached by `enter` and one reached by `socket` are both *the box*; a value read from the store
and one read from the host's clone are both reached by `file`. Neither determines the other, so
`Answer` carries both — `reach` for §2.3's Source and `vantage` for which copy of the fact it is.
They were both called `source` until they weren't, and a reader who knew this section read the wrong
one.

**`sbx exec` is not a Source, and §2.3 is not short one.** The question was real: three of the
board's signals were reached by `sbx exec <sandbox>`, which reaches the *sandbox* and not a box, and
§2.3 names nothing for that. Rule 2 says a thing that cannot be composed from the primitives means
the primitive set is wrong — so it had to be settled rather than assumed. It is settled by §13a,
which already puts "every `sbx exec` path" and "sandbox listing as the truth about boxes" on the
delete list and says they survive only until skein moves into the fleet. `sbx exec` is the
**transport** around a reach. What a signal declares is what the script inside it touches, which is
the same answer before and after the move: the liveness sweep is `file` and `socket` today, and
`file` and `socket` when the shell is gone.

The one signal that names no Source is the sandbox listing, and it is the one §13a deletes — nothing
in §2.3 reaches a sandbox *manager*, and §8.3 has already decided what replaces it (a `Source: http`
call to the warden). It is not on the board, and a test requires every signal that *is* to name one.

**A signal declares Sources, plural.** The liveness sweep reads `/proc` for every anchored box and
falls back to each undecided box's socket, so which one answered is per box and per tick. This
section says "which Source produced it", singular — which is right for an *observation* and wrong for
a *declaration*. The one that answered belongs beside `observed_at`; the set a signal may use belongs
in its definition.

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

**This exists in the code now** (`src/operation.rs`, SKEIN-574), with the three qualifications that
make the pattern safe rather than merely tidy and nothing else: `Check` is three-valued, the doer is
`Option<Doer>`, and `Operation::may_drive` refuses on `unknown`, on `destructive`, and on the absent
doer — all three from the value alone.

**The optional doer is not decoration, and the second operation is what proved it** (SKEIN-576).
Publishing the cockpit's port is *idempotent*, so the class does not withhold it, and on a host the
check answers `unsatisfied` rather than `unknown`, so the check does not either. What withholds it is
that no doer exists: `warden_client::Act::Publish` deliberately has none, because §9.4 makes opening
a hole a different act from closing one. Without the third refusal `may_drive` grants permission for
an act nothing can perform, and the caller then has to invent a performer — which is `sbx`, the
fallback `docs/delivery.md` says must not exist "because that fallback would be taken on exactly the
day something was wrong". `Doer` names who may act rather than carrying a closure; the performing
stays in `warden_client::perform`, where the approval and the audit are.

The lease, `requires` and a registry remain deliberately absent: `crate::attempt` already holds the
lease machinery for the one operation that needs one (`ensure_fleet`'s create), and a list nothing
iterates is a second place to keep in step.

### 2.5 Act — a non-idempotent interaction

Sending a message to an agent. Answering its question. Interrupting a turn. Uploading a file.
Attaching a terminal. Taking a box over onto another runtime.

These have no `desired` and no `check`. They are streaming, unacknowledged, and doing them twice is
doing them twice. Forcing them into Operation makes "`ensure`, never `do`" a lie; leaving them
unnamed makes them grow *beside* the primitives, which is the debt §12.8 exists to prevent.

**An Act emits an edge signal as a side effect** — skein knows it delivered the keystroke, and that
knowledge is what makes an optimistic state clear correct.

**An Act outlives its watchers, and that is what makes it a primitive rather than a stream.** Box
creation was the case that proved it: it lived inside the terminal WebSocket that watched it, so a
surface that never opened a terminal could not create a box at all, and a browser that reloaded
mid-create reconnected to a closed terminal and found nothing. (`fleet::remember_start_failure`
exists because of exactly that, and keeps the *reason*; an Act keeps the transcript.)

So: begin it, watch it, poll it, or come back after its stream has closed. Many watchers, one run —
§10.1's "one producer, fanned out", and the same lesson as check-then-act giving every browser tab
its own subprocess. A watcher that falls behind is **told** rather than silently skipped. The
transcript is bounded and says where it dropped, because an act that prints a gigabyte is a build
with a broken progress bar and a cockpit that dies of it is worse than one showing the last megabyte.

And the guard cannot be "check whether it is needed", because an Act has no check: **an id already
running is refused**, and the caller is handed the one that is running.

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

**A repo is a remote, and a path is not one.** Adding one clones a bare mirror onto the volume; a box
clones its checkout from the mirror onto VM-local disk. An earlier revision of this section said the
opposite of that first sentence — *"a local filesystem path is a valid remote, so a repo with no
server anywhere still works: skein fetches from your path"* — which is a fact about git and not one
about skein. `registrable_source` (`src/repos.rs:855`) requires a scheme and accepts `https://`,
`http://`, `ssh://` and `git@host:` only; `add_repo` refuses everything else before it clones
anything (`src/repos.rs:1512`).

What is lost, precisely: **a repo with no server anywhere cannot be registered at all**, and with it
goes the visibility of uncommitted work that adopting a checkout in place used to give. You commit
and push, and skein fetches from the remote.

Two consequences to state rather than discover:

- **In-fleet skein cannot reach host paths, which is why the refusal is right and not merely
  convenient.** skein runs inside the fleet sandbox; a path-registered repo would have nothing to
  fetch from there and would differ from a URL repo in nothing a box could observe. A local-path
  remote would therefore have been host-driven only — a real asymmetry between the two deployments —
  and refusing the path removes the asymmetry instead of documenting it.
- **Three host-side features read the working checkout directly** — `diff`, `moduledocs`,
  `codeowners`. They repoint at the mirror. That is a refactor, not a deletion, and it is budgeted
  in `docs/delivery.md`. Done, through `repos::Tree` (`src/repos.rs:1191`), and one thing had to be
  separated to do it: the files a repo keeps **out of git** are not in any mirror, so what
  `shared-paths.txt` names is surfaced into a box out of the store's own `shared-rw/` by
  `sandbox-bootstrap.sh` rather than read off a checkout. Nothing on the host seeds that directory
  any more — the two calls that did went with local-path repos (`src/fleet.rs:1568`).

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

`sbx` is the substrate and is named deliberately, because its quirks are load-bearing: **no verb
adds a mount to an existing sandbox and none resizes one** (`sbx --help`; `cp` copies *into* a
sandbox, it does not mount), so the mount set and the resource numbers are fixed at create and
changing either destroys the sandbox; and `sbx create` prompts before mounting host directories,
which is why fleet creation carries a microVM-sized budget rather than an action timeout.

**Ports are the exception, and this document used to get it backwards.** Earlier revisions asserted
*"there is no unpublish"* and reasoned from it — a mapping permanent, every publish a one-way bet.
`sbx ports --help` takes `--unpublish`; a mis-aimed host mapping is recoverable. The claim was
quoted from a nine-verb list recalled from memory, which is also short (`sbx --help`). Where the old
claim did work below, the work has been redone from what is actually true, and §9.4's port-squat
argument survives it — see there for why.

### 7.2 Sandbox root — used constantly, in normal operation

`grep -c "sudo " src/box-session.sh` → **27**; tree-wide (`grep -rn "sudo " src/ | wc -l`,
`grep -rc "sudo " src/ | grep -v ':0$'`) **85 lines across 11 files**, measured 2026-09-06 — and
those greps count comments as well as calls, which `docs/inventory.md` §1.2 breaks down. Four kinds:

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
caller.** Real resize is `sudo tar` of the whole box tree and `tar -xf` back (`archive_script` and
`restore_script` in `src/fleet.rs`), which is why it demands 1.2× the box size free before starting.
`box_archive`'s doc comment records the move away from reconstruction: *"the reconstruction is
slower, less faithful, and it is where the fragility lives."* (Cited by name, not by line: the line
number here drifted twice, by thousands of lines each time, before anybody noticed — and parity's
own rule is that a citation nobody can follow reads the same as a capability that vanished.
`grep -n 'fn box_archive' src/fleet.rs`.)

An earlier draft called resize "a composition carrying a small delta … what today's snapshot already
does". It prescribed a regression and described it as the status quo. **Resize starts from the byte
copy.** It is destroy + create around a `tar`, it is `destructive` (§2.4), and it needs a doer that
can outlive skein (§7.5).

Also unstated before: the fleet's **mount set is a function of registered repos**, so adding a repo
after create makes its store unreachable and skein's own remedy is a rebuild. That is a third trigger
for resize wearing another name — and it disappears once repos are clones on the volume (§6), which
is a reason for §6 beyond the ones already given.

### 7.4 Port publishing folds into create — conditionally

Only because the cockpit is the sole port. Publishing was a **recurring, self-healing host
operation**, re-run from `heal_fleet` on every server start, and it was built in that shape on the
belief that sbx cannot unpublish, so a failed attempt was permanent — `sbx ports --help` takes
`--unpublish`, so it is not, and a wrong guess is withdrawable rather than burned. The agent's half
of that loop is gone with the agent (§13a); the cockpit's mapping is what is left.
Host-side skein escapes the loop entirely by binding loopback.

**If skein ever needs a second port in-fleet, it inherits that machinery whole** — the healing loop,
the backoff and the candidate-port search. What it no longer inherits is a cannot-withdraw trap. The
fold is a consequence of the one-port decision, not an independent simplification.

### 7.5 The two operations that terminate their own reconciler

Create and destroy both kill skein — create because it does not exist yet, destroy because it will
not afterwards. So **fleet lifecycle cannot live inside the fleet**, permanently. That is the
warden's reason to exist (§8), and it is a boundary rather than a limitation.

**This is about where the *doer* runs, and never about who may ask** (SKEIN-576). In-fleet skein
asks the warden over `http` — §2.3 already lists that Source as reaching "GitHub, and the warden" —
and the warden performs on the host with its own approval. Reading this paragraph as "in-fleet skein
must refuse" put the create inside `ensure_fleet`, where it was a *side effect of starting a box*
and, in-fleet, unreachable: `ensure_fleet` asks about the fleet the process is inside, a question
that answers itself. Creating a fleet is an explicit act a person initiates from the cockpit
(`fleet::request_fleet_create`), and with no warden reachable it refuses and prints the line rather
than falling back to `sbx` — `docs/delivery.md` step 3 says why that fallback must not exist.

Package approval is different and must not be confused with it: it needs sandbox root, not host
privilege, so the warden is not involved — but the *authority* question is real, and §8.4 answers it.

## 8. The host warden

A small host service owning fleet create and destroy. **Three removable doers, and two endpoints that are not removable** (§8.3).

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

**The surface is `/dev/tty`, and that choice carries the test for whether anyone is there.** Not
stdin: `/dev/tty` reaches the person's terminal even when the warden's output is redirected to a log,
and — the half that matters — it *fails to open when there is no terminal*. A warden started by a
supervisor therefore has no approval surface, refuses every doer, and says so at startup. There is
deliberately no configuration that turns that into a yes.

**Approving is typing the operation id, not `y`.** It makes "what you see is what will run" literal,
because answering requires reading the line; it stops a person clearing a queue of prompts approving
the wrong one by rhythm; and it is the only confirmation that cannot be given by a keystroke already
in the buffer. §8.5's flooding is a real risk against a surface where the answer is one character.

**And the request has nowhere to put an approval, or a description.** The wire struct refuses unknown
fields rather than ignoring them — serde's default would make `{"approved": true}` a field silently
dropped, which is the same outcome as one that does not exist and a very different message. A
requester who sends it has misunderstood the boundary and is told so.

**What this does not buy, before the uid split.** Any process running as the same uid can reach the
warden's file descriptors. Today skein runs as that uid, so today's guarantee is against a *box* —
not against a compromised skein on the same machine. §9.5.1 is what closes the rest, and it is
delivery step 4b. This is the strongest form available before it, and it is strictly stronger than a
flag on a request.

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

**Three doers and two reporters, and only the doers are removable.** The doers are `create`,
`destroy` and `unpublish` — one Cargo feature each — and the default build ships all three
(`warden/Cargo.toml`, `warden/src/capability.rs`). Written as a shape rather than a total for §13's
reason. The total had already been unified once — `48d2f7c1`, *"one warden count, everywhere"*,
2026-08-20 — and nine days later `6256aba7` added `unpublish` and made every copy of it wrong at the
same instant. No gate catches that: `tools/prose-check.py` fails on a symbol the code does not have,
and every symbol here is real; it is the number that rotted (SKEIN-607).

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

§8.1's rule needs its sharper half, because the code had the weaker one and a live defect to show
for it. **The defect below is closed** — the "Done" further down names the commits; it is written
out because the rule is only legible next to the thing it forbids.

`substrate::install` re-read the whole request at install time and checked `state` and the *shape* of
the package names — but `packages` came from that same re-read, and the comment above it said the
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

**And the rule as stated is necessary but not sufficient.** `gitgate::decide` *already* implemented it
— it never trusted a `state` field and wrote a `Grant` to a host-side file the refresher alone reads —
and it was exploitable, because the artifact was written from a **re-read by id after the human
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

**Done** — `2dfb32c` (packages) and `3a462f2` (git write). No digest was needed, because nothing
re-opens the file: the cockpit sends back the fields it rendered, and the decision is made on those.
`install` reads a host-side artifact under `~/.skein/substrate/` and the grant refresher reads the
host-side grant; neither consults the queue. `substrate::decided_over` makes the host's decision win
over the box's copy of it, so a box that rewrites its request after approval changes neither what is
shown nor what runs.

Four ways to implement that and still be wrong, all four closed:

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

And the artifact should carry the **resolved argv**, not a name list: if the actor re-splices names
into a command, the splice is still the trust boundary.

**This one is deliberately not implemented literally, and the divergence is recorded rather than
glossed.** The artifact carries the kind and the names. The splice happens host-side over host-owned
bytes, each name `sh_quote`d and re-checked by `Request::problem` immediately before it reaches the
command line — so the remaining exposure is a hand-edited artifact, which that check covers. A
resolved argv was rejected because the *manifest replay* needs names: a rebuilt sandbox reinstalls
from the manifest, and an argv frozen at approval time would replay a command rather than a
dependency.

**One more the shape check missed, now fixed.** The package-name whitelist admitted `/`, and both
`apt-get install ./x.deb` and `npm install -g /path/pkg` install a *local* package and run its
maintainer or lifecycle scripts as root — then persist into the replayed manifest, so one approval
becomes root execution at every future launch. The whitelist's own comment claimed the rejected set
covered "the only two that matter"; there was a third. It is per-kind now, and `/` is admitted only
as an npm scope — with the scope required to *be* a scope, since "at most one slash" admits `@a/../b`.

**And the deeper point is now on the card rather than left to be inferred.** *Any* approved apt or npm
package is arbitrary root code by design — maintainer and lifecycle scripts run as root, fleet-wide,
and a remembered one runs them again at every launch. The whitelist is an argv-injection guard and
never a privilege guard. Package approval is a grant of root code execution at fleet scope,
permanently, and the cockpit says so next to the button.

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

A compromised skein controls *what* is proposed and *when*. Every individual operation is confirmed
by a person, so the attack that survives is **volume**: enough prompts that somebody approves the
wrong one, or so many that the real one is buried. One outstanding request at a time, a rate limit,
and the timeout §11.5 already names.

**A second request is refused and told which operation is in front of the person** — not queued and
not dropped. Queueing is the flood with extra steps; dropping silently leaves a caller unable to tell
"busy" from "lost", which is the ambiguity §8.2 exists to remove. The slot is a guard that releases
on every path out, a panic included: forgetting one turns "one at a time" into "one, ever", and it
would look like a warden that had simply stopped answering.

**The rate limit counts arrivals, not approvals.** A flood of *refused* proposals is the attack, so
counting only the ones that got through would count nothing while it was happening.

**The reading endpoints are exempt, and this is the part worth getting right.** Fleet observation is
the check that gates skein's own first run (§8.3). Rate-limiting it into unavailability would let a
flood achieve by refusal what it could not achieve by approval — skein unable to start at all. The
audit sink is exempt for the mirror reason: the account of what just happened has to survive the
thing it is accounting for.

**The timeout is the approval surface's, not the doorway's**, and it is not built. A prompt nobody
answers holds the one outstanding slot for ever, which is the same denial by another route; the seat
lock in the console approver is where it belongs.

### 8.6 Where it listens, and what it is spoken to in

Left open by every earlier draft; decided here, and both halves have a reason rather than a
preference.

**Loopback, and the host's own address on each Docker bridge. Never `0.0.0.0`.** The owner decided
that when the move made it live; §9.5 R5 records the decision and `warden::serve::bind` implements it.

**The paragraph this replaces was wrong, and the way it was wrong is the point.** It said: a box
reaches the host through the gateway address rather than through `127.0.0.1`, so a loopback listener
answers host processes and nothing inside the sandbox — therefore bind narrow and let step 4 open it
deliberately. The first half is true of a **Linux** host and false of **Docker Desktop**, which
proxies the gateway address from the host side, so the connection arrives at the host's own loopback.
Measured, from inside the fleet sandbox against a warden bound to `Ipv4Addr::LOCALHOST` on a macOS
host: `host.docker.internal` resolves to `169.254.1.1`, port 7879 answers, and what it answers is
this warden's own 401. The barrier the sentence described was never there on the machine skein was
being moved onto — and it had been copied into `warden/src/serve.rs`, `src/health.rs` and the
Sources list before anybody opened a socket to check. §1's rule is *derive, do not assert*, and a
claim about what a kernel does is exactly the kind that reproduces or drifts.

**So why widen at all.** Because the claim is right where it was always right. On a Linux host the
gateway is a real bridge address, a loopback listener genuinely answers nothing inside the sandbox,
and fleet create and destroy would have no path at all. Binding the bridge gives that host what
Docker Desktop hands this one for free, and the derivation finds nothing on a host that has no
bridge — so one implementation is correct on both.

**Why not `0.0.0.0`.** It is one line, it works everywhere, and it puts port 7879 on whatever network
the laptop is attached to, with the shared secret as the only thing between a café and `sbx rm -f`.
The bridge is reachable from the sandbox and from nowhere else. The exposure §9.4 names — "reach to
the warden over the gateway, indistinguishable from skein by address or uid" — is the one this
deliberately accepts, and R5's secret is what pays for it: it is checked before anything is routed,
so reaching the port and being skein are different things.

**And the bridge is found rather than guessed.** `/proc/net/route` says which subnets belong to an
interface named `docker0` or `br-*`; `/proc/net/fib_trie` says which addresses are the host's own;
the answer is the intersection. The shortcut — "the bridge is the `.1` of its subnet" — is true of
every Docker install anybody has seen and is still a guess. A subnet rule instead of a name rule
would be worse than a guess: `172.16.0.0/12` is a range a corporate VPN hands out too, so the bind
would widen onto somebody's office network the day their VPN changed, which is the outcome choosing
the bridge over `0.0.0.0` exists to avoid.

**And the secret now exists** (`warden/src/secret.rs`, §9.5 R5). It is checked before anything is
routed, so the two reporting endpoints are not readable by whoever can open the port and §8.5's
doorway cannot be spent by a caller who was never going to be approved. It proves possession of a
file and nothing more — **a doer still runs because a person at the host said so** (§8.1), and the
narrow bind still stands beside it. What it buys is that the bind can widen at 4c without the
exposure above arriving with it. A warden that cannot read its own copy refuses everything and says
so, because the alternative is "no secret" quietly meaning "no checking" — a failure that would
arrive by a file being deleted rather than by anybody deciding anything.

**A hand-written HTTP subset, and this is the one place that trade is made.** skein's rule is
"compose, don't reinvent — standard wheels only", and it is right for skein. The warden is the
exception because of what it is *for*: it exists to be the thing a compromised skein has to get past,
so its dependency list is part of its argument, and `axum` would bring tokio, hyper, tower and their
tree into the one process on the host that runs privileged commands. What is needed is one method,
one path, a `Content-Length` body under a cap, from one client, on an address only a sandbox on this
machine can reach.

The subset is strict, and each restriction removes a class of bug rather than a feature: **one
request per connection** (no keep-alive, no pipelining — which makes request smuggling impossible by
construction rather than by two length rules agreeing), **`Content-Length` only** (`Transfer-Encoding`
refused outright), **a duplicate `Content-Length` refused** rather than first-or-last winning, and
caps on the request line, the headers and the body, all read through a bounded reader.

**A doer that was not built answers 404, not 403.** "This warden cannot" and "this warden will not"
are different facts, and a client told the wrong one retries the wrong thing.

**`state` and `ok` are two fields on every reply**, because they are two questions: `state` is what
the *warden* did — ran it, answered from the record, cannot say — and `ok` is what the *operation*
did. Collapsing them makes a replayed failure indistinguishable from a fresh one, which is the whole
of §8.2 lost in a rendering decision.

## 9. The trust model

Rewritten twice. The first draft asserted a namespace escape was "the only way through", which was
false. The second draft over-corrected: it named the tmux socket as a box-to-box code path, and the
source had already closed that. Both errors came from reading the `bwrap` exec without reading the
isolation block eighty lines above it.

### 9.1 What a box actually shares

A box is isolated by **two** namespaces — mount and user (`grep -n 'user namespace'
src/box-session.sh`, and the `exec bwrap` at the foot of the file). It **shares** with everything
else in the sandbox:

- **network** — no `--unshare-net`. Any port bound in the sandbox is reachable from every box.
- **PID** — no `--unshare-pid`, deliberately: *"the pid recorded below has to be the pid skein sees
  from outside, or nsenter has nothing to address."*
- **IPC, UTS, cgroup**, and **uid** — every box is uid 1000.

**Files, however, are covered** (`grep -n 'tmpfs "\$fleet_root_dir"\|tmpfs "\$state_parent"'
src/box-session.sh`, and asserted by `tests/git_write_request.rs`). A `--tmpfs` goes over the fleet root and over the box-state parent,
then only *this* box's root and state are bound back, with `.skein` read-only. So one box cannot read
another's checkout, conversation or tokens — and cannot reach another's tmux socket, which lives
under the covered root.

**And there was a third path, out of the sandbox entirely, that the file cover did not reach.** It is
the reason §9.5.2's cover is derived per box rather than listed, and both halves of it have since
moved — so what it says now is narrower than what it said, and the narrowing is the point.

What is mounted was the first half, and this document had it wrong. `fleet_mounts()` mounts
`~/.skein/repos`, the box-state parent, and **every repo's `store` and nothing else** — the loop is
literally `for path in [repo.store.clone()]` (`src/fleet.rs:1573`). It used to mount the host's own
working checkout as well, for a repo adopted in place; there are no such repos (§6), and the code
says in as many words that the tree its user works in "is not in the sandbox at all", which is
stronger than the read-only bind it replaced.

The cover was the second half, and it is built. The launcher is *given* the mount set, tmpfses every
path in it (`src/box-session.sh:1441`) and binds back only the one store this box is entitled to
(`src/box-session.sh:1449`) — the inversion §9.5.2 asks for, not an enumeration. So:

- **across repos, the file boundary holds for a covered box.** Another repo's store, launch specs
  and status are under a tmpfs. It does not hold for an **uncovered** one: a launcher already
  installed in a running sandbox predates the mount set and passes none, and that is deliberately
  read as "no cover" rather than "cover with nothing bound back", which would take every box's store
  away (`src/box-session.sh:1416`). A fleet that has not had its boxes restarted onto a current
  launcher is still in the old state.
- **the box → host code-execution path has lost both of its named instances, and its shape
  survives.** The two host-side git calls this section cited ran against a repo's *working checkout*
  — module notes and a repo pull. Neither exists: the module notes, the diff and CODEOWNERS read the
  mirror through `repos::Tree` (`src/repos.rs:1191`), and `pull_repo` fetches the mirror and does
  nothing else (`src/repos.rs:1603`). What has not changed is that skein still runs git **on the
  host** against a tree inside `~/.skein/repos` — the mirror, via `fetch_mirror` — so a box that
  could write that mirror's `config` would still get execution as the host user at the next fetch.
  The cover above is what stops it, which means the cover is load-bearing for more than file
  confidentiality and must not be weakened on the grounds that only stores are mounted now.

**That cover list is an enumeration, and it must grow with every new shared path.** It covers exactly
two parents today. It did not cover `/run` at all when this was written, and the launcher itself
recorded that `/run/user/1000/cc-socks/` was box-visible; R11 covers `/run` now and binds that one
directory back on purpose, which is a different thing from never having reached it.

### 9.2 The three real box-to-box paths

Named because §9.1's cover makes the *file* axis safe and it is easy to stop there.

**1 — Shared writable toolchains.** `share_paths=(".local" ".cargo" ".rustup" ".npm")`
(`grep -n 'share_paths=' src/box-session.sh`) are bound read-write from the sandbox's real `$HOME`
into every box, so
boxes share one toolchain and one build cache. **`~/.local/bin/claude` is the agent binary every
other box executes on next start.** Any box can overwrite it. This is stronger than any socket path:
it is persistent, it survives restarts, and it needs no live target.

> **No shared writable path may contain anything another box executes.**

**2 — Cross-box agent messaging, by design — and kept.** `~/.claude/sessions` is deliberately shared,
the inbox sockets in the sandbox-wide `/run/user/1000/cc-socks/` are deliberately bound back through
the `/run` cover (§9.5 R11), and every box's settings are seeded with `crossSessionInbound: "accept"`
so messages are delivered rather than held for approval. Every box is addressable by name. So any box
can drive any other box's agent with text of its choosing.

Decided: this **stays on**, because it is a real feature and boxes are already one trust domain — the
change is that the architecture states it rather than implying files are the only axis of separation.
**Control flows between boxes even though files do not.**

Both halves are one switch, per repo, defaulting on, and R11 carries the whole of it. The two were
briefly not one switch: the registry was shared here while the cover in R11 took the sockets away,
and for months this paragraph described a channel the fleet did not have — every box advertising an
inbox nothing could reach, and messages silently detouring through Anthropic's servers.

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
- **port squatting, the reverse direction.** **Shared netns is the whole of it, and it is enough.**
  This used to be argued as "shared netns plus no-unpublish"; `sbx ports --help` takes `--unpublish`
  (§7.4), and the argument does not need it. Unpublish acts on the *host* end — it can withdraw a
  mapping aimed at the wrong thing. The squat happens at the *sandbox* end, inside the shared
  namespace, where a box binds the cockpit port before skein does and the mapping then points at the
  box. Nothing on the host side can un-bind that. So **a box that binds the cockpit port before
  skein starts becomes the cockpit**, and the browser hands it the token on the first request. A distinct uid stops SO_REUSEPORT theft
  from a live listener; it does not stop an empty port at sandbox start.

  **The sharpest of the five**, because §9.5 R3 decided to keep the TCP port: the filesystem socket
  would have closed it as a side effect, and nothing else here does. The answer is that the port is
  never free — opened once before any box exists and inherited across restarts, rather than re-bound
  by whoever gets there first.

  **Closed, in-fleet** (SKEIN-77 and SKEIN-105). skein-server takes a listening socket it was handed
  over the `LISTEN_FDS`/`LISTEN_PID` convention and serves on it, and refuses a descriptor that
  cannot be a door — not a socket, or the wrong end of a connection — rather than entering an accept
  loop that fails forever and cannot tell that from `EMFILE` (`src/doorway.rs`).
  `SKEIN_LISTEN_INHERITED_ONLY=1` makes a *missing* descriptor a startup failure instead of a bind,
  because the two deployments want opposite answers there and the difference has to be said:
  host-driven, nobody upstream can open a socket and binding is the only way to start; in-fleet, a
  missing one means the start sequence did not do its job, and binding anyway runs this race from
  the one process that was meant to close it.

  The other end is `src/server-doorway.py`, and **where it runs is the whole point**: `ensure_fleet`
  opens it at fleet *create*, before the launcher every box needs is installed, so there is no
  interval in which a box and a free cockpit port coexist. It then holds the listener for as long as
  it lives — with no server behind it until one is installed — and hands the same descriptor to
  every skein-server it starts.

  Four things keep the port from ever being free again, and each was a way it became free:

  * a **server** restart is a fork behind a socket the doorway never let go of;
  * a **doorway** restart is an `exec` (`SIGUSR1`) that carries descriptor 3 across, so
    an upgrade replaces the server on a live fleet with the listener never closed — it used to stop and
    start, which is this race run by the process that exists to close it;
  * a doorway that is **killed** takes its server with it (`PR_SET_PDEATHSIG`) — otherwise the
    orphan holds the inherited listener and nothing can ever re-bind — and its supervisor re-runs it
    at once rather than after a fixed delay, measured at ~20ms against the 2s that was there;
  * and the host mapping is **published only to the doorway**, judged by the pid it stamps rather
    than by a TCP connect, because a squatter accepts a connect exactly as the doorway does. (That
    the mapping could be withdrawn afterwards with `sbx ports --unpublish` is no help: by then the
    browser has already been handed the token.)

  What is left is stated rather than claimed away: a *first* start into a sandbox that already has
  something on the port refuses and names the squat instead of publishing to it, which is a fleet
  that will not serve rather than a fleet served by a box. Host-driven skein still binds on the
  host, where there is no shared namespace and no mapping to inherit.
- **pre-auth connection exhaustion.** ~~The gate runs after accept, and §10.1's cap is post-auth~~ —
  **closed at the accept loop, where it is the only place it could be closed**: `src/knock.rs`, the
  doorstep, is what a connection is between `accept` and saying who it is. A limit that *refused*
  when full would have converted exhaustion into denial and called it a fix — the flooder still
  decides who gets in, because the honest client arrives to a full room — so an arrival is always
  admitted and the **oldest connection that still has not authenticated** leaves instead. The
  asymmetry is the mechanism: proving takes one request and removes you from the doorstep, so a
  flood displaces itself and never the client that is about to authenticate. A grace deadline is the
  other half, and the cheaper one: a socket that has not presented a credential in ten seconds is
  not a cockpit, so a held slot costs a reconnection rather than nothing.

  **What is not closed, and why it cannot be here.** The usual answer is a per-source allowance, and
  skein has none to give: in-fleet every box shares skein's network namespace, so a box's peer
  address *is* skein's own and there is nothing to key on. That is answerable only after requirement
  1 below — a connection attributable to a uid — and until then a flooder and the cockpit are
  indistinguishable at the moment a connection arrives. Two consequences follow and are written down
  rather than left to be discovered: a fast enough flood can make an honest client's *handshake*
  race for a place (bounded, because the honest one wins it in the time of one request and the
  flooder never leaves the doorstep by proving); and the deadline is a wall clock, not a price, so
  it bounds how long a slot is held and not how many are attempted.
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

   ### The address must not come from the box — **done**, `6a4371b` and `94c3426`

   R1 crosses into `/proc/<anchor>/ns/user`, and the anchor was read out of a file the box can
   write: `read_anchor` `cat`ed the pidfile under the box's own root, which is bound read-write. A
   box writes a *sibling's* tmux server pid into its own anchor, and skein's next provisioning, diff,
   upload or takeover for that box executes **in the sibling's namespace**. A confused deputy walking
   straight through §9.1's file cover — and §9.5.8's rule names it, while §9.5.8's enumerated list
   missed it.

   All five parts below are in. What follows is why each is required, kept because the reasoning is
   what stops any one of them being dropped later as belt-and-braces: the vulnerability survives any
   four of the five.

   > **The anchor is reported to skein over the channel skein opened, never read from a path the box
   > can write.** skein spawns the launcher; the launcher asks its own tmux server for the server pid
   > and writes it to **stdout**; skein records it. The pidfile in the box's tree may remain for the
   > box's own use, and skein must never read it.

   Three consequences, each of which the channel alone does not give:

   **(a) Adoption must not read it either.** `start_box_inner` read the anchor *unconditionally* —
   including on the branch that keeps an already-live session, where **no launcher is spawned at
   all**. "The launcher reports it" never reached that path, so the confused deputy survived there.
   Adopting a session and trusting the box's file for its address is the one thing that must not
   happen.

   Resolved by the second of the two options: skein re-derives the anchor from something it owns —
   the placement record — and *checks* it. `adopt_anchor` refuses with a sentence naming the fix
   when it cannot, rather than falling back to the file.

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

   **(d) The check runs in the process that crosses.** A check anywhere earlier is a check with a
   gap after it, and a box that exits in the gap is entered as whatever took its number. So the
   crossing is a shell: read, compare, then `exec nsenter` — the caller's argv rides in as `"$@"`,
   so nothing between the two re-quotes anything. An address that cannot be proved builds no
   `nsenter` at all, rather than one behind a check.

   **And the report must come from a binary the box cannot shadow.** It was produced by an
   unqualified `tmux` inside a **login shell**, whose PATH includes `~/.local/bin` — which is shared
   read-write with every box (§9.2.1), so the anchor's integrity silently depended on R4. Resolved
   in the launcher's outer shell against a PATH of root-owned directories only, before any box
   namespace exists.

   **The fleet agent is handed the crossing, not the address.** It used to be sent `ns_pid` and build
   the hop itself, so that a host newer than the agent could not hand it a command *shape* it did not
   understand. Sound for shapes and wrong for checks: an agent predating (c) would ignore the fields
   carrying the proof and cross anyway. It now receives one script with its check in front, which an
   agent of any age runs correctly or not at all.

   **The upgrade path is a restart.** A record written before the stamp existed cannot be checked, so
   it is refused rather than trusted — a fleet where the guard is present and silently does nothing
   for every box nobody restarted is worse than one that says so.

   This had to be settled before R1 is built, because every other part of R1 is downstream of an
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
   | a box reaches a GitHub repository it was not granted | **neither, and nothing else here** — SKEIN-548, open; §9.6 |

   The last row is in the table because leaving it out is what made the earlier drafts read as a
   claim. Neither requirement touches it, and no requirement below does: the sandbox routes HTTP
   through a credential-injecting proxy, so a request carrying no credential is answered as the
   account and `git` inherits it. Scoping narrows what a box's own token can **do**; it does not
   narrow what a box can **reach**. `docs/parity.md` records that as a known non-property.

   So the cover carries most of the value and is cheap; the split is defence in depth and costs a
   `sudo` hop per crossing plus a sudoers policy. **Do the cover first.** If the split slips, what
   remains exposed is a denial of service against the control plane — which a supervisor restarts —
   rather than a disclosure.


2. **The cover is an inversion, derived from the fleet's mount set.** Not from one root: a repo's
   `store` is an **arbitrary host path chosen at repo-add time** — `--store` takes one and keeps it
   (`src/repos.rs:1498`) — so a rule written over the state root alone never reaches
   `/home/you/code/thing`. This used to name a second such path, `repo.work`, the host checkout of a
   repo adopted in place; there is no `work` field on `Repo` and no adopted repo to have one (§6),
   and the argument survives its loss intact, because one arbitrary path is enough to defeat a rule
   written over a root. **Built**: the launcher is *given* the mount set rather than learning it, as
   `SKEIN_FLEET_MOUNTS` from `mount_manifest` (`src/fleet.rs:4975`), and each box gets back only its
   own repo's store. `tmpfs` the whole of the state root and bind
   back the short list a box needs — which is what the launcher's `--tmpfs "$fleet_root_dir"`
   already does for the fleet root (`grep -n 'tmpfs "\$fleet_root_dir"' src/box-session.sh`). Enumerating what to *hide* is the wrong direction and an earlier revision froze that
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
   — write `fleet` and keep the account-wide token and the forwarded ssh-agent. (And `repo`, the
   other position, does not take GitHub away: see §9.6 and SKEIN-548.)

   > **`boxes/<name>/` splits four ways** — `declared/` (never bound in), `recorded/` (bound
   > read-write), `artifacts/` (bound **read-only**) and `transitions` (recorded, **not bound in** — skein writes it, the box has no use for it). Only binding
   > `recorded/` would remove git push, since the box's credential helper reads its own token out of
   > `artifacts/git-tokens/`.

   **A cover applies at box start, so which boxes have it is a per-box fact the fleet must report.**
   The inversion is derived and cannot be forgotten *for a box being started* — and that is the only
   moment it reaches. `install_launcher` rewrites `box-session.sh` in the sandbox at every start and
   every heal (`src/fleet.rs`; `grep -n 'install_launcher(' src/fleet.rs` finds the definition, one
   test and its four callers — `ensure_fleet`, `heal_fleet`, `apply_box_limits` and
   `ensure_box_session`), so the
   copy on disk always describes the **next** box; a box already up keeps the mount namespace it was
   born with until somebody restarts it, and nothing on the host distinguishes the two.

   Found by looking rather than by reading: a box reporting `SKEIN_BOX_PRIVILEGED` unset — an
   ordinary box — with `/boxes/` listing every other box and `$SSH_AUTH_SOCK` a live socket. Neither
   is a code defect. It started the day before the launcher carrying those covers was installed.

   So the launcher stamps its own revision into the bytes `install_launcher` writes, reports it on
   the channel it already reports the anchor pid on (`SKEIN_LAUNCHER`, beside `SKEIN_ANCHOR`), and
   the placement record keeps it — the `launcher` field of `places/<name>.json`, declared state under the cover, beside
   the `(generation, pid, starttime)` of §9.5.1. The board compares it with the running binary's
   own, which is one file read it already makes and no subprocess (`tests/board_cost.rs` is what
   holds that). **Stamped, not passed at launch**: a revision skein hands the script says what skein
   is running, and the question is what the script does — they disagree in exactly the case that
   matters.

   The revision is derived from the launcher with its comments cut and nothing else cut. Narrower —
   hashing only the lines carrying a bwrap mount directive — is wrong in the direction that costs a
   cover: `/run/user` and `/run/secrets` are tmpfs'd for non-privileged boxes only (R11), so a change
   to *which* boxes are covered moves no `--tmpfs` line at all.

   **Reported, never acted on.** Restarting a box discards whatever its agent had half-finished, and
   §11 makes the board's job surfacing what needs a person. The line says what a restart buys and
   what it costs, and leaves the moment to whoever is running the fleet.
3. ~~**skein's control API is a filesystem socket under that cover, owned by skein's uid, never a TCP
   port**~~ — **decided against, by the owner, and it is a product decision rather than a security
   one being lost.** The cockpit is a browser page and a browser cannot open a filesystem socket, so
   this requirement was never a transport swap: it decided how a person reaches their own board.
   Every way of keeping a URL that works costs something the owner was not willing to spend — a
   proxy in front of it (the warden growing a second job, and the browser then depending on the
   warden being up), or a tunnel before the first page load (`ssh -L`, `tailscale serve`), which
   makes the first thing a new user does a configuration exercise on the one surface §11.6 says must
   not be one.

   **So the port stays, and what that leaves open is written here rather than left looking closed.**
   The cockpit's auth token remains a file under the cover, owned by skein's uid — skein mints it,
   so it cannot be root-owned. (An earlier draft said *root-owned*; skein is not root — R1.) Boxes
   may still *connect* — shared netns makes that unavoidable — and cannot authenticate.

   Three residual exposures, and the third is the one the socket would have closed outright:

   - **Reachability of the whole HTTP surface.** Every box can open the port and speak HTTP to it. A
     future bug anywhere in that stack — the router, the parser, a handler reached before the gate —
     is reachable from every box, where a covered socket would have made it unreachable. This is
     defence in depth, and the decision spends it deliberately.
   - **The cost of connecting.** Bounded, not zero: `src/knock.rs` evicts the oldest connection that
     has not authenticated and closes it after ten seconds, so a flood displaces itself rather than
     the cockpit. `/api/machine/doorstep` is where a person sees it happening.
   - **Port squatting, which the token does not answer.** Shared netns alone (§7.4, §9.4) means a
     box that binds the cockpit port *before* skein does becomes the cockpit, and the browser hands
     it the token on the first request. `sbx ports --unpublish` does not reach it: it withdraws the
     host end of a mapping, and the bind that was stolen is at the sandbox end. **A covered socket would have closed this**,
     because a box cannot create a socket at a path it cannot see. Keeping the port keeps it, and
     the answer has to be that the port is never free for a box to take — the listening socket is
     opened once, before any box exists, and inherited across restarts rather than re-bound. That
     is 4c's work, where there is a fleet start to hang it on, and it is not a reason to hold 4c up.
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

   "Under the cover" holds by construction, not by coincidence of defaults: both ends derive the
   secret's home from the volume root — `{$SKEIN_HOME | ~/.skein}/warden` (`warden/src/lib.rs`
   `home()`, `src/warden_client.rs` `secret()`) — the same root the cover is derived over, so R5
   is established for every volume location, not only `~/.skein`. A secret found at the old fixed
   default is **moved** to the derived home by the warden at start — moved rather than re-minted,
   so the existing pairing survives and nothing secret-shaped stays at an uncovered path — and it
   is instance-scoped on the volume: a migration drops it and the warden re-mints
   (`src/volume.rs` `INSTANCE_SCOPED`). `$SKEIN_WARDEN_HOME` still overrides both ends, for tests
   and development, and setting it is the operator explicitly stepping outside the cover.

   **And this is what the secret bought, now that it has been spent.** The bind was narrow because
   the secret did not exist; §8.6 promised the move would open it deliberately. The move landed, so
   here is the decision, made by the owner rather than derived: **the warden binds loopback and the
   host's own address on each Docker bridge, and never `0.0.0.0`.** The bridge is reachable from a
   sandbox on this machine and from nowhere else, where `0.0.0.0` would put fleet destroy on
   whatever network the laptop is attached to with the secret as the only control. §8.6 carries the
   mechanism, the measurement that corrected its old reasoning, and why the bridge is derived from
   two kernel tables rather than guessed from a subnet.

   Two residuals, named rather than left implied. The **workshop box can read the secret** — the
   launcher exempts it from the cover (`grep -n 'SKEIN_BOX_PRIVILEGED' src/box-session.sh` — the two
   `!= "1"` guards are the cover and the credential tmpfs), which is what makes it a workshop —
   so a workshop box can authenticate as skein. That is a property of the exemption, not of the
   bind, and it was true before the widening. And a box can now **open the port** on a Linux host
   where it previously could not, which is the §9.4 exposure this spends: reaching the port and
   being skein are different things, and only the second one gets past `warden/src/secret.rs`.
6. **The audit log is written by the warden, on the host, on a path no box's mount view includes.**
   "Append-only" is unenforceable on a path a uid-1000 box can reach — there is no `chattr +a`
   without `CAP_LINUX_IMMUTABLE`.

   **So the warden's home splits, and the two halves want opposite things** (SKEIN-218). R5 puts the
   secret under the cover, which means following the volume root; delivery §3 4c mounts that volume
   *into* the fleet, where skein runs. The record therefore lives beside the volume rather than on
   it: `warden/src/lib.rs` `audit_home()` derives `$SKEIN_WARDEN_AUDIT | ~/.skein-warden` — a
   sibling of the volume, so it stays outside wherever the volume is pointed — and a log or an
   outcome left under the volume by an earlier warden is **moved out** at start
   (`warden/src/audit.rs` `adopt_left_behind`). The outcomes travel with the log rather than the
   secret, and not only for tidiness: an outcome the audited thing can write is an answer the warden
   would then serve as its own. `$SKEIN_WARDEN_HOME` still keeps both halves in one directory, since
   that override is a test or a development run saying where everything goes.

   **The sink existed and nothing reported into it**, which is the same as not having one with the
   reassurance of the code being present. skein now reports the acts it takes *without* asking the
   warden to take them — **a box destroyed, a push credential granted or withdrawn, a box handed to
   another agent** — after the fact and carrying the outcome, because "it was destroyed" can be
   checked against a box that is gone and "it is about to be" can be checked against nothing.

   What is deliberately **not** logged, so an empty stretch is not read as silence: the fleet
   lifecycle, which the warden records itself because it runs it; a token *rotation*, which happens
   on a cadence and would be a line an hour per box; and starting or stopping a box, which is
   reversible and visible on the board. And the cost of reporting after: a crash between the act and
   the entry leaves no line, which is why the acts chosen are ones whose result is visible elsewhere.

   It cannot fail what it records — nothing is returned, the failure goes to stderr, and the
   timeouts are its own rather than a doer's, since an audit sink that could hold a box destroy open
   for half an hour would be a reason to stop auditing.
7. **Credentials are compared on evidence skein controls, never on a field the file asserts** (§9.3).
8. **No privileged actor reads, writes, chowns or follows a path a box can influence.**

   **Site by site, because "influence" means something different at each**, and a helper that
   pretended otherwise would hide the case it did not cover. `git-tokens/` is done and it was half
   closed already: the host mints a write credential and places it at
   `<box state>/git-tokens/<repo>`, and the *write* was never the hole — `secret::write` renames into
   place, and `rename` replaces a symbolic link at the destination rather than following one. The
   **directory** was: `create_dir_all` follows a link, so a `git-tokens` pointing elsewhere is a
   directory the host creates through and drops a live token into. It is refused now, and the
   refusal names the path rather than repairing it — an ordinary box cannot make that link (the
   cover binds its state read-only in its own namespace), so finding one means something is wrong.
   The guard sits **before** the unscoped-box path, because that path does not write, it *deletes*
   every file in the directory: the dangerous half of this site is the half that looks like cleanup.

   **`disk`, `identity`, `privileged`, `git-scope` — closed in the library and open in the panel,
   which is not what "closed" was supposed to mean.** SKEIN-7 moved the four security-deciding files
   to `declared/`, which is host-only and never in `fleet_mounts`, and `declared_read` refuses a
   value left at the old path rather than migrating it. Every *enforced* read goes through it. But
   the cockpit's per-box panel read `git-scope` and `disk` **straight out of `box_state`**, which a
   box writes: it could not promote itself, and it could tell you its own setting was something
   else — and the next thing anybody does with a settings panel is press Save. Both read through
   `declared_read` now, and the test that keeps them there is a **source** assertion, because the
   failure is a path rather than a value and a path is a string somebody types.

   **A container is bounded, and weighed against a box.** `skein/containers` had no ceiling of its
   own and weighed exactly what a box weighs, so one container's overshoot stalled every box —
   `high 5551` on the live fleet — and one container's `-j64` starved the daemon it depends on.
   It now carries a memory ceiling at the fraction a box gets (a **ceiling**, not the reservation
   `MemoryPlan` argues against: it withholds nothing while containers are idle) and a `cpu.weight`
   of half a box. A weight rather than a cap, for the reason the launcher gives about boxes: a cap
   idles cores while somebody waits, and a weight costs nothing until the machine is contended.
   Half a box is a judgement, not a derivation — a box is somebody at a terminal, a container is
   work that box started and can wait a little longer for.

   **The Docker daemon is guaranteed memory and shielded from the killer.** `/docker` is uncapped by
   decision — §9.5's memory plan says "what stays behind in `/docker` is the sandbox itself, which
   nothing caps and nothing should" — and was **unprotected by omission**, which is a different
   thing. It now carries a `memory.min` of half the plumbing share (never reclaimed, and not what
   the kernel reaches for first), and dockerd and containerd carry `oom_score_adj -500`, re-applied
   on every watchdog pass because a restarted daemon is a new pid. Not `-1000`: an OOM-immune daemon
   on a sandbox with nothing left to kill is a wedged machine rather than a survivable one.

   **The resize archive — the risk was on the create side, not the restore.** Both halves, quoted
   rather than described:

   ```
   create:  sudo rm -f <archive>; \
            sudo tar -C <box root> --exclude=./anchor.pid --warning=no-file-ignored -cf <archive> .
   restore: sudo mkdir -p <root>; sudo tar -C <root> -xf <archive>; rm -f <archive>
   ```

   The **restore** is safe, and measured rather than assumed: GNU tar 1.35, asked for nothing
   special, refuses a `..` member outright, strips a leading `/` so an absolute member lands *inside*
   the destination, and will not write through a symlink member — and it exits non-zero, so the
   `set -e` around it aborts the resize rather than half-restoring a box. That property belongs to
   tar and to the flags it is given, and `-P` anywhere in the pipeline would end it, so it is pinned
   by a test that builds an escaping archive by hand and extracts it.

   The **create** was the hole. `tar -cf` follows a symbolic link at its output path, and the output
   path is inside the box's own state directory — it has to be, since that is the one place mounted
   into the sandbox that outlives the sandbox. Anything able to plant a link there gets root to write
   a tar file wherever it points. The cover stops an ordinary box; the workshop box is exempt from
   the cover **by design**, which is precisely the actor the rule is about. `rm -f` before the create
   unlinks the link rather than following it.

   **The anchor — closed, and confirmed rather than assumed.** `box_pidfile` still exists and the
   launcher still writes it; nothing in skein reads it. The address comes from the placement record
   under the cover (SKEIN-4), and the launcher's own report is read by its marker rather than by
   position, so a box echoing a number on login cannot choose the namespace skein enters.

   **The file stays, and "skein must never read it" is now a test rather than a sentence.** It was
   worth asking whether to delete it — a file in a box-writable root whose only purpose was to tell
   a privileged process which namespace to enter is not obviously worth keeping — and the answer is
   that the launcher writes it *for the box*, deliberately, which is somebody else's affordance to
   remove. What decays is the prohibition: the next person who needs a pid sees an obvious file. So
   the source is asserted — no read of that path anywhere in `src/`, with the two non-reads named
   (the helper, and the launch command that hands the path to the launcher).

   **And the same rule, one layer up: `mailbox` (§9.5 R10).** A message's `from` is a field its
   writer fills in, and the shared store's `mailbox/` is writable from inside every box — so
   `{"from":"skein"}` there needs no script and no trickery, only a file. Provenance is therefore
   **which directory it was found in**: the owner's messages go to a box's own `inbox/` under its
   state directory, which the launcher binds read-only into the box, and nothing inside a box can
   put a message there. The delivery hook says so in the words the agent reads — `from you`, or the
   claimed name *with the fact that nobody checked it*. A box's message still arrives, because
   §9.2.2 is kept deliberately; it arrives saying what it is.

   §8.4 states
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

   **Done, and the terms are three rather than two.** The switch and the launcher's per-start banner
   both name them: it sees every box's files, it acts at fleet scope, and **it holds the fleet agent
   token** — every ordinary box gets an empty file bound over that path and a privileged one does
   not. Beside them, the line a person cannot discover by using the box: **the mount cover is off
   for it**, which is not only its own business — the guards on the git-token directory (R8) and the
   resize archive both hold *because an ordinary box cannot plant a link where the host writes*, and
   this is the switch that turns that off.

   Short on purpose: a warning long enough to be skipped is a warning nobody reads. A test asserts
   the banner keeps naming all four, because wording drifts out of a shell script silently.
10. ~~**Cross-box messaging renders provenance**~~ **done** (§9.2.2, kept). Inbound-from-a-box is
   distinguishable from inbound-from-you, and by **which directory a message was found in** rather
   than by a field its writer fills: the shared store's `mailbox/` is writable from every box, a
   box's own `inbox/` under its state is bound read-only into it. The delivery says which — `from
   you`, or the claimed name with the fact that nobody checked it.
11. **`/run` is covered, or its exposure is stated.** ~~§9.1 notes the cover reaches neither `/run`
   nor the per-user socket directory, and no requirement followed.~~ **Both, and the split is the
   answer.** Read from inside a box (`ls -la /run`, `id`; no socket was connected to, and none needs
   to be for the modes to say this):

   | path | mode | reachable by an ordinary box | now |
   |---|---|---|---|
   | `/run/user/<uid>` | `drwx------ agent:agent` | yes — **one directory for every box**, since every box is the same uid | private tmpfs per box, with `cc-socks/` bound back (see below) |
   | `/run/secrets` | `drwxrwxrwt` | yes, world-writable | private tmpfs per box |
   | `/run/ssh-agent.sock` | `srw-rw-rw-` | **already covered** — the launcher binds a regular file over `$SSH_AUTH_SOCK`, so `connect()` fails on a thing that is not a socket | unchanged |
   | `/run/docker.sock` | `srw-rw----` `nobody:nogroup` | yes — a box's supplementary groups include `65534(nogroup)` | **left reachable, deliberately** |

   The second costs nothing to close: a world-writable directory nothing skein ships uses. The
   first was argued the same way — *"empty today, which is exactly when to close it"* — and that
   premise has since been retired by events, which is the subject of the next three paragraphs.

   **The cover over `/run/user/<uid>` is opened for exactly one directory, `cc-socks/`, and this is
   the decision rather than the mount somebody notices later** (SKEIN-572, owner 2026-09-07). The
   agent runtime puts every session's inbox socket there, so the tmpfs that was free when the
   directory was empty had, by the time anybody looked, severed the fleet's only inter-box channel
   that does not leave the sandbox. Measured from a live box on 2026-09-07: the shared session
   registry held six sessions, every one advertising a socket, and exactly one resolved — the box's
   own. R11's argument survives intact, because it was never *"never open this"* but *"do not let
   it become an undeclared channel"*; what changes is that the channel is declared, reasoned about
   here, and asserted by `tests/isolation_bwrap.rs`, where the cover previously had no test at all.
   Everything else under the runtime directory stays private per box, which is the second of those
   two tests.

   **Discovery and transport are never independently switchable.** Session messaging is two shared
   paths — `~/.claude/sessions/<pid>.json`, which `ListAgents` reads, and the socket under
   `cc-socks/`, which `SendMessage` connects to — and skein spent months sharing the first while
   covering the second. That half-open state is worse than either whole one, and the reason is not
   symmetry but who pays: *"the problem with only peer half closed is that everyone else thinks
   that it is live so they write to it but it never gets delivered and left wondering what
   happened"* (owner). The sender did nothing wrong and is told nothing; the message goes out
   through Anthropic's servers instead, which needs a claude.ai login this fleet should not need to
   talk to itself, is unavailable on Bedrock, Vertex and Foundry, and reads offline for any box
   whose connection has dropped. So both binds are decided in one block of `box-session.sh` from
   one variable, and the invariant — **a box is never discoverable on a socket it cannot reach** —
   is what the test asserts, in both switch positions rather than in one.

   **What that costs, said once here rather than found.** Every box in a fleet is the same uid, so
   the socket's own protection separates nothing: the mount was the only boundary and it is now
   deliberately open. With `crossSessionInbound: "accept"` seeded into each box (§9.2.2) **there is
   no approval gate left between boxes** — any box can put text of its choosing in front of any
   other box's agent, and that agent may hold credentials the sender does not. The receiving side's
   mitigations are real and partial: the runtime says the message came from another session rather
   than from you, grants it no approval and no configuration change, and never runs commands out of
   its text. That is why a relayed human review travels as a NOTICE whose authoritative copy stays
   in the read-only owner inbox no box can write (§9.5 R10), and any future feature delivering over
   this socket owes the same split.

   **Per repo, defaulting on, and enforced by the mount.** `Repo::peer_messaging` ships ON;
   `fleet::session_script` turns it into `SKEIN_BOX_PEERS` the way it already turns the git switch
   into `SKEIN_GIT_SCOPE`. Off means **full isolation** — neither bind — so that repo's boxes
   neither see peers nor are seen. Not a setting inside the box: a box owns its own
   `settings.json`, so `permissions.deny` and `crossSessionInbound: "refuse"` are advisory where
   this is a boundary, and `refuse` would drop what skein sends the box as well, which is not what
   turning off box-to-box means. **And the switch travels with the box, not with the config**:
   `launcher_revision` hashes `box-session.sh`, and a flag in `repos.json` changes not one byte of
   it, so `cover_is_current` would keep calling a box current while it ran the opposite mount. The
   launcher therefore reports what each box was **born with** on its own stdout — `SKEIN_PEERS`,
   beside `SKEIN_LIMITS` and `SKEIN_LAUNCHER` — into `PlaceRecord::peers`, which
   `fleet::cover_is_current` compares alongside the revision. Flipping the switch asks for a
   restart on the board, and that assertion is the difference between the feature working and
   looking as though it does.

   **The last one is a product decision, not an oversight, and it is stated here because it bounds
   everything above it.** `fleet::install_docker_config` points the sandbox's dockerd at the workload
   cgroup *so that containers a box starts are accounted for* — running containers from a box is a
   supported thing. What that grants is a container in this sandbox, **as root, with any bind mount
   it asks for**: out of the box's bwrap namespace and into the sandbox, which is every other box's
   files, the fleet root and the volume. So the mount cover (§9.5 R2) is careful, derived per box,
   and **bounded by a socket in a directory it never touches** — and the R8 guards that rest on "an
   ordinary box cannot plant a link where the host writes" are bounded by it too.
   
   Covering it would be one line beside the two above. It is not taken here because it removes a
   capability the design supports, and that is the owner's call rather than this document's. Until
   it is made, this paragraph is the honest version of the boundary.

   **What the supported capability had no answer for: whose container is it.** Docker records
   nothing about which box asked. Every box reaches the same daemon over the same socket at the same
   uid, and `CONTAINER_CGROUP` is one parent shared by all of them — deliberate, since it is what
   makes a box's containers count against the fleet's ceilings. Shared *accounting* and per-box
   *ownership* are different questions, and the second had no answer at all, which is why stopping a
   box left its containers running with nothing able to say which they were.

   `box-session.sh` now shims `docker` the way it already shims `git`, stamping `run` and `create`
   with `--label skein.box=<box>` and `--cgroup-parent /skein/containers/<box>`. Each reaches a case
   the other cannot: the label is how the stop finds them, and it removes rather than signals,
   because killing a container's processes leaves the daemon believing it runs; the per-box cgroup —
   still inside the shared parent, so the fleet ceiling above is untouched — is what the stop can
   reach when dockerd is the thing that has stopped answering, which a runaway container is one way
   to cause.

   **A convention, not a boundary**, in exactly the sense the git shim is: *"the shim is the message,
   not the boundary."* A box holds the daemon. It can curl the socket directly, `docker compose`
   composes its own create calls, and neither carries what this stamps. So the stop **names every
   running container it cannot attribute** rather than reporting success over the ones it missed —
   which is the same discipline as the rest of this section, an exposure stated where closing it is
   not on offer.

**A box's ceiling is reported, because nothing on the host could read it.** The launcher records
what it managed to apply in `limits.state` under the box's own root — which is *inside the sandbox* —
so "an uncapped box says so" was true only of a file no surface could open, and a box with no
ceiling looked exactly like a box with one from the board, `skein doctor` and the API alike. It now
travels the way the anchor and the launcher revision do: on the launcher's stdout, into the placement
record, where a board tick reads it for nothing.

And the cgroup is made **whether or not there is a ceiling to write into it**. It does two jobs and
only one of them is optional: it is also the box's identity as a set of processes, which is what
`cgroup.kill` needs at stop and what the fleet's accounting rests on. Gating the whole block on
"skein computed a limit" gave a box with none *neither* — so the box that most needed containing was
the one that had nothing containing it, and `2>/dev/null || true` on the kill kept that quiet.

Three states, and they are not one problem. *capped* is the intended one. *uncapped
no-limit-computed* means the box is contained but unbounded — skein's own memory plan produced
nothing for it, and a restart puts it under the current one. *uncapped no-cgroup-delegation* and
*could-not-join-cgroup* are the sandbox's answer, and no setting here changes them.

**The forwarded ssh-agent does not change when skein moves in, and that is the answer rather than a
gap.** §2 listed it as one of six things the move invalidates. It is not one: the forward is `sbx
create`'s doing, from the host into the **sandbox**, so `$SSH_AUTH_SOCK` inside is the host's agent
whether skein is beside it or outside it. The launcher's cover above is unaffected, and so is what
`SKEIN_GIT_SCOPE=fleet` re-exposes (§9.6) — both act on the socket, which is in the same place.

What does not travel is the key *file*. `~/.ssh/id_ed25519` names a path on the host, and the sandbox
has its own `~`, so `ensure_ssh_key` cannot load it from inside — it refuses with where to run
`ssh-add`, rather than failing on the file, which reads as a mistyped path and sends somebody to fix
a setting instead of running one command where their key already is. Nothing is lost: skein never
handles the key on a host either, only the agent socket is forwarded, and a host `ssh-add` reaches an
in-fleet deployment exactly as it reaches a host-driven one.

Corrected from an earlier draft: the cgroup control plane is **not** box-writable. Every cgroup write
in the launcher goes through `sudo` before `bwrap`, and the source is explicit that a write from
inside a box "is not an option at all" — the userns maps only uid 1000 and cgroupfs is root-owned.

**And in-fleet, two of the three paths work and the third cannot.** A GitHub App and per-repo stored
tokens are skein's own — minted here, written into each box's `artifacts/git-tokens/`, flowing down
only (R7). The account token is not: `gh auth token` reads the host's login and `sbx secret set`
writes the host's keyring, and neither is reachable from inside the sandbox. So a fleet that has not
been seeded before the move reads *nothing chosen* rather than *the account token* — the label is
what the first-run checklist reads as "boxes can push", and a fleet told that when it cannot learns
otherwise from a 403 inside a box, minutes later and three layers from the cause.

A fleet seeded on the host and then moved in still has it: the secret lives in sbx's store, which
outlives the volume, and `gh-secret-seeded` travels with the volume as the evidence
(`docs/delivery.md` §4.1a, and a test on the migration).

### 9.6 The agent credential cannot be scoped

GitHub tokens can be scoped per repo, short-lived and revoked — with one caveat that belongs beside
the credit: `SKEIN_GIT_SCOPE=fleet` is an opt-out restoring the account-wide token *and* re-exposing
the forwarded ssh-agent. And the guard is the token, never the shim: *"The shim is the message, not
the boundary."*

**And the credit is narrower than it reads — SKEIN-548, open.** Measured from inside a live box on
2026-09-06, again on 2026-09-07, and re-measured unchanged on 2026-09-11: the sandbox routes HTTP
through a credential-injecting proxy, so a request carrying no Authorization header — or a
deliberately invalid one — comes back authenticated as the account, while the same request sent
direct is refused. **`git` inherits it, and that is not an inference** — re-checked 2026-09-11
against the git wire protocol itself rather than only the REST API: a request to
`/<owner>/<repo>/info/refs?service=git-upload-pack` for a **private** repository that is not this
box's own, carrying no credential at all, comes back `200` with a real ref advertisement, and the
identical request with the proxy bypassed comes back `401`. So a box with `GH_TOKEN` unset and its
credential helper answering nothing still reads a private repository it was never granted.

**Which transport git picks decides whether it is injected, and that is a trap for anyone checking
this.** The injection rides on HTTP(S) through the proxy; the same fetch attempted over SSH is
refused outright, because SSH does not go through the proxy at all. So a `git ls-remote` that an
`insteadOf` rule quietly rewrites to `git@github.com:` prints a clean "Repository not found" and
reads as evidence that the boundary holds. It is not — it is evidence that the request never met
the proxy. Check the HTTPS endpoint explicitly, and check which transport git actually chose. The `GH_TOKEN` the launcher is so careful about returns 401 when
sent directly, which makes it a placeholder rather than the credential anything authenticates with.

**How it is in a position to do that**, stated because it is the part that can be checked rather
than inferred from a status code: the proxy **terminates TLS**. Inside a box the certificate for
GitHub's API is issued by the sandbox's own proxy CA and not by GitHub's issuer, which is what lets
it read and replace an `Authorization` header in flight —
`curl -v https://api.github.com/rate_limit 2>&1 | grep issuer` prints the sandbox CA through the
proxy and GitHub's real issuer under `--noproxy '*'`. So this is not a gateway that adds a header to
requests that lack one; it is a man in the middle that has the final say on the credential
regardless of what the box sent.

**The size of the reach, counted rather than sampled** (2026-09-11, no credential sent): 227 private
repositories, 462 in total, and of the private ones 223 answer `permissions.push` true and 211
answer `permissions.admin` true. `docs/parity.md` carries the reproduction.

So the scoping machinery narrows what a box's own token can **do** — that half is real and GitHub
enforces it server-side — and it does not narrow what a box can **reach**. Nothing in §9.5 closes
that, and nothing in this document should be read as a claim about a box's network reach; `docs/parity.md`
records it as a known non-property rather than a capability. Closing it needs the substrate: this is
sbx behaviour, unsetting the proxy variable is not a boundary (the address is well known, and
anything in the box can export it again — the same reasoning the launcher applies to the ssh-agent
socket, which it binds a real file over rather than merely unsetting), and direct egress bypasses
the proxy anyway, so the proxy is not a chokepoint either — verified 2026-09-11: with `--noproxy
'*'` the request reaches GitHub's own front end (a certificate issued by GitHub's real CA) and is
refused there with GitHub's own `401` body, rather than being blocked on the way out.

**What is retracted and what is still open**, because they are different halves. Retracted: every
claim in this tree that git scoping bounds what a box can **reach**. Still open: whether the
injection can be turned off for a sandbox.

That second half **cannot be settled from inside a box, and this document should not guess at it.**
What a box can establish is only the shape of the thing: `sbx` is not on a box's `PATH`; no sandbox
configuration is mounted in (nothing in `/proc/self/mountinfo` carries it, and
`/var/log/sbx-kit-startup.log` records only which startup units ran); and the whole of the evidence
a box has is `SBX_CRED_GITHUB_MODE=apikey` in its environment, which names *a* mode without
establishing that another exists. Nothing here says whether the substrate offers a switch, so
nothing here should be read as saying it does — or that it does not.

One more piece of it is visible from a box, and it explains why the interception is silent rather
than merely possible: **the proxy's CA is installed in the box's own trust store**, at
`/usr/local/share/ca-certificates/proxy-ca.crt` (and handed to a box a second time through the
environment the substrate sets). That is what makes an ordinary `curl` or `git` accept the
substituted certificate without a warning, and it is why "a box could notice" is not a defence.
Neither of those is skein's to place or remove — both arrive with the sandbox.

Settling it is a host-side task: on the machine that owns the sandbox, read the sandbox tool's own
help and its per-sandbox credential configuration to find out whether GitHub injection is
configurable at all, and if it is, turn it off and **re-run the reproduction in `docs/parity.md`
from inside a box** — an invalid token that still answers `200` means nothing changed. Until that
is done the honest state of this section is: the exposure is measured, the claim is withdrawn, and
the remedy is unestablished — which is not the same as impossible.

The agent's own OAuth login is different. It must be **inside the box** for the agent to run, and no
provider offers a scoping primitive for it. So it is carved out rather than covered by a claim that
does not hold: **the agent login is fleet-shared and unscopable.**

Two defences exist and neither is built: a proxy that injects it outside the box's reach, and
**per-box logins** — which the launcher already falls back to when `python3` is absent, so the path
is not hypothetical.

One thing that *was* built, because it was worse than unscopable — it was **forgeable**. The login
used to flow both ways, with the winner chosen by the `expiresAt` inside the file, so a box that
wrote itself a credential dated far in the future had it copied up into the fleet's canonical copy
and seeded into every box started afterwards. The comparison could not be repaired by comparing
something else: a box legitimately holds the refresh token, so anything it can produce honestly it
can produce dishonestly, and no field in a file a box writes is evidence about that file. The
direction carries the rule now — the fleet's login flows **down** only, and a box's reaches the
fleet only when the fleet has none, where there is nothing to displace.

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

**The board tick, measured.** `src/signal.rs` declares what each of the board's eleven signals
spends, and `tests/board_cost.rs` runs a tick under a `PATH` of counting wrappers and compares the
tally with the sum. A twelve-box fleet forks **two** processes on a cold tick — one `du` over the
fleet root, one liveness sweep — and **the same two at fifty boxes**, because each answers for the
whole fleet in one call. Warm, inside every gate's window, it forks **nothing**.

It was three. `sbx ls` rode on every tick as the source of record for which boxes exist, which was
true in the per-VM model and false in this one: **a box is not a sandbox**, and `sbx ls` has never
heard of one. The placement records answer that question for nothing, so the listing became what it
is actually good for — *what sandboxes are on this machine*, another skein fleet beside this one
included — and that is a question somebody asks. It is `machine::sandboxes` and
`GET /api/machine/sandboxes` now, and the cockpit fetches it when the `foreign:` filter is typed.
Neither the name nor the subject is the one this started with. The rows were kept alive as
`BoxView`s at first, so that the filter did not silently return nothing, and that is the **foreign
sandbox display** `docs/parity.md` §7 removes: a sandbox is reported as a sandbox now — a name, a
run state, and whether it is a skein fleet — because a box with an empty branch and no signals read
as a fleet full of broken ones.

The count found one thing, which is what counting is for. **The branch fallback forked per box and
had no gate**: when the registry, the launch spec and the repo all failed to name a box's branch,
`git rev-parse` was asked per row, every tick, per open tab — twelve boxes measured at twelve forks
on a *warm* tick. It was the only per-box fork on the board and invisible until the costs had to be
written down. `HEAD` is a symref in a text file, so the fork was never buying anything; it is a file
read now, and the same measurement reports zero. **No signal on the board is both ungated and
forking**, and `signal.rs` asserts that quadrant stays empty.

A cost declares its **basis** — the file and line, or the measurement — and one with an empty basis
fails a test. That is the "measured, not asserted" line above, made into something that can fail.

Correcting the first draft: in-fleet mode does **not** make observation cheaper, because host cost was
already zero by design. What it does is put skein's web server, SSE fan-out, git operations and
GitHub polling **inside the fleet's memory reservation** — the reservation whose summing is the entire
reason the one-VM design exists. Every byte skein takes is a byte a box cannot have. That is the real
cost and nobody had costed it.

### 10.1 Delivery, and what the design owes it

The event stream today re-sends the whole fleet every two seconds: no deltas, no bounded channel, no
lag counter, no connection cap, and a missed-tick policy that bursts at a drained slow client. The
diagnosis is easy and an earlier draft stopped there. The design:

- **one producer, fanned out.** ~~Today there is no broadcast channel at all~~ — **done**: every SSE
  client used to run the fleet snapshot on the blocking pool every two seconds, so five tabs were
  five snapshots a tick. A gate could not have fixed it; the work was per client by construction.
  One producer now, **started by the first client and stopped when the last one leaves**, so a server
  nobody is watching does no work at all — a property the old shape could not have, because there was
  nobody to notice.
- **transitions, not snapshots.** Done: a full snapshot on connect, and after that only what moved.
  `gone` is its own list rather than an absence, because "not in this update" and "no longer there"
  are different facts and conflating them means re-sending everything to express one. A tick where
  nothing moved sends **nothing** — which is what lets "nothing needs you" be a state rather than an
  absence. The comparison is over the serialised form, because a hand-written one stops noticing the
  newest field silently, and in the direction of showing a stale row.

  **Except the fields derived from the clock**, and the first version left them in: a box's `age`
  moves every second whether or not anything happened to it, so every box changed on every tick and
  transitions cost exactly what snapshots did, plus the machinery. It surfaced as a test that hung,
  because the stream never stopped sending. *Changed* has to mean something happened, not that time
  passed.

  The cost was a displayed age that froze between real changes, and **the client ages its own rows**
  now: the server sends `age_secs` — the age at the moment of the observation — and the client knows
  when it received it, so the age is the sum, recomputed on a timer that costs no traffic at all. One
  formatter, in the language the person reads it in, tested in node. A row whose update did not
  mention it keeps the moment it was last seen, which is what makes its age keep advancing rather
  than resetting every tick.

  The periodic full snapshot survives for a different reason and is far rarer: a client whose applied
  deltas have drifted has no way to notice on its own, and a quiet fleet gives it nothing to correct
  against.
- **a bounded per-client channel with a lag counter.** Done: the client is told how many ticks it
  missed and re-syncs from a snapshot. A hole is worse than a gap you can see — the board would look
  current and be wrong.
- **a connection cap**, as the PTY path already has. Done, and **it is post-auth**, like the PTY one.
  That bounds authenticated clients and says nothing about the connections before them — which is
  stated here rather than implied, because a cap that looks like it covers that is worse than one
  that admits it does not. The pre-auth half is a **different mechanism in a different place**, not
  a bigger number here: it is at the accept loop, it evicts rather than refuses, and §9.4 is where
  it and its remaining hole are written down.

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

  ⚠ operation::check changed shape — 12 mentions
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

  The mock-up above said **call sites**, and the number is **mentions** — lines of the base tree that
  name the symbol. The scanner now records the symbol it matched (`contracts::Signal::symbol`), which
  is what made any count possible at all; what `git grep` can then answer is how many lines name it,
  not how many of them are calls, because it cannot tell a call from a comment or a string. A
  reviewer who trusts "12 call sites" and finds four is worse off than one who was told what was
  counted. A signal whose detector could not name a symbol — a deleted file, a rename — shows **no
  count**, never a zero: "0 mentions" reads as "nothing uses this", which is the opposite of "we did
  not look".

Both are **signals whose subject is a module** (§2.2). No new machinery.

Begun: `shape::of_diff` is the join. Contract signals are **file**-scoped and module notes are
**module**-scoped, and the notes already map changed files to modules — so a signal rolls up to the
module it is about, by the same longest-match rule (`src/web/x` belongs to `src/web`, not to `src`,
because that is the module whose note is about it). It is served for a pull request *and* for a box's
own branch, from one function: the shape of a change does not depend on whether it arrived as a PR or
as work somebody is still doing.

The mock-up's three classifications are computed — `NEW`, `CHANGED`, `SHRANK`, and `GONE` beside them
— and **`SHRANK` earns its own word rather than being a negative number**: a module that lost more
than it gained is usually a deletion or an extraction, and it is the one shape a reviewer reads
differently.

**The order is what is worth looking at, not what is biggest.** A module carrying a contract signal
comes first however small its change, because the signal is exactly the structural consequence a
summary missed — a one-line change that moved a default outranks a thousand-line rename that moved
nothing. That rule is its own function with its own test, which is how the first version's inverted
comparison was caught: it had put the rename first, and the end-to-end test could not see it because
its fixture produced no signals.

**"N call sites" is not built, and that is a decision rather than an omission.** A contract signal
carries prose — "the default changed" — not a symbol, so there is nothing to search the repository
for without inventing one, and a confident count about the wrong symbol is worse than no count. It
needs the scanner to name what moved before the number can be honest.

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

**Rank by what a row is waiting for, never by which subsystem produced it** — and there is a concrete
trap in this exact merge. A box's turn-state `waiting` means *waiting on you*: the agent asked
something and stopped. A pull request's `Waiting` lane means *waiting on everyone else*: you have
already reviewed it and it moves without you. Same word, opposite meanings — and a merge that mapped
them onto each other because they matched would put the thing you have finished with above the thing
that is asking you a question. `queue::Need` is the one ladder both are mapped onto, with the
argument written at each arm.

The tie-break inside a rank is **how long it has waited, longest first**, because the thing that has
been waiting longest is the thing most likely to have been forgotten. An unknown age sorts *last*:
"we do not know how long" is the absence of evidence, not evidence of urgency.

**It is not on the board's tick.** The pull-request half comes from the review queue's own
sixty-second cache, and the merge is a surface's call rather than the board's — which is what keeps
§10's measured tick cost honest while adding a source to the queue.

### 11.3 Three states most tools botch

- **Nothing needs you.** Say so, plainly and calmly. A dashboard that looks the same whether or not
  anything is wrong has failed at its only job.
- **You were away.** Continuity across absence is a requirement, not a nicety, and it needs
  server-side transitions (§3) rather than a client-side delta on tab focus.
- **Setup is incomplete.** Failing checks at the top of the same queue — but see §11.5, because the
  cockpit is not where a new user starts.

**They are one value, not three renderings.** `queue::Standing` is `setup-incomplete`, `needs-you`
or `calm`, derived from the rows the board would draw — so the headline and the list cannot disagree,
which is what a separate "all clear" banner invites. Calm is not "the busy case with a zero in it":
a fleet of working boxes is calm, because the machine is busy and nothing is owed.

**Why "you were away" is server-side, stated as the three things a client-side delta cannot do.** A
mark in a tab's memory dies on reload. A mark per tab makes two tabs disagree. A mark computed from
"when this tab gained focus" cannot tell a box that finished while you were out from one that
finished before you opened the tab. All three look like the feature working. So there is **one mark,
on disk**, and the server answers *what happened since it* from a bounded journal of state changes —
state, not any field, because a digest of every diffstat and headline is a log, and would be re-read
as noise on every reload.

Two details that are decisions rather than mechanics. The acknowledgement is stamped **by the
server**: a client supplying its own timestamp is choosing which moments it will never be shown, and
a clock a minute fast silently swallows a minute of them. And a mark that cannot be read shows
**everything** rather than nothing — a digest that silently shows nothing is indistinguishable from a
quiet night, and only one of those is true.

A box with no previous state has not transitioned, which covers the producer's first tick *and* a box
that has just been created: it arrived, it did not move.

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

Begun: `cockpit/src` holds the pure functions as modules, `cockpit/build.mjs` concatenates them into
the one bundle the page loads, and `node --test` runs over them in CI. **No dependency**, which is
the point rather than an economy — a build step for leaf functions that needed a package tree would
cost more trust than it bought, and the cockpit is served from a binary to a browser on the same
machine for the same reason `xterm` is vendored.

**The build refuses a module with an `import` in it**, and that is the constraint the modules keep
rather than a limitation of the build: what belongs there is leaf functions — all inputs as
arguments, no module state, no DOM — which is exactly the set §13's law is about. Making `boardRows`
pure meant its foreign rows became an argument instead of module state it reached for, which is what
had made it untestable.

**The bundle is committed and checked.** `cargo build` does not run node, so a build artefact in the
tree can go stale silently — which here means a cockpit quietly running last week's code. A test
rebuilds it and compares, and a second asserts the page carries no second copy of what the bundle
defines: two sources of truth is worse than one, because the page keeps working while the tested copy
drifts and every node test passes against code nobody runs.

Two kinds of assertion remain in `src/cockpit.rs`, and they are not the same kind. A **wire**
assertion — the page reads a field the server sends — is about a join between two languages and
nothing but a string match can make it; those stay. A **logic** assertion is a string match standing
in for a test, because the function could not be imported; those are what this retires, and the ones
still there are DOM-coupled.

**Assets are embedded, and overridable from a directory.** A build emits files whose names carry
content hashes, so neither the count nor the names are known at compile time — which is why there is
one route over a table generated from a directory rather than a constant and a handler per file.
Embedded is the default because skein is one binary that cannot be half-upgraded, and a cockpit
whose scripts came from somewhere else is one that can talk to an API that has moved.
`$SKEIN_COCKPIT_ASSETS` overrides it, because most of what a build step buys during development is
that changing a stylesheet is a reload rather than a `cargo build`. It **overrides rather than
replaces**: a directory holding one file is a developer editing one file.

The root is resolved **once, at startup, and canonicalised**, and every candidate is canonicalised
again after joining. The hazard is precise — a root taken per request from anything a caller sends is
a way to serve a box's files through skein's own authenticated origin — and the check has to be on
the resolved path, because a symlink inside the directory passes every test done on the request.
A path that climbs is refused rather than sanitised: stripping `..` turns a check into a
transformation, and a transformation is something somebody later finds a way through.

Two cache rules: a name carrying a content hash is `immutable` for a year, and everything else is
`no-store`, for the same reason the document is. The conservative one is the default, because an
asset wrongly cached for a year is a cockpit that restarting the server cannot fix.

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
- **The warden is built and tested once per doer, plus the minimal build and the default**:
sink-and-observation only (the minimal build — not "empty", since two endpoints are never
removable), plus each doer alone, plus the default set. Said this way rather than as a count,
because it was written as "four ways" when there were two doers and a third (`unpublish`) made the
number wrong while the rule it was standing for stayed exactly right.
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
| the in-sandbox agent and its transport | it exists to survive a host-to-guest hop that no longer happens — **true as written, and it was not until the two jobs the same file had grown moved out**: the Docker watchdog and the machine-pressure counters are now `skein-server`'s, which is the long-lived in-sandbox process the agent used to be (SKEIN-573) |
| its port publishing, healing loop and backoff | **done** (SKEIN-576). Same reason, and it was also the one thing built around a supposed no-unpublish trap that `sbx ports --unpublish` turns out not to be (§7.4). What replaced it is not a deletion: the cockpit's mapping is `fleet::publish_cockpit_port`, an Operation with a printable recipe and **no doer**, because `Act::Publish` has none by §9.4. §9.4's stamp guard moved to `cockpit_port_advice` — it now decides whether a *person* is told to publish, which is the same hazard with a different hand on it |
| every `sbx exec` path **and its fallback twin** | with them, the transport-failure-versus-command-failure distinction that made the pairing necessary — but see below |
| two placement shapes | one remains |
| sandbox listing as the truth about boxes | replaced by the box's own anchor (§6) |
| the machine-global secret store | with it, two fleets on one host sharing one token |
| host-absolute mount path translation | no host mounts of repos remain |
| adopt-in-place mounts | nothing replaced them, and this row used to say local-path remotes had: a repo is a remote, a path is refused at registration, and the host checkout is not mounted into the sandbox at all (§6) |

**The hazard the fallback twin guarded is not deleted, it moves.** "Did it run or not?" becomes a
timeout on a warden request, and §8.2's operation ids are what answer it there. Deleting the
distinction without carrying the safety property forward is how this becomes worse than what it
replaced. It is carried and it is checked: `warden_client::operation_id` is **derived** from the
verb, the sandbox and the argv rather than minted per attempt — so a retry after a restart names the
same operation instead of making a second one — and `Answered::happened` is three-valued, with
`Undecided` and `Unknown` mapping to `None` and never to `false`, because for a destroy "we do not
know" and "it did not happen" license opposite actions.

**And the same rule applied to the agent's deletion.** Two of its jobs were never transport, so
neither went with it: the Docker watchdog (`dockerd` runs pid-1-parented in the sandbox, and a
container that kills it costs a rebuild of the whole fleet) and the cgroup/`vmstat` counters behind
`Signal::MachinePressure`. Both are `src/dockerd.rs` now, driven by `skein-server`. A deletion that
had taken them would have removed a safety mechanism and a first-class signal under cover of
removing a transport, which is the same mistake one level up.

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
| `api` | HTTP transport, auth, routes, the WebSocket | every module above, plus `stream` — **not** `cockpit`, `migrate` or `cli` |
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

### 14.2 What had to be dismantled before any of this became checkable

`src/lib.rs` re-exported sixteen modules with `pub use <mod>::*`, so cross-module references went
through a flat root namespace — `grep -rn "crate::signals::"` from other modules returned **zero**,
not because nothing used it but because everything used the re-exports. Every dependency rule above
was unverifiable against the code, which is why removing the façade was the first task of extraction
rather than a tidy-up afterwards.

**Done** — commit `9d1ddf5`. The modules are `pub mod`, there are no re-exports at the root, and
every cross-module reference is a qualified `crate::<mod>::` path or an explicit
`use crate::<mod>::…`. Check: `grep -c 'pub use' src/lib.rs` → **2**, and **both are comments** —
lines 14 and 20, which explain what was removed and why the `use` below is not a `pub use`. There is
no re-export. (`grep -cE '^ *pub use' src/lib.rs` → 0 is the version of the check that answers the
question it was asked; the loose one counts the prose about itself.) The edge set is now readable
straight off the imports.

The catch-all went with it (`a8edef1`). `src/lib.rs` is now **78** lines (`wc -l src/lib.rs`,
2026-09-06), every one a module declaration or the doc that says why; the ~2,570 lines of implementation it held became `registry`,
`sbx`, `board`, `kit`, `probes`, `digest`, `handoff`, `takeover`, `sharedhome` and `cockpit`.
Check: `wc -l src/lib.rs`, and `grep -cE '^(pub )?(fn|struct|enum|impl) ' src/lib.rs` → **0**.

**And the table above is now enforced.** `docs/modules.toml` is this section in machine-readable
form, and `tools/module-check.py` runs in CI. It holds three lines:

- every edge in `src/` is in the allow-list, so a new dependency is a reviewed diff;
- no module joins a dependency cycle that is not already recorded;
- **this table is consistent with itself** — every dependency it names is a module it declares, and
  the graph is acyclic. That is what enforces "`state` and `source` depend on nothing" and
  "`source` never depends on `operation`": both are properties of a DAG with those rows empty, and
  an edit that breaks either one fails the build with the cycle spelled out.

What neither change fixed is larger than the `place → fleet` edge §14.2 was written about — that
edge has since been removed (SKEIN-22) and the knot did not shrink, which is the point. The exact
graph has **two cycles, and the larger holds eighteen of the fifty-six modules** (2026-09-06;
`python3 tools/module-check.py` prints the graph's size and `docs/modules.toml` records both cycles
by name) — see `docs/inventory.md` §6. That is the condition this section exists to end, and it ends
by extraction into the modules above rather than by untangling the ones below.

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
