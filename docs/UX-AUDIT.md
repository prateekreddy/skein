# Skein — UX audit & roadmap to "smooth"

> Written at the v0 → v1 inflection: the plumbing works (live fleet board + embedded
> terminal + real status), so this is the stop-and-make-it-feel-good pass. Benchmarked
> against Conductor and ~20 peer tools (Crystal, Vibe Kanban, Sculptor, Devin, Cursor,
> Codex cloud, Copilot agent, uzi, Claude Squad, …) and the Linear/Raycast/Vercel design
> canon. Companion to `VISION.md` and `ARCHITECTURE.md`.

---

## 1. Honest baseline — where skein actually sits

**What skein is today:** a read-only, live-updating **status list** + a **single embedded
terminal** (one box at a time, in a modal). That's it. You can *see* the fleet and *talk* to
one agent. Everything between "talk" and "shipped" — review the diff, comment back, open a
PR, merge, archive — happens elsewhere (the terminal, the CLI, GitHub).

So measured purely on **interaction surface**, skein is roughly Conductor's left sidebar
with the entire middle and right panes removed. That's the sad part, and it's worth saying
plainly.

**But** the *foundation* is ahead of Conductor on three axes that the whole GUI field is weak
on, and that are expensive to retrofit:

- **Real per-agent isolation** (sbx microVMs) — Conductor explicitly has *none* (full host
  access); Sculptor is the only peer that matches us, via Docker.
- **A live shared brain across boxes** (shared `.claude` memory + cross-box mailbox) —
  research called this "notably rare." No GUI competitor has it.
- **Web-first / cross-platform / remote-able** — Conductor is macOS-only, the #1 structural
  complaint against it. We're a URL away from any device.

**Verdict:** the chassis is good and partly *better* than the benchmark; the cockpit it's
wrapped in is a v0. The work ahead is almost entirely **interaction + polish**, not
re-architecture — which is the cheap kind of work.

### Scorecard (skein today vs. the field)

| Dimension | skein today | Best-in-class | Gap |
|---|---|---|---|
| Fleet overview | flat list, tier-sorted | grouped cards / dense table, filter/search | **medium** |
| "Who needs me?" | red glow + count | jump-to-next, pinned zone, OS notify | **large** |
| Notifications | none | title/favicon badge, web-push, tiered sound | **large** |
| Launch a box | CLI only | create-from prompt/branch/issue in UI | **large** |
| Talk to agent | 1 terminal, modal | multi-tab, follow, send-without-opening | medium |
| Review diff | none (use terminal) | inline diff + comment-back-to-agent | **large** |
| Ship (PR/merge/archive) | none | one-click PR, merge-readiness, archive | **large** |
| Keyboard / ⌘K | none | palette + j/k grammar | **large** |
| Metrics | age only | context-% · cost · tokens · diff± · time | medium |
| Visual craft | clean, ~Primer | layered, motion-tokened, optimistic | small–medium |
| **Isolation** | **microVM** | Docker (Sculptor) only | **ahead** |
| **Shared brain** | **memory + mailbox** | — | **ahead (unique)** |
| **Reach** | **web/remote** | macOS-only (Conductor) | **ahead** |

---

## 2. The diagnosis: the core loop is missing its middle

Every good tool is built around one loop. Conductor's (their words): **break down →
one workspace per unit → run → verify/review/comment → PR → merge → archive**, with a
**"suggested next action" chip** that advances per workspace so you always know the next move.

Skein implements the *ends* (see fleet, talk to agent) and **none of the middle**. The single
highest-leverage thing we can do is build the **review → ship** half:

```
   HAVE NOW                         MISSING (the value)                  HAVE NOW
 ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐
 │  see the │ → │  launch  │ → │  review  │ → │  PR /    │ → │ archive  │
 │  fleet   │   │  a box   │   │  + steer │   │  merge   │   │          │
 └──────────┘   └──────────┘   └──────────┘   └──────────┘   └──────────┘
   ✅ done        ❌ CLI only     ❌ terminal     ❌ GitHub      ❌ none
        └────────────── talk to agent (✅ terminal) ──────────────┘
```

