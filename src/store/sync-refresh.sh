#!/usr/bin/env bash
# sync-refresh.sh — bring this box's work-tracking documents up to date with the store's.
#
# sync-install.sh installs once and hands off: after setup the box owns its CLAUDE.md, its memory and
# its skill, and owning them includes deleting or rewriting the parts it does not want. That is
# deliberate and stays true here. But it left no way to deliver a CORRECTION. When upstream moved
# decomposition from `capture` per child to `decompose`, every box already set up kept the superseded
# rule, sitting next to a reference copy the host refreshes on every launch.
#
# So: refresh what skein put there and the box has not touched, and nothing else.
#
#   current  installed content already equals the reference — nothing to do
#   stale    installed content is exactly what skein last installed, and the reference has moved
#   yours    the box edited it — positive evidence, because it matches neither
#   unknown  differs from the reference, and there is no record of what was installed
#   absent   never installed here (sync-install.sh's job, not this one)
#
# Only `stale` is rewritten. The invariant worth stating plainly: **skein never overwrites an edit it
# can see.** A refresh that did would be the second thing to stop the box owning its config, and the
# box would be right to stop trusting either.
#
# Telling `stale` from `yours` needs to know what was installed, which is what the manifest records:
# the reference's hash at the moment it was written. Boxes set up before the manifest existed have no
# such record, and for those the two are genuinely indistinguishable — so they are `unknown` rather
# than quietly sorted into whichever bucket is convenient. `--force` accepts `unknown`, because a
# human pressing the button is the missing evidence. It still refuses `yours`.
#
#   --check   print one `name<TAB>state` line per document and change nothing
#   --force   also rewrite `unknown` — for boxes installed before the manifest existed
#
# Fail-soft: this is invoked from the cockpit against a live box, and a work-tracking problem must
# never take a box down with it.
set -uo pipefail

project="${WORKSPACE_DIR:-}"
[ -n "$project" ] || project="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
# The store. Two copies of this script exist and they find it differently, as mailbox.sh's do: the
# store's own, two levels below it (`.claude/skein/bin/`), and skein's read-only plugin's
# (`probe/`), which is the one skein runs (SKEIN-1149) and which finds it from the project's
# `.claude`, with the merged layout's hop (box-status.sh says why).
self="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
if [ "$(basename "$(dirname "$self")")" = skein ]; then
  store="$(dirname "$(dirname "$self")")"
else
  store="$project/.claude"
  if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"
  elif [ -L "$store" ]; then store="$(readlink -f "$store")"; fi
fi
src="$store/skein/sync"

slug="$(printf '%s' "$project" | sed 's#/#-#g')"
state_dir="$HOME/.local/state/skein"
manifest="$state_dir/sync-$slug.manifest"
stamp="$state_dir/sync-$slug.done"

check_only=""
force=""
for arg in "$@"; do
  case "$arg" in
    --check) check_only=1 ;;
    --force) force=1 ;;
  esac
done

if [ ! -d "$src" ]; then
  echo "[sync] $src is missing — restart the host server to refresh this store" >&2
  exit 0
fi
if [ ! -e "$stamp" ]; then
  # Never set up. Refreshing would install the documents behind the "only after registration
  # succeeds" rule that keeps a box from being told to call tools it does not have.
  echo "[sync] this box is not set up for work tracking — use Track work first" >&2
  exit 0
fi

sha() { [ -r "$1" ] && sha256sum < "$1" 2>/dev/null | cut -d' ' -f1; }
sha_text() { printf '%s' "$1" | sha256sum 2>/dev/null | cut -d' ' -f1; }
recorded() { [ -r "$manifest" ] && awk -F'\t' -v k="$1" '$1==k{print $2; exit}' "$manifest"; }

# Rewrite one key in the manifest, creating it if absent. Whole-file rewrite because it has three
# lines; a partial update that lost the other two would silently reclassify them as `yours` forever.
record() {
  mkdir -p "$state_dir" 2>/dev/null || return 0
  local tmp="$manifest.tmp.$$"
  { [ -r "$manifest" ] && awk -F'\t' -v k="$1" '$1!=k' "$manifest"; printf '%s\t%s\n' "$1" "$2"; } \
    > "$tmp" 2>/dev/null && mv "$tmp" "$manifest" 2>/dev/null || rm -f "$tmp" 2>/dev/null
}

