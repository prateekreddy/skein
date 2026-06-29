#!/usr/bin/env bash
# skein box-status.sh — record THIS box's agent turn-state into the shared store so skein's fleet
# view can show working / waiting / needs-input. SHIPPED AND INSTALLED BY SKEIN (not the repo) —
# skein writes this into <store>/skein/bin/ and wires it from <store>/settings.json:
#   UserPromptSubmit -> working      (you gave it work; it's running)
#   Notification     -> needs-input  (blocked on a decision/permission — most urgent)
#   Stop             -> waiting       (turn ended — your move)
# Writes only <store>/status/<vmid>.json = {"status":..,"ts":..}. Idempotent, fail-soft, and prints
# NOTHING to stdout (a UserPromptSubmit hook's stdout would be injected into the prompt).
set -uo pipefail

status="${1:-}"
[ -n "$status" ] || exit 0
cat >/dev/null 2>&1 || true   # drain hook stdin; ignore it

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"

dir="$store/status"
mkdir -p "$dir" 2>/dev/null || exit 0
tmp="$(mktemp "$dir/.st.XXXXXX" 2>/dev/null)" || exit 0
printf '{"status":"%s","ts":"%s"}\n' "$status" "$ts" >"$tmp" 2>/dev/null \
  && mv "$tmp" "$dir/$vmid.json" 2>/dev/null \
  || rm -f "$tmp" 2>/dev/null
exit 0
