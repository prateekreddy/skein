#!/usr/bin/env bash
# skein box-journal.sh — copy this box's `.skein/journal.md` into the shared store so the cockpit's
# Session tab can actually show it.
#
# `.skein/journal.md` lives in the box's OWN clone, not the shared store. For a clone-mode box
# (skein's primary mode — a private --clone per box) the host has zero filesystem visibility into
# that clone at all, so a host-side read of "the box's dir" (which for a repo box is the shared
# *host* working clone, not the box's private one — see lib.rs's read_journal) silently finds
# nothing, every time. Same root cause box-diff.sh already solves for the diff/commit list: only an
# in-box hook can see the box's own private state, so it has to copy it OUT to the mount the host can
# read. Wired from the Stop hook, alongside box-diff.sh. Fail-soft; prints nothing to stdout.
set -uo pipefail

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
# Which store this box reports into, and under which name: both decided in box-self.sh, installed
# beside this script, which says why neither is guessed (SKEIN-1174). Either one unknown is a quiet
# exit — a hook's stdout is read by the agent, and a signal nobody files is one the board already
# calls missing, while one filed in the checkout or under the sandbox's name is one it believes.
here="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
. "$here/box-self.sh" 2>/dev/null || exit 0
store="$(skein_box_store "$root")" || exit 0
vmid="$(skein_box_name)" || exit 0

journal="$root/.skein/journal.md"
[ -r "$journal" ] || exit 0

dir="$store/journals"
mkdir -p "$dir" 2>/dev/null || exit 0

# Copy the FULL journal, not a tail — the host-side reader (read_journal) caps what it *displays*,
# but this store copy is the durable record: it must survive the box being destroyed (a --clone's
# journal.md dies with it) so past runs stay reviewable for the workflow/process learn-loop this
# feeds. Never truncate here; truncate only at display time. Deliberately absent from
# delist_box/destroy_box's per-box cleanup (status/launch files only) — keep it that way.
cp -f "$journal" "$dir/$vmid.md.tmp" 2>/dev/null && mv "$dir/$vmid.md.tmp" "$dir/$vmid.md" \
  || rm -f "$dir/$vmid.md.tmp" 2>/dev/null

exit 0
