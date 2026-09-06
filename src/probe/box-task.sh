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
store="$root/.claude"
# Merged layout: the shared store is that link's target parent, not the repo dir (box-status.sh).
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
