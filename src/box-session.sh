#!/usr/bin/env bash
# skein box-session.sh — start one box inside a shared sandbox.
#
# Several boxes live in one sbx sandbox so that memory is a pool they share rather than N
# reservations that sum. A shared VM does not hand out /tmp and the agent's own state per box, so
# this makes them private deliberately, in a bwrap mount namespace.
#
# Without it, two boxes running a suite that writes /tmp/fixture stomp each other — and far worse,
# they share ~/.claude.json (so every box gets the same MCP servers, defeating per-repo work-tracking
# gateways) and ~/.config/sync/env (so two boxes claim work as the SAME agent, silently defeating the
# atomic claim the tracker exists for).
#
# It binds those PATHS rather than the whole of $HOME, and the difference is not a refinement — it is
# the difference between a box that works and one that cannot start. `claude` itself lives at
# ~/.local/bin/claude and its credentials at ~/.claude/, so a box handed an empty private $HOME has
# no agent and no way to authenticate one. Binding only what must differ also leaves ~/.cargo,
# ~/.rustup and ~/.npm SHARED, so boxes reuse one toolchain and one build cache instead of each
# paying for its own.
#
# Each private path is seeded from the sandbox's copy on first start, which is how a box inherits a
# working, logged-in agent and then diverges from it.
#
# usage: box-session.sh <box> <root> <pidfile> <session> <cmd…>
#
# Starts the box and RETURNS; the box keeps running. <cmd…> becomes the agent inside tmux session
# <session>, and that tmux server is what holds the namespace open (see "the anchor" below).
#
# One server per box, on the box's own socket — matching skein's existing rule that a box has one
# Skein-owned tmux server. Later runtimes add their own session (`skein-agent-<runtime>`) to this
# same server, so the anchor and the server-global tmux options keep applying to all of them.
#
# `root` and `pidfile` must be on paths that are NEITHER under /tmp NOR under $HOME — both are bound
# over from inside, so anything under them is written into the box's private view and is unreadable
# from anywhere else. /boxes/<name> is the intended layout; the shared store is the natural home for
# the pidfile, and skein already reads it host-side.
#
# skein reaches back in with:
#   nsenter --user=/proc/<pid>/ns/user --mount=/proc/<pid>/ns/mnt --preserve-credentials -- …
# Both namespaces must be joined together (mount alone is refused) and credentials preserved (or
# setgroups fails for an unprivileged caller). Getting either wrong reads as a permissions bug
# rather than a missing flag, which is why it is written down here and asserted in place.rs.
set -uo pipefail

box="${1:?usage: box-session.sh <box> <root> <pidfile> <session> <cmd…>}"
root="${2:?missing box root}"
pidfile="${3:?missing pidfile}"
session="${4:?missing session name}"
shift 4
[ "$#" -gt 0 ] || { echo "skein: no command to run" >&2; exit 2; }

