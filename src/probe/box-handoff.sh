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
# Which store this box reports into, and under which name: both decided in box-self.sh, installed
# beside this script, which says why neither is guessed (SKEIN-1174). Either one unknown is a quiet
# exit — a hook's stdout is read by the agent, and a signal nobody files is one the board already
# calls missing, while one filed in the checkout or under the sandbox's name is one it believes.
here="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
. "$here/box-self.sh" 2>/dev/null || exit 0
store="$(skein_box_store "$root")" || exit 0
vmid="$(skein_box_name)" || exit 0
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
