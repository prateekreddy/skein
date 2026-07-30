#!/usr/bin/env bash
# sync-install.sh — wire this box to the `sync` work tracker, and install the discipline for it.
#
# Lives in the store (`<store>/skein/bin/`), which is mounted live into every box for the repo. That
# is deliberately not the kit: a kit only reaches boxes created after it changed, and the whole point
# is that an EXISTING box can be wired up too. Refreshed host-side on every launch, so a box picks up
# a newer version of this script by doing nothing.
#
# Runs from two places, and must behave the same in both:
#   - the kit's startup hook, on every box start (silently does nothing until credentials exist);
#   - `skein::sync_provision_box`, right after it writes those credentials.
#
# Installs three things, and only AFTER registration succeeds:
#   - a "Work tracking" section in CLAUDE.md (and so in AGENTS.md) — always in context, because
#     "claim before you start" has to fire when the agent was not thinking about tools at all;
#   - a memory, so the rules that must fire unprompted survive into a fresh session;
#   - the `work-tracking` skill — Plane's full surface, loaded on demand.
# An instruction to "call capture" in a box with no sync server is a rule the agent cannot follow
# and will learn to read past, which is worse than no instruction at all.
#
# ONCE per box, then hands off. Non-destructive copying is not enough on its own: after setup the box
# owns all of this, and owning it includes deleting the parts it does not want. A script that
# re-asserts on every start makes an edit the box cannot make stick. SKEIN_SYNC_FORCE=1 re-applies —
# that is how a rotated token gets installed.
#
# Fail-soft throughout: this runs during startup, and a work-tracking problem must never stop a box.
set -uo pipefail

# The store is two levels up from this script — exact, and free of any guess about layout.
store="$(cd "$(dirname "$0")/../.." 2>/dev/null && pwd)"
src="$store/skein/sync"

project="${WORKSPACE_DIR:-}"
[ -n "$project" ] || project="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# Credentials: the environment first, then the box-private file skein writes. Never the store — the
# agent token is a bearer credential and the store is mounted live into every box for this repo.
cred="$HOME/.config/sync/env"
# shellcheck disable=SC1090
[ -r "$cred" ] && . "$cred" 2>/dev/null
url="${SYNC_GATEWAY_URL:-}"
token="${SYNC_AGENT_TOKEN:-}"

if [ -z "$url" ] || [ -z "$token" ]; then
  # The common case at box startup. Quiet on purpose: a box with no tracker is not a broken box.
  exit 0
fi
if [ ! -d "$src" ]; then
  echo "[sync] $src is missing — restart the host server to refresh this store" >&2
  exit 0
fi

slug="$(printf '%s' "$project" | sed 's#/#-#g')"
state="$HOME/.local/state/skein"
stamp="$state/sync-$slug.done"
if [ -e "$stamp" ] && [ -z "${SKEIN_SYNC_FORCE:-}" ]; then
  echo "[sync] already set up — this box owns its tracker config now (SKEIN_SYNC_FORCE=1 to re-apply)" >&2
  exit 0
fi

# The gateway URL is a base; the MCP endpoint is /mcp under it. Accept either spelling so a pasted
# endpoint URL does not silently become /mcp/mcp.
url="$(printf '%s' "$url" | sed 's#/*$##')"
case "$url" in
  */mcp) ;;
  *) url="$url/mcp" ;;
esac

registered="no"

# Claude Code. User scope rather than local: the box is single-purpose, and a user-scoped server is
# found whichever directory the agent starts in. The credential lands in the box's own
# ~/.claude.json, which is private to this box.
if command -v claude >/dev/null 2>&1; then
  claude mcp remove sync -s user >/dev/null 2>&1 || claude mcp remove sync >/dev/null 2>&1 || true
  if claude mcp add --transport http sync "$url" \
       --header "Authorization: Bearer $token" --scope user >/dev/null 2>&1; then
    registered="yes"
  else
    echo "[sync] claude mcp add failed — check the gateway URL and token" >&2
  fi
