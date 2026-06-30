#!/usr/bin/env bash
# skein box-diff.sh — write this box's branch-vs-base diff to the shared store so skein's
# fleet view and diff panel can show what the agent has changed on its branch.
# Wired from the Stop hook (turn-end). Fail-soft; prints nothing to stdout (Stop hook output
# is injected into the prompt).
set -uo pipefail

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"

dir="$store/diffs"
mkdir -p "$dir" 2>/dev/null || exit 0

# Find the merge-base against the nearest base branch.
base=""
for ref in origin/main origin/master main master; do
  if git -C "$root" rev-parse --verify -q "$ref" >/dev/null 2>&1; then
    base="$ref"
    break
  fi
done

range=""
if [ -n "$base" ]; then
  mb="$(git -C "$root" merge-base HEAD "$base" 2>/dev/null || true)"
  [ -n "$mb" ] && range="$mb"
fi
[ -n "$range" ] || range="HEAD"

# Patch: diff from merge-base to working tree (matches host-side logic; shows all branch work
# including uncommitted). Capped at ~2 MB so a huge patch can't wedge the browser.
{
  git -C "$root" diff "$range" 2>/dev/null | head -c 2000000
} >"$dir/$vmid.patch" 2>/dev/null || true

# Shortstat JSON: {"files":N,"ins":N,"del":N} — used for the fleet badge.
stat_out="$(git -C "$root" diff --shortstat "$range" 2>/dev/null || true)"
files=0; ins=0; del=0
if [ -n "$stat_out" ]; then
  files="$(printf '%s' "$stat_out" | grep -o '[0-9]\+ file' | grep -o '[0-9]\+' || echo 0)"
  ins="$(printf '%s' "$stat_out" | grep -o '[0-9]\+ insertion' | grep -o '[0-9]\+' || echo 0)"
  del="$(printf '%s' "$stat_out" | grep -o '[0-9]\+ deletion' | grep -o '[0-9]\+' || echo 0)"
fi
printf '{"files":%s,"ins":%s,"del":%s}\n' \
  "${files:-0}" "${ins:-0}" "${del:-0}" >"$dir/$vmid.json" 2>/dev/null || true

# Recent commits: last 20 subjects newest-first, written for the session digest.
if [ "$range" != "HEAD" ]; then
  git -C "$root" log --format="%s" -n 20 "$range..HEAD" >"$dir/$vmid.commits" 2>/dev/null || true
else
  : >"$dir/$vmid.commits" 2>/dev/null || true
fi

exit 0