# The block is a section of a file the box also writes, so it is compared and replaced as a section:
# from its heading to the next one, leaving everything the box added around it untouched.
block_of() {
  [ -r "$1" ] || return 0
  awk '/^## Work tracking$/{f=1} f&&/^## /&&!/^## Work tracking$/{f=0} f' "$1"
}

classify() { # installed_sha reference_sha manifest_sha -> state
  [ -z "$1" ] && { echo absent; return; }
  [ "$1" = "$2" ] && { echo current; return; }
  [ -z "$3" ] && { echo unknown; return; }
  [ "$1" = "$3" ] && { echo stale; return; }
  echo yours
}

# What this run is willing to rewrite. `yours` is absent from both lists on purpose.
writable() {
  [ "$1" = "stale" ] && return 0
  [ "$1" = "unknown" ] && [ -n "$force" ] && return 0
  return 1
}

changed=0
report() { printf '%s\t%s\n' "$1" "$2"; }

# ── skill and memory: whole files, both in the store and shared by every box of this repo ──────────
for pair in "skill:$store/skills/work-tracking/SKILL.md:$src/work-tracking.skill.md" \
            "memory:$store/memory/work-tracking.md:$src/work-tracking.memory.md"; do
  name="${pair%%:*}"; rest="${pair#*:}"; dest="${rest%%:*}"; ref="${rest#*:}"
  ref_sha="$(sha "$ref")"
  [ -n "$ref_sha" ] || continue
  state="$(classify "$(sha "$dest")" "$ref_sha" "$(recorded "$name")")"
  report "$name" "$state"
  [ -n "$check_only" ] && continue
  if writable "$state"; then
    if cp "$ref" "$dest" 2>/dev/null; then
      record "$name" "$ref_sha"
      changed=$((changed + 1))
      # The skill's two linked pages ride with it rather than being classified on their own. They
      # have no separate manifest entry because nothing edits them in isolation: they are reached
      # only by a link from SKILL.md, so their state is whatever SKILL.md's is. Refreshing the entry
      # point and leaving the pages it links to at an older revision is the one combination that
      # would read as current and not be.
      if [ "$name" = "skill" ]; then
        for page in organising troubleshooting; do
          cp "$src/work-tracking.$page.md" "${dest%/*}/$page.md" 2>/dev/null || true
        done
      fi
    else
      echo "[sync] could not write $dest" >&2
    fi
  fi
done

# ── the block: a section inside CLAUDE.md, which is per box rather than per repo ───────────────────
ref_block="$(cat "$src/work-tracking.block.md" 2>/dev/null)"
ref_block_sha="$(sha_text "$ref_block")"
for doc in CLAUDE.md AGENTS.md; do
  path="$project/$doc"
  [ -f "$path" ] || continue
  [ -L "$path" ] && continue   # normally the same file as CLAUDE.md; refreshing it twice would double the work
  have="$(block_of "$path")"
  state="$(classify "$(sha_text "$have")" "$ref_block_sha" "$(recorded "block")")"
  # An empty section hashes like an empty string, which is not absence; say absent explicitly.
  [ -z "$have" ] && state=absent
  report "block:$doc" "$state"
  [ -n "$check_only" ] && continue
  writable "$state" || continue
  tmp="$path.skein.tmp.$$"
  if awk -v blockfile="$src/work-tracking.block.md" '
        /^## Work tracking$/ && !done {
          while ((getline line < blockfile) > 0) print line
          close(blockfile); done=1; skip=1; next
        }
        skip && /^## / { skip=0 }
        !skip { print }
      ' "$path" > "$tmp" 2>/dev/null && [ -s "$tmp" ]; then
    cat "$tmp" > "$path" 2>/dev/null && changed=$((changed + 1))
    rm -f "$tmp" 2>/dev/null
    record "block" "$ref_block_sha"
  else
    rm -f "$tmp" 2>/dev/null
    echo "[sync] could not rewrite the Work tracking section in $doc" >&2
  fi
done

[ -n "$check_only" ] && exit 0
if [ "$changed" -gt 0 ]; then
  echo "[sync] refreshed $changed work-tracking document(s)" >&2
else
  echo "[sync] nothing to refresh" >&2
fi
exit 0
