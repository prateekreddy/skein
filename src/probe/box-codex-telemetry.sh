#!/usr/bin/env bash
# skein box-codex-telemetry.sh — Codex adapter for durable per-turn telemetry.
#
# Modes:
#   tool  (PostToolUse) append the completed tool name for this turn
#   stop  (Stop)        append one telemetry JSONL entry and clear the tool accumulator
#
# The official Codex hook payload exposes transcript_path but documents the rollout format as
# unstable. We therefore parse token_count records defensively and treat them as an enrichment:
# duration and hook-counted tools remain available even if a future rollout shape changes.
# Prints nothing, never blocks a turn.
set -uo pipefail

mode="${1:-}"
[ -n "$mode" ] || exit 0
payload="$(cat 2>/dev/null || true)"
cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

# The BOX, not the VM. In a shared sandbox every box has the same SANDBOX_VM_ID, so keying a
# signal on it makes every box write one file and the board see none of them report.
#
# SKEIN_BOX names the box wherever it was set: the launcher exports it before it starts the box's
# tmux server (src/box-session.sh), so the agent and every hook it forks inherit it, and every
# placement hop into a shared box exports it too (`wrap` in src/place.rs).
#
# The old chain ran on from there to SANDBOX_VM_ID and then `hostname` unconditionally, and in a
# shared sandbox BOTH of those name the sandbox — one string for every box in it. Whether that
# fallback is sound depends on which world this box is in, and the fact that answers it here is the
# fleet launcher: skein installs it at `fleet::box_session_path()` in the one sandbox that holds
# boxes, and never in a per-VM sandbox, which `sbx create` builds with no fleet machinery at all.
# box-pane.sh answers the same question from SKEIN_TMUX_SOCK and spells the argument out in full; a
# hook is not started by the attach and never sees that variable, but the launcher is a fact about
# the SANDBOX and so is visible to anything running inside it, whatever its lineage. So
#   · SKEIN_BOX set          — that is the box, whatever else is in the environment;
#   · unset, no launcher     — a legacy box, alone in its VM, where the two names are the same
#                              string. Unchanged: this is the path that has always worked;
#   · unset, with a launcher — a shared sandbox and no identity. Writing under SANDBOX_VM_ID here
#                              files this box's signal under a name that is not its own, and
#                              overwrites whichever box does own that name.
#
# Refusing is the conservative half. A box with no signal reads as one that has not reported, which
# is TRUE and which the board already says out loud; a signal under the wrong name is well-formed,
# fresh, and renders as another box's state with nothing to mark it. Measured residue of the
# writing version: five repo stores hold a `status/skein-fleet.json`, one holds a `skein-fleet`
# entry in its registry — `skein-fleet` is `config::default_fleet_sandbox`, the SANDBOX's name, and
# no box has ever been called that.
if [ -n "${SKEIN_BOX:-}" ]; then
  vmid="$SKEIN_BOX"
elif [ ! -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
  vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
else
  exit 0
fi
vmid="${vmid//\//-}"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"
dir="$store/telemetry"
state="$dir/.codex-turn"
mkdir -p "$state" 2>/dev/null || exit 0
tools_file="$state/$vmid.tools"

field() { command -v jq >/dev/null 2>&1 && printf '%s' "$payload" | jq -r --arg k "$1" '.[$k] // ""' 2>/dev/null || true; }

if [ "$mode" = "tool" ]; then
  tool="$(field tool_name)"
  [ -n "$tool" ] || exit 0
  (
    flock -w 5 9 || exit 0
    printf '%s\n' "$tool" >>"$tools_file"
  ) 9>"$tools_file.lock" 2>/dev/null || true
  exit 0
fi
[ "$mode" = "stop" ] || exit 0

command -v jq >/dev/null 2>&1 || exit 0
transcript="$(field transcript_path)"
offset_file="$state/$vmid.offset.json"
prev=0
if [ -r "$offset_file" ] && [ -n "$transcript" ]; then
  old_path="$(jq -r '.path // ""' "$offset_file" 2>/dev/null)"
  [ "$old_path" = "$transcript" ] && prev="$(jq -r '.lines // 0' "$offset_file" 2>/dev/null)"
fi
case "$prev" in '' | *[!0-9]*) prev=0 ;; esac

total_lines=0
usage='{"entries":0,"input":0,"cache_read":0,"output":0,"reasoning":0}'
if [ -n "$transcript" ] && [ -r "$transcript" ]; then
  total_lines="$(wc -l <"$transcript" 2>/dev/null || echo 0)"
  case "$total_lines" in '' | *[!0-9]*) total_lines=0 ;; esac
  # A replaced/truncated rollout starts a new offset domain.
  [ "$total_lines" -ge "$prev" ] || prev=0
  if [ "$total_lines" -gt "$prev" ]; then
    usage="$(tail -n "+$((prev + 1))" "$transcript" 2>/dev/null | jq -sc '
      [ .[] | select(.type? == "event_msg" and .payload.type? == "token_count")
             | (.payload.info.last_token_usage // {}) ] as $u
      | { entries: ($u|length),
          input_total: ($u|map(.input_tokens // 0)|add // 0),
          cache_read: ($u|map(.cached_input_tokens // 0)|add // 0),
          output: ($u|map(.output_tokens // 0)|add // 0),
          reasoning: ($u|map(.reasoning_output_tokens // 0)|add // 0) }
      | .input = ([.input_total - .cache_read, 0] | max)
      | del(.input_total)' 2>/dev/null || echo '{"entries":0,"input":0,"cache_read":0,"output":0,"reasoning":0}')"
  fi
  tmp_off="$(mktemp "$state/.codex-off.XXXXXX" 2>/dev/null)" || tmp_off=""
  if [ -n "$tmp_off" ]; then
    jq -n --arg p "$transcript" --argjson n "$total_lines" '{path:$p,lines:$n}' >"$tmp_off" 2>/dev/null \
      && mv "$tmp_off" "$offset_file" 2>/dev/null || rm -f "$tmp_off" 2>/dev/null
  fi
fi

tools='{}'
if [ -r "$tools_file" ]; then
  tools="$(sort "$tools_file" 2>/dev/null | jq -Rsc '
    split("\n") | map(select(length>0)) | group_by(.)
    | map({key:.[0],value:length}) | from_entries' 2>/dev/null || echo '{}')"
fi

duration=""
start_marker="$dir/.turn-start/$vmid"
if [ -r "$start_marker" ]; then
  start_epoch="$(cat "$start_marker" 2>/dev/null || echo 0)"
  now_epoch="$(date -u +%s 2>/dev/null || echo 0)"
  case "$start_epoch" in '' | *[!0-9]*) start_epoch=0 ;; esac
  if [ "$start_epoch" -gt 0 ] && [ "$now_epoch" -ge "$start_epoch" ]; then duration=$((now_epoch-start_epoch)); fi
fi

jq -c -n --arg t "$ts" --argjson u "$usage" --argjson tools "$tools" --arg d "$duration" '
  ($u.input + $u.cache_read + $u.output) as $total
  | {ts:$t,agent:"codex",input:$u.input,output:$u.output,cache_read:$u.cache_read,
     cache_creation:0,reasoning:$u.reasoning,total:$total,
     duration_secs:(if $d=="" then null else ($d|tonumber) end),tools:$tools}' \
  >>"$dir/$vmid.jsonl" 2>/dev/null || true
rm -f "$tools_file" "$tools_file.lock" 2>/dev/null || true
exit 0
