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

Four, and two of them dissolve: the credential seed becomes a file write once credentials live on the
volume, and port publishing folds into create **only if the cockpit is the sole port**. That is what
survives of the collapse to two — it was right *for this domain* and was wrongly stated as covering
everything.

### 1.2 Sandbox root — used constantly, in normal operation

`grep -c "sudo " src/box-session.sh` → **21**. The distinct call sites:

| when | what | where |
|---|---|---|
| **every box start** | create the box's cgroup, enable `+memory +pids` | `box-session.sh:86-87, 860-862` |
| **every box start** | write `memory.max`, `memory.high`, `pids.max` | `:871-873` |
| **every box start** | move the session into its cgroup | `:877` |
| **every server start** | write cgroup ceilings for every box | `fleet.rs:904` |
| **every server start** | replay the approved-package manifest as root | `fleet.rs:1625-1628` |
| **every server start** | create and chown the fleet root | `fleet.rs:1876` |
| fleet setup | write `/etc/docker/daemon.json` | `fleet.rs:1053` |
| **resize** | `tar` the whole box tree, and restore it | `fleet.rs:3171, 3205` |
| **on approval** | `apt-get install` the approved packages | `substrate.rs:236-237` |

So the honest statement is: **normal operation is full of sandbox-root work.** Cgroups on every box
start, package replay and ceiling healing on every server start.

### 1.3 What this means for the design

The decision already taken — **skein runs as root inside the fleet sandbox** — is not only a security
measure. It *supplies this entire domain directly.* Today these operations are reached by
unprivileged processes through a `sudo` shim; with skein as root they are ordinary calls, and the
shim's remaining job is what it was always documented to be: **a message, not a boundary.**

Three kinds of sandbox-root work, and each wants naming in the architecture rather than eliding:

- **resource ceilings** — cgroups, per box start and per server heal
- **package installation** — apt, on approval and on replay
- **filesystem ownership** — the fleet root, and box archive/restore

Note the last one contains the resize mechanism, which is the next finding.

---

## 2. Resize does not do what the architecture says

`grep -rn "snapshot_box" src/ tests/` → **no production caller.** Two test call sites only.

The architecture describes resize as carrying "the delta — unpushed commits, index and worktree
patches, untracked files" and claims "that is what today's snapshot already does". It is not.

Real resize is `sudo tar -cf` of the entire `/boxes/<name>` tree and `sudo tar -xf` to restore
(`fleet.rs:3171, 3205`) — a root byte copy including `.git`, `node_modules`, `target`, the private
HOME and `/tmp`, which is why it demands 1.2× the box size free before starting.

`fleet.rs:3122` records the move away from the bundle-and-patches approach deliberately: *"the
reconstruction is slower, less faithful, and it is where the fragility lives."*

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

"Declared state has one writer" is a goal, not a description. And the split into *declared* and
*recorded* is not exhaustive: the package queue and the git-write queue are **durable, written by an
untrusted party (a box), and read by the approving side.** That is a third kind — call it
**requested** — and giving it no rules is what produced the defect in §4.

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
| 3 | edge is newer than the sample **and** is an outcome | the edge overrides the level |
| 4 | `Screen::Unknown` | the edge alone |

The architecture says "no displayed state may rest on an edge alone". The code is **right** and the
law is too absolute — rule 1 with nothing shown would be worse, and rule 3 is simply the fresher
observation winning.

What makes the code honest is the shipped provenance display: `hooks only`, `screen lost`,
`screen unread`. So the law should be:

> **No displayed state may rest on an edge alone *without saying so*.**

Which the parity gate already requires, and the first draft's absolute version forbade.

---

## 6. The module graph carries no information

`src/lib.rs:35-50` is `pub use <mod>::*` over sixteen modules, so almost every cross-module reference
goes through the flat root namespace rather than a module path. `grep -rn "crate::signals::"` from
other modules returns **0** — not because nothing uses it, but because everything uses the
re-exports.

There is also a live cycle: `place.rs:309` calls into `fleet`, and `fleet.rs:19` imports `place`.

**Any module graph drawn today is aspiration.** The architecture's dependency rules cannot be checked
against the code until the façade is removed, and removing it is the first real task of any
extraction step — not a tidy-up after.

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
