#!/usr/bin/env bash
# skein box-pane.sh — the LEVEL observation of a box's agent screen.
# SHIPPED AND INSTALLED BY SKEIN (not the repo) — written into <store>/skein/bin/ and started detached
# by the attach command (see agent_attach_argv in lib.rs). Usage: box-pane.sh <tmux-session>
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
#   · a `capture-pane` only when something changed or the heartbeat is due — not every tick;
#   · a WRITE only on a real change or every HEARTBEAT seconds, so the host-mounted store sees ~6
#     writes a minute rather than one a second;
#   · adaptive cadence: 1s while the screen is moving, 2s when it has been quiet a while, 5s when
#     it has been quiet for minutes;
#   · `nice`d by the caller, single-instance via a box-local lock, and it exits the moment its tmux
#     session is gone.
# Measured: ~0.35% of one core while an agent works, ~0.1% idle (example-box-6, 60s samples).
set -uo pipefail

sess="${1:-skein-agent}"
TAIL_LINES=24          # the composer/dialog region — never scrollback, so the agent's own prose
                       # cannot be mistaken for a dialog (see classify_pane in lib.rs)
HEARTBEAT=10           # seconds: proves the observation is fresh even when nothing changes, which is
                       # what lets the host expire a stale one instead of trusting it forever
MAX_QUIET=5            # cadence in seconds once the screen has been still for QUIET_LONG
QUIET_LONG=300

command -v tmux >/dev/null 2>&1 || exit 0

# Store resolution matches the other probes: the repo root's .claude, hopping the kit's symlink when
# the repo ships its own .claude (the shared store is the link target's parent, not the repo dir).
cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
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

write_obs() { # $1 = activity epoch, $2 = age, $3 = moving(0|1), $4 = dead(0|1), $5 = title, $6 = cmd, $7 = tail
  local tmp lines line first=1
  tmp="$(mktemp "$dir/.pane.XXXXXX" 2>/dev/null)" || return 0
  {
    printf '{"contract":1,"ts":%s,"activity":%s,"age":%s,"moving":%s,"dead":%s,"session":"%s","title":"%s","cmd":"%s","tail":[' \
      "$EPOCHSECONDS" "$1" "$2" "$3" "$4" "$(esc "$sess")" "$(esc "$5")" "$(esc "$6")"
    while IFS= read -r line; do
      [ "$first" = 1 ] || printf ','
      first=0
      printf '"%s"' "$(esc "$line")"
    done <<<"$7"
    printf ']}\n'
  } >"$tmp" 2>/dev/null
  mv "$tmp" "$out" 2>/dev/null || rm -f "$tmp" 2>/dev/null
}

prev_activity=""; prev_moving=-1; prev_title=""; prev_tail=""; last_write=0
while :; do
  # One tmux round-trip carries everything cheap: when the pane last produced output (the spinner
  # redraw — that IS the "working" signal), whether the pane is dead, and the title, which Claude
  # Code sets to the tool it is running.
  if ! meta="$(tmux display-message -p -t "$sess" '#{window_activity}|#{pane_dead}|#{pane_current_command}|#{pane_title}' 2>/dev/null)"; then
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
  cmd=${rest%%|*}
  title=${rest#*|}
  case $activity in '' | *[!0-9]*) activity=0 ;; esac

  age=$((EPOCHSECONDS - activity))
  [ "$age" -lt 0 ] && age=0
  moving=0
  [ -n "$prev_activity" ] && [ "$activity" != "$prev_activity" ] && moving=1

  # Capture (and write) only when the picture actually changed, or when the heartbeat is due. A
  # transition is exactly when the tail matters: output just started, or just settled.
  due=0
  [ "$moving" != "$prev_moving" ] && due=1
  [ "$title" != "$prev_title" ] && due=1
  [ $((EPOCHSECONDS - last_write)) -ge "$HEARTBEAT" ] && due=1
  if [ "$due" = 1 ]; then
    tail_text="$(tmux capture-pane -p -t "$sess" -S -"$TAIL_LINES" 2>/dev/null | tr '\t' ' ')"
    # Skip the write when nothing a reader could act on differs — but never skip past the heartbeat,
    # which is what proves the observation is still live.
    if [ "$tail_text" != "$prev_tail" ] || [ "$moving" != "$prev_moving" ] \
       || [ "$title" != "$prev_title" ] || [ $((EPOCHSECONDS - last_write)) -ge "$HEARTBEAT" ]; then
      write_obs "$activity" "$age" "$moving" "$dead" "$title" "$cmd" "$tail_text"
      last_write=$EPOCHSECONDS
    fi
    prev_tail=$tail_text
  fi
  prev_activity=$activity; prev_moving=$moving; prev_title=$title

  # Cadence: responsive while things move, lazy when they don't. A box nobody is watching and nothing
  # is happening in costs one tmux call every 5 seconds.
  if [ "$age" -lt 15 ]; then sleep 1
  elif [ "$age" -lt "$QUIET_LONG" ]; then sleep 2
  else sleep "$MAX_QUIET"; fi
done
