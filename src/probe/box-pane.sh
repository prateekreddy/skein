#!/usr/bin/env bash
# skein box-pane.sh — the LEVEL observation of a box's agent screen.
# SHIPPED AND INSTALLED BY SKEIN (not the repo) — written into skein's read-only plugin under .skein
# (and a copy into <store>/skein/bin/) and started detached from the plugin's copy by the attach
# command (see runtime::pane_observer_start, SKEIN-1149). Usage: box-pane.sh <tmux-session>
#
# Why this exists: every other signal skein has is an EDGE (a hook firing on an event). Edge coverage
# is incomplete — no runtime fires anything when a human answers a permission prompt, dismisses a
# dialog with esc, interrupts a turn, or when the agent dies — so a state nobody clears is shown
# forever (a permission answered at 13:27 still read "decision" at 13:47; see docs/turn-state.md).
# This is the missing level signal: what the box's screen says RIGHT NOW, which is self-clearing by
# construction — the dialog is either on screen or it isn't.
#
# It deliberately does NOT classify. It records what it observed (activity age, title, the visible
# tail) into <store>/status/<vmid>.pane.json and lets the host interpret, because:
#   · the grammar is provider-specific and changes with each provider release — in Rust it is
#     unit-tested against captured fixtures and ships with the binary, instead of needing a probe
#     rollout into every store to fix a regex;
#   · the box stays cheap, which is the whole point (see the cost budget below).
#
# COST BUDGET — this runs forever beside a human's editor, so it is bounded on purpose:
#   · one `tmux display-message` per tick: ~2-4ms of CPU, no pipes, no jq, no `date` (bash's
#     $EPOCHSECONDS is a builtin);
#   · a `capture-pane` every tick only while the screen is live (age < 15s), else on change or
#     the 10s heartbeat — a quiet box costs one tmux call every few seconds and nothing else;
#   · a WRITE only when the screen's *shape* changes (a dialog appearing or clearing, the busy
#     marker arriving or going, the last line moving), on a 2s floor for streaming churn, or on the
#     heartbeat — so the host-mounted store sees a handful of writes a minute, not one a second;
#   · adaptive cadence: 1s while the screen is moving, 2s when it has been quiet a while, 5s when
#     it has been quiet for minutes;
#   · `nice`d by the caller, single-instance via a box-local lock, and it exits the moment its tmux
#     session is gone.
# Measured: 0.11% of one core over a 61s window spanning a live turn (nice 19, in-box, 100Hz ticks).
set -uo pipefail

sess="${1:-skein-agent}"
TAIL_LINES=24          # the bottom of the VISIBLE pane, never scrollback (see `start` below), so
                       # neither the agent's own prose nor a previous turn's status line can be
                       # mistaken for the current one (see classify_pane in lib.rs)
HEARTBEAT=10           # seconds: proves the observation is fresh even when nothing changes, which is
                       # what lets the host expire a stale one instead of trusting it forever
MIN_WRITE=2            # seconds: floor between writes for cosmetic churn (streaming output). A real
                       # state transition ignores this floor and is reported on the next tick.
MAX_QUIET=5            # cadence in seconds once the screen has been still for QUIET_LONG
QUIET_LONG=300

command -v tmux >/dev/null 2>&1 || exit 0

# Which tmux server to ask. A box in the shared sandbox has its own, on its own socket — bare `tmux`
# would reach the sandbox's default socket, find no `skein-agent` there, read that as "the agent's
# window is gone" and exit on the first tick. Every fleet box therefore had an observer that died
# immediately and a board that said "screen lost" for all of them, permanently.
#
# The attach has exported SKEIN_TMUX_SOCK since the shared model existed; this is the half that was
# missing. Empty (a box that is its own sandbox) keeps the bare call, so nothing changes there.
tm() {
  if [ -n "${SKEIN_TMUX_SOCK:-}" ]; then
    tmux -S "$SKEIN_TMUX_SOCK" "$@"
  else
    tmux "$@"
  fi
}

# Merged layout: the shared store is that link's target parent, not the repo dir (box-status.sh).
cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

# The BOX, not the VM — and here the question is settled by SKEIN_TMUX_SOCK rather than by the fleet
# launcher the hooks read: `pane_observer_start` (src/runtime.rs) exports the box's own tmux socket
# under the shared model and exports nothing at all when the sandbox IS the box. So SKEIN_BOX is the
# box wherever it is set; no socket means a legacy box alone in its VM, where the sandbox's name IS
# the box's; a socket with no SKEIN_BOX means a shared sandbox that cannot say which box this is,
# and an observation written there would land on whichever box owns that name. The argument in full,
# and the measured residue that settled it, is in box-status.sh — beside this one in skein/bin/.
if [ -n "${SKEIN_BOX:-}" ]; then
  vmid="$SKEIN_BOX"
