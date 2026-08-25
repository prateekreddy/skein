#!/usr/bin/env bash
# skein box-diff.sh — write this box's branch-vs-base diff to the shared store so skein's
# fleet view and diff panel can show what the agent has changed on its branch.
# Wired from the Stop hook (turn-end). Fail-soft; prints nothing to stdout (Stop hook output
# is injected into the prompt).
set -uo pipefail

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
# Merged layout: when the repo ships its own .claude/, the kit links only skein/ into it — the
# shared store is that link's target parent, NOT the repo dir. Writing here without this hop
# would land signals in the box-local clone where the host can never see them.
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

# The BOX, not the VM. In a shared sandbox every box has the same SANDBOX_VM_ID, so keying a
# signal on it makes every box write one file and the board see none of them report.
#
# SKEIN_BOX names the box wherever it was set: the launcher exports it before it starts the box's
# tmux server (src/box-session.sh), so the agent and every hook it forks inherit it, and every
# placement hop into a shared box exports it too (`wrap` in src/place.rs).
#
# The old chain ran on from there to SANDBOX_VM_ID and then `hostname` unconditionally, and in a
# shared sandbox BOTH of those name the sandbox — one string for every box in it. Whether that
# fallback is sound depends on which world this box is in, and the fact that answers it here is the
# fleet launcher: skein installs it at `fleet::box_session_path()` in the one sandbox that holds
# boxes, and never in a per-VM sandbox, which `sbx create` builds with no fleet machinery at all.
# box-pane.sh answers the same question from SKEIN_TMUX_SOCK and spells the argument out in full; a
# hook is not started by the attach and never sees that variable, but the launcher is a fact about
# the SANDBOX and so is visible to anything running inside it, whatever its lineage. So
#   · SKEIN_BOX set          — that is the box, whatever else is in the environment;
#   · unset, no launcher     — a legacy box, alone in its VM, where the two names are the same
#                              string. Unchanged: this is the path that has always worked;
#   · unset, with a launcher — a shared sandbox and no identity. Writing under SANDBOX_VM_ID here
#                              files this box's signal under a name that is not its own, and
#                              overwrites whichever box does own that name.
#
# Refusing is the conservative half. A box with no signal reads as one that has not reported, which
# is TRUE and which the board already says out loud; a signal under the wrong name is well-formed,
# fresh, and renders as another box's state with nothing to mark it. Measured residue of the
# writing version: five repo stores hold a `status/skein-fleet.json`, one holds a `skein-fleet`
# entry in its registry — `skein-fleet` is `config::default_fleet_sandbox`, the SANDBOX's name, and
# no box has ever been called that.
if [ -n "${SKEIN_BOX:-}" ]; then
  vmid="$SKEIN_BOX"
elif [ ! -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
  vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
else
  exit 0
fi
vmid="${vmid//\//-}"

dir="$store/diffs"
mkdir -p "$dir" 2>/dev/null || exit 0

# Find the merge-base against the nearest base branch.
base=""
for ref in origin/main origin/master main master; do
  if git -C "$root" rev-parse --verify -q "$ref" >/dev/null 2>&1; then
    base="$ref"
    break
  fi
done

range=""
if [ -n "$base" ]; then
  mb="$(git -C "$root" merge-base HEAD "$base" 2>/dev/null || true)"
  [ -n "$mb" ] && range="$mb"
fi
[ -n "$range" ] || range="HEAD"

# Patch: diff from merge-base to working tree (matches host-side logic; shows all branch work
# including uncommitted). Capped at ~2 MB so a huge patch can't wedge the browser.
{
  git -C "$root" diff "$range" 2>/dev/null | head -c 2000000
} >"$dir/$vmid.patch" 2>/dev/null || true

# Shortstat JSON: {"files":N,"ins":N,"del":N} — used for the fleet badge.
stat_out="$(git -C "$root" diff --shortstat "$range" 2>/dev/null || true)"
files=0; ins=0; del=0
if [ -n "$stat_out" ]; then
  files="$(printf '%s' "$stat_out" | grep -o '[0-9]\+ file' | grep -o '[0-9]\+' || echo 0)"
  ins="$(printf '%s' "$stat_out" | grep -o '[0-9]\+ insertion' | grep -o '[0-9]\+' || echo 0)"
  del="$(printf '%s' "$stat_out" | grep -o '[0-9]\+ deletion' | grep -o '[0-9]\+' || echo 0)"
fi
printf '{"files":%s,"ins":%s,"del":%s}\n' \
  "${files:-0}" "${ins:-0}" "${del:-0}" >"$dir/$vmid.json" 2>/dev/null || true

# Recent commits: last 20 subjects newest-first, written for the session digest.
if [ "$range" != "HEAD" ]; then
  git -C "$root" log --format="%s" -n 20 "$range..HEAD" >"$dir/$vmid.commits" 2>/dev/null || true
else
  : >"$dir/$vmid.commits" 2>/dev/null || true
fi

exit 0
