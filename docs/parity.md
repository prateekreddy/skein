# Parity gate

**The rewrite is not done when it works. It is done when it does everything the current
implementation does.** `docs/architecture.md` governs *how*; this governs *what*.

## How this was audited, and how to re-audit it

The first version of this list was written from prose recollection. It contained three capabilities
that **do not exist** (`download`, a `max` subcommand, an "adopted-module surface"), omitted the
CLI's default command, and missed roughly fifty-five real capabilities. A list with fabricated
entries cannot function as a gate, because the absence of an item then carries no information.

This version is derived mechanically:

```sh
grep -n '\.route("' src/bin/skein-server.rs          # 67 routes — 7 are multi-line
sed -n '25,90p' src/bin/skein.rs                      # subcommands and flags
grep -oE 'id="[a-zA-Z0-9_-]+"' src/web/index.html     # 155 elements
grep -c 'function ' src/web/index.html                # 257 functions
```

Re-run these before declaring parity. **A grep for words is not an audit** — that is exactly how the
first version acquired a `download` feature that has no route, no UI and no function.

**The rule:** an item leaves this list only by moving to §7 with a reason. Never by being forgotten.

---

## 1. Triage

- The board, its ranking, and **collapsible per-repo grouping** with counts, persisted collapse
  state, and per-group pull and new-box actions.
- Live updates over the event stream; the live/reconnecting pip and a manual reconnect action.
- Attention states with four decision kinds — permission · question · trust · auth-or-quota — and
  the four-way pause classification (needs-input / fork / proceed / statement) with its ranking.
- **Signal provenance display**: the half-filled dot, and `hooks only` / `screen lost` /
  `screen unread`. This is the architecture's own level-versus-edge thesis, already shipped.
- Per-box task line; search and filters; the needs strip.
- **The away digest** — snapshot on `visibilitychange`, per-box deltas on return (needs a decision,
  paused for you, finished with line count, back to work, +N lines, started, left the board),
  priority-sorted, each row clicking through.
- Fleet gauges: load, resources, limits. **Per-box load view** (which box is eating CPU, memory,
  disk, ranked) and the per-box resource hover card and disk chip.
- **Continue N** — batch resume of boxes classed proceed, with an AI safety gate that can only ever
  *add* a hold.
- Alerts, toasts, **favicon badge, document title count, audio beep**.
- Voice: mouth and ear, including *read what needs me*.
- Command palette; mobile on-screen key bar; **resizable sidebar gutter** with persisted width and a
  docked rail mode.
- `?demo` mode — a seeded board. Needed for design work on the component library.

## 2. Converse

- **Two session kinds per box — agent and shell.** Not one terminal.
- **The tabbed session dock**: multiple concurrent live terminals, persisted across reload,
  drag-to-reorder with order remembered, `⌥1`–`⌥9`, `⌥[`/`⌥]`, `⌥⇧[`/`⌥⇧]`, close-all, cycling.
- **Attachments**: paste a file, drag-and-drop onto a board row or the dock, or the button; folders
  keep structure; capped at 200; streamed not buffered; one batch directory per drop; the in-box path
  is pasted into the live terminal.
- **Terminal clipboard** — `⌘C`/`⌘V` inside the keydown gesture; `⌃C` deliberately never intercepted.
- Session lifecycle: start, stop, restart the agent, resume, **resume in batch**.
- **Session digest** — "what happened here": blocked-on, last message, journal, commits, diff
  summary, assembled from free signals with no model tokens. Distinct from `narrate`, which is the
  paid version.
- **Takeover is a runtime migration**, not an attach: it snapshots the box, creates a *replacement*
  box on the other runtime with a generated handoff brief, and keeps the original as rollback.
- **Update rules / refresh docs** — re-apply work-tracking documents that are older than the store's,
  rewriting only documents the box has not edited; shift-click widens it.
- Transcript reader; statusline; runtime selection; `skein attach --handoff` / `--agent`.

## 3. Review

- Diff, with **provenance**: the base ref is named, and when the box is down the last-turn patch is
  shown *and labelled stale*.
- **Files tab with fallback**: reads the box's own tree; when the box is down it falls back and
  **labels the source**; markdown with relative-link navigation; README auto-opens; images inline;
  symlink escape guarded. See §7 — the fallback's current target is being removed and needs a new one.
- **Inline diff comment composer**, comments interleaved between diff lines, `⌘↵` to save.
- The pull-request queue. **Six actions, not one**: approve, request-changes, comment, **merge**,
  **ask** (Q&A against the PR), **draft** (model-drafted comment). Merge is destructive and must not
  hide behind a verb.
- **CODEOWNERS parsing and ownership attribution**, including the gitignore-anchoring rule and the
  fact that team-requested reviews are not returned by `review-requested:@me`.
- **Contract signals** — a mechanical diff scanner that escalates a PR the model called boring,
  capped and deduplicated, deliberately non-redundant with the AI summary.
- Review filter chips: all / author / reviewer / mentioned.
- **Blind-spot reporting** — the queue states what it could not see rather than under-reporting.
- Summary caching keyed by head SHA; a parallelism throttle so summaries do not take the rate-limit
  window from working boxes; the count poll deliberately off the board tick.
