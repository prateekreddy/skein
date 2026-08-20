# skein, in the fleet

The architecture for a clean rewrite. Decided 2026-08-20.

This describes the target whole rather than a path from what exists. The root `ARCHITECTURE.md`
still describes the current system and stays accurate until this replaces it.

---

## 1. What changes, in one sentence

**skein moves inside the sandbox and stops holding host privilege.**

Everything below follows from that. The first framing of this work was "decouple skein from sbx",
and it was wrong — abstracting the substrate would have produced a trait, two implementations, and
no answer to the actual question. Moving skein into a sandbox does not take away `sbx`. It takes away
**privilege**, and privilege is the axis the design has to be organised around.

## 2. The shape

```
host
  ├── durable volume            the only thing that persists
  ├── sidecar (optional)        performs privileged operations, with a human approving
  └── :PORT ──────────────┐     one published port, to the cockpit
                          │
  fleet sandbox           │
    ├── skein ────────────┘     control plane: cockpit, board, review queue, orchestration
    └── boxes                   each a namespace; skein enters them directly
```

One VM. skein is a process in it. Boxes are namespaces beside it. There is no host-to-guest command
path in normal operation at all.

## 3. The central invariant

**The durable volume is the only thing that persists. Everything else is reconstructible.**

Destroy the fleet, recreate it, remount the volume, and skein is back with nothing lost. Every other
decision here exists to keep that true.

On the volume:

| | |
|---|---|
| `config.json` | skein's settings |
| `repos.json` | the registry: id, remote URL, default branch |
| `credentials/` | GitHub token, agent logins — `0700`, see §9 |
| `boxes/<name>/` | launch spec, conversation, transcript, signals, notes |
| `repos/<id>/mirror` | a bare git mirror, so box clones are local and fast |
| `repos/<id>/store` | the shared `.claude` every box for that repo mounts |

Not on the volume, deliberately:

- **box checkouts** — VM-local disk, measurably several times faster than a mount for build work, and
  reclonable from the mirror in seconds. (This is already how skein works; checkouts were never
  mounted.)
- **namespace anchors** — a box's reachability is a live pid. It is meaningless across a restart and
  must not be persisted, or skein will confidently address a corpse.
- **caches, build output, package state** — cheap to rebuild, expensive to sync.

The test for anything new: *if the fleet were destroyed right now, would losing this hurt?* Yes goes
on the volume. No stays out. The invariant is worth more than any individual convenience.

## 4. A box

A box is an **identity**: a name, a repo, a branch, a conversation. Its identity lives on the volume
and outlives every process.

A box **runs** as a bwrap namespace with its own `/tmp` and `$HOME`, anchored by its tmux server.
The server is the honest anchor — the launcher double-forks away and its pid names a corpse while the
box runs happily, so:

**box alive ⇔ tmux server alive ⇔ namespace joinable.**

One condition, checked locally, instantly, with no sandbox listing and no remote call. That replaces
the current arrangement, where "does this box exist" was answered by listing sandboxes on the host.

## 5. Reaching a box

`nsenter` into the anchor. That is the whole mechanism.

Both the user and mount namespaces must be joined together — joining the mount namespace alone is
refused — and credentials must be preserved or `setgroups` fails for an unprivileged caller. Those
two details are load-bearing and were learned the hard way; getting either wrong looks like a
permissions bug rather than a missing flag.

What this removes is worth naming precisely, because it is most of the complexity in skein today:

- no `sbx exec`, so **no stalls to defend against**;
- therefore **no in-sandbox HTTP agent**, no port to publish for it, no healing loop, no backoff, no
  "did the script run or not" ambiguity between transport failure and command failure;
- **no fallback path**, so no pairs of code that must stay in step;
- terminal attach is a local tmux attach.

The current transport exists to survive a hop that no longer exists. It is not being ported.

## 6. Repos are remotes

A repo is a URL. Adding one clones a bare mirror onto the volume; every box clones its checkout from
that mirror onto VM-local disk.

There is no adopt-in-place, and no mounting of a working checkout — in **either** deployment. That is
the point for someone who did not want skein on their machine: skein never touches the code you are
working on. It also collapses what would otherwise be two repo models to maintain forever.

The cost, stated plainly: **uncommitted work on your host is invisible to boxes.** Push it, or it is
not there. That is a real behavioural change from adopt-in-place and the one thing existing users
will notice first.

## 7. Privileged operations

Every privileged thing skein does. There are six, and all six are fleet lifecycle:

| operation | check |
|---|---|
| create the fleet | it is listed, and it answers |
| publish the cockpit port | connect; does anything answer |
| mount the durable volume | the path is present and writable |
| resize | reported cpu / memory / disk match config |
| store the push credential | the token resolves |
| destroy the fleet | it is gone |

Each has three parts:

- a **recipe** — the exact command, with its environment. Always present, always printable.
- a **check** — can skein see the result? Always present, cheap, safe to poll.
- a **doer** — skein performing it. **Optional.**

With a doer, skein runs the recipe. Without one, skein shows the recipe and polls the check until it
passes. Manual operation is not a degraded path; it is the same path with the doer removed.

Three doer sources, configurable **per operation**:

- **direct** — skein runs it (host-driven skein, `sbx` in reach)
- **sidecar** — skein asks the host service (§8)
- **none** — recipe and check only

And the property that makes this worth building: **skein can never be blocked without being able to
say what is missing and what would fix it, because the check is how it knows it is blocked.** The
current code fails this — a missing fleet produced `skein box <name> doesn't exist` and nothing more.
That was a missing check-with-recipe, and this is the general form of the fix.

