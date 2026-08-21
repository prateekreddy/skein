# Delivering the rewrite

Companion to `docs/architecture.md`, which is the destination. This is how to get there without
destroying a working tool on the way.

## 1. The measurement that should govern the plan

**376 commits since 2026-06-28. 164 are `fix:`, 131 are `feat:`.** Fifty-six percent of the
*conventional-commit* work is fixing what was already built — 164 of 295; against all 376 it is 44%.
Count subjects, not `--grep='^fix'`: `^` anchors at any line start in the body, which inflates it by
three. The denominator is named because the number invites a challenge that would discredit the rest, and the fix titles are not polish:

> *a box could read the fleet agent's token, and be root in the sandbox* · *close the ways a
> credential outlived the decision to withdraw it* · *a logged-out box can no longer log out the
> fleet* · *a published port reaches the sandbox's address, not its loopback* · *one unreadable
> directory blanked every box's disk usage*

A big-bang rewrite reproduces the 131 features and rediscovers most of the 164 fixes. The plan below
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
all four below, plus `skein migrate` and a `VERSION` (`5b49362`), which refuses a volume it does not
understand rather than half-reading it. **What no test here can establish** is the step's own claim:
that a fleet can be destroyed, recreated and remounted with nothing lost. That needs a live fleet.

- **the mount split.** The API token is safe today *because* `~/.skein/repos` and `~/.skein/boxes` are
  bind-mounted into boxes while `~/.skein` itself is not — "checked, not assumed". Mounting a volume
  root whole puts `credentials/`, `api-token`, `github-pats/` and `tokens/` inside every box's reach
  on the shared uid. The cover is an **inversion derived per box** — tmpfs the state
  root, bind back what this box needs — not a list of things to hide (architecture §9.5.2).
  **Done** (`d676d51`, `0158d51`, `9d0dc98`, `0b93ab6`): the cover is derived per box; declared state
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
  **Done** (`f8056a7`, `2644f99`, `04c10d9`, `0b93ab6`): `repos/<id>/mirror` is a bare mirror and is
  what boxes clone from; `codeowners` takes a reader and `moduledocs` reads `repos::Tree`
  (`git show HEAD:<path>`); `diff` had already stopped, when box diffs moved inside the box. The
  trap this bullet does not name, and the one that cost the most to see: **a mirror can never supply
  a gitignored file**, so `shared-paths.txt` — the `.env` and the `CLAUDE.md` a project keeps out of
  git — is not a mirror question at all. Those come from the repo's *source tree*, which is now
  copied into the store on the host, and the checkout is no longer mounted into the sandbox. A repo
  registered from a URL has no source tree at all (`5300c56`), so `repos/<id>/` holds a mirror and a
  store and nothing else.
- **no lock on `config.json`/`repos.json`.** Adding schema versions without a writer discipline
  versions the corruption. **Done** (`48c375d`): the read moved *inside* the lock —
  `update_config`/`update_repos` — because an atomic write makes each write whole and does nothing
  about two writers. The test that proves it has to **count**: a version where each thread writes its
  own distinct field passes against the unlocked code, which is how the first one did.

A caveat on schema versions: per-box status and pane JSON are written by **shell probes generated from
the binary**, so versioning those couples probe version to volume schema to binary version. That is
not a serde attribute and should be scoped deliberately.

**2 — Extract `state`, `source`, `signal` and `operation` as modules in the current binary.**
`signals.rs` is already most of the way there, `util.rs` already implements the gate contract, and
sixteen `ensure_*` functions already exist (fifteen `pub`, plus a private one that is itself a
sandbox-root apt install) — the Operation primitive names something the codebase
does. `doctor` becomes "every check, reported" with no UI change.

**Two structural obstacles hit on day one**, and neither was optional: `lib.rs` re-exported sixteen
modules with `pub use *`, so the module graph carried no information about real edges; and there is a
live `place ↔ fleet` cycle.

Both are dealt with except the cycle. The façade is gone (`8e38964`) — that was the first task of
this step rather than a tidy-up after it — and the crate-root catch-all it exposed went with it
(`6e3944b`): `src/lib.rs` is 58 lines of module declarations, and the ten modules its contents became
are real nodes in a 417-edge graph. The cycle remains, and dissolves with the transport in step 4
rather than needing work of its own.

**3 — Build the warden, and route create/destroy through it from *host* skein.** Both callers
exercised before anything moves — which was the whole argument for having a warden.

