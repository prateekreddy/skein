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
# SKEIN_BOX is exported by box-session.sh, the only thing that knows which box a process is
# in. A legacy box has no SKEIN_BOX and is alone in its VM, where the two are the same name.
vmid="${SKEIN_BOX:-${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}}"
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