fi

# Codex. Its config is TOML that may carry hand-written entries, so append only when the section is
# absent and never rewrite it — except on a forced re-apply, which is how a new token arrives.
codex_cfg="$HOME/.codex/config.toml"
if command -v codex >/dev/null 2>&1 || [ -f "$codex_cfg" ]; then
  mkdir -p "$HOME/.codex" 2>/dev/null || true
  if [ -n "${SKEIN_SYNC_FORCE:-}" ] && [ -f "$codex_cfg" ]; then
    tmpcfg="$codex_cfg.skein.tmp"
    if awk '/^\[mcp_servers\.sync\]/ { skip=1; next } /^\[/ { skip=0 } skip != 1 { print }' \
         "$codex_cfg" > "$tmpcfg" 2>/dev/null; then
      mv "$tmpcfg" "$codex_cfg" 2>/dev/null || rm -f "$tmpcfg" 2>/dev/null
    else
      rm -f "$tmpcfg" 2>/dev/null
    fi
  fi
  if grep -Fq '[mcp_servers.sync]' "$codex_cfg" 2>/dev/null; then
    registered="yes"
  elif printf '\n[mcp_servers.sync]\nurl = "%s"\nhttp_headers = { Authorization = "Bearer %s" }\n' \
         "$url" "$token" >> "$codex_cfg" 2>/dev/null; then
    registered="yes"
  else
    echo "[sync] could not write $codex_cfg" >&2
  fi
fi

if [ "$registered" != "yes" ]; then
  echo "[sync] no runtime registered — work-tracking docs not installed" >&2
  exit 0
fi

# --- the discipline, now that the tools behind it exist -------------------------------------------

# 1. The always-in-context rules. Appended once to CLAUDE.md; AGENTS.md is normally the same file
# through a symlink and is skipped as such. If they are two real files, both get it.
for doc in CLAUDE.md AGENTS.md; do
  path="$project/$doc"
  [ -f "$path" ] || continue
  [ -L "$path" ] && continue
  grep -Fq '## Work tracking' "$path" 2>/dev/null && continue
  printf '\n---\n\n' >> "$path" 2>/dev/null || continue
  cat "$src/work-tracking.block.md" >> "$path" 2>/dev/null \
    && echo "[sync] added the Work tracking section to $doc" >&2
done

# 2. The memory, plus its line in the index loaded every session. A memory file with no index entry
# is never recalled, so the two are one step. Both land in the STORE, which is this repo's `.claude`
# — so every box of the repo sees them, not just this one.
mkdir -p "$store/memory" 2>/dev/null || true
[ -e "$store/memory/work-tracking.md" ] \
  || cp "$src/work-tracking.memory.md" "$store/memory/work-tracking.md" 2>/dev/null || true
idx="$store/memory/MEMORY.md"
[ -f "$idx" ] || printf '# Memory index\n' > "$idx" 2>/dev/null
if [ -f "$idx" ] && ! grep -Fq '(work-tracking.md)' "$idx" 2>/dev/null; then
  printf '\n## 🗂 Work tracking\n- [Work tracking](work-tracking.md) — `capture` on notice, `claim` before you work, `held` after a restart; the `work-tracking` skill has the rest\n' \
    >> "$idx" 2>/dev/null || true
fi

# 3. The skill. Loaded only when the model judges it relevant, which is why it can afford to be the
# long one — Plane's whole 47-tool surface and what each answer is for.
if mkdir -p "$store/skills/work-tracking" 2>/dev/null; then
  [ -e "$store/skills/work-tracking/SKILL.md" ] \
    || cp "$src/work-tracking.skill.md" "$store/skills/work-tracking/SKILL.md" 2>/dev/null || true
fi

# Stamped last, and only on a run that got this far: a box left half-wired by a failure should be
# finished on the next start, not frozen in that state.
mkdir -p "$state" 2>/dev/null && : > "$stamp" 2>/dev/null || true
echo "[sync] work tracking ready — tracker at $url" >&2
exit 0