Until that middle exists, skein is a *monitor*, not a *cockpit*. Conductor's most-praised
feature by a wide margin is **inline diff comments that get sent back to the agent** — review
locally, before any push. That's the centre of gravity to aim for.

---

## 3. Gap analysis & what to steal (by theme)

### A. Fleet overview & "who needs me?"
The flat list is fine at 3 boxes, thin at 15. Steal:
- **Group by status** (`needs you · working · done · idle/stale`) with collapsible sections,
  *or* group by repo when multi-repo. The field has converged on **grouped card lists** over
  flat lists and over kanban (kanban is for *planning*, not the live view).
- **A pinned "needs you" zone** at the very top — turn "scan 15 rows" into "look up top."
- **Jump-to-next-attention** — one hotkey (`]` or palette action) that cycles only through
  boxes that are blocked-on-you or have unread output. Conductor's single highest-leverage
  primitive.
- **Density toggle**: comfortable cards ⇄ dense table (uzi-style `NAME · STATE · BRANCH ·
  DIFF± · CTX% · AGE`) for power users at 20 boxes.
- **Filter/search** (`/` to focus): by status, repo, name. Devin's filters are the gold model.

### B. Attention & notifications — *the cheapest big win*
We currently rely on the user staring at the tab. The whole point of a fleet is to **look away
and get yanked back exactly when needed.** Tiered, restrained (over-notifying kills trust):
- **`document.title` counter** — `(2) skein — 2 need you`. ~10 lines, throttled, cleared on
  focus. Best pull-back-per-effort in the whole audit.
- **Favicon badge** (canvas overlay or `navigator.setAppBadge`) for needs-input/error only.
- **Web Notifications**, gated on a user gesture (an "Enable alerts" toggle), fired only when
  `document.hidden`, with a per-box `tag` so re-notify *replaces*. Fire on real transitions
  only: `needs-input`, `done`, PR-opened.
- **Sound, opt-in, needs-input only**, debounced, visibly mutable. (Conductor's transit
  chimes give "done" an audio identity — optional flourish.)
- Best-practice from the field (AI Beacon): notify on **exactly three** transitions and let
  **only "needs you" make a sound**.

### C. Create / launch a box from the UI
Today you drop to `setup-sandbox.sh`. Steal Conductor's **create-from-anywhere**:
- A `+ New box` button / `⌘N` → prompt for branch name (+ optional first prompt) → POST to a
  new `/api/boxes` that shells `setup-sandbox.sh <branch>`. Stream the launch in the terminal.
- Later: create from a branch / GitHub issue; **prompt-prefilled-but-not-sent** (Conductor's
  Alt+Enter) so you tweak before firing.

