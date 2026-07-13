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
store="$root/.claude"
# Merged layout: when the repo ships its own .claude/, the kit links only skein/ into it — the
# shared store is that link's target parent, NOT the repo dir. Writing here without this hop
# would land signals in the box-local clone where the host can never see them.
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"

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
