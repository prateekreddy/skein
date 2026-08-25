#!/usr/bin/env bash
# skein box-handoff.sh — provider-neutral Claude <-> Codex takeover context.
#
# Wired into SessionStart and UserPromptSubmit for both runtimes. The host writes a one-shot pending
# brief before switching agents; this hook adds it as model-visible context, along with live in-box
# git facts that the host cannot see for a clone-mode sandbox. The durable handoff remains in
# <store>/handoffs/<vmid>.md; only the per-target pending copy is consumed.
set -uo pipefail

target="${1:-agent}"
cat >/dev/null 2>&1 || true   # drain the hook payload; the brief itself is file-backed
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
pending="$store/handoffs/$vmid.$target.pending.md"

[ -r "$pending" ] || exit 0
echo
echo "----- BEGIN SKEIN CROSS-AGENT HANDOFF -----"
cat "$pending" 2>/dev/null || true
echo
echo "Live sandbox state (authoritative):"
echo "branch: $(git -C "$root" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
echo "HEAD: $(git -C "$root" log -1 --format='%h %s' 2>/dev/null || echo '?')"
echo "working tree:"
git -C "$root" status --short 2>/dev/null | head -n 120 || true
echo "----- END SKEIN CROSS-AGENT HANDOFF -----"

consumed="$store/handoffs/$vmid.$target.consumed.md"
mv "$pending" "$consumed" 2>/dev/null || rm -f "$pending" 2>/dev/null || true
exit 0