- Archive; review counts.
- **Standing module notes**, including the freshness model: each note records the commit its module
  was at, and a stale note is never used.

## 4. Box and fleet management

- Create, destroy with confirmation, stop, repin, per-box disk allowance, identity, git scope,
  tracking, notes, sync provisioning and refresh.
- **The package-request queue** — a box asks for apt/npm, the owner approves, and the approval is
  **remembered in a manifest replayed into every future launch**. An approval system with an install
  path, not a setting.
- **The git-write-request queue** — the git shim intercepts a push the box is not scoped for and
  files a request; grants are hour-limited, revocable, displayed live or expired; plus the credential
  probe.
- Fleet create with sizing, resize, plan and host capacity, resource and limit editing, transport and
  substrate reporting, GitHub credentials, read token, health.
- **`ensure_probe_all` / `ensure_kit` / fleet healing** — skein installs 19 probe scripts and hook
  wiring into every registered repo's store on every start, and repairs a running fleet to match the
  binary. **Without these there is no turn state at all.**

## 5. Cross-cutting

- **API authentication** — the bearer token, the gate middleware, `?t=` → HttpOnly SameSite=Strict
  cookie → redirect stripping the token from the URL, and the no-token landing page. This exists
  *because a box reached the host cockpit*. In-fleet skein needs it more, not less.
- **WebSocket Origin guard** (DNS rebinding), the Tailscale ULA allowance, `$SKEIN_ALLOWED_ORIGINS`,
  and the off-loopback bind warning.
- The event stream; mailbox including **broadcast to all boxes** and the **cross-project relay**
  (host-side, because a box only mounts its own project's store).
- Settings: seven panes, ~45 controls, save-on-blur, unsaved-changes indicator, diagnostics pane,
  host-capacity measurement, and refusal to save when config is unparseable.
- `.env` loading, with a malformed file reported rather than silently truncated.
- Sync connections and their tokens.
- **SSH key handling** — loads a host key into the host ssh-agent, which is forwarded into boxes.
  See §7.
- **`/api/pick-path`** — the native host folder/file picker. See §7.
- CLI: `ls`/`status` (**the default command**), `add`, `repos`, `remove`/`rm`, `doctor`, `shared`
  (including `shared import <box> [--include] [--apply]`), `start`, `login`, `resize`, `attach`,
  `version`, `help`; flags `--branch --agent --attach --id --store --include --apply --disk
  --drop-docker`.
- **`--drop-docker`**: resize *refuses* rather than warns when Docker holds unpushed images, and
  refuses on "could not ask" too.

---

## 6. Items listed here that have no UI caller

`repin` is API- and CLI-only. Listing it as parity overstates the gate; it is kept because removing
it should still be a decision.

## 7. Deliberate removals and forced changes

Each is a decision, with its cost stated in the user's terms.

**Adopt-in-place → local-path remotes.** A local filesystem path is a valid git remote, so a repo with
no server still works: skein clones it into the mirror and fetches from your path. What is lost is
**visibility of uncommitted work in your host checkout** — you commit, not push, and skein fetches.
Consequences that follow and must be built, not assumed: `diff`, `moduledocs` and `codeowners` read
the working checkout directly today and must repoint at the mirror; in-fleet skein cannot reach host
paths at all, so a local-path remote is host-driven only unless the mirror is seeded at import.

**Foreign sandbox display.** The board's rows for sandboxes skein did not create, and the `foreign:`
filter. That feature mitigated skein listing every sandbox on the host; the rewrite does not list
sandboxes, so the confusion cannot arise.

**`/api/pick-path`.** The native host picker needs a host process with display access, which in-fleet
skein cannot have. It backs the Browse buttons for adding a repository, the shared-data folder and
the SSH key path. Replacement needed, not just removal — typing an absolute path into a text field is
a worse first run and law 7 forbids it.

**The host ssh-agent path.** In-fleet skein has no host ssh-agent to load a key into. SSH remotes
either move to the warden or to HTTPS with injected credentials.

**Transport reporting.** The first draft listed this as parity *and* deleted the transport. The
transport goes; the readout goes with it. This is recorded here because §12.2 of the first draft
claimed no user-visible feature hid in the deletion list, and this was the counter-example.

**Resize discards nothing, but it does copy.** Corrected from the first draft's claim of
"nothing to copy": the checkout is reclonable from the mirror, but uncommitted work is not, so resize
carries unpushed commits, index and worktree patches, untracked files, and deliberately-swept ignored
files. If that machinery is not built, resize destroys every box's uncommitted work — which would be
a larger removal than everything else on this page combined.

---

## 8. Known gaps in this audit

Stated so the next reader knows what has not been checked, rather than inferring completeness:

- The 257 JavaScript functions were sampled, not enumerated one by one.
- Keyboard shortcuts are listed where found in the dock and palette; there is no single place they
  are declared, so the list may be short.
- Per-repo and per-box settings were counted from the UI, not cross-checked against every `Config`
  field — `Config` has 24 fields and the UI exposes more controls than that.
