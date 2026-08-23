# Parity gate

**The rewrite is not done when it works. It is done when it does everything the current
implementation does.** `docs/architecture.md` governs *how*; this governs *what*.

## How this was audited, and how to re-audit it

The first version was written from recollection. It contained capabilities that **do not exist**
(`download`, a `max` subcommand, an "adopted-module surface") and missed roughly fifty-five real
ones. A second pass added two more inventions (`substrate reporting`, a per-box "notes" field) and
printed three counts none of its own commands reproduced. **A list with fabricated entries cannot be
a gate**, because the absence of an item stops carrying information.

These commands reproduce the numbers stated here. They have been run, and — since the numbers went
stale once, which is the failure this whole section is about — **`tests/parity_numbers.rs` now runs
them on every `cargo test` and fails when a count moves.** A number here is a claim about the code,
so it is checked like one. Updating it is one line, and the failure says which.

```sh
grep -c '\.route('  src/bin/skein-server.rs                    # 84   (NOT '.route("' — that gives 75)
grep -oE 'id="[a-zA-Z0-9_-]+"' src/web/index.html | sort -u | wc -l   # 155 unique, 158 occurrences
grep -c 'function ' src/web/index.html                          # 291
sed -n '41,120p' src/bin/skein.rs                               # the dispatch: subcommands and flags
```

**Line citations below are grep-able rather than numbered wherever a name exists**, because the
numbered ones all drifted between the first audit and the second and a reader who checks two and
finds both wrong stops checking the third. A citation nobody can follow is how a capability that
quietly disappeared comes to read the same as one that moved.

Keyboard shortcuts have a single declaration — `const KEYMAP` in `src/web/index.html`, whose
comment says it exists "so the keys documented here cannot drift from the keys the app binds". **The
code has drifted from it anyway**, so this is still a gap: `ArrowDown`/`ArrowUp` alias `j`/`k`, `o`
aliases `↵`, `[` and `}` move between sessions (asymmetric because `]` is taken — likely a latent
bug), and **holding right-Alt for 260 ms is push-to-talk**. Read KEYMAP *and* the keydown handlers.

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
- **The board's state taxonomy** — `GROUPS`, in `cockpit/src/groups.mjs` since the pure functions
  moved there (`6e6f1b9`), with node tests of its own: eight ranked groups over ~14 states,
  plus `NEEDS_YOU` and `labelOf`. Its comment records a shipped defect: three copies of "owed to you"
  disagreed, so the title said "3 need you" while the mouth stayed shut. **One definition, or the bug
  returns.**
