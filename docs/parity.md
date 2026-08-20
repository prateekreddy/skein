# Parity gate

**The rewrite is not done when it works. It is done when it does everything the current
implementation does.** `docs/architecture.md` governs *how*; this governs *what*.

## How this was audited, and how to re-audit it

The first version was written from recollection. It contained capabilities that **do not exist**
(`download`, a `max` subcommand, an "adopted-module surface") and missed roughly fifty-five real
ones. A second pass added two more inventions (`substrate reporting`, a per-box "notes" field) and
printed three counts none of its own commands reproduced. **A list with fabricated entries cannot be
a gate**, because the absence of an item stops carrying information.

These commands reproduce the numbers stated here. They have been run:

```sh
grep -c '\.route('  src/bin/skein-server.rs                    # 67   (NOT '.route("' — that gives 60)
grep -oE 'id="[a-zA-Z0-9_-]+"' src/web/index.html | sort -u | wc -l   # 155 unique, 158 occurrences
grep -c 'function ' src/web/index.html                          # 265
sed -n '25,90p' src/bin/skein.rs                                # subcommands and flags
```

Keyboard shortcuts are **not** a known gap: `const KEYMAP` at `src/web/index.html:4636` is the single
declaration, and its comment says it exists "so the keys documented here cannot drift from the keys
the app binds."

**The rule:** an item leaves this list only by moving to §7 with a reason. Never by being forgotten.

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
- **The board's state taxonomy** — `GROUPS` (`index.html:1422`): eight ranked groups over ~14 states,
  plus `NEEDS_YOU` and `labelOf`. Its comment records a shipped defect: three copies of "owed to you"
  disagreed, so the title said "3 need you" while the mouth stayed shut. **One definition, or the bug
  returns.**
