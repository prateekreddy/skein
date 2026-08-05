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
# usage: [SKEIN_FLEET_LIMITS=…] box-session.sh <box> <root> <pidfile> <session> <state> <limits> <cmd…>
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

box="${1:?usage: box-session.sh <box> <root> <pidfile> <session> <state> <cmd…>}"
root="${2:?missing box root}"
pidfile="${3:?missing pidfile}"
session="${4:?missing session name}"
# The box's durable state on the HOST (a mounted path). Required rather than optional: if skein
# forgot to pass it the box would still start, and the only symptom would be a conversation that
# vanishes the next time the sandbox dies — the silent-degradation shape this file exists to avoid.
state="${5:?missing host state dir}"
# cgroup limits as key=value pairs, e.g. "max=18G,high=14G,pids=8192". Present but may be empty —
# skein always passes it, and empty means it could not work out a ceiling rather than that it wants
# none, which is why the warning below fires either way.
limits="${6-}"
shift 6
# The ceilings on everything that is NOT one box, as `<cgroup>=<max>/<high>` pairs, e.g.
# "skein=15975M/14377M,docker=7987M/7188M".
#
# In the environment rather than a seventh positional so that skein and this script can be updated
# independently: a sandbox keeps whichever copy of this file was installed when it was built, and an
# argument it did not expect would be read as part of the agent command — every box restart failing
# until something reinstalled the launcher. Unset here so it does not follow the agent into the box.
fleet_limits="${SKEIN_FLEET_LIMITS-}"
unset SKEIN_FLEET_LIMITS
[ "$#" -gt 0 ] || { echo "skein: no command to run" >&2; exit 2; }

