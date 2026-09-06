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