elif [ -z "${SKEIN_TMUX_SOCK:-}" ]; then
  vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
else
  exit 0
fi
vmid="${vmid//\//-}"
dir="$store/status"
mkdir -p "$dir" 2>/dev/null || exit 0
out="$dir/$vmid.pane.json"

# Single instance per box. The lock lives in the box's own /tmp, never the shared store — locks and
# runtime state must not go there (see .claude/skein/SHARED-HOME.md).
exec 9>"/tmp/skein-pane.$vmid.lock" 2>/dev/null || exit 0
flock -n 9 2>/dev/null || exit 0

# JSON string escape without jq: backslash, quote, then drop control characters. Tabs are turned into
# spaces upstream (capture below) so no \t can reach here.
esc() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '%s' "$s"
}

write_obs() { # $1 = activity epoch, $2 = age, $3 = moving(0|1), $4 = dead(0|1), $5 = title, $6 = cmd, $7 = tail, $8 = title_age
  local tmp lines line first=1
  tmp="$(mktemp "$dir/.pane.XXXXXX" 2>/dev/null)" || return 0
  {
    # `box` says WHOSE screen this is, inside the observation, so the filename is a claim the reader
    # can check instead of a claim it has to believe (`pane_is_ours`, src/signals.rs). The guard
    # above means the probe never writes the wrong name; this means a file that somehow carries one
    # anyway — an old probe's leftovers, a copied store, a restored backup — is refused rather than
    # classified as the box it was filed under.
    #
    # Deliberately NOT a contract bump: `contract` says what the EXISTING fields mean, and this adds
    # a field without changing any of them. A reader that has never heard of `box` ignores it (serde
    # drops unknown keys), so bumping would only make every current skein call every new observation
    # "newer" and go dark on the whole fleet at once — the failure PANE_CONTRACT exists to prevent.
    printf '{"contract":1,"box":"%s","ts":%s,"activity":%s,"age":%s,"moving":%s,"dead":%s,"session":"%s","title":"%s","title_age":%s,"cmd":"%s","tail":[' \
      "$(esc "$vmid")" "$EPOCHSECONDS" "$1" "$2" "$3" "$4" "$(esc "$sess")" "$(esc "$5")" "${8:--1}" "$(esc "$6")"
    while IFS= read -r line; do
      [ "$first" = 1 ] || printf ','
      first=0
      printf '"%s"' "$(esc "$line")"
    done <<<"$7"
    printf ']}\n'
  } >"$tmp" 2>/dev/null
  mv "$tmp" "$out" 2>/dev/null || rm -f "$tmp" 2>/dev/null
}

# The part of the screen a reader could act on: is a dialog up, is the busy marker present, and what
# is the last non-empty line. Pure bash over ~24 lines — no forks. A change here is a state
# transition and is reported at once; anything else is cosmetic churn.
SIG=""
shape() {                # sets $SIG; deliberately not echo + $(…), which forks a subshell per tick
  local dialog=0 interrupt=0 last="" line
  while IFS= read -r line; do
    [ -n "${line//[[:space:]]/}" ] && last=$line
    # A numbered option, in every marker either runtime uses (Codex's onboarding switches to a plain
    # `> 1.`), or the footer Codex puts under every dialog.
    case $line in
      *'❯ 1.'* | *'› 1.'* | *'❯ 1)'* | *'> 1.'*) dialog=1 ;;
      *'Press enter to confirm'* | *'Press enter to continue'*) dialog=1 ;;
    esac
    case $line in *'esc to interrupt'* | *'Esc to interrupt'*) interrupt=1 ;; esac
  done <<<"$1"
  SIG="$dialog$interrupt|$last"
}

