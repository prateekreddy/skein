# skein — product roadmap

*From a 4-track audit (hooks reliability · web UI · core lib/server · product/DX) run 2026-07-05.
This file is the shared source of truth for "what's next"; strike items as they land.*

**The thesis:** skein should be the place a dev spends their whole working day — an attention
inbox over a fleet of agent sandboxes, with the reading, reviewing, and shipping loops all
in-cockpit. Two things gate that: signals must be **trustworthy** (hooks that always report, or
visibly say why not), and the daily loops must not **force exits** (docs, files, transcripts, CI).

---

## Shipped in the 2026-07-05 wave

**Hooks/reliability (the "hooks mostly don't work" package):**
- `box-session.sh` now exists and is wired (Stop + Notification): the headline / fork-detector /
  session-digest pipeline finally has a writer. (It read a file *nothing wrote* since inception.)
- Kit: a repo that ships its own `.claude/` no longer silently skips store linking — skein merges
  (links `skein/` in, merges hook wiring into the repo's settings.json) and every probe resolves
  the real store through the `skein/` symlink. Boot report written to `<store>/skein/boot/`.
- Every hook command is now `bash "<script>" …` (immune to exec-bit squashing on the mount);
  old bare-path entries are retired on upgrade (no double-firing).
- Hook heartbeats: probes append to `<store>/hook-log/<vmid>.jsonl` first thing; a Running box
  that has *never* reported gets a red "⚠ no signals" badge in the cockpit (`hook_health`).
- Registry writes from inside boxes are now same-filesystem atomic renames (no more torn
  `sandboxes.json`); `refresh_branch` also stamps `lastSeen`.
- Stale-busy demotion: a "working" claim older than 45 min degrades to liveness-derived state —
  a crashed agent can no longer look busy forever.
- `sbx ls` bounded by a 5s timeout + 1.5s micro-cache; `repos.json` micro-cached (was hundreds of
  reads/tick); `write_atomic` temp names are per-call (no same-dir clobber).
- `branch_of` (feeds **PR create/merge**) now uses the same launch-spec-aware cascade as the
  board — a clone-mode box can no longer merge the wrong branch.

**Files tab (the #1 forced exit):** per-box file browser over the host workspace — rendered
markdown (vendored marked.js, raw HTML escaped), inline images, plain-text everything else,
README auto-open, relative-link navigation, traversal/symlink-escape hardened (tested).

**UI wave 1:** streaming-flag DOM churn fixed (typing latency during agent output); dockbar +
ship status refresh with the 2s tick (45s TTL); error states join every attention pathway
(count/next-needs-you/notifications); notifications click-to-focus, fire on `!hasFocus()` (second
monitor), no restore-burst, audible beep (AudioContext primed in the click gesture); keyed tab
reconcile + positional row moves (no more selection/animation churn every tick); title no longer
lies after refocus; per-box one-click continue on the `proceed?` chip; staleness banner when the
SSE feed goes quiet; `confirm_destroy` setting actually honored; duplicate ✨ summary fixed;
hover-vs-keyboard selection fixed.

---

## P0 — trust the board (reliability leftovers)

- [x] **Explicit jq probe contract.** `box-task.sh`, `box-token-usage.sh`, registration, and `mailbox.sh`
  still silently no-op without jq, and jq's only installer is apt through a default-deny network.
  Either bundle a fallback parser path per script or make the kit's jq failure loud in the boot
  report + cockpit. Skein now installs jq by default, removes apt indexes afterward, and reports a
  failed install through the boot record + `/api/health` banner. (hooks audit #3)
- [x] **Notification payload belt-and-braces.** Read `notification_type` from the payload inside
  `box-status.sh` with one unconditional entry as fallback, so needs-input survives Claude Code
  versions whose matcher semantics differ. (hooks audit #4)
- [x] **Stale-session detector.** Compare a settings-config hash echoed by the SessionStart
  bootstrap into the boot report against the current store hash; badge "N boxes running an older
  probe config" + one-click agent-session restart (kill tmux `skein-agent`; reattach recreates).
  (hooks audit #5)
- [x] **`/api/health` + cockpit environment banner.** Surface probe-install status, registry
  parse health, mailbox relay errors, sbx/gh presence (doctor-over-HTTP). The server is detached;
  today every startup failure is an eprintln to nowhere. (core audit F9, DX audit §5)
- [x] **Resume that can't lie.** `resume_box` reports ok when the detached shell forks, not when
  the agent resumes; log stderr to `<store>/status/<name>.resume.log` and pre-flight liveness.
  (core audit F7)
- [x] **Timeouts on the remaining subprocess calls** (git/gh action paths; `sbx ls` done).
  (core audit F3)
- [x] **mailbox stop-check hygiene**: respect `stop_hook_active`, don't re-block on the same
  messages when seen-marking fails, auto-prune. (hooks audit #7/#9)

## P1 — kill the remaining forced exits (daily-driver)

- [ ] **Transcript timeline tab.** Per-box "Turns" view from the transcript JSONL (the
  token-usage probe already parses it); persist to the store so it outlives clone-mode boxes.
- [ ] **Diff v2.** Syntax highlight, per-file collapse + file tree, parse-to-model before render
  (also unlocks comment anchoring on the model). Keep the inline-comment→agent flow.
- [ ] **CI logs inline.** Expand a failing check row with `gh run view --log-failed`.
- [ ] **Fleet economics strip.** Surface `telemetry/*.jsonl` (tokens, turn duration, tool
  counts): per-box sparkline + fleet burn today. Data already durable; pure UI.
- [ ] **Quick-reply.** One-line answer input on `needs-input` rows (reuse the bracketed-paste
  path `sendReview` uses) — collapses the most frequent loop of the day.
- [ ] **Search across the fleet** (`rg` host-side over workspaces, results deep-link into Files).

## P2 — compound the unique position

- [ ] **Collision reconcile view**: box A / base / box B panes + one-click "ask A to rebase onto
  B" mailbox message (radar + mailbox + diff already exist; this composes them).
- [ ] **Best-of-N launcher**: same prompt, N boxes, compare digests/diffstats, keep-one.
- [ ] **Inbox triage verbs**: Answer / Redirect / Park / snooze-until-CI-green on the headline.
- [ ] **Push events** (replace the 2s SSE poll; share one snapshot across tabs — core F4 partial:
  caching landed, fan-out sharing not yet).

## P3 — polish & docs debt

- [ ] README quickstart inversion (managed-repo flow first, prerequisites section, de-thing the
  CLI help/error strings); `.env.example` refresh. *(partially done — see README updates)*
- [ ] `docs/UX-AUDIT.md` completion ledger (most of its "missing" items now exist);
  mark `docs/GENERALIZATION.md` superseded by `repos.json`; sync ARCHITECTURE.md.
- [ ] In-app modal (type-the-name) for destroy instead of native `confirm()`; toast queue with
  severity; localStorage versioned envelope; keyboard coverage (header buttons focusable, `?`
  help overlay, tab cycling); auto-reconnect dead terminals with backoff; terminal search addon +
  font-size control; mailbox unread badge; "Start" affordance for stopped boxes; launch-failure
  overlay (zombie tab fix); pane registry + ES-module split of index.html (precondition for the
  next big pane).
- [ ] Fix `parse_boxes` single-object NDJSON edge; `box_liveness` reuse of the fleet cache
  (done implicitly via cache) — verify.

## Deliberately parked

- Light theme (expensive; palette is factored, do it when demand is real).
- `tailscale funnel` auth token (public exposure is off the table for now).
- Editing files in-cockpit (the agent is the editor; revisit after Files tab usage data).
