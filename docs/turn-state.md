# Turn state: why it goes stale, and the primitives it should be built from

Status: **implemented for Claude** (§8), Codex pending its approval-dialog capture. Written from
evidence collected on a live box (`example-box-6`) on 2026-07-27, and revised by what a live
Claude TUI actually did.

## 1. The symptom

A box shows **decision** long after the decision was made. Same class of staleness, other guises: a
box that shows **working** after its agent died, **ended** while it is happily working, **live**
while it sits on a trust prompt nobody can see.

## 2. What the evidence says

Every box keeps a hook heartbeat at `<store>/hook-log/<vmid>.jsonl`. This box's log, verbatim:

```
2026-07-25T13:24:10Z box-status working          ← UserPromptSubmit
2026-07-25T13:27:13Z box-status notify-blocked   ← permission prompt: status := "blocked"
2026-07-25T13:47:25Z box-status waiting          ← Stop, TWENTY MINUTES LATER
```

Nothing fired in between. The permission was answered seconds after 13:27:13 and the agent worked
for twenty more minutes — and for those twenty minutes `status/<vmid>.json` said `blocked`, so the
board said **decision**. That is the bug, exactly as reported, in the user's own box.

Three more findings from the same log:

- **17 `ended` events in 114 seconds** (13:20:55 → 13:22:49). Those are the reconnect loop running
  `claude --continue` against a box with no conversation: each failed launch is a *session* that
  starts and ends, and `SessionEnd` → `status := "ended"` (tier 4, sticky). One box, one status
  file, but *every* session in the box writes it — interactive, headless `claude -p` helpers,
  failed launches. A helper's death is indistinguishable from the agent's.
- **`agent-stop` with no matching `agent-start`.** `SubagentStop` fired (compaction runs as a
  subagent); `PreToolUse(Task)` never did. The in-flight counter that gates `notify-blocked` is
  therefore not a reliable count of anything. It clamps at 0, so today it mostly fails safe — but
  a leaked increment (interrupted `Task`) makes every real decision in that turn invisible.
- **A `/compact` whose PreCompact/PostCompact hooks reported success wrote nothing to the log.**
  The probe silently `exit 0`s when it can't resolve a store. "Hook ran" and "state recorded" are
  not the same event, and nothing tells them apart.

Wiring gaps confirmed in the live `settings.json`:

| event | wired to | clears `blocked`? |
| --- | --- | --- |
| `Notification` (permission matcher) | `box-status.sh notify-blocked` | sets it |
| `PostToolUse` | `box-task.sh` — **matcher `TodoWrite` only** | no |
| `Stop` | `box-status.sh waiting` | yes, at end of turn |
| `UserPromptSubmit` | `box-status.sh working` | yes, next turn |

Claude Code has no "the human answered" event. Codex does better by accident: its `PostToolUse`
is wired with matcher `*` → `working-tool`, so an approved tool completing clears the block.

## 3. Why — the structural fault, not the four bugs

Skein derives turn state from **edges** (hook events) and stores the result as **latched state**.
That design has one failure mode, and every symptom above is an instance of it:

> An edge-triggered latch with incomplete edge coverage cannot recover. A state nobody clears is
> shown forever.

The edges we never get: permission answered, dialog dismissed with Esc, turn interrupted, agent
crashed/OOM-killed, trust prompt shown before any session exists, auth expired, usage limit hit,
tmux session replaced by a takeover. Each one leaves the last latch in place.

The two secondary faults are the same shape:

- **Wrong key.** State is keyed by *box* but written by *session*. Any session in the box can
  overwrite the agent's state (17 `ended`s above).
- **No reconciler.** Only `working|running|compacting` have a TTL (45 min). `blocked`, `waiting`,
  `error`, `ended` are eternal. There is no observation that says "check whether that is still
  true", because there is no *level* signal anywhere in the system.

## 4. Primitives

Separate **observations** (things that can be measured, each with a timestamp and a failure mode)
from **states** (compositions over observations). Today's code has one observation, promoted
straight to state.

### 4.1 Observations

