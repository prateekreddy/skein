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

# Private by DEFAULT, with a short list of deliberate escapes.
#
# The earlier shape was the other way round — share $HOME, bind over the paths known to matter — and
# that is unsafe for a reason no list can fix: an agent harness keeps state wherever it likes, and
# anything unanticipated was silently SHARED, so two boxes corrupt each other quietly. This way
# round, something unanticipated is merely private: it costs a re-download, not an identity two
# boxes both claim work under.
#
# (A copy-on-write overlay would be the honest version of this, and it is unavailable here: the
# sandbox root is ITSELF overlayfs, and overlayfs refuses an overlayfs upperdir. Measured, not
# assumed — bwrap 0.11.1 and this kernel both support it fine.)
#
# Seeded into the box on first start and diverging from there: credentials, per-box conversation
# history, and the MCP registration that points each box at its own repo's work-tracking gateway.
seed_paths=(".claude" ".claude.json" ".codex" ".gitconfig" ".bashrc" ".profile")
# Bound back through, genuinely shared. ~/.local carries the agent CLIs themselves — software, not
# state, and 547M of it, so copying it per box would cost gigabytes to isolate binaries every box
# wants identical. Conversation state lives in ~/.claude, which IS seeded. The rest are package
# caches no box needs its own copy of.
#
# NOT ~/shared, though it is the most obviously shared thing here. It is scoped to a REPO, not to a
# sandbox — the two were the same object when a box was a sandbox, and this is where they come apart:
# one fleet sandbox hosts boxes from many repos, so binding its copy through would hand all of them
# one `shared` and quietly cross project boundaries. Each box gets its own instead, created during
# provisioning by shared-home.sh as a symlink into that box's repo store — which is host-mounted, so
# it stays live across boxes of the SAME repo, which is what `shared` has always meant. Binding it
# also broke provisioning outright: shared-home.sh refuses to replace a real path, and it gates
# startup, so every fleet box would have failed to come up.
share_paths=(".local" ".cargo" ".rustup" ".npm")

for rel in "${seed_paths[@]}"; do
  mine="$home/$rel"
  [ -e "$mine" ] && continue
  [ -e "$HOME/$rel" ] || continue
  mkdir -p "$(dirname "$mine")" || exit 1
  cp -a "$HOME/$rel" "$mine" 2>/dev/null || { echo "skein: could not seed $rel for $box" >&2; exit 1; }
done

# $HOME first, then the shared escapes ON TOP of it. bwrap resolves every source against the
# ORIGINAL filesystem, so these still name the sandbox's real directories even though each
# destination now sits inside the box's private HOME — a symlink could not do this, because the
# path it would point at is the one being shadowed.
binds=(--bind "$home" "$HOME")
for rel in "${share_paths[@]}"; do
  [ -e "$HOME/$rel" ] && binds+=(--bind "$HOME/$rel" "$HOME/$rel")
done
# /var/tmp is world-writable and a plausible scratch path, so it is private too — but only when the
# box root is not itself under it, or this would hide the box's own tree and socket from inside.
# (Caught exactly that while testing from /var/tmp; the real layout is /boxes.)
case "$root" in
  /var/tmp/*) ;;
  *) mkdir -p "$root/vartmp" || exit 1; binds+=(--bind "$root/vartmp" /var/tmp) ;;
esac

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