case "$box" in
  */*|*..*|"") echo "skein: refusing box name: $box" >&2; exit 2 ;;
esac

for p in "$root" "$pidfile"; do
  case "$p" in
    /tmp/*|"$HOME"/*)
      echo "skein: $p is under a path this box binds over; it would be unreadable from outside" >&2
      exit 2 ;;
    /*) ;;
    *) echo "skein: $p must be an absolute path" >&2; exit 2 ;;
  esac
done

command -v bwrap >/dev/null 2>&1 || { echo "skein: bwrap is not installed in this sandbox" >&2; exit 3; }
command -v tmux  >/dev/null 2>&1 || { echo "skein: tmux is not installed in this sandbox" >&2; exit 3; }

home="$root/home"
tmp="$root/tmp"
tree="$root/tree"
# The socket sits in the box root rather than in /tmp: /tmp is bound over, so a socket there would be
# invisible outside, and skein wants to list and attach to boxes from the sandbox without entering
# every namespace first. Same path inside and out.
sock="$root/session.sock"

mkdir -p "$home" "$tmp" "$tree" "$(dirname "$pidfile")" || exit 1
# 0700: this holds the box's tracker token and its agent credentials. Other boxes here are not a
# security boundary, but they are not entitled to read them by accident either.
chmod 700 "$home" "$tmp" 2>/dev/null || true

# The agent state that must differ per box. Everything else in $HOME stays shared on purpose.
#   .claude.json    — MCP registration, so each box gets its repo's own work-tracking gateway
#   .claude/        — credentials, settings and this box's conversation history
#   .codex/         — the same, for the other runtime
#   .config/sync/   — the tracker identity; sharing it makes two boxes claim work as ONE agent
private_paths=(".claude.json" ".claude" ".codex" ".config/sync")

binds=()
for rel in "${private_paths[@]}"; do
  mine="$home/$rel"
  if [ ! -e "$mine" ]; then
    mkdir -p "$(dirname "$mine")" || exit 1
    # Seed from the sandbox's own copy so the box starts with a logged-in agent; an absent source
    # (a tracker identity skein has not provisioned yet) simply starts empty.
    if [ -e "$HOME/$rel" ]; then
      cp -a "$HOME/$rel" "$mine" 2>/dev/null || { echo "skein: could not seed $rel for $box" >&2; exit 1; }
    else
      mkdir -p "$mine" || exit 1
    fi
  fi
  binds+=(--bind "$mine" "$HOME/$rel")
done

# Starting a box is not the same as adding a session to one. This creates the namespace, so running
# it twice would build a SECOND namespace and server for the same box: the new server takes over the
# socket, the anchor pid moves to it, and the first namespace is stranded with no way left to address
# it. So the test is whether ANY server answers, not whether this session exists — a later runtime
# adds `skein-agent-<runtime>` through the running server, which places it in the namespace for free.
if tmux -S "$sock" ls >/dev/null 2>&1; then
  echo "skein: box $box is already running; add a session with: tmux -S $sock new-session -d -s $session …" >&2
  exit 4
fi
# Only now is unlinking safe: kill-server leaves the socket behind, so a socket nothing answers on is
# a dead box's litter. Unlinking one that IS answering is what strands a namespace.
rm -f "$sock"

# --dev-bind / / keeps the sandbox's own filesystem visible (the repo, the toolchains, the store
# mount) and then binds the box's private directories over the two paths that must not be shared.
# No --unshare-pid: the pid recorded below has to be the pid skein sees from outside, or nsenter has
# nothing to address.
exec bwrap \
  --dev-bind / / \
  --bind "$tmp" /tmp \
  "${binds[@]}" \
  -- \
  bash -lc '
    session="$1"; sock="$2"; pidfile="$3"; tree="$4"; shift 4
    # $TMUX is inherited from whatever session started skein, and when it is set tmux takes the
    # socket path from it VERBATIM instead of computing one and creating its parent directory. That
    # path names the OUTER /tmp, which does not exist in this box private one, so the server fails
    # to start — while the client still exits 0. Silent, and it reads exactly like a namespace
    # restriction. Unset it: this box session is not nested inside anything.
    unset TMUX TMUX_TMPDIR
    # cd here rather than relying on bwrap --chdir: this is a LOGIN shell, and sourcing the
    # profile can move it. Observed doing exactly that — the box started at / instead of its tree.
    cd "$tree" || exit 1
    tmux -S "$sock" new-session -d -s "$session" -- "$@" || exit 1
    # The anchor. A namespace lives as long as some process is in it, and the tmux server
    # double-forks away from this shell — so the shell exits while the box keeps running, and its
    # pid would name a corpse. The server is the right anchor on its own terms: it is in the
    # namespace, it lives exactly as long as the box does, and skein already treats it as the box life.
    # Box alive <=> server alive <=> namespace joinable, and kill-server drops the last process in
    # the namespace, which frees it.
    tmux -S "$sock" display -p "#{pid}" > "$pidfile"
  ' bash "$session" "$sock" "$pidfile" "$tree" "$@"