**4 — The mount cover, then the uid split, then move in.** In that order, and the first two are gates
rather than follow-ups (architecture §9.5).

**4a — the cover.** `tmpfs` the state root and bind back what a box needs, *derived per box*. This is
most of the value and the cheapest part: it closes a box reading skein's credentials, token and
declared state, and it supplies the secret that lets the warden tell skein from a box. It also closes
the one that is live today and that nothing in the current cover reaches — `~/.skein/repos` is mounted
into the sandbox uncovered, so every box has read-write access to every repo's store and to the host's
own working checkouts, and skein runs `git -C <repo.work>` **on the host**.

**4b — the uid split.** skein on its own uid, boxes on theirs, every crossing through
`sudo -u <box uid>` — for the launcher and for `nsenter` alike. Do not attempt this as "skein runs as
root" (no user namespace is created at all) or as "skein runs as another uid" (every `setns` is
EPERM); architecture §9.5.1 has the derivation.

**Do not start 4b until the anchor moves.** skein crosses to `/proc/<anchor>/ns/user`, and the anchor
is currently read from a file inside the box's own writable root — so a box picks the namespace skein
lands in. Everything else in 4b is downstream of an address it trusts. Budget the sudoers policy as the security-critical
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
| R3 control API on a covered socket | move the control plane off a TCP port | independent of 4b |
| R4 no shared writable executable path | read-only toolchains with a per-box overlay | **user-visible** — one box's `cargo install` stops reaching the others, and the shared build cache goes. In `docs/parity.md` §7. |
| R5 warden secret under the cover | falls out of 4a | |
| R6 audit log, warden-written, host-side | new: the sink endpoint, and skein reporting into it | never compilable-out |
| R7 credentials never win on self-asserted freshness | ~~replace the expiry comparison~~ — **done**: the comparison could not be fixed, the *direction* was | The expiry is a field inside a file a box writes, and a box legitimately holds the refresh token — so nothing it can produce honestly it cannot also produce dishonestly, and no field in that file is evidence about it. The fleet's login now flows **down only**; a box's reaches the fleet solely when the fleet has none, where there is nothing to poison. Cost, stated: a token refreshed in a box no longer improves the fleet's copy, which ages until `skein login`. |
| R8 no privileged actor follows a box-influenced path | the resize archive, `git-tokens/`, `disk`/`identity`, and the anchor | the largest of the nine; several distinct sites |
| R9 workshop toggle states its terms | wording plus a per-start banner | already half-built |
| R10 cross-box messaging renders provenance | inbound-from-a-box distinguishable from inbound-from-you | §9.2.2 is kept, so this is the mitigation |
| R11 `/run` covered, or its exposure stated | decide which | the per-user socket directory is the live case |

R5, R7 and R9 are small. R4 is a product decision as much as a security one. R8 is a cluster, not an
item.

**Which of these gate 4c**, since 4c is the point of no return and the rest is a backlog:

| must land before 4c | may follow |
|---|---|
| **R3** — the control API is on a TCP port today, and moving in is what makes that reachable from every box | R4 (a product decision, and the exposure is unchanged by the move) |
| **R5** — the warden cannot tell skein from a box without it, and after the move it must | R9, R10, R11 |
| **R6** — skein cannot audit itself once it shares a sandbox with the agents | |
| **R7, R8** — both are live today and the move puts skein's own state inside their blast radius | |

So 4c is gated on five, not nine.

**4c — the move**, with host-driven mode still working one environment variable away. This is where
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
- **the asset layer is compile-time.** Five `include_str!` and a shared `static_asset` helper behind
  four `/vendor/*` routes — so the machinery exists, but every file is embedded and hand-registered. A
  built `/v2` bundle needs a **runtime** asset route: smaller than building one from scratch, and real
  either way. Do it first, or every UI change rebuilds the binary and "orthogonal" is untrue. Treating "ground-up surfaces" and "new topology" as one project is the single biggest
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

An earlier draft said "refuse when any box has unpushed commits **or a dirty tree**". Every actively
worked box has a dirty tree, so that rule refuses always — and it sat *after* the snapshot, which is
the thing that makes a dirty tree survivable in the first place. The rule matching this codebase's
actual instinct is point 5: `--ignore-failed-read` is banned precisely so a copy that cannot read
everything stops rather than restoring a box short of its contents with nobody told.

## 5. Landmines

Where the current code encodes knowledge a rewrite pays for twice. Each presents as an intermittent
mystery rather than a clean failure.

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
