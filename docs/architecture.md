# skein — architecture

The design for a clean rewrite. Decided 2026-08-20.

The root `ARCHITECTURE.md` describes the system that exists today and stays accurate until this
replaces it.

This document is organised primitives-first: §2 states the four things skein is built from, §3 shows
every feature as a composition of them. If a feature cannot be written as a composition, that is a
signal the primitive set is wrong — and fixing the primitive set is the correct response, not adding
a mechanism beside it.

---

## 1. What skein is

> **A board of boxes. Each box is a coding agent working on a branch. The board's job is to tell you
> which one needs you.**

That sentence is the mental model, and every surface should reinforce it. A user who understands only
that sentence should be able to predict what any screen does.

One structural change from today: **skein runs inside the sandbox, alongside the boxes it manages.**
It therefore holds no host privilege, and that constraint — not sbx, not containers — is what shapes
the rest of this document.

---

## 2. The primitives

Four. Everything else is composition.

### 2.1 Fact — what skein was told

Durable, declared, never inferred. Repo remotes, box identities, config, credentials, the branch a
box was created for.

- Lives on the durable volume (§5), which is the only thing that persists.
- **One writer.** A fact with two writers is a race with a UI on top.
- Surviving a fleet rebuild is the definition of a fact. If losing it on rebuild would not hurt, it
  is not one.

### 2.2 Signal — what skein can observe

An observation about the world right now. Every signal declares four things, and the declaration is
part of its definition rather than documentation:

| | |
|---|---|
| **kind** | `level` or `edge` |
| **source** | who observed it, and how |
| **observed_at** | when — freshness is never implicit |
| **cost** | what one observation costs (§10) |

**Level** signals are re-readable and self-healing: read the pane, stat the file, connect to the
port. Missing one observation costs nothing, because the next one is authoritative.

**Edge** signals are events, and they are **lossy**: a hook that did not fire, a message that was not
delivered, a process that died before writing. They cannot recover.

The law that this whole primitive exists to enforce, learned the expensive way (`docs/turn-state.md`
— a permission answered at 13:27 still displayed as blocking at 13:47):

> **An edge-triggered latch with incomplete edge coverage cannot recover. A state nobody clears is
> shown forever.**

Therefore:

> **No displayed state may rest on an edge alone.** Every derived state is grounded in a level
> signal. Edges may only *accelerate* it — arriving sooner than the next poll — never be its sole
> basis.

And a corollary that was its own bug: **a signal is keyed by what it is about, not by who observed
it.** Turn state keyed by box but written by session meant any helper process could overwrite the
agent's state — seventeen spurious `ended` events in 114 seconds.

### 2.3 Operation — an intent to change the world

An operation is not a function call. It is four things:

| | |
|---|---|
| **desired** | the fact that should be true |
| **check** | a **level signal** that says whether it is |
| **recipe** | the exact command that would make it true — always present, always printable |
| **doer** | skein performing the recipe — **optional** |

Consequences, all of which fall out rather than being built:

- **Idempotent by construction.** You run an operation by reading its check and closing the gap.
  Running it twice is running it once. `ensure`, never `do`.
- **Crash recovery is free.** Re-run everything; the checks decide what is actually needed.
- **Partial failure is free.** Nothing is left half-done that the next reconcile cannot see.
- **Manual operation is not a degraded path.** It is the same path with the doer removed: skein shows
  the recipe and polls the check.
- **skein can never be blocked without saying what would unblock it**, because the check is *how it
  knows* it is blocked.

Note that the check is a level signal. **§2.2 and §2.3 are the same mechanism pointed at different
subjects** — turn state observes boxes, checks observe infrastructure. One reconciler serves both.

### 2.4 Enter — execution inside a box

The one way to run something in a box: join its namespace and execute.

Both the user and mount namespaces must be joined together — joining mount alone is refused — and
credentials must be preserved or `setgroups` fails for an unprivileged caller. Both details are
load-bearing and were learned from a real box; getting either wrong presents as a permissions bug
rather than a missing flag.

Signals and operations both reach into boxes through this and nothing else.

> **`enter` must never depend on `privileged`.** Reaching a box is precisely the thing that must not
> require host privilege. Any change that couples them has broken the architecture, not just a
> module boundary.

### 2.5 The fact/signal square

Facts and signals are independently true, and **their disagreement is the most useful thing skein
knows**. Every reconciler, and most of what skein needs to say to a user, is one of four cells:

| | signal present | signal absent |
|---|---|---|
| **fact present** | healthy | **an operation is owed** — start it, create it, publish it |
| **fact absent** | **foreign** — real, not skein's; report, never touch | absent, correctly |

This single square replaces a set of features that are currently bespoke: a box that will not start,
a fleet that is missing, a port that was never published, someone else's sandbox appearing on the
board, an orphaned namespace after a crash. Same model, four cells, one implementation.

