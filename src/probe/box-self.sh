# box-self.sh — which box this is, and where its store is: the one answer to each question for
# every script skein ships into a box (SKEIN-1174). Sourced, never run: it defines two functions
# and does nothing else, so sourcing it cannot fail a caller or print into a hook's stdout.
#
# Installed beside every script that sources it — in both variants of skein's read-only plugin
# (`probe/`) and in the store's own `skein/bin/` — and found by the caller's own directory.
#
#   skein_box_store <root>   prints this box's store, or prints nothing and returns 1
#   skein_box_name           prints this box's name, or prints nothing and returns 1
#
# What a caller does on a refusal is its own business — a hook exits quietly, the mailbox says why
# — but the DECISION is made here and nowhere else.

# The store. Tried in this order, and anything else is a refusal:
#
#   1. what skein recorded: `<git dir>/skein-store`, written by the launcher (src/box-session.sh)
#      from the store the host named for this box, and by the kit at provisioning. In the checkout's
#      git directory rather than in the environment, so that it answers for THIS checkout and for
#      nothing else: a variable would follow every process a box starts, including a test suite run
#      inside it, whose fixtures would then report into the box's live store;
#   2. the script's own store, when it is the store's copy: this file at `<store>/skein/bin/`;
#   3. the `.claude/skein` link the kit makes (its target's parent), or a `.claude` that is itself a
#      link to the store (a box from before SKEIN-1053, until its kit converts it).
#
# **Never the checkout's own `.claude`.** In a repo that tracks `.claude/`, that is a directory of
# the repo's files, and treating it as the store is SKEIN-1174: a boot report, `$HOME/shared`, the
# memory bridge and every probe's signals written into the clone, where the host never looks. So a
# store at or under the checkout is refused whichever rule found it, and "the link was not there"
# is a refusal rather than a fallback.
skein_box_store() {
  local root="${1:-}" real_root="" gitdir cand here
  [ -n "$root" ] && real_root="$(cd "$root" 2>/dev/null && pwd -P)"
  # 1. What skein recorded for this checkout. `--git-common-dir`, so a worktree of the box's clone
  # reads its clone's record.
  gitdir="$(git -C "${root:-.}" rev-parse --git-common-dir 2>/dev/null)" || gitdir=""
  case "$gitdir" in "" | /*) ;; *) gitdir="$root/$gitdir" ;; esac
  if [ -n "$gitdir" ] && [ -r "$gitdir/skein-store" ]; then
    cand="$(sed -n '1p' "$gitdir/skein-store" 2>/dev/null)"
    if _skein_is_store "$cand" "$real_root"; then printf '%s\n' "$cand"; return 0; fi
  fi
  # 2. The store's own copy of this file. Physical, so a copy reached through the checkout's
  # `.claude/skein` link still names the store and not the checkout.
  # Only when this is a file: pasted into a `bash -c` (src/sharedhome.rs) it has no directory.
  case "${BASH_SOURCE[0]:-}" in */box-self.sh) here="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd -P)" ;; *) here="" ;; esac
  if [ -n "$here" ] && [ "$(basename "$here")" = bin ] && [ "$(basename "$(dirname "$here")")" = skein ]; then
    cand="$(dirname "$(dirname "$here")")"
    if _skein_is_store "$cand" "$real_root"; then printf '%s\n' "$cand"; return 0; fi
  fi
  # 3. The link the kit made. As the link spells it, not resolved, so every caller names the store
  # by the same path the kit linked (`shared-home.sh` compares its link against that spelling).
  [ -n "$root" ] || return 1
  if [ -L "$root/.claude/skein" ]; then
    cand="$(readlink "$root/.claude/skein")"
    case "$cand" in /*) ;; *) cand="$root/.claude/$cand" ;; esac
    cand="$(dirname "$cand")"
  elif [ -L "$root/.claude" ]; then
    cand="$(readlink -f "$root/.claude")"
  else
    return 1
  fi
  if _skein_is_store "$cand" "$real_root"; then printf '%s\n' "$cand"; return 0; fi
  return 1
}

# A store is a directory holding skein's own `skein/` directory, and not the checkout or anything
# in it. $1 the candidate, $2 the checkout's physical path (empty when there is none).
_skein_is_store() {
  local real
  [ -n "${1:-}" ] && [ -d "$1/skein" ] || return 1
  real="$(cd "$1" 2>/dev/null && pwd -P)" || return 1
  if [ -n "${2:-}" ]; then
    case "$real/" in "$2"/*) return 1 ;; esac
  fi
  return 0
}

# The BOX, not the VM. In a shared sandbox every box has the same SANDBOX_VM_ID, so keying a signal
# on it makes every box write one file and the board see none of them report.
#
# SKEIN_BOX names the box wherever it was set: the launcher exports it before it starts the box's
# tmux server (src/box-session.sh), so the agent and every hook it forks inherit it, and every
# placement hop into a shared box exports it too (`wrap` in src/place/argv.rs).
#
# With SKEIN_BOX unset, whether SANDBOX_VM_ID is this box's name depends on which world this box is
# in, and the fact that answers it is the fleet launcher: skein installs it at
# `fleet::box_session_path()` in the one sandbox that holds boxes, and never in a per-VM sandbox,
# which `sbx create` builds with no fleet machinery at all. The launcher is a fact about the
# SANDBOX, so it is visible to anything running inside it, whatever its lineage. So
#   · SKEIN_BOX set          — that is the box, whatever else is in the environment;
#   · unset, no launcher     — a legacy box, alone in its VM, where the two names are the same
#                              string. Unchanged: this is the path that has always worked;
#   · unset, with a launcher — a shared sandbox and no identity: refused. Writing under
#                              SANDBOX_VM_ID here files this box's signal under a name that is not
#                              its own, and overwrites whichever box does own that name.
#
# Refusing is the conservative half. A box with no signal reads as one that has not reported, which
# is TRUE and which the board already says out loud; a signal under the wrong name is well-formed,
# fresh, and renders as another box's state with nothing to mark it. Measured residue of the
# writing version: five repo stores hold a `status/skein-fleet.json`, five hold a
# `status/skein-fleet.pane.json` written within five minutes of each other on 2026-08-04, and one
# holds a `skein-fleet` entry in its registry — `skein-fleet` is `config::default_fleet_sandbox`,
# the SANDBOX's name, and no box has ever been called that.
#
# Slash-safe: the name keys files (`status/<box>.json`), so a `/` in it becomes `-`.
skein_box_name() {
  local name
  if [ -n "${SKEIN_BOX:-}" ]; then
    name="$SKEIN_BOX"
  elif [ ! -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
    name="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
  else
    return 1
  fi
  printf '%s\n' "${name//\//-}"
}
