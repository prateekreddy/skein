#!/usr/bin/env bash
# install-codex-hooks.sh — idempotently refresh Skein's generated Codex hooks in a box-private HOME.
#
# Reads the hooks from BESIDE itself, and is run from skein's read-only plugin (`probe/`, SKEIN-1149):
# both used to be the store's (`skein/bin/` and `skein/codex-hooks.json`), which every box of the
# repo can write, so one box could choose what a sibling's Codex hooks run. The store still gets a
# copy of this script, which finds nothing beside it and exits; nothing skein runs uses it.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
source_hooks="$here/codex-hooks.json"
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
