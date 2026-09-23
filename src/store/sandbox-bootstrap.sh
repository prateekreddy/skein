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
# Merged layout: the shared store is that link's target parent, not the repo dir (box-status.sh).
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

# The BOX, not the VM. SKEIN_BOX names the box wherever it was set; with it unset, skein's fleet
# launcher decides — installed at `fleet::box_session_path()` only in a sandbox that HOLDS boxes,
# so its absence means a legacy box alone in its VM where the sandbox's name IS the box's, and its
# presence means a shared sandbox, where SANDBOX_VM_ID is one string for every box in it and a
# signal keyed on it lands on whichever box owns that name. The argument in full, and the measured
# residue that settled it, is in box-status.sh — installed beside this one in <store>/skein/bin/.
#
# Empty rather than `exit 0`, because most of what this hook does is not keyed on identity at all —
# the shared-home contract, the memory bridge and the gitignored-path surfacing are what make the
# box usable, and they are the same work whoever it turns out to be. Only the parts that write
# under a name are skipped, each at its own use below.
if [ -n "${SKEIN_BOX:-}" ]; then
  vmid="$SKEIN_BOX"
elif [ ! -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
  vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
else
  echo "[skein-bootstrap] no SKEIN_BOX in a shared sandbox: this box cannot say which box it is, so it registers and reports under no name" >&2
  vmid=""
fi
vmid="${vmid//\//-}"
# Where this repo's SOURCE TREE is — the checkout the gitignored files below come from. Not a
# mirror: a mirror is a remote and carries tracked files only, so the .env and the CLAUDE.md a
# project keeps out of git exist in no clone of any shape, only in somebody's working tree.
#
# `--clone` mode bind-mounts that tree read-only at /run/sandbox/source. A fleet box has no such
# mount (one sandbox, many repos) and cannot reach the tree at all — and nothing on the host copies
# the manifest's files into the store for it either: the two calls that did went with local-path
# repos, and src/fleet/start.rs records why at the point they were removed ("a repo is a remote now, no
# checkout is reachable from inside the fleet, and the pair had already been reduced to printing a
# warning that the files had not arrived"). What a fleet box surfaces is whatever its store already
# holds under shared-rw/. So this is a fallback, and an EMPTY answer is an ordinary state rather
# than a failure: everything below is gated on the manifest, never on this.
# $SKEIN_SOURCE names it outright (a runtime that is not sbx, and the seam the tests drive); then
# the clone-mode bind; then the path skein recorded. Only the bind is read-only. `skein/mirror` is
# read for stores seeded before the two things had separate names.
source_tree="${SKEIN_SOURCE:-}"
source_is_ro=0
if [ -z "$source_tree" ]; then
  if [ -d /run/sandbox/source ]; then
    source_tree="/run/sandbox/source"
    source_is_ro=1
  else
    # The first recorded path that is actually THERE, not the first one written. A path that has
    # gone away passes every "is anything recorded" test and fails every `-d` one, so it shadowed
    # the older name's answer while being no answer itself, and everything below went quiet with
    # nothing to read (SKEIN-472). Nothing on the host writes these files any more, and nothing
    # clears a dead one: the only host-side code that still touches them is the volume move's
    # marker rewrite (src/volume.rs), which repoints a path under a moved volume rather than
    # dropping one that has gone. So this `-d` test is the whole of the defence, and it is here
    # because the paths it guards against were written by a skein older than this script.
    for recorded in "$store/skein/source" "$store/skein/mirror"; do
      candidate="$(sed -n '1p' "$recorded" 2>/dev/null || true)"
      if [ -n "$candidate" ] && [ -d "$candidate" ]; then
        source_tree="$candidate"
        break
      fi
    done
  fi
fi

# Echo the exact installed probe contract from SessionStart. The host compares this with the current
# store revision and can offer a targeted agent-session restart when a long-running process is old.
# Skipped without an identity: `probe_is_stale` reads this file BY BOX NAME, so one filed under the
# sandbox's name answers for no box and would answer for the wrong one if a box ever bore that name.
boot_dir="$store/skein/boot"
boot="$boot_dir/$vmid.json"
revision="$(sed -n '1p' "$store/skein/probe-revision" 2>/dev/null || true)"
if [ -n "$vmid" ]; then
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
fi

# --- surface gitignored shared paths from the repo's source tree ---------------------------------
# A clone carries only TRACKED files, so gitignored ones the project needs (CLAUDE.md, .env, …) are
# absent. Each non-comment line of <store>/shared-paths.txt is a repo-relative path (file or dir),
# optionally followed by `rw`, to surface into the clone. Repo-agnostic: the manifest is the
# project's own list. Surfaced paths are added to the clone's .git/info/exclude so a careless
# `git add -A` can't stage a host-absolute symlink (the tracked .gitignore is never touched).
#
# Default (no `rw` suffix) — RO: symlinked straight from the source tree. `/run/sandbox/source` is a
# read-only bind mount (sbx's own doing — no per-file trick makes it writable), so edits fail.
# `rw` suffix — RW + host-visible + shared: the file is seeded ONCE into the store's shared-rw/
# (a genuinely writable host directory, same as memory/), then symlinked from THERE instead of the
# source tree. Edits inside the box persist to that store path and are live across every box on the
# repo (last-write-wins, same semantics as the memory bridge below) — just not back to the file's
# original repo-relative host path, since that path is unreachable read-write from inside a clone.
manifest="$store/shared-paths.txt"
if [ -f "$manifest" ]; then
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
    # Outside --clone mode the source tree is the repo's real work tree, and a box that can see it
    # at all sees it READ-WRITE. An RO entry
    # symlinked straight at it would let a box silently edit the host's own checkout, which the
    # read-only bind used to make impossible for free. So every entry takes the `rw` shape there:
    # seeded into the store's shared-rw/ and linked from there, so a box can still never reach the
    # host checkout. RO stops meaning "edits fail" and starts meaning "edits do not reach the host".
    if [ "$flag" = "rw" ] || [ "$source_is_ro" = "0" ]; then
      rwcopy="$rw_root/$p"
      # Usually already there from an earlier box on this repo: this is the seed, run from inside
      # the box against the read-only `$source_tree` bind — nothing on the host seeds `shared-rw/`
      # any more. Idempotent, so a settled store just skips it, and it does nothing at all when
      # there is no source tree to read.
      if [ ! -e "$rwcopy" ] && [ -d "$source_tree" ]; then
        mkdir -p "$(dirname "$rwcopy")" 2>/dev/null || true
        [ -e "$source_tree/$p" ] && cp -a "$source_tree/$p" "$rwcopy" 2>/dev/null
      fi
      [ -e "$rwcopy" ] || continue
      # self-heal: an existing symlink from before this box was RW-flagged pointed at the RO source.
      if [ -L "$dst" ] && [ "$(readlink "$dst")" != "$rwcopy" ]; then
        rm -f "$dst"
      fi
      if [ ! -e "$dst" ] && [ ! -L "$dst" ]; then
        mkdir -p "$(dirname "$dst")" 2>/dev/null || true
        ln -s "$rwcopy" "$dst" 2>/dev/null
      fi
    else
      src="$source_tree/$p"
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
  # `${vmid:-unknown}` and not a skip: this suffix only keeps two rescued directories apart, and a
  # box that cannot name itself still has notes worth not overwriting.
  mv "$mem_link" "$mem_link.pre-skein.${vmid:-unknown}" 2>/dev/null || true
  ln -s "$canonical_mem" "$mem_link"
else
  ln -s "$canonical_mem" "$mem_link" 2>/dev/null || true
fi

# --- register this box (who is on what) ---------------------------------------------------------
# Gated on the identity, and this is the entry with the longest reach of any of them: the registry
# is keyed by box name and the host reads it every couple of seconds, so a row under the sandbox's
# name is a box the board can be asked to show and nothing will ever clean up — `delist_box` and
# `destroy_box` remove a box's rows BY NAME, and no box is named after the sandbox. One is already
# on disk, in sync's registry, from before this guard existed.
branch="$(git -C "$cwd" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
reg="$store/sandboxes.json"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"
if [ -n "$vmid" ] && command -v jq >/dev/null 2>&1; then
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

# --- materialise the settings' enabled plugins (backgrounded) ------------------------------------
#
# The marker records WHICH plugins are known to be installed, not merely that this ran once. Both
# halves of that matter, and the earlier version got both wrong:
#
#   * it wrote the marker unconditionally, and every step here is `|| true`. A box that could not
#     reach the marketplace — no network yet, an unauthenticated agent, a slow first boot — finished
#     having installed nothing and was marked done for good. Silently, and permanently.
#   * it recorded nothing about *what* was wanted, so enabling a plugin later never reached a box
#     that already existed. The setting would look applied and only new boxes would have it.
#
# So: re-run whenever the wanted set differs from what was last confirmed, and record only what is
# actually present afterwards. A failure leaves the marker alone and the next box start tries again.
marker="$HOME/.claude/.skein-plugins-materialized"
settings="$store/settings.json"
wanted=""
if [ -f "$settings" ] && command -v jq >/dev/null 2>&1; then
  # Sorted, so the same set never looks like a different one and re-installs on every start.
  wanted="$(jq -r '.enabledPlugins // {} | to_entries[] | select(.value==true) | .key' "$settings" 2>/dev/null | sort | tr '\n' ' ')"
fi
if [ -n "$wanted" ] && [ "$wanted" != "$(cat "$marker" 2>/dev/null || true)" ] \
   && command -v claude >/dev/null 2>&1; then
  (
    claude plugin marketplace add anthropics/claude-plugins-official >/dev/null 2>&1 || true
    have="$(claude plugin list 2>/dev/null || true)"
    for id in $wanted; do
      printf '%s' "$have" | grep -qF "$id" || claude plugin install "$id" --scope project >/dev/null 2>&1 || true
    done
    # Asked again rather than assumed: `plugin install` is best-effort above, so the only honest
    # record of what happened is what the agent lists now. Anything missing means this box is not
    # done, and leaving the marker unwritten is what brings it back here next start.
    have="$(claude plugin list 2>/dev/null || true)"
    for id in $wanted; do
      printf '%s' "$have" | grep -qF "$id" || exit 0
    done
    printf '%s' "$wanted" > "$marker"
  ) >/dev/null 2>&1 &
fi

# --- surface unread mailbox hand-offs addressed to this box --------------------------------------
# Handed over as SKEIN_BOX, not SANDBOX_VM_ID. The identity resolved above is a BOX name, and
# putting a box name in the variable that means "the sandbox" made mailbox.sh re-derive it through
# the same chain this file just fixed — so it inherited the fault instead of the answer. Skipped
# outright when there is no identity: an inbox read under the wrong name delivers nothing that was
# addressed to this box and marks other boxes' broadcasts seen by a name nobody owns.
[ -n "$vmid" ] && [ -x "$store/skein/bin/mailbox.sh" ] \
  && SKEIN_BOX="$vmid" "$store/skein/bin/mailbox.sh" inbox 2>/dev/null || true

exit 0
