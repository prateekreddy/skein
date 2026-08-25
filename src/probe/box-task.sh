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
