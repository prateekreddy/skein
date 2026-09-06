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

# The BOX, not the VM. SKEIN_BOX names the box wherever it was set; with it unset, skein's fleet
# launcher decides — installed at `fleet::box_session_path()` only in a sandbox that HOLDS boxes,
# so its absence means a legacy box alone in its VM where the sandbox's name IS the box's, and its
# presence means a shared sandbox, where SANDBOX_VM_ID is one string for every box in it and a
# signal keyed on it lands on whichever box owns that name. The argument in full, and the measured
# residue that settled it, is in box-status.sh — installed beside this one in <store>/skein/bin/.
#
# Refusing is the conservative half: a box with no signal reads as one that has not reported, which
# is TRUE and which the board already says out loud.
if [ -n "${SKEIN_BOX:-}" ]; then
  vmid="$SKEIN_BOX"
elif [ ! -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
  vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
else
  exit 0
fi
vmid="${vmid//\//-}"
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