---

## 3. Features as compositions

The payoff. Nothing below is a mechanism of its own.

| feature | composition |
|---|---|
| the board | current level signals per box, ranked by attention |
| "what needs me" | a predicate over signals |
| turn state | fusion of a level signal (pane grammar) with edges (hooks) that only accelerate it |
| voice, notifications | signal **transitions**, filtered |
| the review queue | signals whose source is GitHub rather than a box |
| `doctor` | every operation's check, reported |
| **onboarding** | failing checks, rendered with their recipes |
| **the blocked state** | *the same thing* — see §8.2 |
| launching a box | an operation whose check is "the namespace is alive" |
| resize | an operation whose check is "reported resources match config" |
| manual mode | operations with the doer omitted |
| foreign detection | the bottom-left cell of §2.5 |
| crash recovery | reconcile every operation |

Two of those are worth pausing on, because they are unifications rather than restatements.

**The review queue stops being a feature.** It is a signal source: observations about pull requests
instead of observations about panes. It gets ranking, staleness, transitions and voice for free,
because those are compositions over signals and it is now producing signals.

**Onboarding stops being a flow.** There is no separate first-run experience to build, and therefore
none to rot. A new user's screen is the blocked state, which is failing checks plus their recipes,
which is the same code that handles a fleet that breaks eighteen months later.

---

## 4. Topology

```
host
  ├── durable volume            the only thing that persists
  ├── sidecar (optional)        performs privileged operations, a human approving
  └── :PORT ──────────────┐     one published port, to the cockpit
                          │
  fleet sandbox           │
    ├── skein ────────────┘     control plane: cockpit, board, signals, operations
    └── boxes                   each a namespace; skein enters them directly
```

One VM. No host-to-guest command path in normal operation.

---

## 5. The durable volume

**The volume is the only thing that persists; everything else is reconstructible.** Destroy the
fleet, recreate it, remount, and nothing is lost. Every other decision exists to keep that true.

| on the volume | |
|---|---|
| `config.json` | settings |
| `repos.json` | the registry: id, remote, default branch |
| `credentials/` | tokens and logins — `0700`, see §9 |
| `boxes/<name>/` | launch spec, conversation, transcript, notes |
| `repos/<id>/mirror` | a bare git mirror, so box clones are local and fast |
| `repos/<id>/store` | the shared `.claude` every box for that repo reads |

Deliberately **not** on it:

- **box checkouts** — VM-local disk is several times faster for build work than a mount, and a
  checkout is reclonable from the mirror in seconds. (Already true today; checkouts were never
  mounted.)
- **namespace anchors** — reachability is a live pid, meaningless across a restart. Persisting it
  would have skein confidently address a corpse.
- **caches and build output** — cheap to rebuild, expensive to sync.

The test for anything new: *if the fleet were destroyed right now, would losing this hurt?*

This is also what makes **resize non-destructive**. Today resize is destroy-recreate-copy. With
durable state on the volume it is recreate-and-remount: nothing to copy, nothing to lose.

---

## 6. Boxes

A box is an **identity** — name, repo, branch, conversation — and its identity is a fact that
outlives every process.

A box **runs** as a bwrap namespace with its own `/tmp` and `$HOME`, anchored by its tmux server. The
server is the honest anchor: the launcher double-forks away, so its pid names a corpse while the box
runs happily.

> **box alive ⇔ tmux server alive ⇔ namespace joinable**

One level signal, read locally and instantly. It replaces listing sandboxes on the host to find out
whether a box exists.

**Repos are remotes.** A repo is a URL; adding one clones a bare mirror onto the volume; a box clones
its checkout from that mirror. There is no adopt-in-place and no mounted working checkout in *either*
deployment — skein never touches the code you are working on.

The cost, stated plainly: **uncommitted work on your host is invisible to boxes.** Push it or it is
not there. That is the change existing users notice first.

---

## 7. Privileged operations

Six, all of them fleet lifecycle:

| operation | check |
|---|---|
| create the fleet | it is listed, and it answers |
| publish the cockpit port | connect; does anything answer |
| mount the durable volume | present and writable |
| resize | reported cpu / memory / disk match config |
| store the push credential | the token resolves |
| destroy the fleet | it is gone |

Doer sources, configurable **per operation**:

- **direct** — skein runs it (host-driven skein, with the tooling in reach)
- **sidecar** — skein asks the host service (§8)
- **none** — recipe and check only

**Nothing in normal operation appears in that table.** Not running commands in boxes, not reading
trees, not terminals, not signals, not the review queue, not the cockpit. A skein that can perform
none of the six still does its entire job, provided someone set the fleet up.

## 8. The host sidecar

