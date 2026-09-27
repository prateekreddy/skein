#!/usr/bin/env bash
# skein box-task.sh — record the in-progress TodoWrite item as "what this box is doing right now",
# the peripheral signal skein shows on every row. SHIPPED AND INSTALLED BY SKEIN. Wired from
# <store>/settings.json as PostToolUse(TodoWrite). Writes <store>/tasks/<vmid>.json = {"task":..}.
# Idempotent, fail-soft, prints nothing.
set -uo pipefail

payload="$(cat 2>/dev/null || true)"   # the hook's tool-call JSON on stdin
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

# The in-progress todo's activeForm (its "doing" phrasing), else its content. Empty if none.
task="$(printf '%s' "$payload" \
  | jq -r '(.tool_input.todos // [])[] | select(.status=="in_progress") | (.activeForm // .content)' \
    2>/dev/null | head -n1)"

dir="$store/tasks"
mkdir -p "$dir" 2>/dev/null || exit 0
tmp="$(mktemp "$dir/.tk.XXXXXX" 2>/dev/null)" || exit 0
# `box` names whose task this is, inside the file — the same claim-the-reader-can-check as the
# status and narrative signals (`signal_is_ours`, src/signals.rs). A misfiled task line is the
# quiet kind of wrong: the row shows a plausible sentence about work this box is not doing.
jq -n --arg t "$task" --arg b "$vmid" '{task:$t,box:$b}' >"$tmp" 2>/dev/null \
  && mv "$tmp" "$dir/$vmid.json" 2>/dev/null \
  || rm -f "$tmp" 2>/dev/null
exit 0
