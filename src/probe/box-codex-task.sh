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
