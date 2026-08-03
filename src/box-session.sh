#!/usr/bin/env bash
# skein box-session.sh — start one box inside a shared sandbox.
#
# Several boxes live in one sbx sandbox so that memory is a pool they share rather than N
# reservations that sum. A shared VM does not hand out /tmp and $HOME per box, so this makes them
# private deliberately: a bwrap mount namespace with the box's own directories bound over both.
#
# Without it, two boxes running a suite that writes /tmp/fixture stomp each other — and far worse,
# they share ~/.claude.json (so every box gets the same MCP servers, defeating per-repo work-tracking
# gateways) and ~/.config/sync/env (so two boxes claim work as the SAME agent, silently defeating the
# atomic claim the tracker exists for).
#
# usage: box-session.sh <box> <root> <pidfile> <cmd…>
#
# `pidfile` must be on a path that is NEITHER under /tmp NOR under $HOME — both are bound over from
# inside, so a pidfile under either is written into the box's private view and is unreadable from
# anywhere else. The shared store is the natural place, and skein already reads it host-side.
#
# skein reaches back in with:
#   nsenter --user=/proc/<pid>/ns/user --mount=/proc/<pid>/ns/mnt --preserve-credentials -- …
# Both namespaces must be joined together (mount alone is refused) and credentials preserved (or
# setgroups fails for an unprivileged caller). Getting either wrong reads as a permissions bug
# rather than a missing flag, which is why it is written down here and asserted in place.rs.
set -uo pipefail

box="${1:?usage: box-session.sh <box> <root> <pidfile> <cmd…>}"
root="${2:?missing box root}"
pidfile="${3:?missing pidfile}"
shift 3
[ "$#" -gt 0 ] || { echo "skein: no command to run" >&2; exit 2; }

case "$box" in
  */*|*..*|"") echo "skein: refusing box name: $box" >&2; exit 2 ;;
esac

case "$pidfile" in
  /tmp/*|"$HOME"/*)
    echo "skein: pidfile $pidfile is under a path this box binds over; it would be unreadable" >&2
    exit 2 ;;
esac

command -v bwrap >/dev/null 2>&1 || { echo "skein: bwrap is not installed in this sandbox" >&2; exit 3; }

home="$root/home"
tmp="$root/tmp"
tree="$root/tree"
mkdir -p "$home" "$tmp" "$tree" "$(dirname "$pidfile")" || exit 1
# 0700: the box's HOME holds its tracker token and its MCP registration. Other boxes here are not a
# security boundary, but they are not entitled to read it by accident either.
chmod 700 "$home" "$tmp" 2>/dev/null || true

# --dev-bind / / keeps the sandbox's own filesystem visible (the repo, the toolchains, the store
# mount) and then binds the box's private directories over the two paths that must not be shared.
# No --unshare-pid: the pid printed from inside has to be the pid skein sees from outside, or
# nsenter has nothing to address.
# --die-with-parent so a killed session cannot strand a namespace holding a pid skein would later
# try to enter.
exec bwrap \
  --dev-bind / / \
  --bind "$tmp" /tmp \
  --bind "$home" "$HOME" \
  --die-with-parent \
  -- \
  bash -lc '
    pidfile="$1"; tree="$2"; shift 2
    # cd here rather than relying on bwrap --chdir: this is a LOGIN shell, and sourcing the
    # profile can move it. Observed doing exactly that — the box started at / instead of its tree.
    cd "$tree" || exit 1
    # Written from INSIDE and after the mounts: a pid captured before them would name a process
    # whose namespace is not yet the one we mean. `exec` keeps the pid, so this stays the pid of
    # whatever the box actually runs.
    printf "%s\n" "$$" > "$pidfile"
    exec "$@"
  ' bash "$pidfile" "$tree" "$@"
