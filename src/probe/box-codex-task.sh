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
store="$root/.claude"
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"

task="$(printf '%s' "$payload" | jq -r '.prompt // ""' 2>/dev/null | sed -n '/[^[:space:]]/{p;q;}')"
task="$(printf '%s' "$task" | tr '\n\r\t' '   ' | head -c 240)"
[ -n "$task" ] || exit 0

dir="$store/tasks"
mkdir -p "$dir" 2>/dev/null || exit 0
tmp="$(mktemp "$dir/.codex-task.XXXXXX" 2>/dev/null)" || exit 0
jq -n --arg t "$task" --arg ts "$ts" --arg a codex '{task:$t,ts:$ts,agent:$a}' >"$tmp" 2>/dev/null
mv "$tmp" "$dir/$vmid.json" 2>/dev/null || rm -f "$tmp" 2>/dev/null
exit 0
