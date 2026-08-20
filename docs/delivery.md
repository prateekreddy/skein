# Delivering the rewrite

Companion to `docs/architecture.md`, which is the destination. This is how to get there without
destroying a working tool on the way.

## 1. The measurement that should govern the plan

**343 commits since 2026-06-28. 164 are `fix:`, 131 are `feat:`.** Fifty-six percent of the work in
this codebase is fixing what was already built, and the fix titles are not polish:

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

**1 — The durable volume, on the current codebase.** The highest-value idea in the architecture and it
needs no rewrite. Move declared and recorded state onto a mounted volume; add schema versions. Every
claim that follows — rebuild survival, non-destructive resize, no machine-global secret — falls out,
and all of it is testable today.

**2 — Extract `state`, `source`, `signal` and `operation` as modules in the current binary.**
`signals.rs` is already most of the way there and `util.rs` already implements the gate contract.
`doctor` becomes "every check, reported" with no UI change. Checks ship and are tested one at a time.

**3 — Build the warden, and route create/destroy through it from *host* skein.** Both callers
exercised before anything moves — which was the whole argument for having a warden.

**4 — The privilege split, then move skein into the fleet.** In that order, and the split is a gate
rather than a follow-up: skein runs as root inside the fleet sandbox and boxes stay unprivileged
(architecture §9.3.2). Until it lands, a box can signal the control plane, read its files and write
the cgroup plane, so moving in first and hardening after would mean shipping a window in which every
one of those is open.

Then the move itself, with host-driven mode still working one environment variable away. This is
where the six items in §2 get answered, with a fallback available while answering them.

**5 — The cockpit.** Orthogonal, and it can start on day one: the API is a stable seam, so the new
cockpit ships at `/v2` against the *existing* server and cuts over when `docs/parity.md` is
satisfied. Treating "ground-up surfaces" and "new topology" as one project is the single biggest
avoidable risk in the plan.

**What runs in parallel from the start:** the component library, the GitHub module (already
self-contained since `gh` was dropped), and the warden's two capability modules.

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
4. **refuse to run when any box has unpushed commits or a dirty tree, naming them**;
5. leave the old state directory intact.

Point 4 is not optional. Without it this destroys someone's week, and `fleet.rs` already contains
four separate places where the authors learned to refuse rather than warn.

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
The bundle must be checked for the box's own branch. tar preserves symlinks as symlinks. Not
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

**Memory and cgroups.** The reserve is 574 MiB measured, of which 191 MiB is unreclaimable kernel
memory. `memory.high` sits deliberately below `memory.max` so an overshooting box gets slow rather
than killed. The cgroup must be removed only after the tmux server is gone, or a box later given the
same name inherits the old limits.

**Terminal responsiveness.** Every library call in the server is on a blocking-safe executor, guarded
by a named regression test, because the fleet tick run inline caused typing to lag *only when the box
was idle*.

## 6. Underspecified — settle before two engineers build incompatible things

- **Signal schema**: identity/key, value type, how cost is expressed, who enforces the budget, and
  what "fusion" is as a function. §13 of the architecture demands a scenario matrix for a function
  nobody has defined.
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
