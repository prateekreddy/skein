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
grep -c '\.route('  src/bin/skein-server.rs                    # 96   (NOT '.route("' — that gives 84)
grep -oE 'id="[a-zA-Z0-9_-]+"' src/web/index.html | sort -u | wc -l   # 157 unique, 160 occurrences
grep -c 'function ' src/web/index.html                          # 424
sed -n '16,139p' src/bin/skein.rs                               # the dispatch: subcommands and flags
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
  moved there (`9116043`), with node tests of its own: eight ranked groups over ~14 states,
  plus `NEEDS_YOU` and `labelOf`. Its comment records a shipped defect: three copies of "owed to you"
  disagreed, so the title said "3 need you" while the mouth stayed shut. **One definition, or the bug
  returns.**
- **The standing-debt announcer** — `announceStandingDebt` / `settledOwed` in `index.html`, with the sentence itself in
  `cockpit/src/announce.mjs` (`owedSentence` is gone — there is one sentence-maker now, SKEIN-112): level-triggered rather than edge ("a box that turned while you were
  looking at the board was marked seen and never spoken"), a grace window, once-per-box dedup, a ≤2
  threshold before collapsing to a count. The channels below are not the feature; this policy is.
- Alerts, toasts, **favicon badge, document title count, audio beep**.
- **The first-run checklist** — `firstRunHtml()` in `index.html`: **five** gated steps — sbx, the
  warden, an agent login, a repository, and how boxes push (`h.sbx`, `h.warden`, `h.logins`,
  `repos.length`, `h.git_credential` in the `steps` array of `firstRunHtml`) — each carrying the
  action that resolves it, driven from `/api/health`. This is the shipped implementation of the
  architecture's laws 1, 6 and 7. This entry said **four**, and there are five — count the objects
  in the `steps` array. A step is a gate, so an undercount is a gate nobody is holding the rewrite to.
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
- **Files tab**: reads the box's own tree (`list_files_in_box`, an `enter`); markdown with
  relative-link navigation; README auto-opens; images inline; symlink escape guarded. **The
  labelling is parity. The fallback's target is not, because it is already empty for some repos.**
  When the box is down `list_box_files` falls back to `list_host_files` — the *host clone* — and
  labels it *"read from the host clone"*; `src/files.rs` says of that path, in its own comment, that
  "for a repo whose host clone never got a working tree it is empty — which is exactly how 'the
  Files tab shows nothing' happened while the agent had a full tree three feet away". That is
  today's behaviour, not a consequence of a future removal, and in-fleet it widens: architecture §6
  says in-fleet skein cannot reach host paths at all. So the requirement is the box read, the
  `Answer::from_host` / `Answer::from_box` labelling, and a fallback whose target exists — **§7
  frames the fallback's target as being removed later; it is already unreliable now.**
- **Inline diff comment composer**, comments interleaved between diff lines, `⌘↵` to save.
- The pull-request queue. **Six actions, not one**: approve, request-changes, comment, **merge**,
  **ask** (Q&A against the PR), **draft** (model-drafted comment). Merge is destructive and must not
  hide behind a verb.
- **The summary and the review are one reading, always** — including "draft again". Both halves come
  out of a single CONVERSATION over a single diff download (`review::visit` → `summarise_and_draft`),
  and both are stored together, so a row can never show a summary of one reading of a commit beside
  a review from another. A reading is two TURNS of one conversation and not two readings (SKEIN-393):
  the first produces the summary and the review, and the second asks that review which files it did
  not open. **The conversation belongs to the pull request, not to the reading** (SKEIN-376): its id
  is derived from `<repo_id>#<number>` (`ai::conversation_for`) rather than stored, and every turn
  runs in that pull request's own directory (`review::conversation_of`, carried by `ai::Turn`)
  because Claude Code
  files a session under the working directory it was opened in — unpinned, every resume misses and
  every round is a cold read while appearing to work. Whether this round opens or resumes is asked
  rather than recorded (`ai::claude_in_conversation`): resume, then open, then no conversation at
  all, so a sandbox recreated between rounds degrades to today's cold read instead of failing.
  **That directory is a checkout of the change** (`review::stand_the_change_up`, SKEIN-395), which
  is why it is per pull request rather than per repo: measured over one 26KB diff, a reviewer with
  nowhere to look made zero tool calls in one turn, and the same prompt in a checkout made thirty
  over thirty-one — reading the changed file around each hunk and following the caller into
  another. It is stood up at exactly the commit being reviewed **or left empty**: a checkout at the
  wrong commit is a review confidently wrong about code the change does not contain, which is worse
  than no checkout at all. Every failure to stand it up is silent and the reading goes ahead. The second turn can only add to what the first wrote — it posts its own
  addition to GitHub and there is nothing on skein's side to fold it into — resends no diff, and is
  not a second unit — the unit is the pull request analysed, the same rule that makes
  stage 2 free after stage 1. There is no standalone drafter: `draft_critique` and its own prompt were
  deleted, because nothing needs a review without a summary (SKEIN-263). A press for a review
  re-runs the reading rather than drafting beside the one on disk.
- **One read control** (SKEIN-293). "Read it again" reads the whole change and reads it again from
  scratch — always both halves, on the merged reading, over one diff download, for one unit. It
  carries the reader's intent to the server as `?redraft=1` (`src/web/index.html`), which the server
  reads as `review::Review::Always`.

  **The confirmation this bullet used to describe is gone with the thing it protected.** The press
  asked before replacing a review the reader had VETTED — a kept or dropped comment, or edited text
  — and there is no such review to lose: skein stores none, and the vetting panel that recorded
  those decisions went with it. Where there is nothing to lose it just goes, which is now every
  time. The receipt and undo themselves are untouched and still carry every verdict.
- **~~Approving with skein's own review~~ — cut, and the rule it was an exception to stands.** One
  control used to post skein's drafted review as the approval body. Skein holds no review to post:
  the session posts its own to GitHub under the reader's account (`src/review.rs`, `gh pr review`).
  Verdicts themselves did not move — approve, request-changes and comment are still on the row
  (`revVerdictHtml`), and the keyboard still refuses `a` on a row that is not open.
- **CODEOWNERS parsing and ownership attribution**, including the gitignore-anchoring rule and the
  fact that team-requested reviews are not returned by `review-requested:@me`.
- **Contract signals** — a mechanical diff scanner that escalates a PR the model called boring,
  capped and deduplicated, deliberately non-redundant with the AI summary.
- Review filter chips: all / author / reviewer / mentioned.
- **Stacks, as trees rather than lines** (`revChains`, SKEIN-147/160/288). Detection is
  `base_ref → head_ref` over the queue the pane already holds — no network, no model — severed at the
  repo's own `trunk` by NAME, so a trunk pull request cannot dissolve every stack rooted on it and a
  FORK is not mistaken for a trunk. Steps are laid out depth-first, a branch says which step it left
  from, and a step's number is its DEPTH — carrying `+` ("at least") whenever the stack's lowest
  steps are not in the queue, because a number that claims more than it knows is the defect.
- **Blind-spot reporting** — the queue states what it could not see rather than under-reporting.
- Summary caching keyed by head SHA; a parallelism throttle so summaries do not take the rate-limit
  window from working boxes; the count poll deliberately off the board tick.
- **The queue payload is the ROW shape, and the prose arrives when a row opens** (SKEIN-287). A
  collapsed row draws the line, the flags, the depth and whether a review is drafted; the brief, the
  signals, the ownership and the drafted review itself come back per row, off disk, through
  `/review/:n/summary?held=1` — a request to REMEMBER, which can never become a model call on any
  head at any hour of the budget. Measured locally over thirty-nine readings: 155,167 B → 12,055 B
  for the list, 3,969 B for one opened row.
- **The scope of what skein reads on its own**, in one sentence: *if you pressed it, it is free and
  unconditional; if skein decided to read it, that happens only in a repo you switched read-ahead on
  for, and it is counted against the day.* Per-repo consent is off until somebody switches it on
  (`repos::Repo::read_prs`), the scope is a review somebody asked you for or a pull request you
  opened — a mention is neither, and a draft is never read whoever wrote it (`review::worth_reading`
  over `review::worth_a_visit`) — and BOTH readers obey it: the ten-minute background pass and the
  review pane's own pump. It is enforced at the model call (`review::unasked_scope`) so no client
  can widen it, and the day's ceiling (`Config::review_reads_per_day`) counts only that side.
  Anything a person presses — expanding a row, "read it", "re-read" — is `review::Trigger::Asked`:
  unscoped, never refused, never counted. SKEIN-242/265/277.
- **A round runs when somebody asks for one** (SKEIN-444). A pull request skein has read before is
  not re-read because a commit landed. The trigger is GitHub's own review request — `Pr::
  my_review_requested`, checked in `review::spend_a_visit` before the diff is downloaded — so the
  round costs nothing at all until an author says they are ready. This matters because
  `Reason::Reviewed` keeps a pull request in `review::in_reading_scope` for ever: without it, one
  review of a busy branch buys a round on every push to it, indefinitely. The kept reading is NOT
  cached under the un-read commit, deliberately — a review request arrives without the diff
  changing by a byte, and a cache entry keyed by the commit would swallow it. The row says which
  commit went unread and why (`Summary::not_reread`) rather than looking current. A press is
  `Trigger::Asked` and never reaches the check.

  This replaced a gate that spent the round's first turn asking the model whether re-reading was
  worth it (SKEIN-379). It worked; it was the wrong shape. It cost a turn per commit to find out,
  and its answer was a judgement nobody could predict or audit. The owner's verdict:
  *"I think we were complicating what the trigger for new round should be. Let's just reuse github
  request review thing."*
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
- Fleet resource and limit editing, GitHub credentials, read token, health.
  (`/api/fleet/substrate` is **not** a second readout — it *is* the package-request queue above.)
  Transport reporting is **not** on this list — §7 deletes the transport and the readout with it.
- **Fleet create, resize and sizing are parity for the *warden*, not for in-fleet skein.** Create
  and destroy terminate their own reconciler, so `docs/architecture.md` §7.5 puts fleet lifecycle
  outside the fleet *permanently* — requiring in-fleet skein to serve them would require the thing
  the architecture forbids. The gate is that the operations survive the move, at the warden (§8),
  with skein's side being the surface that asks for them.
- **Host capacity is no longer measured, and reporting nothing is the requirement.**
  `fleet::host_capacity()` (`src/fleet.rs`, `pub fn host_capacity`) used to read
  `available_parallelism()`, `/proc/meminfo`'s `MemTotal` and `df -Pk /`. Inside the sandbox all
  three answer for the **sandbox**, not the host — `nproc` → 11 and `MemTotal` → 25.8 GiB on this
  box, against a 12-core machine — and `proposed_fleet_size` would then offer 70% of the fleet's own
  share as 70% of the machine, so a fleet resized from that proposal shrinks every time somebody
  accepts it. Skein only runs inside the sandbox now (SKEIN-576), so every reading it could take is
  the wrong one and it takes none: the function returns zeros and an empty disk path, which is the
  vocabulary `HostCapacity` already had for "could not be read". So the parity requirement is **not**
  "reports host capacity". It is that a number which cannot be honestly measured is not supplied to
  the sizing decision at all — the same failure `proposed_fleet_size` refuses to make with
  `configured_field`. Sizing a new fleet therefore needs a person's number or the host's, which is
  §7's entry on the rebuild control.
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
  host-capacity measurement (of whichever machine skein stands on — see above), and refusal to
  save when config is unparseable.
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
  **The forward survives the move; only the key file does not.** See §7.
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

**The fleet-wide GitHub secret is no longer seeded, and the control that did it is gone.** Skein
ran `sbx secret set -g github -t "$(gh auth token)"` once per machine, so a box that had not been
scoped could still fetch, push and open PRs. Architecture §13a deletes the machine-global store, and
both halves of the seeding were the host's anyway: `gh auth token` reads the host's login and
`sbx secret set` writes the host's keyring. The **Overwrite token on startup** switch went with it
(`force_gh_secret`, `$SKEIN_FORCE_GH_SECRET`) — it existed only to pass `-f` to a call that no longer
happens.

What is lost, stated in the user's terms: **a fleet that has never been seeded can no longer give an
unscoped box any GitHub credential at all.** Scoping is the path — a GitHub App or a per-repo PAT,
both skein's own and both configured in Settings → GitHub & keys. What is *not* lost is a fleet that
was seeded before this: the secret is in `sbx`'s store, which rebuilding the fleet does not touch,
and `repos::gh_secret_seeded` reads the marker on the volume, so `gitgate::box_credential` still
answers `Account` for it correctly. The remaining switch says whether boxes are *meant* to push as
the account; it no longer claims one is there.

**Moving the volume is reported, never performed** (SKEIN-574). `skein volume move` used to do the
work: measure, copy the tree whole with its symlinks, re-mint the secrets, repoint the paths, and
leave a marker on the old copy so it could not be quietly reused. It still refuses when the fleet
sandbox is up — and now it always is, because skein runs *inside* that sandbox and
`config::load_config` gives an empty fleet name the default (SKEIN-484), so `fleet::fleet_exists`
answers `Some(true)` for the one name that matters. The volume is bind-mounted at sandbox create;
moving it is architecture §7.5's shape one level down, an act that ends the process performing it.

So it is an **Operation** (§2.4) with `class: destructive`, which means nothing in skein may drive
it however the check reads. What a person gets instead of a button is the recipe, printed where the
refusal is: the `sbx rm -f` line, the `mv`, the `export SKEIN_HOME=`, and the `sbx create` line with
the new path — the two `sbx` lines rendered by `warden_client::Act::command`, the same renderer the
warden's approval prompt and `skein doctor` use, so what somebody is told to type cannot drift from
what the warden would run. The operation's id is derived, not minted, so asking twice about one move
names one operation.

What is lost is the one-command move. What is not lost is any of the machinery: the copy, the
re-minting and the "do not quietly reuse the old one" marker are all still there and still tested —
they are what a person's `mv` is checked against, and what a host-side doer would call the day one
exists.

**`skein doctor` no longer has an `sbx` row, and a missing `sbx` is never a fault.** It used to be
one: on a host, no `sbx` meant no box could be created, started or entered, and the report said so
with `PATH` in the fix. Skein runs inside the fleet sandbox and `sbx` is host-only, so its absence
here is the expected state — a red banner for it would hand somebody a fault they cannot clear and
hide, behind a false alarm, the one thing they wanted to know.

What is lost, in the user's terms: **nothing on skein's own report will tell you `sbx` is broken or
missing on your host.** If it is, `sbx` says so when you run it, and the surfaces that need it —
the fleet-lifecycle recipe below, and `bootstrap.sh` — print the exact line to run. What is not
lost is the check itself as an answer: `health` still reports `sbx` as *satisfied with a reason*,
saying that skein enters a box by its namespace rather than through `sbx`, so a reader who wonders
where it went is told.

**The deployment panel is gone, and with it the sentences saying where your skein runs.**
`skein doctor` printed a `deployment` line — `host-driven` or `in-fleet`, and one sentence on what
that meant for the file picker, the keyring and the ssh-agent — and `/api/health` carried the same
three fields (`label`, `implies`, `in_fleet`) so the cockpit could hide the fleet-rebuild button
where pressing it would destroy the fleet. There is one deployment, so there is nothing to report
and nothing to branch on.

**The rebuild button went with it, and it is not coming back on a flag.** Settings → Fleet used to
offer *Rebuild the fleet at these limits*, shown only when the server said skein was on a host. It
is now absent unconditionally, and the reason is not that a flag says in-fleet: `docs/architecture.md`
§7.5 puts fleet lifecycle **outside the fleet permanently**, because create and destroy both kill
skein — create because the sandbox does not exist yet, destroy because it will not afterwards — and
a resize is a destroy followed by a create.

What a person gets instead, in the same place on the same pane: a row saying applying those numbers
is a job for the host, and the `sbx` lines to run, rendered by `warden_client::Act::command` — the
same renderer the warden's approval prompt and `skein doctor` use, so what somebody is told to type
cannot drift from what the warden would run. `POST /api/fleet/resize` still exists and still
refuses with those lines, because a post can still arrive from a tab left open on an older build.

What is genuinely lost: **memory, CPUs and fleet disk can no longer be changed from the cockpit at
all.** They are fixed when the sandbox is created. The numbers are still saved and still describe
what the *next* sandbox gets; making that sandbox is a person's act on the host.

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

**And the message that named one is gone too** (SKEIN-576). Opening a terminal on a name skein has
no placement for used to have three answers, not two: placed, absent, and *foreign* — a sandbox
`sbx ls` knew about that skein did not create. The foreign one said whose sandbox it was and how to
reach it anyway (`sbx exec -it <name> bash -l`), and how to let skein own it (`skein add`). That
question is about the HOST's machine and nothing inside the sandbox can answer it, so the arm is
gone rather than guessed at. In the user's terms: **a sandbox you made yourself now reads as "box
&lt;name&gt; does not exist"**, and the way to reach it is `sbx exec -it <name> bash -l` from the host,
typed rather than offered.

**`/api/pick-path` and every Browse button — removed, not replaced. Already done, not pending.**
The route is gone from `src/bin/skein-server.rs` (`grep -c pick-path` → 0) and `src/cockpit.rs`
asserts its *absence* from the served page, so §5 no longer lists it as a capability to preserve —
this section is where it lives now. The native host picker needs a host process with display access,
which in-fleet skein cannot have. Browse existed mainly to pick a
local repository path, and repositories are remotes now, so its main job is gone with it. The
remaining fields — the shared-data folder and the SSH key path — become text inputs **with a check
that reports whether the path resolved**, which satisfies law 1 without a host round-trip. A
warden-served picker was considered and rejected: another warden endpoint for an affordance used twice in a fleet's life.

**The host ssh-agent path — the key file, not the agent.** An earlier revision of this section said
in-fleet skein has *no host ssh-agent*, and prescribed moving SSH remotes to the warden or to HTTPS
on that basis. That is work the move does not require. `sbx create` forwards the host's agent into
the sandbox, so `$SSH_AUTH_SOCK` inside the fleet **is** the host's agent and `ssh-add -l` lists the
host's keys — the forward is in the same place whether skein stands beside the sandbox or inside it
(`docs/delivery.md`, SKEIN-108). What does not travel is the key **file**: `~/.ssh/id_ed25519` is a
host path and the sandbox has its own `~`, so `ensure_ssh_key` refuses in-fleet with *where to run
`ssh-add`* rather than failing on a missing file, which would read as a mistyped path
(`src/config.rs`, the `in_fleet()` branch of `ensure_ssh_key`). **The parity requirement is the
refusal message and the forwarded agent, not a replacement transport.**

**Transport reporting.** The first draft listed this as parity *and* deleted the transport. The
transport goes; the readout goes with it. This is recorded here because §12.2 of the first draft
claimed no user-visible feature hid in the deletion list, and this was the counter-example.

**Done, and here is what actually went** (SKEIN-573). `src/fleet-agent.py` is deleted, with its
client in `place`, its install and supervision in `fleet`, its `fleet_agent` and `fleet_agent_port`
settings, its `/api/fleet/transport` route, and the cockpit's `link` gauge — together with the
browser check that read that gauge (`tests/ui/smoke.mjs`), which asserted the row exists and names
`sbx exec`, and so could only have been repaired by putting the row back. `Place::bytes`, `attempt`
and `write` each had two implementations — the agent, then the spawn — and now have one;
`exec_sbx`, which existed only to bypass the agent, is gone with the thing it bypassed.

What a person loses: **the readout**, which said which way skein was calling the fleet and warned
when an agent was configured but not answering. There is nothing left for it to report. What they do
*not* lose, because both moved to `src/dockerd.rs` under `skein-server` rather than going with the
agent: the **Docker watchdog** (a container that kills `dockerd` would otherwise cost a rebuild of
the whole fleet) and **`Signal::MachinePressure`**, whose cgroup and `vmstat` counters are now read
directly rather than fetched over HTTP — which makes the signal cheaper, not poorer, since the reads
were always what produced it.

One capability is genuinely narrower and it is worth naming: a write into a box used to be able to
report an agent-side timeout as *"the command did not finish in time"* distinctly from a transport
failure, because the agent could tell "the script ran and is still running" from "I could not reach
you". With one path there is no such distinction to draw — a spawn that does not come back within
its deadline is reported as exactly that. Nothing is silently retried, which was the property the
distinction protected.

**The installer, the port, and the verb** (SKEIN-576). `skein fleet-serve` is deleted, with
`install_server` (a 1 MiB binary carried in over stdin), `server_binary` (the ELF chooser that
refused a Mach-O naming `unknown-linux-musl`), and `ensure_fleet_server` (the whole move: volume
checked, binary installed, doorway reloaded, port published). `bootstrap.sh` does all of that inside
the sandbox and its own header says so — *"the host holds one downloaded file"* (SKEIN-312). The
publishing goes too: `ensure_server_port`, `publish_forward` and `free_host_port`, which between
them tried the sandbox's own number, then one OS-assigned port, and prompted only once both had
failed.

**`ensure_fleet_door` stays**, and is not part of this. It is §9.4's squat guard, reopened from
`ensure_fleet` on every box start, and `bootstrap.sh` installs the doorway itself.

What a person loses, in order of how much they will notice it:

- **`skein fleet-serve`**, the verb. Nothing replaces it: a fleet is created through the warden and
  then runs `bootstrap.sh` inside itself.
- **`skein fleet-serve --stop`** is now **`skein cockpit-stop`**. The stopping was never the
  installer — it kills the server and leaves the doorway holding the port — so it needed a name of
  its own rather than a flag on a verb that has gone. Not a bare `stop`, which already means "stop a
  box".
- **Skein publishing the port at all.** It prints the line instead: `publish_cockpit_port` is an
  Operation (§2.4) whose `check` is three-valued and whose `recipe` is the `sbx ports … --publish`
  command, quoted exactly as it must be typed. In the fleet the check is always `unknown`, because
  `sbx` is host-only and *"cannot ask"* is not *"nothing forwards it"*. There is no doer:
  `Act::Publish` deliberately has none (§9.4 — the warden ships `Unpublish` and not its mirror), so
  nothing drives it and the recipe is the whole of what skein offers. This is `docs/delivery.md`'s
  rule at a second site: an unreachable doer does not fall back to running `sbx`, "because that
  fallback would be taken on exactly the day something was wrong".
- **The two-candidate retry.** A publish that does not settle is a person's to notice now. What it
  bought was a second chance at a mapping skein could not withdraw; what it cost was a second
  permanent mapping on every failure.

**§9.4's guard did not go with the publisher — it went with the advice, and that is the part worth
reading twice.** `ensure_fleet_server` refused to publish onto a port the doorway did not hold,
judged by the doorway's own stamp and never by a TCP connect, because a squatter accepts exactly as
a doorway does and a mapping handed to one has given away the browser's token before anyone could
take it back. Deleting the publisher deleted that refusal, and the person who now types the command
is acting on what skein told them — so a guard that is fooled no longer publishes to a squatter
itself, it *advises somebody else to*. `cockpit_port_advice` is where it lives now, and
`ensure_fleet` reports through it: on a fresh fleet it prints either the recipe or the §9.4 refusal,
and never the recipe when the stamp says the doorway is not there.

Of the fourteen tests in `tests/fleet_move.rs`, **two go and twelve stay**. The two are the stdin
carrier's own: a binary replaced while the old one is still executing (`ETXTBSY`, which is a
property of writing onto a live ELF from outside), and the cross-build refusal. A third was two
tests welded into one body — the install-then-start-then-publish ordering, which only existed
because one host-side function did all three, and the fd-3 handover, which was never about the
installer and survives as `the_server_behind_the_door_inherits_the_doorways_socket`. The rest keep
their properties and change their drivers: an upgrade reloads the doorway rather than restarting it
(now driven by `reload_server`, as `bootstrap.sh` drives it), and the squatter is not mistaken for
the door (now `cockpit_port_advice`).

**Creating a fleet stops being a side effect and becomes an act** (SKEIN-576). `ensure_fleet` no
longer creates: it ran on every box start, and creating a fleet as a consequence of launching a box
was never something a person asked for. In-fleet the branch was unreachable anyway — `ensure_fleet`
asks about the fleet this process is *inside*, which answers itself — so the code that appeared to
handle "the fleet is missing" had not run in that deployment at all.

What a person gains: **the cockpit's create works from inside the fleet**. It used to refuse with
the host lines, on the reading that §7.5 forbids lifecycle in the fleet; §7.5 is about where the
*doer* runs, and the warden is on the host with the capability. So the request goes to the warden
over `http` (§2.3 already lists it as a Source), the warden approves and performs on the host, and
`fleet::request_fleet_create` carries the attempt lease that stops two presses becoming two fleets.

What a person loses: **a box start no longer creates a missing fleet for them.** It now refuses,
naming the pane that does it. That is the intended trade — the alternative is a privileged, minutes-
long, machine-shaped act happening because somebody typed `skein start`.

What does NOT change: with no warden reachable this refuses and prints the `sbx create` line. It is
the same `Operation` shape as the cockpit's port, and the only difference is that this one has a
doer (`Doer::Warden`) — which is what makes the difference between them data rather than two
spellings of one decision.

**You can no longer size a fleet from the cockpit** (SKEIN-627). There was a dialog for it — memory,
CPUs and disk, each with what the machine had beside it, on the way to your first box — and it is
deleted, on the same argument as the rebuild button one entry above. `bootstrap.sh` runs inside the
sandbox, so the sandbox is made before skein is, and skein cannot report its own fleet as missing:
`fleet::fleet_exists` answers `Some(true)` for the fleet it is standing in and `None` for any other
name, so the `exists === false` the dialog opened on was a state nothing could produce. Nobody ever
met this screen.

What that costs is real even so, and it is this: **the three numbers are now chosen for you, once,
by whoever ran the create — and sbx fixes all three permanently.** Changing them afterwards means
destroying the sandbox and making it again, which is the entry above, on the host. The place to get
them right is the `sbx create` line, which `skein doctor` prints for this installation.

What is not lost: `POST /api/fleet/create` and `fleet::request_fleet_create` both stay, warden and
attempt lease intact. Creating a *differently-named* second fleet was never the impossible one, and
this removes the sizing surface rather than the route.

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

**The review pane's reading view, and the line notes drafted on it — gone, on the same argument
one paragraph up (CKP-7).** The review pane had its own diff surface: `↵` opened a pull
request's change in skein, `j`/`k` walked it by hunk, `]`/`[` by file, and `c` drafted a comment on
the focused line, posted WITH the verdict under GitHub's review semantics. It is deleted. The row is
the surface now — `↵` opens the row, the verdicts are chips in its body, and the change itself is
read on GitHub.

What is lost is **drafting a line note inside skein**. That is the whole of it, and it is the cost
worth stating plainly: a reader who wants to say "this line, here" now says it on GitHub. What is
NOT lost, and is easy to conflate with it: the server still accepts line comments on
`/review/:n/act` and `prq::submit_review_with_comments` still posts them — the agent's own review
path uses it (`src/prwork/perform.rs`) — so the capability exists, without a cockpit surface that drafts
against a diff. The keys the surface owned (`c`, `r`, `]`, `[`) stay in the REVIEW table because
being there is what stops them reaching the fleet map behind the pane; what they should answer was
settled by SKEIN-568 (`12824652`): each refuses with a toast naming where the thing it addressed
went — `c` and `r` name the chip that now does what they used to (`comment…`, `request changes…`,
the exact labels `revVerdictHtml` writes), and `]`/`[` name the key that reaches the diff now that
nothing in the pane is file-shaped (`g h` opens the change on GitHub). `tests/ui/review.mjs`
asserts all four toasts.

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

**The "no review — read again" control is gone, and a stack can no longer name a step that was
read without one** (SKEIN-660, and it was SKEIN-371's). The row control, its "skein read this commit
and no review came back" tooltip, and the stack's "N read but with no review" shortfall all rested on
one page function, `revNoReviewCameBack`, whose only evidence was a summary field named
`critique_because`. No server has sent that field since the drafted review moved to GitHub — `Known`
is a `Summary` and a `stale` flag, and neither names it (`src/review/summary.rs`) — so the branch
answered false on every real payload and the three surfaces were unreachable. What is lost is stated
rather than hidden: **read is now read**, and nothing on the wire separates a step whose review came
back from one whose review was bought and failed, or from one nobody ever asked to review. SKEIN-371
was a real complaint — ten steps of a twenty-step stack counted as read with no review, no retry
offered on any of them — and that failure mode still exists; the page simply cannot report it. It
comes back only if the server says it again first, as a field on `Summary`.

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
still holds; and the cockpit's browser suites, which had gone unrun for 160 commits, pass and
are run by `cargo test` (SKEIN-113). What that pass found is in SKEIN-110 through SKEIN-117.


- The 265 JavaScript functions were sampled, not enumerated one by one. This is the largest
  remaining hole and the only honest way to close it is to walk them.
- Settings controls were counted from the UI. The count mismatch against `Config`'s 24 fields is
  explained, not outstanding: repo and connection settings are not `Config` at all.
- **Two rounds of this document invented capabilities.** Treat any entry with no file reference
  beside it as unverified until someone greps for it.
- **The git-scope boundary is a property of the token, not of the box — recorded here as a known
  non-property rather than a capability.** Measured from inside a live box on 2026-09-06: the
  sandbox routes HTTP through a credential-injecting proxy, so a request carrying no Authorization
  header, or a deliberately invalid one, is answered as the account. A box's own `GH_TOKEN` returns
  `401` when sent directly, which makes it a placeholder rather than the credential anything
  authenticates with. `SKEIN_GIT_SCOPE`, the per-repo tokens, `git-credential-skein` and the
  ssh-agent bind therefore govern a credential a box does not need in order to reach GitHub.
  The README's claim that the boundary "is real" was corrected rather than deleted, because the
  token half of it is true and the network half never was. **This is substrate behaviour, not a
  skein defect — but skein asserted the boundary, so it is skein's to enforce or to retract.**
  SKEIN-548, open. The audit above did not test egress; nothing in this document should be read as
  a claim about what a box can reach over the network.
