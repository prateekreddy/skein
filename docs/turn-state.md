# Turn state: why it goes stale, and the primitives it should be built from

Status: design note, not yet implemented. Written from evidence collected on a live box
(`example-box-6`) on 2026-07-27.

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

## 7. Open questions

1. Do tmux `alert-silence` hooks fire for the *attached* window? If yes, the 1s loop becomes
   event-driven. (One experiment.)
2. Codex grammar needs the same capture treatment its Claude counterpart just got.
3. Is `pane_title`'s activity text worth surfacing as the row's "what it's doing" (`Run bash
   command true`) — cheaper and fresher than the TodoWrite probe?
4. Should `decision` carry *which* dialog (permission vs question vs trust vs auth)? The inbox
   already ranks pauses; this would make the ranking real rather than text-classified.
