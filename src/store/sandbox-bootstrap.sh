#!/usr/bin/env bash
# sandbox-bootstrap.sh — skein's SessionStart hook (installed into <store>/skein/bin by ensure_store).
#
# Makes a box's shared store fully live, so an EMPTY shared folder still works end-to-end:
#   - bridges this box's per-$HOME memory dir to the store's memory/ (Claude's memory tool then
#     reads/writes the shared folder → cross-box memory, scoped to this one project's store);
#   - registers the box (who is on what);
#   - materialises the settings' enabled plugins once;
#   - surfaces unread mailbox hand-offs addressed to this box.
#
# Repo-agnostic and fail-soft — a bootstrap problem must never block the session. Branch checkout is
# the kit's job (skein-startup.sh), not this.
set -uo pipefail

input="$(cat 2>/dev/null || true)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null || true)"
[ -z "$cwd" ] && cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
[ -d "$store" ] || { echo "[skein-bootstrap] no .claude store at $store — skipping" >&2; exit 0; }

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"

# --- memory bridge: per-$HOME memory dir → the shared store's memory/ (live, cross-box) ----------
# Claude's memory tool writes to ~/.claude/projects/<slug>/memory (slug = cwd with '/'→'-'). Point
# that at the store's memory/ (a RW mount) so memory is the project's, shared across its boxes.
slug="$(printf '%s' "$cwd" | sed 's#/#-#g')"
home_proj="$HOME/.claude/projects/$slug"
mem_link="$home_proj/memory"
canonical_mem="$store/memory"
mkdir -p "$home_proj" "$canonical_mem" 2>/dev/null || true
if [ -L "$mem_link" ]; then
  [ "$(readlink "$mem_link")" = "$canonical_mem" ] || { rm -f "$mem_link"; ln -s "$canonical_mem" "$mem_link"; }
elif [ -d "$mem_link" ]; then
  cp -a "$mem_link"/. "$canonical_mem"/ 2>/dev/null || true   # rescue any box-local notes
  mv "$mem_link" "$mem_link.pre-skein.$vmid" 2>/dev/null || true
  ln -s "$canonical_mem" "$mem_link"
else
  ln -s "$canonical_mem" "$mem_link" 2>/dev/null || true
fi

# --- register this box (who is on what) ---------------------------------------------------------
branch="$(git -C "$cwd" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
reg="$store/sandboxes.json"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"
if command -v jq >/dev/null 2>&1; then
  (
    flock -w 5 9 || exit 0
    [ -s "$reg" ] || echo '{}' > "$reg"
    tmp="$(mktemp "${TMPDIR:-/tmp}/sbxreg.XXXXXX")" || exit 0
    if jq --arg v "$vmid" --arg b "$branch" --arg d "$root" --arg t "$ts" \
          '.[$v] = ((.[$v] // {started:$t}) + {branch:$b, dir:$d, lastSeen:$t})' \
          "$reg" > "$tmp" 2>/dev/null; then mv "$tmp" "$reg"; else rm -f "$tmp"; fi
  ) 9>"$store/.sandboxes.lock" 2>/dev/null || true
fi

# --- materialise the settings' enabled plugins (once per box, backgrounded) ----------------------
marker="$HOME/.claude/.skein-plugins-materialized"
settings="$store/settings.json"
if [ ! -f "$marker" ] && command -v claude >/dev/null 2>&1 && [ -f "$settings" ] && command -v jq >/dev/null 2>&1; then
  (
    claude plugin marketplace add anthropics/claude-plugins-official >/dev/null 2>&1 || true
    have="$(claude plugin list 2>/dev/null || true)"
    while IFS= read -r id; do
      [ -z "$id" ] && continue
      printf '%s' "$have" | grep -qF "$id" || claude plugin install "$id" --scope project >/dev/null 2>&1 || true
    done < <(jq -r '.enabledPlugins // {} | to_entries[] | select(.value==true) | .key' "$settings" 2>/dev/null)
    touch "$marker"
  ) >/dev/null 2>&1 &
fi

# --- surface unread mailbox hand-offs addressed to this box --------------------------------------
[ -x "$store/skein/bin/mailbox.sh" ] && SANDBOX_VM_ID="$vmid" "$store/skein/bin/mailbox.sh" inbox 2>/dev/null || true

exit 0
