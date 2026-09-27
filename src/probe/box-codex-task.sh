#!/usr/bin/env bash
# skein box-codex-task.sh — Codex adapter for the fleet's "current task" signal.
#
# Codex has no TodoWrite hook equivalent. Its UserPromptSubmit hook does expose the prompt that
# starts the turn, so keep the first line as the box's active objective. This gives the cockpit a
# useful peripheral signal throughout the turn; a journal `next` line remains the fallback.
# Writes <store>/tasks/<vmid>.json, fail-soft, and prints nothing to stdout.
set -uo pipefail

payload="$(cat 2>/dev/null || true)"
command -v jq >/dev/null 2>&1 || exit 0
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

task="$(printf '%s' "$payload" | jq -r '.prompt // ""' 2>/dev/null | sed -n '/[^[:space:]]/{p;q;}')"
task="$(printf '%s' "$task" | tr '\n\r\t' '   ' | head -c 240)"
[ -n "$task" ] || exit 0

dir="$store/tasks"
mkdir -p "$dir" 2>/dev/null || exit 0
tmp="$(mktemp "$dir/.codex-task.XXXXXX" 2>/dev/null)" || exit 0
# `box` for the same reason box-task.sh carries it: the two write the same file for two runtimes,
# and a check the reader can only apply to one of them is a check it cannot rely on.
jq -n --arg t "$task" --arg ts "$ts" --arg a codex --arg b "$vmid" \
  '{task:$t,ts:$ts,agent:$a,box:$b}' >"$tmp" 2>/dev/null
mv "$tmp" "$dir/$vmid.json" 2>/dev/null || rm -f "$tmp" 2>/dev/null
exit 0