case "$box" in
  */*|*..*|"") echo "skein: refusing box name: $box" >&2; exit 2 ;;
esac

for p in "$root" "$pidfile" "$state"; do
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

# Credentials seed downward like everything else, but they also flow BACK — the only state here that
# does. Seeding alone is a one-way copy on first start, which answers "log in once" for a box that
# has never run and for nothing else: a login done inside one box would stay there, and a refresh
# that rotates the token would leave the sandbox's copy dead, so every box created afterwards would
# start logged out and need its own login. That is the opposite of once.
#
# Newest wins, in whichever direction. A login anywhere becomes the seed for every box started after
# it. Safe because every box here is the same person — boxes are isolated from each other's *state*,
# not from each other's identity — and the file is written whole, so there is no half-copied token.
for rel in ".claude/.credentials.json" ".codex/auth.json"; do
  mine="$home/$rel"; canon="$HOME/$rel"
  if [ -e "$mine" ] && { [ ! -e "$canon" ] || [ "$mine" -nt "$canon" ]; }; then
    mkdir -p "$(dirname "$canon")" 2>/dev/null \
      && cp -p "$mine" "$canon" 2>/dev/null && chmod 600 "$canon" 2>/dev/null
  elif [ -e "$canon" ] && { [ ! -e "$mine" ] || [ "$canon" -nt "$mine" ]; }; then
    mkdir -p "$(dirname "$mine")" 2>/dev/null \
      && cp -p "$canon" "$mine" 2>/dev/null && chmod 600 "$mine" 2>/dev/null
  fi
done

# $HOME first, then the shared escapes ON TOP of it. bwrap resolves every source against the
# ORIGINAL filesystem, so these still name the sandbox's real directories even though each
# destination now sits inside the box's private HOME — a symlink could not do this, because the
# path it would point at is the one being shadowed.
binds=(--bind "$home" "$HOME")
for rel in "${share_paths[@]}"; do
  [ -e "$HOME/$rel" ] && binds+=(--bind "$HOME/$rel" "$HOME/$rel")
done
# The conversation lives on the HOST, not in this VM.
#
# Everything else here is about isolating boxes from each other; this is about surviving the sandbox
# itself. A transcript under the box's private HOME is VM-local, so it dies whenever the sandbox
# does — and a snapshot only rescues it on a *planned* resize. An OOM, a crash, or an `sbx rm` by
# hand never runs one, and the conversation is simply gone. Bound from a mounted host path it
# survives all of those, and the cockpit can read it directly instead of shelling into the box —
# which also means a STOPPED box still has a readable conversation.
#
# Only the record, never the credentials: `.credentials.json` and the rest of ~/.claude stay in the
# box's private HOME, seeded from the sandbox. Naming the two record directories rather than
# host-mounting ~/.claude wholesale is the same allowlist rule applied to durability instead of
# privacy — anything unanticipated stays VM-local rather than landing on the host by default.
#
# The cost is honest: these paths are virtiofs (~5× slower to write, ~14× to read than VM-local), and
# a resume reads the whole file. Appends per turn are small; a slower resume is worth a conversation
# that cannot be lost.
for pair in ".claude/projects:claude-projects" ".codex/sessions:codex-sessions"; do
  rel="${pair%%:*}"; host="$state/${pair##*:}"
  mkdir -p "$host" || exit 1
  chmod 700 "$host" 2>/dev/null || true
  binds+=(--bind "$host" "$HOME/$rel")
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

# The box's ceiling, applied HERE — before exec'ing bwrap — for two reasons that both matter.
#
# Privilege: this is still the sandbox, where sudo works. Inside bwrap the box is in an unprivileged
# user namespace and a setuid sudo has nothing to escalate to, so a cgroup written from in there is
# not an option at all.
#
# Inheritance: a process's children start in its cgroup, so putting THIS shell in it means the tmux
# server, the agent, and every compiler it forks are all inside the same ceiling. Moving the anchor
# pid afterwards would move one process and leave its existing children outside — a limit that looks
# applied and holds nothing.
#
# Memory only, and on purpose. CPU stays uncapped: cpu.weight is already equal per box, so they
# fair-share under contention while a lone box still gets every core — and a cap would idle cores
# while a box waits, which is the waste this design exists to end. Memory cannot be shared that way:
# two boxes wanting 20G do not each get 13 slowly, they hit the wall and the kernel starts killing
# processes — as readily another box's agent as the guilty one.
# The ceilings on everything that is not one box — and the ones that actually keep the sandbox
# answering, because a per-box ceiling cannot bound a sum and does not reach inside Docker.
#
# `skein` is the parent of every box's cgroup, so it is the only place the boxes *together* can be
# bounded. `docker` is where a box's `docker build` or `docker compose up` really runs: the sandbox
# has ONE Docker daemon shared by every box, dockerd places its containers under
# /sys/fs/cgroup/docker, and nothing written on a box's own cgroup reaches them.
#
# Why it matters more than the per-box limit: there is no swap here, so reaching the VM's memory is
# not a slowdown, it is the kernel's global OOM killer picking a victim by badness rather than by
# blame — as readily whatever answers the host as the build that caused it. That is a sandbox that
# stops responding until it is cycled. Bounded cgroups turn the same overshoot into an OOM inside
# the guilty one, which kills a build.
#
# Written on every box start, not once: dockerd recreates its cgroup when the sandbox cycles, and
# takes any limit written on it along too.
apply_fleet_ceilings() {
  [ -n "$fleet_limits" ] || return 0
  for pair in $(printf '%s' "$fleet_limits" | tr ',' ' '); do
    dir="/sys/fs/cgroup/${pair%%=*}"
    spec="${pair#*=}"
    # Only ever narrow what is already there. Creating `docker` ourselves would hand dockerd a
    # cgroup it did not make and expects to own; a sandbox with no inner Docker simply has none.
    [ -d "$dir" ] || continue
    # `high` before `max`, because these are written over a cgroup that may already be busy: the
    # soft limit makes the kernel reclaim, so the hard one lands on a cgroup that has just given
    # back its page cache rather than on one still over the line, which would be killed on the spot.
    sudo sh -c 'echo "$1" > "$2"' _ "${spec##*/}" "$dir/memory.high" 2>/dev/null || true
    sudo sh -c 'echo "$1" > "$2"' _ "${spec%%/*}" "$dir/memory.max"  2>/dev/null || true
  done
}