| # | observation | how | kind | cost | fails when |
| --- | --- | --- | --- | --- | --- |
| O1 | box exists / running | `sbx ls --json` (already) | level | cached 1.5s | sbx absent, direct-mode box |
| O2 | pane meta | `tmux display-message -p '#{pane_dead} #{window_activity} #{pane_title} #{pane_current_command} #{pane_pid}'` | level | ~1 exec | no tmux (legacy box) |
| O3 | pane tail | `tmux capture-pane -p -S -30` | level | ~1 exec | provider redesigns its TUI |
| O4 | lifecycle edge | today's `status/<vmid>.json`, reread as *last edge* | edge | free (file) | event not emitted |
| O5 | human input | bytes skein's own PTY proxy forwards to the box | edge | free | user acts outside the cockpit |
| O6 | transcript tail | in-box session JSONL | level+edge | jq over a tail | path/schema drift |
| O7 | agent process | `pgrep`/`ps` under `pane_pid` | level | ~1 exec | wrapper shells |
| O8 | statusline pull | provider invokes our statusline command | edge | free | provider statusline disabled |

Measured facts behind O2/O3 (this box, tmux 3.6):

- Busy: `pane_title` = `⠂ Claude Code`, `window_activity` advances every ~1s (the spinner redraw —
  the "glowing thing" *is* a machine-readable signal).
- Idle: `pane_title` = `✳ Claude Code`, `window_activity` stops advancing (9s+ stale).
- **The title lags**: while blocked on a Fetch permission it still read `✳ Run bash command true`
  from the *previous* tool. The title is a hint, never a verdict.
- Blocked, verbatim bottom-of-pane:
  ```
   Do you want to allow Claude to fetch this content?
   ❯ 1. Yes
     2. Yes, and don't ask again for example.com
     3. No, and tell Claude what to do differently (esc)
  ```
  The composer frame and the `⏸ manual mode on · ? for shortcuts` hint line are *gone* while a
  dialog is up, and back the moment it is answered. That presence/absence is the level signal.
- Blocked and idle are indistinguishable to `window_activity` (both silent). Busy/not-busy is
  cheap; splitting not-busy into waiting/decision/error/dead needs the pane text.

### 4.2 Runtime grammar (per provider, table-driven — `RUNTIME_ADAPTERS` is already the seam)

Five predicate groups per runtime, matched against the **last ~15 lines only** (never scrollback,
so the agent printing "Do you want to…" in its own answer cannot fake a dialog):

- `busy`: spinner line (`✻ Baked for…`, `✢ …(4s · ↓ 16k tokens)`), `esc to interrupt`
- `decision`: `Do you want to…?`/`Would you like…` + a `❯ N.` option list; trust prompt; `/login`;
  Codex's approval box
- `waiting`: composer frame + hint line, no dialog, no spinner
- `error`: `API Error`, `usage limit reached`, `Overloaded`
- `dead`: `pane_dead`, a shell prompt where the TUI should be, no agent process under `pane_pid`

### 4.3 Composition

State is a **pure function of observations**, not an accumulator:

```
state = reconcile(liveness=O1, level=class(O2,O3,O7), edge=O4, acted=O5)
```

with four rules that between them kill the whole bug class:

1. **Hard gate.** `O1 = Stopped` → `stale`, whatever anyone claims.
2. **Attention never latches.** `decision`/`error` survive only while a *level* observation
   younger than ~2 sample intervals still shows them. No corroboration → decay to `waiting`.
3. **Freshest wins, edges lead.** An edge newer than the last level sample is displayed
   immediately (latency win); the next level sample confirms or corrects it. Answering in the
   cockpit (O5) instantly clears attention optimistically, because skein *delivered* the answer.
4. **`unknown` is a value.** An unparsed pane reports `unknown` + a health flag, never a guess.