A small host service exposing privileged operations. Reachable → doers exist. Unreachable → recipes
and checks. Host-driven and in-fleet skein are therefore not two code paths: both call the sidecar,
one over localhost and one over the sandbox gateway, so the in-fleet path is exercised by everyone
rather than only by those who chose it.

### 8.1 Capabilities are compiled, not configured

**Each capability is its own module, and a sidecar that does not need a capability is built without
it.** Not disabled by a flag, not gated by a permission check — the code is not in the binary.

This is the strongest form of the guarantee available:

| defence | defeated by |
|---|---|
| a runtime permission check | a bug in the check, a path that forgets to call it, a confused deputy |
| a config setting | anything that can write config |
| **absent code** | nothing |

A sidecar built without `destroy-fleet` cannot destroy a fleet through any bug, any injection, any
mistake in an unrelated module, because there is no code path that ends in that call. The class of
accident is removed rather than defended against.

**What is removable and what is not.** Only the **doer** lives in a capability module. Recipes and
checks live in skein, are always compiled, and are never privileged — they are needed *precisely
when* the doer is absent. So a stripped sidecar degrades to "show the command and watch for the
result", which is the ordinary path, not a failure.

**The capability set is advertised, not declared.** At handshake the sidecar reports which
capabilities it holds, and that list is *derived from what is linked* rather than read from a config
— a configured list can be wrong, and a wrong one here means skein waits for a doer that does not
exist. skein renders the difference directly: an operation with no doer shows its recipe, an
operation with one shows an approval button. Same component (§8.3), different affordance.

**The default build has no capabilities.** You opt in, explicitly, at build time. Onboarding does not
need a sidecar at all — recipes and checks carry it — so nothing is lost by making the safe build the
default one, and the list of what a given host's sidecar can do is then a fact someone chose rather
than a default nobody read.

**Capability modules are mutually independent.** No capability may reference another. That is what
keeps the build matrix linear rather than exponential (§15) and what makes each one reviewable in
isolation.

**Where this principle stops.** Compile-time removal is for capabilities that **cross a trust
boundary**. Applying it to ordinary features would produce 2^N build configurations, of which CI
tests two, which is its own defect factory. The privileged operations qualify. Almost nothing else
does.

### 8.2 It is an approval channel, not an execution channel

Even a compiled capability does not run unattended. A host service that creates sandboxes and mounts
host directories, reachable from inside the fleet, would otherwise hand every process in that fleet
the ability to mount `/` into a fresh sandbox and read the machine — and skein shares the fleet with
the boxes, which run coding agents.

So **the sidecar removes the copy-paste, not the human.** It receives a request, shows the exact
command, and does nothing until a human approves. The precedent already works here: a box asks for a
package through the sudo shim and its owner approves it in the cockpit.

Two layers, and they fail independently: a capability that is not compiled cannot be invoked at all,
and one that is compiled cannot be invoked without a person seeing what it will run.

This also dissolves the doer/recipe distinction — the recipe is *always* what a human sees; the only
question is whether approving it costs one click or a terminal window. Rare, consequential operations
are exactly where a human in the loop costs nothing and buys the entire boundary.

A pre-authorised allowlist can come later, narrowly, per capability, with the widening stated. Not in
the first version.

### 8.3 Why this is also the UX

The sidecar's approval card, the onboarding screen, and the "something is broken" screen are **the
same component**: a failing check, its recipe, and a live indication of when it passes. The only
variation is whether a doer exists to offer a button. Build it once, well.

## 9. The credential boundary

The honest cost of §4, which must not be discovered later.

Host skein keeps credentials outside the VM the boxes run in. In-fleet skein keeps them on the
durable volume, **inside** it. A box that escapes its namespace reaches them. That is inherent to
skein sharing the fleet, and it is what the simplicity in §2.4 and §12 is bought with.

Two requirements, not preferences:

1. **`credentials/` is mounted into skein's namespace, not the fleet root.** Boxes cannot reach the
   master credential by walking the filesystem — only by defeating a namespace.
2. **Boxes receive scoped, short-lived tokens** minted per box for what that box needs, never the
   master credential handed down.

Neither makes an escape harmless. They make a namespace escape the *only* way through, which is the
property that can actually be defended and tested.

---

## 10. Cost

A hard constraint, restated by the user twice and measured rather than asserted: **skein must never
be expensive enough to disturb development on the machine it runs on.** The current budget is one
tmux round-trip per box per second, niced, with adaptive backoff, measured at 0.11% of a core.

Therefore every signal declares its observation cost (§2.2), and the board's total is a budget rather
than an emergent property. A signal whose cost is not known is not admissible.

In-fleet mode makes this strictly cheaper: observation no longer crosses a host-to-guest boundary,
so the per-tick cost falls to a local process spawn.

---

## 11. Surfaces

### 11.1 The laws

1. **Never report a problem without the action that resolves it.** A message with no next move is a
   bug, not a message.