if [ -n "$limits" ]; then
  cgroup_root="/sys/fs/cgroup/skein"
  cg="$cgroup_root/$box"
  # A controller is only available in a child if the PARENT delegates it, so the order is: make the
  # parent, delegate, then make the leaf. Processes live only in the leaf — cgroup v2 forbids a
  # cgroup having both children and processes.
  if sudo mkdir -p "$cgroup_root" 2>/dev/null \
    && sudo sh -c 'echo "+memory +pids" > '"$cgroup_root"'/cgroup.subtree_control' 2>/dev/null \
    && sudo mkdir -p "$cg" 2>/dev/null; then
    apply_fleet_ceilings
    for kv in $(printf '%s' "$limits" | tr ',' ' '); do
      case "$kv" in
        max=*)  sudo sh -c 'echo "$1" > "$2"' _ "${kv#max=}"  "$cg/memory.max"  2>/dev/null || true ;;
        high=*) sudo sh -c 'echo "$1" > "$2"' _ "${kv#high=}" "$cg/memory.high" 2>/dev/null || true ;;
        pids=*) sudo sh -c 'echo "$1" > "$2"' _ "${kv#pids=}" "$cg/pids.max"    2>/dev/null || true ;;
        *) echo "skein: ignoring unknown limit $kv for $box" >&2 ;;
      esac
    done
    if sudo sh -c 'echo $1 > "$2"' _ "$$" "$cg/cgroup.procs" 2>/dev/null; then
      # Recorded, not just logged. skein reads a command's stdout and drops its stderr on success,
      # so a warning here would vanish exactly when nothing looked wrong — and "this box has no
      # ceiling" is a fact worth still being true tomorrow, not a line in one launch's output.
      printf 'capped %s\n' "$limits" > "$root/limits.state"
    else
      printf 'uncapped could-not-join-cgroup\n' > "$root/limits.state"
      echo "skein: $box could not join its cgroup; it runs without a memory ceiling" >&2
    fi
  else
    printf 'uncapped no-cgroup-delegation\n' > "$root/limits.state"
    # Not fatal: an uncapped box still works, and refusing to start one because the image lacks
    # cgroup delegation would be a worse trade. Loud, though — this is the guard that keeps one
    # box's runaway build from killing every other box in the sandbox.
    echo "skein: no cgroup delegation in this sandbox; $box runs WITHOUT a memory ceiling, so a runaway build in it can take the whole fleet down" >&2
  fi
else
  printf 'uncapped no-limit-computed\n' > "$root/limits.state"
  echo "skein: no memory ceiling computed for $box; it runs uncapped" >&2
fi

# A login the user made must beat the placeholder key the sandbox ships with.
#
# sbx puts `ANTHROPIC_API_KEY=proxy-managed` (and the OpenAI equivalent) in the environment of every
# non-agent image, for the proxy to substitute a real key into. With no such secret configured the
# placeholder is simply an invalid key — and the runtimes prefer an API key over a stored login, so
# an agent in a box failed with "Invalid API key" while a perfectly good credential sat unused
# beside it. Measured both ways in a real box: with the variable set, `claude -p` fails; with it
# unset, the same box answers.
#
# Conditional on the credential existing, so a fleet that genuinely runs on API keys is untouched:
# no login, no unset, and the proxy path works exactly as before.
[ -s "$home/.claude/.credentials.json" ] && unset ANTHROPIC_API_KEY
[ -s "$home/.codex/auth.json" ] && unset OPENAI_API_KEY

# Who this box is, for everything that runs inside it.
#
# The probes record their signals under an identity, and every one of them used to read
# `SANDBOX_VM_ID`. That names the VM. It was also the box name for as long as a box WAS a sandbox,
# and the moment several boxes share one it stops being an identity at all: every box in this
# sandbox reports the same string, so they all write one another's status file and the board, which
# looks for each box by name, finds nothing and calls them all stale. Measured on the first migrated
# box — it showed `stale` on the board while visibly working.
#
# Exported here because this script is the only place that knows the answer: the agent, its hooks
# and every process they fork are all children of the tmux server started below, so one export
# reaches all of them. The sandbox cannot tell them apart, and a box cannot be asked to work out its
# own name from a path without guessing at a layout.
export SKEIN_BOX="$box"

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
