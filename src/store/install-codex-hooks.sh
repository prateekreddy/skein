#!/usr/bin/env bash
# install-codex-hooks.sh — idempotently refresh Skein's generated Codex hooks in a box-private HOME.
set -euo pipefail

store="${1:-}"
source_hooks="$store/skein/codex-hooks.json"
target_dir="$HOME/.codex"
target="$target_dir/hooks.json"
[ -r "$source_hooks" ] || { echo "[skein-codex-hooks] source unavailable: $source_hooks" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "[skein-codex-hooks] jq is required" >&2; exit 1; }
mkdir -p "$target_dir"

if [ ! -s "$target" ]; then
  cp "$source_hooks" "$target.tmp"
  mv "$target.tmp" "$target"
  exit 0
fi

# Preserve every user hook. Remove only commands a Skein probe generated — the store-era ones, which
# ran the store's copy of box-codex-hook.sh, and the current ones, which run the read-only plugin's
# (SKEIN-1144) — then append the current generated set once; this makes container startup and agent
# restart share one installer.
merged="$(jq -s '
  .[0] as $current | .[1] as $skein
  | (($current.hooks // {}) | with_entries(
      .value = [.value[] | select(
        ([.hooks[]? | (.command // "") | (contains("/.claude/skein/bin/")
          or contains("/.skein/plugin-turn-state/probe/box-codex-hook.sh"))] | any) | not
      )]
    )) as $clean
  | $current
  | .hooks = (reduce (($skein.hooks // {}) | to_entries[]) as $entry ($clean;
      .[$entry.key] = ((.[$entry.key] // []) + $entry.value)))
' "$target" "$source_hooks")"
tmp="$(mktemp "$target_dir/.hooks.XXXXXX")"
printf '%s\n' "$merged" >"$tmp"
mv "$tmp" "$target"