- **The standing-debt announcer** — `announceStandingDebt` / `owedSentence` / `settledOwed`
  (`index.html:3744-3852`): level-triggered rather than edge ("a box that turned while you were
  looking at the board was marked seen and never spoken"), a grace window, once-per-box dedup, a ≤2
  threshold before collapsing to a count. The channels below are not the feature; this policy is.
- Alerts, toasts, **favicon badge, document title count, audio beep**.
- **The first-run checklist** — `firstRunHtml()` (`index.html:5240`): four gated steps, each carrying
  the action that resolves it, driven from `/api/health`. This is the shipped implementation of the
  architecture's laws 1, 6 and 7.
- **Keyboard shortcuts** from `KEYMAP`: `j`/`k`, `↵`, `d`, `]`, `l`, `/`, `⌘N`, `?`, `esc`, plus the
  Mac/non-Mac glyph translation.
- Voice: mouth and ear, including *read what needs me*.
- Command palette; mobile on-screen key bar; **resizable sidebar gutter** with persisted width and a
  docked rail mode.
- `?demo` mode — a seeded board. Needed for design work on the component library.

## 2. Converse

- **Two session kinds per box — agent and shell.** Not one terminal.
- **The tabbed session dock**: multiple concurrent live terminals, persisted across reload,
  drag-to-reorder with order remembered, `⌥1`–`⌥9`, `⌥[`/`⌥]`, `⌥⇧[`/`⌥⇧]`, close-all, cycling.
- **Attachments**: paste a file, drag-and-drop onto a board row or the dock, or the button; folders
  keep structure; capped at 200 files and `UPLOAD_CAP` 2 GB per file, enforced as the bytes go past; streamed not buffered; one batch directory per drop; the in-box path
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
  tracking, sync provisioning and refresh.
- **Box creation happens over the WebSocket, not a REST route** — `terminal()` takes
  `?launch=<branch>` and creates the box before attaching. Any plan that ports the API before the WS
  loses box creation.
- **Per-box filesystem isolation** (`box-session.sh:912-990`): a `--tmpfs` over the fleet root and
  over the box-state parent with only this box's own directories bound back — covering boxes created
  *after* this one starts, "which an enumeration could not" — an empty file bound over the
  fleet-agent token, and another over `$SSH_AUTH_SOCK`. The measurement that motivated it is in the
  comment: another box's conversation history was simply readable, and "the shortest path out of a
  box was not an exploit at all, it was `cat`."
- **The workshop (privileged) box** — `POST /api/boxes/:name/privileged`, the `bs-priv` control, and
  a per-start terminal banner whose comment says "the whole risk of this switch is forgetting which
  box carries it". It removes the bind hiding the fleet-agent token, i.e. **it grants a box sandbox
  root at any time from the cockpit**. Deliberately non-exclusive.
- **The package-request queue** — a box asks for apt/npm, the owner approves, and the approval is
  **remembered in a manifest replayed into every future launch**. An approval system with an install
  path, not a setting.
- **The git-write-request queue** — the git shim intercepts a push the box is not scoped for and
  files a request; grants are hour-limited, revocable, displayed live or expired; plus the credential
  probe.
- Fleet create with sizing, resize, plan and host capacity, resource and limit editing, transport
  reporting, GitHub credentials, read token, health. (`/api/fleet/substrate` is **not** a second
  readout — it *is* the package-request queue above.)
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
- **Per-repo settings** — `plane_project`, `sync_connection`, `review_queue`, plus `agent`, `store`
  and a per-repo write PAT from the Add-repository overlay. These live in `repos.json` /
  `connections.json`, not `Config`, and save on blur — a different persistence contract.
- **Four vendored asset routes** — xterm, xterm.css, addon-fit and marked served from the binary.
  "The cockpit works with no CDN and no network" is a capability, and these are the only other
  entries in `open_to_all`.
- **The `sudo` shim** and **`git-credential-skein`** — named, because only the git shim was.
- **`realign_transcript`** — the conversation-slug repair for a moved checkout.
- Mailbox message kinds (note / handoff / review-request) and `⌘↵` to send.
- **SSH key handling** — loads a host key into the host ssh-agent, which is forwarded into boxes.
  See §7.
- **`/api/pick-path`** — the native host folder/file picker. See §7.
- CLI: `ls`/`status` (**the default command**), `add`, `repos`, `remove`/`rm`, `doctor`, `shared`
  (including `shared import <box> [--include] [--apply]`), `start`, `login`, `resize`, `attach`,
  `version`, `help`; flags `--branch --agent --attach --handoff --id --store --include --apply
  --disk --drop-docker --version/-v --help/-h`.
- **`--drop-docker`**: resize *refuses* rather than warns when Docker holds unpushed images, and
  refuses on "could not ask" too.

---

## 6. Items listed here that have no UI caller

`repin` is **API-only** — there is no `repin` CLI arm and no UI caller. An earlier version of this
document said "API- and CLI-only", which was wrong. It is kept because removing it should be a
decision rather than an omission.

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

**`/api/pick-path` and every Browse button — removed, not replaced.** The native host picker needs a
host process with display access, which in-fleet skein cannot have. Browse existed mainly to pick a
local repository path, and repositories are remotes now, so its main job is gone with it. The
remaining fields — the shared-data folder and the SSH key path — become text inputs **with a check
that reports whether the path resolved**, which satisfies law 1 without a host round-trip. A
warden-served picker was considered and rejected: a third warden capability for an affordance used
twice in a fleet's life.

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

**The diff pane and its inline comment composer — replaced, not ported.** Stated by the owner:
diffs do not matter today, because agentic coding is good enough at writing code that line-by-line
reading is rarely where the value is. What matters is **the shape of a change — which modules, how
the design decomposes, drill-down to code when warranted** — and mostly the code is not read at all.
If the text is wanted, GitHub has it.

So skein does not compete on diff rendering. Architecture §11.1 replaces this surface with a change
view built on two things that already exist: standing module notes and contract signals, both
reframed as signals whose subject is a module. **Commenting back to an agent is not lost** — it is an
Act against a box, which the terminal already is.

**The transcript tab — not ported until asked for.** Never opened, and a reader with no loop attached
to it. Kept on this page so its removal stays a decision.

**The CLI stops working without a server** — if the architecture's "the CLI is a client of the
server" stands. Today all twelve subcommands drive the library directly and work with no server
running; `add` writes `repos.json` itself, `start` builds the whole box. As a client, none of them
work before the cockpit exists — and the cockpit is inside the fleet, which does not exist before
`create`. **This collides head-on with "first run is `skein doctor` in a terminal."** One of the two
has to give, and until it does this is an unpriced removal rather than a decision.

**The `~/.skein` mount split.** `apiauth.rs:24-26` records that the API token is safe *because*
`~/.skein/repos` and `~/.skein/boxes` are bind-mounted into boxes while `~/.skein` itself is not —
"checked, not assumed". A durable volume mounted whole into the fleet puts `credentials/`,
`api-token`, `github-pats/` and `tokens/` inside every box's reach on the shared uid. The volume's
privileged subtrees must stay outside every box's mount view, and that cover list is a tested
enumeration.

**Foreign-sandbox *display* goes; the *state* does not.** The `foreign:` filter is removed above, but
the architecture keeps `declared = deleted` as a first-class cell — a half-completed destroy is still
a thing skein must recognise and clean up. Only the board rows go.

---

## 8. Known gaps in this audit

Stated so the next reader knows what has not been checked, rather than inferring completeness:

- The 265 JavaScript functions were sampled, not enumerated one by one. This is the largest
  remaining hole and the only honest way to close it is to walk them.
- Settings controls were counted from the UI. The count mismatch against `Config`'s 24 fields is
  explained, not outstanding: repo and connection settings are not `Config` at all.
- **Two rounds of this document invented capabilities.** Treat any entry with no file reference
  beside it as unverified until someone greps for it.
