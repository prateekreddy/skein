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
# Merged layout: when the repo ships its own .claude/, the kit links only skein/ into it — the
# shared store is that link's target parent, NOT the repo dir. Writing here without this hop
# would land signals in the box-local clone where the host can never see them.
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"

# The in-progress todo's activeForm (its "doing" phrasing), else its content. Empty if none.
task="$(printf '%s' "$payload" \
  | jq -r '(.tool_input.todos // [])[] | select(.status=="in_progress") | (.activeForm // .content)' \
    2>/dev/null | head -n1)"

dir="$store/tasks"
mkdir -p "$dir" 2>/dev/null || exit 0
tmp="$(mktemp "$dir/.tk.XXXXXX" 2>/dev/null)" || exit 0
jq -n --arg t "$task" '{task:$t}' >"$tmp" 2>/dev/null \
  && mv "$tmp" "$dir/$vmid.json" 2>/dev/null \
  || rm -f "$tmp" 2>/dev/null
exit 0