2. **Never show an observation without its freshness.** A stale signal must *look* stale. Rendering a
   five-minute-old "waiting" identically to a current one is lying with a timestamp available.
3. **The board answers one question: who needs me.** Everything else on it is secondary and should
   look secondary.
4. **The CLI and the cockpit share one model and one vocabulary.** Anything doable in one is doable
   in the other, named identically.
5. **Destructive actions say what is lost** — including when the answer is "nothing", which the
   volume now makes common and which is worth saying out loud.
6. **No modal onboarding.** Onboarding is the blocked state rendered well (§3, §8.2).
7. **The first screen has exactly one action.** Progressive disclosure or the tool reads as a
   configuration exercise.

### 11.2 The CLI

```
skein                    the board, in the terminal
skein add <url>          register a repo
skein new <repo> <branch>  create a box
skein open <box>         attach
skein doctor             every check; failures print their recipe inline
skein <operation> --show print the recipe, run nothing
```

`--show` is the Operation primitive surfaced directly: any operation can be asked for its recipe
instead of its effect. That is what makes "I want to run it myself" a first-class request rather than
a documented workaround.

---

## 12. What the rewrite deletes

Not a refactor. These stop existing:

- the in-sandbox agent, its transport, its port publishing, its healing loop and backoff — all of it
  exists to survive a hop that no longer happens
- every `sbx exec` path **and its fallback twin**, and with them the transport-failure-versus-
  command-failure distinction that made the pairing necessary and dangerous
- two placement shapes collapsing to one
- sandbox listing as the truth about boxes, and the foreign-sandbox filtering it required
- the machine-global secret store — and the defect where two fleets on one host share one token
- the destroy-recreate-copy resize dance
- host-absolute mount path translation
- adopt-in-place repos and their mounts

---

## 13. Modules

| module | owns | depends on |
|---|---|---|
| `fact` | the volume: config, registry, credentials, box records. Sole writer. | — |
| `signal` | observation: kinds, freshness, cost, fusion | `enter` |
| `enter` | namespace entry — the one way into a box | — |
| `operation` | desired / check / recipe / doer; the reconciler | `fact`, `signal` |
| `sidecar` | client and protocol | `operation` |
| `github` | HTTP client; a signal source | `fact` |
| `box` | identity and lifecycle | `fact`, `operation`, `enter` |
| `server` | cockpit, API, event stream | everything |
| `cli` | `skein` | everything |

`fact` and `enter` depend on nothing. `operation` knows nothing of boxes or GitHub. `enter` never
depends on `sidecar` or `operation` (§2.4).

---

## 14. Rules that keep it clean

Stated as rules because each one is a specific way this codebase has previously accumulated debt.

1. **One writer per fact.**
2. **No dual code paths to the same outcome.** The agent-plus-`sbx exec` pairing had to be kept in
   step by hand, and the two halves disagreeing was a whole bug class.
3. **Every operation is idempotent.** `ensure`, never `do`.
4. **No displayed state from an edge alone** (§2.2).
5. **Every signal is keyed by its subject, not its observer.**
6. **Every signal declares its cost** (§10).
7. **Everything a human might have to do by hand has a printable recipe.**
8. **A new feature is a composition of primitives — or it adds a primitive deliberately, and says
   so in this document.** Anything that is neither is the debt.
9. **A capability that crosses a trust boundary is a module that can be left unbuilt** (§8.1), and
   capability modules never reference one another.

---

## 15. Testing

- **Every check has a test that it fails when the thing is absent.** That is not coverage, it is the
  mutation: a check that passes unconditionally is worse than no check, because it makes a broken
  system report as healthy.
- **Signal fusion is tested as a scenario matrix** — every combination of level and edge, including
  the missing-edge cases that motivated §2.2.
- **Anything the UI computes is a pure function**, tested in node without a browser. The current
  design fails this and it has cost real defects: voice shipped with `waiting` missing from both
  paths because the only test that would have caught it needed a browser, and a browser cannot run
  in a box.
- Browser tests cover the page, and nothing that could have been a pure function.
- **The sidecar is built and tested three ways: empty, each capability alone, and all of them.**
  That is `N + 2` builds rather than `2^N`, and it is sufficient *because* capability modules are
  independent (§8.1) — which makes that rule a testability property, not only a security one. The
  empty build is the one that must never be skipped: it is what most users should run, and it is the
  one whose breakage nobody would notice.

---

## 16. Open

- **Scoped per-box tokens** (§9.2) need a mechanism. App installation tokens are the obvious
  candidate and skein already mints them; the scoping story is unwritten.
- **Is host-driven mode eventually retired?** First-class for now. Ask once in-fleet has run a while
  rather than assuming here.
- **Multiple fleets on one host.** The volume makes it clean; the cockpit port and the sidecar's
  addressing both currently assume one.