### D. Interacting with a running agent
The terminal works but is single-occupancy and modal. Steal:
- **Sidebar + main pane** instead of a modal: list selects the active terminal in one big
  pane (Warp's model). Keep the terminal *mounted* per box so switching boxes is instant and
  scrollback persists; don't dispose on close.
- **Per-box "typing" indicator** — a blinking caret / animated dots on the row when the PTY is
  streaming, so you see which agent is alive without opening it.
- **Send a prompt without opening the terminal** — a one-line composer on the row/card for a
  quick "yes, proceed" to a waiting box.
- `⌘↵` maximize terminal / `Esc` back. Cap scrollback; pause auto-scroll when scrolled up with
  a "jump to latest ↓" pill.

### E. Review & diff — the missing half (highest value)
This is where skein becomes a cockpit. We already have the data path: each box pushes a branch,
reachable via `git fetch sandbox-<box>` / the RO mirror, or `gh`. Build incrementally:
1. **Read-only diff viewer** — `/api/boxes/:name/diff` returns the branch-vs-base diff;
   render syntax-highlighted (this is the Svelte moment in the roadmap). Show **diff± and
   files-changed** on every row immediately (cheap, from `git diff --shortstat`).
2. **Inline comments → back to the agent** — click a line, write a comment; bundle the
   comments and inject them into that box's session (via the terminal PTY or the mailbox).
   This is Conductor's killer feature and our shared-brain/mailbox makes it natural.
3. **Three-way toggle** (Codex): uncommitted / branch-vs-base / last-turn-only.
4. **Checks / merge-readiness panel** (Conductor's "Checks" tab): git status + CI + unresolved
   comments + tests, with red = blocker.

### F. Ship — PR / merge / archive
- **One-click PR** (`gh pr create`) with auto-filled body; then follow checks.
- **Archive-when-done** → moves the box out of the active list but keeps branch + history
  recoverable (a History view). Keeps the cockpit focused.
- **"Suggested next action" chip** per box that advances Run → Review → Create PR → Merge →
  Archive, so the next move is always one obvious button.

### G. Command palette ⌘K + keyboard grammar
The most "Linear/Raycast" thing we can build, and pure-local so it's instant:
- `⌘K` over a local action array: fuzzy subsequence match, **sections** (Boxes · Actions ·
  Navigation), **recents**, `↵` run / `→` actions-on-selection / `Esc`. Footer hints.
- **Keyboard grammar:** `j/k` move row focus, `↵` open, `d` diff, `a` archive, `]` next-attn,
  `/` search, `g i` go inbox. `:focus-visible` accent rings; focused row gets a 2px left
  accent border + elevated bg.

### H. Metrics that matter
Age alone is weak. Add (in order of value):
- **Context-window %** — research's single most-cited "actionable" live metric (how close the
  agent is to running out of room). Needs a hook to report it.
- **Diff±** and files-changed (free from git).
- **Cost / tokens / elapsed** — the field barely does this (only Devin/Terragon); a real wedge
  given parallel-burn is a known Conductor pain. A `Stop`-hook can log tokens per turn.

### I. Visual craft & motion (small but premium)
- **Fix the status semantics** (do this first, zero cost): we have `working = green`,
  `done = blue`. That **inverts the universal convention** (green = done/success everywhere:
  CI, GitHub, Grafana). Swap to **done = green, working = blue (pulsing)**. Blue pulsing reads
  as "live"; green reads as "finished." Removes a permanent micro-hesitation. Frees indigo
  `--accent` to be UI-only (never a status).
- **Layered elevation by lightness** (not shadows): bg `#0b0d10` → panel `#14171c` → elevated
  `#1b1f26` → overlay `#20252e`; three border tiers; four text tiers. Add a 3% inner top
  highlight on panels (`inset 0 1px 0 rgba(255,255,255,.03)`) — the subtle "premium" tell.
- **Tinted pills via `color-mix()`** + one `.pill` class (dot + label; never a bare dot —
  ~8% of men are colour-blind).
- **Motion tokens**: `--dur-fast 100ms / --dur 150ms / --dur-med 250ms`, one ease
  `cubic-bezier(.2,0,0,1)`; animate **only** `transform`/`opacity`; popovers scale from their
  trigger origin. Make attention pulses **finite** (2 cycles), not infinite (infinite = anxiety).
- **`prefers-reduced-motion` reset** (use `0.01ms` not `0` so `transitionend` still fires);
  swap pulsing dots for a static ring.
- **Optimistic, in-place DOM updates**: keep a `Map<name, nodeRefs>` and mutate the changed
  cell — never `innerHTML` the whole list (it kills xterm focus and jank at 20 rows). Actions
  flip the UI instantly, roll back on failure.
- Skeleton rows (shimmer) over spinners; empty states that name the keystroke to fix them.

### J. Skein's unique wedge — *lean into the shared brain*
No competitor has this; it's our identity, not a checkbox:
- **Mailbox view** — read/compose cross-box hand-offs in the UI (we have `mailbox.sh`).
- **Broadcast** — send one instruction/decision to N boxes at once (e.g. "rebase on main").
- **Fleet memory surfacing** — show that boxes share house rules / recent memory writes; make
  "the fleet is a team, not N strangers" *visible*.
- **Coordinator view (later)** — Devin-style: a planning box that spawns workers, children
  indented under the parent, per-box cost attributed.

---

## 4. Prioritised roadmap (impact ÷ effort)

**Tier 0 — polish pass (hours, do first; makes today's thing feel premium):**
1. Status-semantics swap (done=green, working=blue-pulsing). *(token edit)*
2. `document.title` + favicon attention badge. *(~30 lines)*
3. `⌘K` palette (boxes + actions) and `j/k` row focus. *(local, instant)*
4. Pinned "needs you" zone + jump-to-next (`]`). 
5. In-place DOM updates (node-ref map) + motion tokens + reduced-motion + finite pulses.
6. Empty/loading states; diff± and files-changed on each row (cheap git shortstat).

**Tier 1 — close the loop (days):**
7. Read-only **diff viewer** (`/api/boxes/:name/diff`, Svelte, syntax-highlighted).
8. **Create box from UI** (`⌘N` → branch → launch, stream in terminal).
9. Sidebar+pane terminal (persistent per box) + "typing" indicator + send-without-opening.
10. **Web notifications + opt-in sound** (tiered: needs-input/done/PR-opened).
11. Context-% + cost/tokens metrics (needs a reporting hook, like `box-status.sh`).

**Tier 2 — ship & steer (1–2 weeks):**
12. **Inline diff comments → back to the agent** (via mailbox/PTY) — the Conductor-killer.
13. One-click **PR / merge-readiness (Checks) / archive + History**.
14. "Suggested next action" chip per box.
15. **Mailbox + broadcast** view (the shared-brain wedge).

**Tier 3 — scale & reach:**
16. honker push (replace SSE polling); durable launch/merge/archive jobs.
17. Remote/mobile (tunnel + auth + browser push).
18. Coordinator/children view; best-of-N (one prompt → N boxes → compare → keep one).

---

## 5. Target layout (where Tier 1–2 lands)

```
┌─ skein ───────────────────── 3 need you · 5 working · 2 done ──── ⌘K ─ ⏺ live ─┐
│ ░░ NEEDS YOU ░░                │                                                │
│ ● auth-fix     decision  2m ┐ │   auth-fix · feat/auth-fix      [Terminal][Diff]│
│ ● export-pdf   waiting   5m │ │  ┌──────────────────────────────────────────┐  │
│ ── working ──              │ │  │  (embedded terminal / syntax-hl diff)      │  │
│ ◔ example-box-8    working  +120│ │  │                                            │  │
│ ◔ registry-ck  working  -30 │ │  │                                            │  │
│ ── done ──                 │ │  └──────────────────────────────────────────┘  │
│ ✓ limitation   done   3 fls │ │  next: ▸ Create PR        ctx 41%  $0.62  4m12s │
│ ── idle ──                  │ │  ── checks ──  ✓ build  ✓ fmt  ✗ 1 comment      │
│ ○ spike-foo    idle    9h  ┘ │                                                │
└──────────────────────────────┴────────────────────────────────────────────────┘
   sidebar: grouped, pinned needs-you, j/k focus, diff± inline      main: tabbed
```

---

## 6. What *not* to do

- **Don't become Conductor.** Skip what fights our model: it's macOS-native, single-desk,
  un-sandboxed. Our wins are isolation + shared brain + web; spend design budget there.
- **Don't add kanban for the live view.** The field tried; grouped cards won. Keep kanban (if
  ever) for *planning*, not monitoring.
- **Don't over-notify.** Three transitions, one sound. Restraint is the feature.
- **Stay calm and dense.** Linear's lesson: colour is spent only on status and the focused
  row; everything else is quiet. Don't decorate.

---

*Sources: conductor.build (site/docs/changelog) + HN 44594584; Crystal, Vibe Kanban, Sculptor,
Devin, Cursor 2/3, Codex cloud, Copilot agent, uzi, Claude Squad, Tembo, Charlie, Factory,
Jules, Terragon, Marc Nuri "AI Beacon"; Linear (performance.dev breakdown), Raycast/Primer/
Datadog/Warp/MDN for the design canon. Full notes in the research threads behind this audit.*
