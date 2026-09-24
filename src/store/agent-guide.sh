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
- Project memory is Claude's memory tool, which skein points at this repo's store: it is shared by this person's boxes on this project, not by a team, which shares only the repository and the tracker. A settled project decision belongs in the repository (for example `docs/decisions/`), where every box and contributor reads it, not in memory. Treat a root `CLAUDE.md`, when present, as project guidance alongside `AGENTS.md`.
- **To reach another box, talk to it directly**: `ListAgents` names every live box in this fleet, `SendMessage` reaches one. That is a conversation — the other box answers — so prefer it whenever the box you want is running. A peer listed as **local** is reached over a socket inside this sandbox, with no network in the path; one listed as `Remote Control` is being routed through Anthropic's servers on this box's claude.ai login, which is not the fleet talking to itself and is unavailable when that connection is down. If every peer reads `Remote Control`, the fleet's own channel is off for this repo (skein's per-repo peer-messaging switch) — use the mailbox and say so, rather than assuming the box you wrote to got it.
- The mailbox, `.claude/skein/bin/mailbox.sh send`, is for what messaging cannot do: a box that is **not running** (mail waits on disk and is delivered at its next turn boundary), a **Codex** box (it cannot receive a message), and **another project** (`all-projects`, or `project:<repo-id>`). Use it there and nowhere else.
- Full guide: `.claude/skein/SHARED-HOME.md`.
<!-- skein:shared-home:end -->
GUIDE

if [ -f "$target" ] && cmp -s "$tmp" "$target"; then rm -f "$tmp"; else mv "$tmp" "$target"; fi