Note what is *not* in the table: running commands in boxes, reading trees, attaching terminals,
watching signals, the review queue, the cockpit. None of it needs privilege. **A skein that can
perform none of the six can still do its entire job, provided someone set the fleet up.**

## 8. The host sidecar

A small host service exposing exactly those six operations. Reachable → doers exist. Unreachable →
recipes and checks.

This collapses a distinction that looked fundamental: host-driven and in-fleet skein are not two code
paths. Both call the sidecar — one over localhost, one over the sandbox's gateway — so the in-fleet
path is exercised by everyone, not only by the people who chose it.

### It is an approval channel, not an execution channel

A host service that creates sandboxes and mounts host directories, reachable from inside the fleet,
hands every process in that fleet the ability to mount `/` into a fresh sandbox and read the machine.
skein shares the fleet with the boxes, and the boxes run coding agents.

So **the sidecar removes the copy-paste, not the human.** It receives a request, shows the exact
command, and does nothing until a human approves. The precedent already works in this codebase: a box
asks for a package through the sudo shim and its owner approves it in the cockpit.

This also dissolves the doer/recipe distinction — the recipe is *always* what a human sees. The only
question is whether approving costs one click or a terminal window. Six rare, consequential
operations is exactly where a human in the loop costs nothing and buys the entire boundary.

A pre-authorised allowlist for unattended operation can come later, narrowly, with the widening
stated. Not in the first version.

## 9. The credential boundary

The honest cost of §2, and it must not be discovered later.

Today, host skein keeps credentials on the host, outside the VM the boxes run in. In-fleet skein
keeps them on the durable volume, **inside** that VM. A box that escapes its namespace reaches them.
That is a real weakening, and it is inherent to skein sharing the fleet — it is what §1 buys the
simplicity with.

Two requirements follow, and they are requirements rather than nice-to-haves:

1. **`credentials/` is mounted into skein's namespace, not into the fleet root.** Boxes cannot read
   the master credential by walking the filesystem, only by defeating a namespace.
2. **Boxes get scoped, short-lived tokens** minted per box for what that box needs — not the master
   credential handed down.

Neither makes a namespace escape harmless. They make it the *only* way through, which is the
property that can actually be defended.

## 10. Bootstrap

**Creating the fleet.** A human runs one command, or approves it through the sidecar. It carries the
sizing, the volume mount, and one published port. skein prints it; skein does not require itself to
have run it.

**Starting skein.** The sandbox's own startup hooks. A sandbox restart restores skein with no host
involvement — which is the answer to "what if it dies": restart the sandbox.

One wrinkle to expect rather than debug: `sbx create` returns *before* durable startup hooks finish,
so immediately after a create the cockpit is not up yet. The check reports that as **starting**, not
broken.

**First run, with an existing host skein.** If `~/.skein` is present on the host, skein imports from
it once: settings, credentials, and repo **remotes**. Repos come across as URLs to clone, never as
mounts. After the import the volume is authoritative and the host directory is never read again —
two skeins writing one state is not a supported shape, and the import is deliberately one-way and
one-time.

**First run, with nothing.** The cockpit comes up empty. A token goes into Settings, repos are added
by URL. Nothing secret in the create command, nothing in shell history.

## 11. The control plane

One process in the fleet: the cockpit and its API, the board and its event stream, the review queue,
box orchestration, signal watching.

GitHub is HTTP with a token — no `gh`, no external binary, no keyring. That is already true and
carries over unchanged.

Reachability is one published port to the cockpit. It is the only inbound path to the fleet.

## 12. What the rewrite deletes

Not a refactor. These stop existing:

- the in-sandbox agent, its transport, its port publishing, its healing loop and backoff
- every `sbx exec` path and its fallback twin, and the transport-failure-versus-command-failure
  distinction that made the pairing necessary
- two placement shapes (per-VM and shared) collapsing to one
- sandbox listing as the truth about boxes, and the foreign-sandbox filtering it needed
- the machine-global secret store — and with it the defect where two fleets on one host share one
  GitHub token
- the destroy-recreate-copy resize dance: with durable state on the volume, resize is
  recreate-and-remount, with nothing to copy and nothing to lose
- host-absolute mount path translation
- adopt-in-place repos and their mounts

## 13. Module boundaries

| module | owns |
|---|---|
| `state` | the volume: config, registry, credentials, box records. The only writer. |
| `box` | identity and lifecycle: create, start, stop, destroy |
| `enter` | namespace entry — the one way to reach a box |
| `privileged` | the six operations: recipe, check, doer |
| `sidecar` | client and protocol |
| `github` | HTTP client, review queue |
| `server` | cockpit, API, event stream |
| `cli` | `skein` |

The dependency rule: `state` depends on nothing, `enter` depends on nothing, and `privileged` knows
about neither boxes nor GitHub. Anything that makes `enter` need `privileged` is a design error —
reaching a box is exactly the thing that must never require host privilege.

## 14. Open

- **Is host-driven mode eventually retired?** It stays first-class for now. Ask again once in-fleet
  has run for a while, rather than assuming an answer here.
- **Scoped per-box tokens** (§9.2) need a mechanism decided — GitHub App installation tokens are the
  obvious candidate and skein already mints them, but the scoping story is not written.
- **Multiple fleets on one host.** Nothing here forbids it and the volume makes it clean, but the
  cockpit port and the sidecar's addressing both assume one.
