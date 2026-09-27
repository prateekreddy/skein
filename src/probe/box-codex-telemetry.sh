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
# Which store this box reports into, and under which name: both decided in box-self.sh, installed
# beside this script, which says why neither is guessed (SKEIN-1174). Either one unknown is a quiet
# exit — a hook's stdout is read by the agent, and a signal nobody files is one the board already
# calls missing, while one filed in the checkout or under the sandbox's name is one it believes.
here="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
. "$here/box-self.sh" 2>/dev/null || exit 0
store="$(skein_box_store "$root")" || exit 0
vmid="$(skein_box_name)" || exit 0
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
