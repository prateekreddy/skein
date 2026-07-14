#!/usr/bin/env bash
# agent-guide.sh — install Skein's concise durable guidance without clobbering user instructions.
set -euo pipefail

store="${1:-}"
relative="${2:-}"
override="${3:-}"
[ -d "$store" ] && [ -r "$store/skein/SHARED-HOME.md" ] \
  || { echo "[skein-agent-guide] shared-home guide unavailable" >&2; exit 1; }
case "$relative" in ""|/*|..|../*|*/..|*/../*) echo "[skein-agent-guide] unsafe instruction path" >&2; exit 1 ;; esac
case "$override" in /*|..|../*|*/..|*/../*) echo "[skein-agent-guide] unsafe override path" >&2; exit 1 ;; esac

# Codex-style override files replace the normal global file. The runtime adapter supplies that
# optional path; other runtimes simply leave it empty.
target="$HOME/$relative"
if [ -n "$override" ] && [ -s "$HOME/$override" ]; then target="$HOME/$override"; fi
mkdir -p "$(dirname "$target")"

begin='<!-- skein:shared-home:start -->'
end='<!-- skein:shared-home:end -->'
tmp="$(mktemp "$(dirname "$target")/.agent-guide.XXXXXX")"
if [ -f "$target" ]; then
  awk -v begin="$begin" -v end="$end" '
    $0 == begin { inside=1; next }
    $0 == end { inside=0; next }
    !inside { print }
  ' "$target" >"$tmp"
fi
cat >>"$tmp" <<'GUIDE'

<!-- skein:shared-home:start -->
## Skein shared working files

- `$HOME/shared` is durable, project-scoped, read-write, and live across this repo's Claude/Codex boxes. Use it for reference documents, sample corpora, captures, and working notes.
- Real `$HOME` remains box-private. Never place credentials, agent runtime state, caches, repositories, build outputs, sockets, or locks in `shared`; coordinate concurrent edits to the same file.
- Shared project context lives under `.claude`: `memory/` is team memory, `skills/` contains reusable workflows, and `mailbox/` contains cross-box handoffs. Treat a root `CLAUDE.md`, when present, as project guidance alongside `AGENTS.md`.
- Full guide: `.claude/skein/SHARED-HOME.md`.
<!-- skein:shared-home:end -->
GUIDE

if [ -f "$target" ] && cmp -s "$tmp" "$target"; then rm -f "$tmp"; else mv "$tmp" "$target"; fi