# `title_at` is when this observer SAW the title change — deliberately not set from the first sample,
# because a title inherited from a turn that ended ten minutes ago is not evidence of current work
# (observed: Claude Code kept "Fetch and quote robots.txt file" in the title through an unrelated
# later turn). Until a change is actually witnessed the age is reported as -1, "unknown", and the
# host will not use the title's text.
prev_activity=""; prev_moving=-1; prev_tail=""; prev_sig=""; last_write=0
title_at=0; first=1; prev_text=""; prev_text_w=""; title_text=""; lead=""
while :; do
  # One tmux round-trip carries everything cheap: when the pane last produced output (the spinner
  # redraw — that IS the "working" signal), whether the pane is dead, and the title, which carries a
  # spinner glyph in both runtimes, the running tool in Claude Code, and `[ ! ] Action Required` in
  # Codex — the only place either provider says outright that a human has to act.
  if ! meta="$(tm display-message -p -t "$sess" '#{window_activity}|#{pane_dead}|#{pane_current_command}|#{pane_height}|#{pane_title}' 2>/dev/null)"; then
    meta=""
  fi
  if [ -z "$meta" ]; then
    # No session: the agent's window is gone (exited, killed, or never started). That is itself a
    # level observation the host needs — it is the "shows working for 45 minutes after a crash" case.
    write_obs 0 0 0 1 "" "" ""
    exit 0
  fi
  activity=${meta%%|*}; rest=${meta#*|}
  dead=${rest%%|*};     rest=${rest#*|}
  cmd=${rest%%|*};      rest=${rest#*|}
  height=${rest%%|*}
  title=${rest#*|}
  case $activity in '' | *[!0-9]*) activity=0 ;; esac
  case $height in '' | *[!0-9]*) height=0 ;; esac

  # Where the visible pane's bottom TAIL_LINES start. capture-pane's coordinates are relative to the
  # top of the VISIBLE pane, so a negative `-S` reaches into scrollback: `-S -24` means "24 lines of
  # history plus the whole screen", which on a tall pane handed the classifier ~70 lines including
  # the previous turn's status line. That is how an idle box could still read as working.
  start=$((height - TAIL_LINES))
  [ "$start" -lt 0 ] && start=0

  age=$((EPOCHSECONDS - activity))
  [ "$age" -lt 0 ] && age=0
  moving=0
  [ -n "$prev_activity" ] && [ "$activity" != "$prev_activity" ] && moving=1

  # Freshness — and the decision to write at all — has to be measured on the title's TEXT, never the
  # raw title: the animated part is part of the string and changes every frame, so the raw title
  # always looks like it just changed. That made a ten-minute-old tool description look like current
  # work (Claude's `⠂ `), and it would make an unanswered Codex dialog (`[ ! ] ` → `[ . ] `) rewrite
  # the observation file once a second for as long as it went unanswered.
  # Strip the whole leading run of non-alphanumerics, so `[ ! ] Action Required | skein` and
  # `⠂ Claude Code` both reduce to something stable.
  lead=${title%%[[:alnum:]]*}
  title_text=${title#"$lead"}
  if [ "$first" = 1 ]; then first=0
  elif [ "$title_text" != "$prev_text" ]; then title_at=$EPOCHSECONDS
  fi
  prev_text=$title_text
  title_age=-1
  [ "$title_at" != 0 ] && title_age=$((EPOCHSECONDS - title_at))

  # Look every tick while the screen is live: a turn starting, or a dialog appearing or being
  # answered, changes the tail without changing anything cheaper — and those are exactly the
  # transitions the host must see within a second or two. Waiting for the heartbeat made "working"
  # arrive up to ten seconds late (caught in a live test, not in review).
  due=0
  [ "$age" -lt 15 ] && due=1
  [ "$moving" != "$prev_moving" ] && due=1
  [ "$title_text" != "$prev_text_w" ] && due=1
  [ $((EPOCHSECONDS - last_write)) -ge "$HEARTBEAT" ] && due=1
  if [ "$due" = 1 ]; then
    tail_text="$(tm capture-pane -p -t "$sess" -S "$start" 2>/dev/null | tr '\t' ' ')"
    shape "$tail_text"; sig=$SIG
    since=$((EPOCHSECONDS - last_write))
    # The raw title still goes into the file — the animated glyph IS the host's spinner evidence —
    # but only a change in its text counts as a reason to write.
    if [ "$sig" != "$prev_sig" ] || [ "$title_text" != "$prev_text_w" ] || [ "$since" -ge "$HEARTBEAT" ] \
       || { [ "$tail_text" != "$prev_tail" ] && [ "$since" -ge "$MIN_WRITE" ]; }; then
      write_obs "$activity" "$age" "$moving" "$dead" "$title" "$cmd" "$tail_text" "$title_age"
      last_write=$EPOCHSECONDS
    fi
    prev_tail=$tail_text; prev_sig=$sig
  fi
  prev_activity=$activity; prev_moving=$moving; prev_text_w=$title_text

  # Cadence: responsive while things move, lazy when they don't. A box nobody is watching and nothing
  # is happening in costs one tmux call every 5 seconds.
  if [ "$age" -lt 15 ]; then sleep 1
  elif [ "$age" -lt "$QUIET_LONG" ]; then sleep 2
  else sleep "$MAX_QUIET"; fi
done