Note what this deletes: the sub-agent counter (the dialog's presence is the truth, so "is it
waiting on its own agents" stops mattering), `ended`-from-`SessionEnd` (derive from O7/O2 instead,
so helper sessions can't kill a live box), and the 45-minute `working` TTL (rule 2 generalises it).

### 4.4 Where the classifier runs

Recommended: **in-box, on settle.** A tiny loop (or `monitor-silence` + `alert-silence` tmux hook,
if it fires for the attached window — needs one experiment) samples O2 each second and runs O3 +
classification only when output settles, writing the level observation into the store beside
today's edge file. Consequences: host cost stays zero (it reads a file, as now), clone-mode boxes
work, every runtime is covered by one mechanism, and the states hooks can never see (trust prompt,
`/login`, crash, Esc) become visible.

Rejected alternative: host-side polling via `sbx exec` per box per tick — N execs × every 2s ×
every open cockpit, and it re-derives on the *host* what the box can compute for free.

Free extra: for a box whose terminal is open, the server is already relaying every byte — it can
classify from that stream with no exec at all.

## 5. Scenarios

`✗` = wrong today. Assume Claude unless noted.

| # | scenario | today | composed |
| --- | --- | --- | --- |
| 1 | permission answered in cockpit | ✗ decision until Stop (20 min observed) | cleared instantly (O5), confirmed on settle |
| 2 | permission answered in a native terminal | ✗ decision until Stop | cleared on settle (≤~2s) |
| 3 | dialog dismissed with Esc | ✗ decision forever (no hook at all) | waiting |
| 4 | turn interrupted mid-tool (Esc Esc) | ✗ working until next prompt | waiting |
| 5 | `AskUserQuestion` / plan approval | decision (Notification) then ✗ sticks | decision → clears on answer |
| 6 | sub-agents running, main thread idle | working (counter) — brittle | busy (spinner is present) |
| 7 | sub-agent asks permission | ✗ working (counter suppresses it) | decision (dialog is on screen) |
| 8 | long tool, no output for minutes | working (spinner still redraws) | busy — unchanged, correct |
| 9 | agent crashed / OOM | ✗ working for 45 min, then `live` | dead/ended within a sample |
| 10 | box stopped | stale (O1) | stale — unchanged |
| 11 | compaction | compacting | busy (hook + pane agree) |
| 12 | API error / rate limit | error *if* StopFailure fired | error from pane too; clears when it retries |
| 13 | usage limit reached | ✗ silent | needs-you (level) |
| 14 | first launch trust prompt | ✗ live/working — hooks can't see it | decision |
| 15 | expired auth (`/login`) | ✗ whatever was latched | decision |
| 16 | launch failed → guard drops to shell | ✗ working | dead/shell |
| 17 | headless helper session ends | ✗ box shows `ended` | ignored (not the agent pane) |
| 18 | reconnect loop failing `--continue` | ✗ 17× `ended` on a live box | ignored |
| 19 | runtime takeover (claude→codex) | ✗ stale status from the old runtime | new pane, new grammar, correct |
| 20 | user typing an unsent prompt | waiting | waiting (optionally "drafting") |
| 21 | queued message while busy | busy | busy |
| 22 | two cockpit tabs open | same | same (single writer in-box) |

## 6. Implications, costs, risks

- **Screen scraping is provider-coupled.** Mitigated by: grammar tables per runtime, bottom-of-pane
  anchoring, `unknown` + a `screen_health` flag so drift surfaces instead of lying, and keeping
  hooks as the corroborating fast path. A provider TUI redesign degrades us to today's behaviour,
  not worse.
- **False positives from content.** Anchor to the last lines, require the structural option list,
  require the composer to be *absent*, require output to have settled.
- **Privacy.** Classification happens in-box; only the derived state (and at most the question
  line, which `box-session.sh ask` already stores) reaches the shared store. No pane text is
  written to a store that other boxes can read.
- **Concurrency.** One writer per box (the in-box classifier) for level state; hook edges keep
  their own key. Same atomic temp+rename as today.
- **Cost.** In-box: one `tmux display-message` per second (µs of CPU), one `capture-pane` +
  classify per settle. Host: unchanged.
- **Compatibility.** Needs a `probe_revision` bump; boxes running an older session keep hook-only
  behaviour and are already badged `hook_health: stale`. No host-side breaking change: the fleet
  keeps reading files.
- **Latency budget.** Cockpit-answered decisions: instant. Externally answered: one sample. Busy →
  waiting: one settle window (~2s). Good enough that a chip never feels wrong.
- **Testability.** The classifier is a pure function from pane text → state, so the grammar gets
  unit tests with the captured fixtures in §4.1 — including the exact 20-minute-stale case.

## 6a. Decisions taken (2026-07-27)

Answered by the user; these are settled, not open:

1. **Codex grammar** — capture it from a real box rather than guess (done below).
2. **Live task text** — surface the terminal title's activity text as the row's "what it's doing",
   ranked *below* the TodoWrite probe, and only while the box is busy (the title keeps the last
   tool's text after it finishes, so on an idle box it would lie).
3. **Decision kinds** — `decision` carries which dialog is blocking: **permission · question ·
   trust · auth-or-quota**. Each needs a different action from the human, and the inbox can then
   rank on fact instead of classifying headline text.
4. **Staging** — land as one complete change, not a stopgap first.

## 6b. Codex pane grammar (captured from `skein-codex-test`, Codex 0.145.0)

Captured by attaching to the box's **shell** session through the cockpit's terminal WebSocket and
running `tmux capture-pane` against its `skein-agent` session — so nothing was ever typed into
Codex's own composer to get these:

| state | evidence |
| --- | --- |
| busy | `pane_title` = `⠋ skein` (braille spinner + dir) **and** a pane line `• Working (1s • esc to interrupt)` |
| idle / waiting | `pane_title` = `skein` (no glyph); composer `› Use /skills to list available skills`; footer `? for shortcuts` + `NN% context left` |
| decision (command) | title **`[ ! ] Action Required \| skein`**; body `Would you like to run the following command?` + `Environment: local` + `$ date`; options `› 1. Yes, proceed (y)` / `2. … don't ask again for commands that start with …(p)` / `3. No, and tell Codex what to do differently (esc)`; footer **`Press enter to confirm or esc to cancel`** |
| decision (edit) | same title and footer; body `Would you like to make the following edits?` above an `• Added <file> (+1 -0)` diff; option 2 becomes `… don't ask again for these files (a)` |
| trust | `Hooks need review` / `19 hooks are new or changed.` / `Hooks can run outside the sandbox after you trust them.` + `› 1. Review hooks` / `2. Trust all and continue` / `3. Continue without trusting (hooks won't run)`, footer `…esc to go back` |
| auth | onboarding: `Welcome to Codex…` / `Sign in with ChatGPT to use Codex as part of your paid plan`; options switch to a plain **`> 1.`**; footer `Press enter to continue` |
| question | a bordered list with a `›`-marked numbered option (`› 1. gpt-5.6-sol (current)`) and the footer `Press enter to confirm or esc to go back` |
| error | a line starting `■ ` carrying **JSON**, e.g. `■ {"detail":"The 'gpt-5.6-sol' model is not supported…"}` |
| *not* an error | a prose `■ ` line — `■ Conversation interrupted - tell the model what to do differently.` — and `⚠ Heads up, you have less than 25% of your monthly limit left.`, both of which sit beside a perfectly live composer |
| tool result / hook | lines starting `• ` (`• bin, kit, lib.rs, probe, store, web`, `• SessionStart hook (completed)`) |

So both runtimes share two robust markers — a spinner glyph in the terminal **title** and the string
**`esc to interrupt`** on screen — which is what makes a single `busy` predicate viable across
providers, with only the dialog/idle grammars needing per-runtime tables.

Three things worth more than the table:

1. **Codex states its blocked-ness in the terminal title**: `[ ! ] Action Required | <dir>`. Captured
   through a full cycle, it appears when the dialog opens and clears on *both* answers (approve `y`
   and `esc`), and stays clear through the rest of a finished turn — so it is a decision marker, not
   an attention-grabber, and it is the best single signal either provider offers. It is the classifier's
   fallback when a body is worded in a way we have never seen, honest that it can't name the kind.
2. **The marker animates** (`[ ! ]` → `[ . ]`, ~1Hz). Anything that treats a title change as news
   would have rewritten the observation file every second for as long as the dialog went unanswered.
   The observer therefore compares the title's *text* with the entire leading run of
   non-alphanumerics stripped (which also reduces `⠂ Claude Code` and `✳ …` to something stable) and
   writes the raw title for the host to read the spinner from. Measured after the fix: one write per
   10s (the heartbeat) while blocked.
3. **A dialog replaces the composer, but working does not.** Codex keeps `? for shortcuts` on screen
   while it works, so "composer present" cannot mean idle for either runtime — `esc to interrupt`
   outranks it.

Two false friends the fixtures now pin down: the user's own submitted prompt is echoed with the same
`› ` glyph the options use (so a dialog needs the numbered form *and* the confirm footer), and Codex's
hook-trust gate blocks **before any hook can fire** — which is precisely the wall a skein box hits,
since skein installs its probes into every store ("19 hooks are new or changed" was skein's own).

How the captures were taken — a throwaway session beside the agent's, never touching `skein-agent`:

```
tmux new-session -d -s captest -x 120 -y 40 \
  'codex -m gpt-5.6-terra -a untrusted -s read-only --dangerously-bypass-hook-trust'
# then: "run the shell command: date"        → the command escalation
#       "create a file called … "            → the edit escalation
# CODEX_HOME=<fresh dir> codex               → the sign-in screen
# codex --cd <dir it has never seen>         → the hook-review screen
```

`-a untrusted` is the correct flag (`--ask-for-approval` values are `untrusted|on-request|never`;
the earlier attempt failed because it passed `--sandbox`/`--ask-for-approval` spellings this build
rejects, so the tmux session died instantly). That box's default model (`gpt-5.6-sol`) is rejected for
its account — `■ {"detail":"… not supported when using Codex with a ChatGPT account"}` — hence `-m`.

Two environment notes from that box, worth knowing before blaming a probe: **`/tmp` and `$HOME` are
read-only** there (Codex warns `Failed to save the conversation transcript … Read-only file system
(os error 30)` and `could not create PATH aliases`), so nothing may be written outside the workspace
and the shared store. And `#{pane_current_command}` reads `bash`, not `codex`/`claude`, because
skein launches the agent through a `||` fallback wrapper — the classifier must not depend on it.

## 7. Open questions

1. ~~Do tmux `alert-silence` hooks fire for the *attached* window?~~ **Answered: no — don't build on
   them.** With `monitor-silence 2` plus an `alert-silence` hook on a live session, no alert fired
   even in the easier unattached case; tmux suppresses activity/silence alerts for a session's
   *current* window, and skein's session has exactly one window, always current. The 1s sampler in
   §4.4 stands (and costs ~0.3% of a core).
2. ~~Codex grammar~~ — captured in full, including every dialog, see §6b.
3. ~~Title activity text~~ — decided, see §6a.2.
4. ~~Decision kinds~~ — decided, see §6a.3.

## 8. Where this stands — built and verified live (2026-07-27)

Shipped for **Claude and Codex**; any other runtime returns `Unknown`, i.e. today's hook-only
behaviour, rather than a guess at a grammar nobody has read.

- `src/probe/box-pane.sh` — the in-box observer. Records activity age, title (+ measured title
  freshness), dead-ness and the visible tail into `<store>/status/<vmid>.pane.json`. Installed by
  `ensure_probe_in`, started `setsid`+`nice -n 19` by the attach command, single-instance via a
  box-local lock, exits when its tmux session goes.
- `classify_pane` / `read_pane` / `fuse_status` in `lib.rs` — the grammar and the four rules, with
  unit tests built from the captures in §4.1.
- `BoxView.blocked_kind` + cockpit chips: `decision` · `asks` · `trust?` · `sign in`.
- Pane file removed alongside the status file on delist.

### Verified against a live Claude TUI (not fixtures)

A throwaway `claude` in its own tmux session, its own fake store, **no hooks installed at all** — so
every state below came from the screen alone:

| moment | board said |
| --- | --- |
| idle composer | `waiting` |
| real WebFetch permission dialog on screen | `needs-input` (kind `permission`) |
| 4s after pressing esc | `waiting` — **the twenty-minute bug, gone** |
| 2s into a turn | `working` (`esc to interrupt` + braille title glyph) |
| turn ended | `waiting` within 1s |

Cost, measured in-box over a 61s window spanning a live turn: **0.11% of one core** (nice 19).

### Two things the live run corrected

1. **Transition latency.** Capturing only on `moving`/title change or the 10s heartbeat meant a turn
   *starting* — which changes only the tail — could take ten seconds to show. The observer now
   captures every tick while the screen is live and writes the moment the screen's *shape* changes
   (dialog present, busy marker present, last line), with a 2s floor for streaming churn.
2. **The terminal title is not "what it is doing".** It froze on `Fetch and quote robots.txt file`
   for fifteen minutes across unrelated turns, including while running a different tool — it appears
   to be set when Claude Code wants attention (so a background tab shows it), not per tool call. The
   title-derived task line (§6a.2) is therefore gated on freshness the observer has *witnessed*: the
   text part must have changed within 90s, and the spinner glyph does not count as a change (it is
   part of the title and changes every frame — which is what made a stale description look fresh).
   Net effect: correct but usually silent. Worth revisiting if a later Claude Code tracks the title
   per tool.

### Verified against a live Codex TUI

Same method: a throwaway `codex -a untrusted -s read-only` session in a box, watched by this
observer under its own vmid, classified from the file it wrote:

| moment | classifier said |
| --- | --- |
| idle composer | `Waiting` |
| real command-approval dialog | `Blocked(Permission)` (title `[ ! ] Action Required \| skein`) |
| after `esc` | `Waiting` — cleared, with nothing firing an event |
| while blocked | one observation write per 10s, not one per second (§6b.2) |

### 8a. Two defects a real box found that a throwaway one could not (2026-07-27)

Reported: *"when the session is waiting on a tool call you say waiting, so the statuses keep switching
regularly — but for this scenario you show working"* (a screenshot of an idle pane just after
`/compact`). Both halves were real, and neither could have shown up in the clean-room verification
above, because both need a **configured statusline** and a **tall pane with history** — i.e. an actual
box.

**1. The busy predicate matched a part of the status line that changes every two seconds.** Sampled
from a live box every 2s through four minutes of continuous work:

```
✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)                    ← old predicate `tokens)` matched → working
✽ Beboppin'… (3m 43s · ↓ 12.9k tokens · thinking)         ← no match                        → waiting
✻ Beboppin'… (4m 6s · ↓ 13.7k tokens · thought for 10s)   ← no match                        → waiting
```

Same state, three renderings — so the board alternated. Both backstops were absent too: `esc to
interrupt` appeared **nowhere on screen** in that build, and the title's glyph was sometimes braille
and sometimes `_`. What *is* invariant is the line's shape: a spinner glyph, a verb ending in an
ellipsis, then a parenthesised **elapsed time**. `is_working_status_line` matches that and nothing
else — tool announcements (`● Running 4 shell commands…`) have the ellipsis but no elapsed time, and
the completion marker (`✻ Sautéed for 24m 3s`) has neither, which matters because it sits above an
idle composer for the whole of the next turn.

The animation cycles `· ✢ * ✶ ✻ ✽` — including a plain ASCII `*`. An allowlist of glyphs was drafted
and thrown out: it would have flapped again the day a release adds a frame, and it *did* reject `*`
until live sampling caught it. The guard is a denylist of things a status line is never (`●` message
bullet, `⎿` tool result, composer prompt, quote, alphanumeric prose).

**2. The observer was capturing scrollback, so a finished turn's status line stayed "current".**
`capture-pane -S -24` does not mean "the last 24 lines": its coordinates are relative to the top of
the *visible* pane, so a negative `-S` reaches into history — `-S -24` meant "24 lines of scrollback
**plus the whole screen**", 54 lines on the reporting box. `/compact` redraws the screen and pushes
the pre-compact bottom — including its status line — into exactly that region, which is why an idle
post-compact box read as `working` and stayed there. Fixed by asking for `#{pane_height}` in the same
round-trip and starting the capture at `height - 24`; verified on the same live pane, 54 lines → 24.

Belt and braces, since the same class of mistake keeps recurring: a status line only counts within the
last 10 non-empty lines (it lives directly above the composer). Anything higher is the agent
*displaying* one — a log, a capture, a fixture in a diff. This file's own test fixtures were on screen
while being written, which is how that hazard was noticed.

One hypothesis was wrong and is worth recording: the missing `? for shortcuts` footer in the
screenshot was **not** the cause. A live idle pane with a statusline configured still shows its mode
line (`⏸ manual mode on · ← for agents`) *below* the statusline, so the composer was detected all
along. A bare-prompt check was added anyway, but the bug was the two defects above.

Verified live after the fix: this box's own pane, mid-turn → `Busy` on ten consecutive samples over
20s; a real idle Claude with a statusline → `Waiting`.

### One correction the Codex run forced on the Claude path

An error string sitting in the visible tail is *history* unless the screen is otherwise idle, so
`busy` now outranks `Error` for both runtimes. Before, an `API Error` from the previous turn — still
within the 24-line tail — read as `error` while the next turn was visibly running, which is the same
fault as latching an edge, just with text instead of an event.

### Still open

- The optimistic path (O5: skein knows it delivered your keystroke, so it could clear an attention
  chip instantly rather than within a second) is not wired; the sampler is fast enough that it has
  not been worth the extra moving part.
- A `screen_health` badge for panes that classify as `Unknown` repeatedly — the drift alarm §6 asks
  for. Currently an unparsed screen silently falls back to hook edges.