- **The standing-debt announcer** — `announceStandingDebt` / `settledOwed` in `index.html`, with the sentence itself in
  `cockpit/src/announce.mjs` (`owedSentence` is gone — there is one sentence-maker now, SKEIN-112): level-triggered rather than edge ("a box that turned while you were
  looking at the board was marked seen and never spoken"), a grace window, once-per-box dedup, a ≤2
  threshold before collapsing to a count. The channels below are not the feature; this policy is.
- Alerts, toasts, **favicon badge, document title count, audio beep**.
- **The first-run checklist** — `firstRunHtml()` in `index.html`: four gated steps, each carrying
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
- **Per-box filesystem isolation** (`src/box-session.sh`, the block that begins `--tmpfs`): a `--tmpfs` over the fleet root and
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

## 5a. Found in the third audit, and previously missing

- **The voice command grammar** — six verb groups, longest-phrase-first matching, and `voiceTarget`
  resolution (named box → last spoken → selected → the only one owed an answer). Including
  **`tell it <words>` / `tell <box> <words>`, which posts a free-form prompt to the agent verbatim** —
  a second write path into a box, by voice.
- **Desktop notifications** — permission, per-box tag and renotify, click focuses and opens the box.
- **Writing a module note from the cockpit**, with a one-at-a-time lock, a fresh/stale/absent chip,
  per-module owners, and the rule that notes answer questions about a PR and never write its summary.
  This matters directly to §11.1's change view.
- **How inline review comments are delivered**: assembled per file and line and **bracketed-pasted
  into the agent's live terminal**, then cleared. That is *why* "commenting is an Act against a box"
  is true rather than aspirational.
- **The shared toolchain and build cache** across boxes (see §7).
- **`shared-paths.txt`** — surfacing a repo's gitignored essentials into every box's clone, RO by
  symlink from the mirror, `rw` seeded once and live fleet-wide, with surfaced paths excluded from git
  so `git add -A` cannot stage a host-absolute symlink.
- **`$HOME/shared`** — a project-scoped durable workspace symlinked into every box, failing loudly.
- **The health banner** — always-on, seven checks plus dark and stale box counts, showing the first
  failure's own sentence and clicking through to diagnostics.
- **Row hook-health with one-click repair** — `no signals` and `update probes`, the latter for a
  session predating the installed probe contract.
- **The `open` scope tag** — badges a box holding the fleet-wide credential.
- **Terminal scrollback carry-over on reconnect**, with its `── reconnected ──` marker.
- **Fleet settings**: Docker shares the fleet disk (one disk vs two — "the two disks are also two
  firewalls"), base branch for PRs, confirm-before-destroy as a *setting*, overwrite-token-on-startup.
- **The inherit/override grammar** in box settings — absent means inherit, empty means explicitly
  nothing, a value overrides — across tracking, identity, disk and git-scope, each default naming what
  inheriting currently means. And **disk is measured, not enforced**, said in the UI.

## 6. Items listed here that have no UI caller

`store/telemetry/<vmid>.jsonl` — durable per-turn token usage, written by two probes and **read by
nothing** anywhere in the server or the page. It belongs here or it should be deleted deliberately.

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

**Satisfied, and one thing survives it that is not the same thing.** The rows are gone from the
board's tick, and the server no longer returns sandboxes shaped as boxes — a `BoxView` with an empty
branch and no signals is what made them read as a fleet full of broken ones. What survives is a
different question, asked by a person: *what fleets are on this machine*, because somebody running
more than one needs to see them. `machine::sandboxes` answers it — a name, a run state, and whether
each is a skein fleet — and says of the run state that "stopped" and "I could not tell" are different,
as it does of "nothing else is here" and "sbx could not be asked". The old board keeps a `foreign:`
filter over it until the old board goes; the new one does not carry it.

**`/api/pick-path` and every Browse button — removed, not replaced.** The native host picker needs a
host process with display access, which in-fleet skein cannot have. Browse existed mainly to pick a
local repository path, and repositories are remotes now, so its main job is gone with it. The
remaining fields — the shared-data folder and the SSH key path — become text inputs **with a check
that reports whether the path resolved**, which satisfies law 1 without a host round-trip. A
warden-served picker was considered and rejected: another warden endpoint for an affordance used twice in a fleet's life.

**The host ssh-agent path.** In-fleet skein has no host ssh-agent to load a key into. SSH remotes
either move to the warden or to HTTPS with injected credentials.

**Transport reporting.** The first draft listed this as parity *and* deleted the transport. The
transport goes; the readout goes with it. This is recorded here because §12.2 of the first draft
claimed no user-visible feature hid in the deletion list, and this was the counter-example.

**Every rule the copy keeps now has a test.** The carrying machinery had thorough ones — ignored
files, the size-not-names filter, the three symlink rules, the bundle's own branch — and the two
rules *around* it had none, though both are in the list of things learned the hard way. A resize that
loses a login, or a Docker volume it could not ask about, looks exactly like one that worked.
`tests/resize_rules.rs` asserts both: the refusal on "could not ask" leaves the sandbox undestroyed,
and the login is read out of the sandbox **before** the destroy rather than after, when there is
nothing left to read.

**Resize is a root byte copy, and the earlier entry here described an abandoned mechanism.** It
`tar`s the whole box tree out and back, which is why it demands 1.2× the box size free first. The
bundle-and-patches reconstruction this page previously demanded is *not* what runs — it has no
production caller, and the comment beside it says the reconstruction "is slower, less faithful, and
it is where the fragility lives". Nothing here is removed; the requirement is that resize keeps
carrying every box's work, by whatever mechanism, with the symlink and ignored-file rules in
`docs/delivery.md` §5 intact.

**The shared toolchain stops being shared.** `share_paths` binds `~/.local`, `~/.cargo`, `~/.rustup`
and `~/.npm` read-write from the sandbox into every box, so one box's `cargo install` or
`npm i -g` reaches all of them and the build cache warms them all. Architecture §9.5.4 removes that —
either read-only with a per-box overlay, or not shared. **The speed and the convenience are the cost**,
and the `sudo` shim's own text currently tells users to rely on it.

**Everything that travels with the diff pane.** Not just a pane: the `d` shortcut, one of the six
voice verbs ("show me the diff"), the palette's per-box `Diff:` entries, the refresh and "Send N"
controls, and the diff tab as an attachment drop target.

**The change view needs machinery that does not exist.** `moduledocs::touched` maps changed files to
modules and contract signals are *file*-scoped — but per-module line counts, the NEW/CHANGED/SHRANK
classification and "12 call sites" are three new things. "No new machinery" was too strong.

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

**The CLI stays standalone — settled, and no longer a removal.** Architecture §14.1 keeps it driving
the library directly, because as a client it was inverted against the code (the server spawns it) and
circular against first run needing a terminal before any server exists. The problem the client idea
was solving is handled by a lock on declared state instead.

**The `~/.skein` mount split.** `apiauth.rs`'s module note records that the API token is safe *because*
`~/.skein/repos` and `~/.skein/boxes` are bind-mounted into boxes while `~/.skein` itself is not —
"checked, not assumed". A durable volume mounted whole into the fleet puts `credentials/`,
`api-token`, `github-pats/` and `tokens/` inside every box's reach on the shared uid. The volume's
privileged subtrees must stay outside every box's mount view — as an **inversion derived per box**
(tmpfs the state root, bind back what this box needs), not a list of things to hide
(architecture §9.5.2). And the mount nothing currently covers is `~/.skein/repos`, which carries
every repo's store and the host's own checkouts.

**Foreign-sandbox *display* goes; the *state* does not.** The `foreign:` filter is removed above, but
the architecture keeps `declared = deleted` as a first-class cell — a half-completed destroy is still
a thing skein must recognise and clean up. Only the board rows go.

### 7.1 The walk against `/v2`

`/v2` ships beside `/` (`src/web/v2.html`, routed in `src/bin/skein-server.rs`), and this is the
record of walking every entry above against it. **It is a record, not a verdict**: the cutover
happens when every row reads *holds*, and today it does not.

Three verdicts, and the middle one is the load-bearing one:

- **holds** — the removal is true at `/v2`, by construction rather than by intention.
- **not yet asked** — the surface the entry is about does not exist at `/v2` yet. This is not a pass.
  Each of these is a reason the cutover is not due, and listing them is what stops "90% ported and
  cut over", which `docs/delivery.md` names as worse than 60% ported and not.
- **not a surface question** — the entry is about the fleet or the host, and `/` and `/v2` are
  equally affected by it. Recorded so nobody reads its absence as an oversight.

| §7 entry | at `/v2` | how it is known |
|---|---|---|
| Adopt-in-place → local-path remotes | not a surface question | about where a repo's bytes come from; neither board changes it |
| Foreign sandbox display | **holds** | `/v2` reads `/api/queue`, whose rows are boxes, pull requests and setup faults (`queue::Source`). There is no sandbox row and no `foreign:` term in the page |
| `/api/pick-path` and Browse | **holds, and gone from `/` too** | `/v2` adds a repository and makes a box, and a path is **typed**: `GET /api/path` says what it found — folder, file, link, or nothing there yet — which is law 1 without a host round-trip. A link is reported as a link. `pick-path` is not referenced by either page now, and one test asserts it of both. SKEIN-106 moved `/`'s three Browse buttons to the same typed path and deleted the route, the handler and `health::pick_path` — it popped the *host's* native dialog, which needs a display the in-fleet skein does not have, and it was already unusable over Tailscale where the advice was "keep typing" |
| The host ssh-agent path | not a surface question | a credential path, not a screen |
| Transport reporting | **holds** | nothing in `/v2` reads a transport field; there is no readout to port |
| Every copy rule has a test | not a surface question | `tests/resize_rules.rs`, unchanged by either board |
| Resize is a root byte copy | not yet asked | `/v2` has no fleet-resize surface yet |
| The shared toolchain stops being shared | not a surface question | a mount policy |
| Everything that travels with the diff pane | **holds** | `/v2` has no diff pane, no `d` shortcut, no `Diff:` palette entry, and no attachment target. It was not ported and will not be |
| The change view needs machinery that does not exist | **holds** | the machinery exists (`shape::of_diff`, and `mentions` beside each contract signal) and `/v2` renders it: a queue row opens the module list, in the server's order, with each module's standing note only when it is fresh |
| The diff pane and its comment composer | **holds** | commenting back to an agent is an Act against a box, and the box's terminal at `/v2` is one |
| The transcript tab | **holds** | not ported, deliberately; kept in §7 so its removal stays a decision |
| The CLI stays standalone | not a surface question | settled in architecture §14.1 |
| The `~/.skein` mount split | not a surface question | a mount inversion, per box |
| Foreign-sandbox *display* goes, the *state* does not | **holds** | `declared = deleted` is fleet state; `/v2` renders no sandbox rows either way |

**What `/v2` is today**: the queue, the three states said in words (`cockpit/src/tone.mjs`), the
change view (`cockpit/src/change.mjs`, §11.1), a box's terminal, and setting up — add a repository,
make a box (§11's fifth job, *one action, no configuration exercise*). **What it is not**: the fleet
controls, and a settings screen — deliberately, since `Config` has twenty-four fields and rendering
them all is one of the things this board is a reaction to. What is left reads *not yet asked* above,
and each is one item's worth of work rather than a question anybody still has to answer.

**The box is made as an Act**, which is what that machinery was for: the answer is an id, the work
outlives the page, and closing the tab loses nothing. A surface that treated the create as a POST
returning a result would report a box that was never started.

**The cutover is a separate change**, and reversible: `/` is untouched, the route is one line, and
switching them is editing which constant `index` serves. Nothing in this section is done by that
edit, which is the point of writing it down before making it.

---

## 8. Known gaps in this audit

Stated so the next reader knows what has not been checked, rather than inferring completeness.
**Last re-audited 22 August 2026**, on `in-fleet`, independently of the documents — from the code —
and the results are recorded where they belong rather than here: the four counts above now reproduce
and are checked by `tests/parity_numbers.rs`; §7.1's `/v2` table was walked row by row and every row
still holds; and the cockpit's eleven browser suites, which had gone unrun for 160 commits, pass and
are run by `cargo test` (SKEIN-113). What that pass found is in SKEIN-110 through SKEIN-117.


- The 265 JavaScript functions were sampled, not enumerated one by one. This is the largest
  remaining hole and the only honest way to close it is to walk them.
- Settings controls were counted from the UI. The count mismatch against `Config`'s 24 fields is
  explained, not outstanding: repo and connection settings are not `Config` at all.
- **Two rounds of this document invented capabilities.** Treat any entry with no file reference
  beside it as unverified until someone greps for it.
