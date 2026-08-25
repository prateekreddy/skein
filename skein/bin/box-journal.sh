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
