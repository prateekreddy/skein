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
# Repo-agnostic. Observability helpers remain fail-soft, but the shared-home contract fails loudly:
# silently presenting a private directory as shared would risk data loss. Branch checkout is the
# kit's job (skein-startup.sh), not this.
set -uo pipefail

input="$(cat 2>/dev/null || true)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null || true)"
[ -z "$cwd" ] && cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
# Merged layout: when the repo ships its own .claude/, the kit links only skein/ into it — the
# shared store is that link's target parent, NOT the repo dir. Writing here without this hop
# would land signals in the box-local clone where the host can never see them.
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || { echo "[skein-bootstrap] no .claude store at $store — skipping" >&2; exit 0; }

# --- durable project workspace: private $HOME/shared -> mounted store/shared-home ----------------
# The same provider-neutral helper runs during durable kit startup. Running it again here self-heals
# boxes created with an older kit as soon as their live shared probe refreshes.
shared_home="$store/skein/bin/shared-home.sh"
[ -r "$shared_home" ] || {
  echo "[skein-bootstrap] shared-home helper missing at $shared_home" >&2
  exit 1
}
bash "$shared_home" "$store" || exit 1

# Materialise each runtime's native durable-instruction file. This hook self-heals older boxes; the
# kit performs the same step before a newly-created agent starts. No prompt-hook context is emitted.
agent_guide="$store/skein/bin/agent-guide.sh"
runtime_manifest="$store/skein/runtimes.tsv"
if [ -r "$agent_guide" ] && [ -r "$runtime_manifest" ]; then
  while IFS="$(printf '\t')" read -r runtime_id runtime_label runtime_exe instruction_file instruction_override; do
    [ -n "$instruction_file" ] || continue
    bash "$agent_guide" "$store" "$instruction_file" "$instruction_override" \
      || echo "[skein-bootstrap] could not install $runtime_id agent guidance" >&2
  done <"$runtime_manifest"
fi

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"
mirror="/run/sandbox/source"   # the RO host repo mirror — present only in --clone mode

# Echo the exact installed probe contract from SessionStart. The host compares this with the current
# store revision and can offer a targeted agent-session restart when a long-running process is old.
boot_dir="$store/skein/boot"
boot="$boot_dir/$vmid.json"
revision="$(sed -n '1p' "$store/skein/probe-revision" 2>/dev/null || true)"
mkdir -p "$boot_dir" 2>/dev/null || true
if command -v jq >/dev/null 2>&1; then
  tmp="$(mktemp "$boot_dir/.boot.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    [ -s "$boot" ] || printf '{}\n' >"$boot"
    jq --arg r "$revision" --arg t "$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')" \
      '. + {probe_revision:$r,ts:$t,jq:true}' "$boot" >"$tmp" 2>/dev/null \
      && mv "$tmp" "$boot" || rm -f "$tmp" 2>/dev/null
  fi
elif [ ! -s "$boot" ]; then
  printf '{"ts":"%s","jq":false,"probe_revision":""}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')" >"$boot" 2>/dev/null || true
fi

# --- surface gitignored shared paths from the RO mirror (clone mode) -----------------------------
# A clone carries only TRACKED files, so gitignored ones the project needs (CLAUDE.md, .env, …) are
# absent. Each non-comment line of <store>/shared-paths.txt is a repo-relative path (file or dir),
# optionally followed by `rw`, to surface into the clone. Repo-agnostic: the manifest is the
# project's own list. Surfaced paths are added to the clone's .git/info/exclude so a careless
# `git add -A` can't stage a host-absolute symlink (the tracked .gitignore is never touched).
#
# Default (no `rw` suffix) — RO: symlinked straight from the mirror. `/run/sandbox/source` is a
# read-only bind mount (sbx's own doing — no per-file trick makes it writable), so edits fail.
# `rw` suffix — RW + host-visible + shared: the file is seeded ONCE into the store's shared-rw/
# (a genuinely writable host directory, same as memory/), then symlinked from THERE instead of the
# mirror. Edits inside the box persist to that store path and are live across every box on the
# repo (last-write-wins, same semantics as the memory bridge below) — just not back to the file's
# original repo-relative host path, since that path is unreachable read-write from inside a clone.
manifest="$store/shared-paths.txt"
if [ -d "$mirror" ] && [ -f "$manifest" ]; then
  git_dir="$(git -C "$root" rev-parse --git-dir 2>/dev/null || true)"
  case "$git_dir" in "") ;; /*) ;; *) git_dir="$root/$git_dir" ;; esac
  exclude="${git_dir:+$git_dir/info/exclude}"
  exclude_path() {                         # $1 = repo-relative path; add an anchored pattern once
    [ -n "$exclude" ] || return 0
    local pat="/${1%/}"
    mkdir -p "$(dirname "$exclude")" 2>/dev/null || true
    grep -qxF "$pat" "$exclude" 2>/dev/null || printf '%s\n' "$pat" >> "$exclude"
  }
  exclude_path ".claude"                   # the store is surfaced by the kit, not this manifest
  rw_root="$store/shared-rw"
  while IFS= read -r line; do
    line="${line%%#*}"
    p="$(printf '%s' "$line" | awk '{print $1}')"
    flag="$(printf '%s' "$line" | awk '{print $2}')"
    [ -z "$p" ] && continue
    dst="$root/$p"
    if [ "$flag" = "rw" ]; then
      rwcopy="$rw_root/$p"
      if [ ! -e "$rwcopy" ]; then
        mkdir -p "$(dirname "$rwcopy")" 2>/dev/null || true
        [ -e "$mirror/$p" ] && cp -a "$mirror/$p" "$rwcopy" 2>/dev/null
      fi
      [ -e "$rwcopy" ] || continue
      # self-heal: an existing symlink from before this box was RW-flagged pointed at the RO mirror.
      if [ -L "$dst" ] && [ "$(readlink "$dst")" != "$rwcopy" ]; then
        rm -f "$dst"
      fi
      if [ ! -e "$dst" ] && [ ! -L "$dst" ]; then
        mkdir -p "$(dirname "$dst")" 2>/dev/null || true
        ln -s "$rwcopy" "$dst" 2>/dev/null
      fi
    else
      src="$mirror/$p"
      [ -e "$src" ] || continue
      # self-heal: an existing symlink from before this path was RO-flagged pointed at the rw copy.
      if [ -L "$dst" ] && [ "$(readlink "$dst")" != "$src" ]; then
        rm -f "$dst"
      fi
      if [ ! -e "$dst" ] && [ ! -L "$dst" ]; then
        mkdir -p "$(dirname "$dst")" 2>/dev/null || true
        if ln -s "$src" "$dst" 2>/dev/null; then
          # CLAUDE.md is read into context at session START — before this hook runs — so on the run
          # that first links it, print it so this fresh clone still gets the project direction.
          [ "$p" = "CLAUDE.md" ] && { echo "[skein-bootstrap] project direction (CLAUDE.md, freshly linked):"; echo "----- BEGIN CLAUDE.md -----"; cat "$src" 2>/dev/null; echo "----- END CLAUDE.md -----"; }
        fi
      fi
    fi
    exclude_path "$p"                       # idempotent; self-heals clones linked before this ran
  done < "$manifest"
fi

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
    # same-dir mktemp: a $TMPDIR temp makes the mv a cross-device copy (not atomic) and the host
    # reads this registry every 2s — a reader mid-copy sees a torn file. Same-dir rename is atomic.
    tmp="$(mktemp "$store/.sbxreg.XXXXXX")" || exit 0
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
