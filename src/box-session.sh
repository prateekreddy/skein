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

# The ceilings on everything that is not one box — and the ones that actually keep the sandbox
# answering, because a per-box ceiling cannot bound a sum and does not reach inside Docker.
#
# `skein` is the parent of every box's cgroup, so it is the only place the boxes *together* can be
# bounded. That is the ceiling worth having: there is no swap here, so reaching the VM's memory is
# not a slowdown, it is the kernel's global OOM killer picking a victim by badness rather than by
# blame — as readily whatever answers the host as the build that caused it. That is a sandbox that
# stops responding until it is cycled. A bounded cgroup turns the same overshoot into an OOM inside
# the guilty one, which kills a build.
#
# `docker` arrives here too, but as `max/max` — see `fleet_limits` in fleet.rs for why it is named
# only in order to be left uncapped. Briefly: that cgroup is not just a box's `docker build`, it
# also holds the sandbox's own init, socat and dockerd, so a ceiling there throttles or kills the
# machinery that answers `sbx exec` rather than the workload that overshot.
#
# Written on every box start, not once: dockerd recreates its cgroup when the sandbox cycles, and
# takes any limit written on it along too.
#
# The numbers arrive as a share of the CONFIGURED fleet size, and this scales them to the memory the
# kernel here actually reports. The two are not always the same: sbx fixes a sandbox's memory when
# it is created, so editing Fleet memory in the cockpit without rebuilding leaves the config
# describing a VM that does not exist — and a ceiling worked out for a machine twice this size is
# not a ceiling. Only ever downward: a sandbox with MORE memory than skein was told about keeps the
# reserve skein intended rather than being handed the surplus nobody asked for.
# The cgroup dockerd is told to put containers in — a CHILD of skein, so the one ceiling on skein
# covers the boxes and the containers they start together. That nesting is the whole point: the two
# share a pool taken first-come rather than holding a reservation each.
#
# Made here rather than left to runc, and made even when no box has started yet, because the
# controller has to be delegated down the chain before a container lands in it: a cgroup whose
# parent has not been handed `memory` cannot have a memory limit, and the container would silently
# be the one thing in the fleet nothing bounds. runc does enable controllers up the path it creates,
# so this is belt to its braces — and it costs two mkdirs on a path that already exists.
#
# `containers` is a name no box can collide with: box names come from git branches, and skein's own
# validation rejects a name that is not a single path component, so nothing else creates it here.
ensure_container_cgroup() {
  # Make each level, THEN delegate it — a controller can only be handed to a cgroup that exists, and
  # only by a parent that was handed it first. Doing these in the wrong order leaves `containers`
  # there but with no memory controller in it, which is worse than not making it at all: dockerd
  # would place containers in a cgroup that cannot hold a limit.
  for c in /sys/fs/cgroup/skein /sys/fs/cgroup/skein/containers; do
    sudo mkdir -p "$c" 2>/dev/null || return 0
    sudo sh -c 'echo "+memory +pids" > "$1/cgroup.subtree_control"' _ "$c" 2>/dev/null || true
  done
}

apply_fleet_ceilings() {
  [ -n "${fleet_limits:-}" ] || return 0
  ensure_container_cgroup
  planned=""
  for pair in $(printf '%s' "$fleet_limits" | tr ',' ' '); do
    case "$pair" in total=*) planned="${pair#total=}"; planned="${planned%M}" ;; esac
  done
  actual="$(awk '/^MemTotal:/ {print int($2/1024); exit}' /proc/meminfo 2>/dev/null)"
  scale=""
  if [ -n "$planned" ] && [ -n "$actual" ] && [ "$actual" -lt "$planned" ] 2>/dev/null; then
    scale="yes"
    echo "skein: this sandbox has ${actual}M, not the ${planned}M skein is configured for; scaling the shared ceilings to fit" >&2
  fi
  for pair in $(printf '%s' "$fleet_limits" | tr ',' ' '); do
    case "$pair" in total=*) continue ;; esac
    dir="/sys/fs/cgroup/${pair%%=*}"
    spec="${pair#*=}"
    # Only ever narrow what is already there. Creating `docker` ourselves would hand dockerd a
    # cgroup it did not make and expects to own; a sandbox with no inner Docker simply has none.
    [ -d "$dir" ] || continue
    # Both halves are read before EITHER is written, so an unreadable spec leaves the cgroup as it
    # found it. Half a ceiling is worse than none: a `high` with no `max` above it is the shape that
    # throttles forever, which is the failure this pair of limits exists to avoid.
    #
    # A value is either `max` — a ceiling deliberately withheld, WRITTEN rather than skipped so that
    # a fleet an older skein capped has that cap taken back off — or a number of MiB, scaled below.
    # Anything else is a word from a newer skein than this launcher, and the check happens here
    # rather than inside `$(( ))` because arithmetic does not fail on a word it cannot read: under
    # `set -u` it aborts the shell. That is not hypothetical. A skein that began sending
    # `docker=max/max` to sandboxes still carrying the launcher before it took every box in the
    # fleet down — the launcher died before tmux, so each reconnect entered an anchor pid from the
    # last boot and reported `nsenter: cannot open /proc/<pid>/ns/user`, an error about namespaces
    # for a fleet that needed a file copied.
    #
    # A launcher is older than the skein driving it more often than not, because the sandbox keeps
    # whichever copy it was last given. So an unfamiliar ceiling has to be one it skips, not one it
    # dies on: skipping costs this cgroup its limit and says so, and the box still starts.
    readable=yes
    for half in "${spec##*/}" "${spec%%/*}"; do
      case "$half" in
        max) ;;
        *) case "${half%M}" in
             "" | *[!0-9]*)
               echo "skein: ignoring the ceilings on ${dir##*/}: '$half' is not a size this launcher understands — it is older than the skein that sent it" >&2
               readable=""
               ;;
           esac ;;
      esac
    done
    [ -n "$readable" ] || continue
    # `high` before `max`, because these are written over a cgroup that may already be busy: the
    # soft limit makes the kernel reclaim, so the hard one lands on a cgroup that has just given
    # back its page cache rather than on one still over the line, which would be killed on the spot.
    for want in "memory.high ${spec##*/}" "memory.max ${spec%%/*}"; do
      file="${want%% *}"
      mib="${want#* }"
      if [ "$mib" = max ]; then
        value=max          # nothing to scale: no ceiling is no ceiling on a VM of any size
      else
        mib="${mib%M}"
        [ -n "$scale" ] && mib=$(( mib * actual / planned ))
        value="${mib}M"
      fi
      sudo sh -c 'echo "$1" > "$2"' _ "$value" "$dir/$file" 2>/dev/null || true
    done
  done
}

# Where a box files a request for a system package, and where the host reads it back.
#
# The fleet root rather than the shared `.claude` store, and that distinction is the whole point: the
# store is per-repo, while one sandbox holds boxes from several repos and a system package changes
# the toolchain under every one of them. Filing a fleet-wide decision in a per-repo queue would scope
# it to whichever repo happened to ask first, and hide it from the boxes it also affects.
substrate_dir() {
  printf '%s/.skein/substrate' "${SKEIN_FLEET_ROOT:-/boxes}"
}

# Is this a name apt or npm could actually be asked for?
#
# This is the boundary that has to hold, not a politeness check. Everything filed here is eventually
# spliced into a `sudo apt-get install` that runs as ROOT in the sandbox, so a name shaped like an
# option or a path traversal must never reach the queue — approving a request must not be a way to
# get an argument of your choosing onto a privileged command line.
valid_package() {
  case "$1" in
    "" | -*) return 1 ;;          # empty, or shaped like a flag
    *..*) return 1 ;;             # traversal, however it is spelled
    *[!A-Za-z0-9._+@/-]*) return 1 ;;
  esac
  return 0
}

# `--request-package <box> <argv…>`: file what a box asked `sudo` to install.
#
# The parsing lives here rather than in the shim it is called from because a shim generated into a
# box's private namespace has nowhere to be tested from, and this is argument handling that ends at
# a root install — exactly the code that should not be written blind. `tests/substrate_request.rs`
# drives this entry point directly.
#
# Exit codes are the shim's whole vocabulary: 0 filed, 2 "not an install command, say the usual
# thing", 3 refused. Anything else is this script failing, which the shim also treats as 2.
request_package() {
  local box="$1" tool="" verb="" kind="" arg
  shift
  # sudo's own options are not the command's. Everything up to the first bare word belongs to sudo.
  while [ $# -gt 0 ]; do
    case "$1" in -*) shift ;; *) break ;; esac
  done
  tool="${1-}"
  [ $# -gt 0 ] && shift
  case "$tool" in
    apt | apt-get) kind=apt ;;
    npm) kind=npm ;;
    *) return 2 ;;
  esac
  while [ $# -gt 0 ]; do
    case "$1" in -*) shift ;; *) verb="$1"; shift; break ;; esac
  done
  case "$kind/$verb" in
    apt/install | npm/install | npm/i | npm/add) ;;
    *) return 2 ;;
  esac

  local -a packages=()
  for arg in "$@"; do
    case "$arg" in -*) continue ;; esac
    if ! valid_package "$arg"; then
      echo "skein: '$arg' is not a package name that can be asked for" >&2
      return 3
    fi
    packages+=("$arg")
  done
  [ "${#packages[@]}" -gt 0 ] || return 2

  command -v jq >/dev/null 2>&1 || return 4

  local dir want
  dir="$(substrate_dir)/requests"
  mkdir -p "$dir" 2>/dev/null || return 4
  # The identity of a request is what it would install, so the same ask from two boxes — or twice
  # from one — is one decision to make rather than a queue that grows every time a stuck agent
  # retries. Sorted, so argument order is not part of that identity.
  want="$kind $(printf '%s\n' "${packages[@]}" | LC_ALL=C sort -u | tr '\n' ' ')"

  local f state existing
  for f in "$dir"/*.json; do
    [ -f "$f" ] || continue
    state="$(jq -r '.state // ""' "$f" 2>/dev/null)" || continue
    case "$state" in pending | approved) ;; *) continue ;; esac
    existing="$(jq -r '(.kind // "") + " " + ((.packages // []) | sort | unique | join(" ")) + " "' "$f" 2>/dev/null)" || continue
    if [ "$existing" = "$want" ]; then
      printf 'skein: already asked for (%s) — request %s is %s.\n' \
        "${packages[*]}" "$(jq -r '.id // "?"' "$f")" "$state"
      return 0
    fi
  done

  local id tmp
  id="$(date -u +%Y%m%d-%H%M%S)-$$"
  tmp="$(mktemp "$dir/.tmp.XXXXXX")" || return 4
  # `--args` puts the packages in as a JSON array of strings rather than a string that has to be
  # split again later. Nothing downstream re-parses this, which is the point.
  if ! jq -n --arg id "$id" --arg box "$box" --arg kind "$kind" \
       --arg asked "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
       '{id:$id, box:$box, kind:$kind, packages:$ARGS.positional,
         asked:$asked, state:"pending", decided:"", remember:true, log:""}' \
       --args "${packages[@]}" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    return 4
  fi
  # Named only once it is complete: the host polls this directory, and a half-written request is a
  # parse error on its side rather than a request that arrives a moment later.
  mv -f "$tmp" "$dir/$id.json" 2>/dev/null || { rm -f "$tmp"; return 4; }
  chmod 644 "$dir/$id.json" 2>/dev/null || true

  printf 'skein: asked the fleet for %s (%s). Request %s is pending approval.\n' \
    "$kind" "${packages[*]}" "$id"
  printf 'skein: it installs for every box once approved in the cockpit; nothing is installed yet.\n'
  return 0
}

if [ "${1-}" = "--request-package" ]; then
  shift
  request_package "$@"
  exit $?
fi

# Where a box asks for write access to a repository that is not its own.
#
# Beside the package queue and in the fleet root for the same reason: the decision is the fleet
# owner's, the box must be able to write the ask, and the shared `.claude` store is per-repo — which
# is precisely the wrong scope for a question *about* another repo.
gitgate_dir() {
  printf '%s/.skein/gitgate' "${SKEIN_FLEET_ROOT:-/boxes}"
}

# Is this `owner/name`, and nothing else?
#
# The slug reaches a URL and a token request on the host, so a `..` or a slash too many must never
# reach the queue. Mirrors `gitgate::slug_is_nameable`, and is checked again on that side — the queue
# is a directory any box can write to, so what arrived here proves nothing about what wrote it.
valid_slug() {
  case "$1" in
    */*/*) return 1 ;;
    */*) ;;
    *) return 1 ;;
  esac
  local owner="${1%%/*}" name="${1#*/}"
  case "$owner" in '' | -* | *[!A-Za-z0-9._-]*) return 1 ;; esac
  case "$name" in '' | -* | *[!A-Za-z0-9._-]*) return 1 ;; esac
  return 0
}

# `--request-write <box> <owner/name> [reason…]`: ask for write access to another repository.
#
# Filing is all this does. Nothing here grants anything, and nothing here needs to: the box holds a
# token scoped to its own repo and a read-only one for everything else, so a push elsewhere is
# refused by GitHub whatever this queue says. The request exists so the refusal has somewhere to go.
request_write() {
  # Defaulted rather than indexed directly: `set -u` is on, and an agent that types this with an
  # argument missing would abort the shell it ran in rather than be told what it forgot.
  local box="${1-}" repo="${2-}"
  if [ -z "$box" ] || [ -z "$repo" ]; then
    echo "usage: box-session.sh --request-write <box> <owner/name> [reason…]" >&2
    return 4
  fi
  shift 2
  local reason="$*"

  if ! valid_slug "$repo"; then
    echo "skein: '$repo' is not a repository name (expected owner/name)" >&2
    return 3
  fi
  command -v jq >/dev/null 2>&1 || return 4

  local dir
  dir="$(gitgate_dir)/requests"
  mkdir -p "$dir" 2>/dev/null || return 4

  # One pending ask per box and repo. A stuck agent retrying a push must not grow the queue by one
  # decision per attempt — it is the same decision every time.
  local f state existing
  for f in "$dir"/*.json; do
    [ -f "$f" ] || continue
    state="$(jq -r '.state // ""' "$f" 2>/dev/null)" || continue
    case "$state" in pending | granted) ;; *) continue ;; esac
    existing="$(jq -r '(.box // "") + " " + (.repo // "")' "$f" 2>/dev/null)" || continue
    if [ "$existing" = "$box $repo" ]; then
      printf 'skein: already asked to write %s — request %s is %s.\n' \
        "$repo" "$(jq -r '.id // "?"' "$f")" "$state"
      return 0
    fi
  done

  local id tmp
  id="$(date -u +%Y%m%d-%H%M%S)-$$"
  tmp="$(mktemp "$dir/.tmp.XXXXXX")" || return 4
  if ! jq -n --arg id "$id" --arg box "$box" --arg repo "$repo" --arg reason "$reason" \
       --arg asked "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
       '{id:$id, box:$box, repo:$repo, reason:$reason,
         asked:$asked, state:"pending", decided:""}' >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    return 4
  fi
  # Named only once complete: the host polls this directory, and a half-written file is a parse
  # error on its side rather than a request that turns up a moment later.
  mv -f "$tmp" "$dir/$id.json" 2>/dev/null || { rm -f "$tmp"; return 4; }
  chmod 644 "$dir/$id.json" 2>/dev/null || true

  printf 'skein: asked to write %s. Request %s is pending approval in the cockpit.\n' "$repo" "$id"
  return 0
}

if [ "${1-}" = "--request-write" ]; then
  shift
  request_write "$@"
  exit $?
fi

# `--ceilings`: apply the shared ceilings and stop, starting nothing. A cgroup limit is live, so
# changing Fleet memory in the cockpit takes effect without restarting a box — and this is the whole
# of what "apply now" means. skein calls back into this file for it rather than carrying a second
# copy of the logic, because the half that matters (scaling to the memory the VM really has) can
# only be done from in here.
if [ "${1-}" = "--ceilings" ]; then
  fleet_limits="${SKEIN_FLEET_LIMITS-}"
  apply_fleet_ceilings
  exit 0
fi


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

# And one that is deliberately STATE, which every other entry above is not.
#
# Claude Code ships session-to-session messaging: `ListAgents` finds the other sessions, `SendMessage`
# talks to them. In this fleet the TRANSPORT is already shared and always was — every session's inbox
# socket lands in `/run/user/1000/cc-socks/`, which belongs to the sandbox, and nine of them were
# visible from inside one box when this was measured. What was private is the DISCOVERY: a session
# registers itself in `~/.claude/sessions/<pid>.json`, `.claude` is seeded per box, so each box could
# see only itself and `ListAgents` answered "no reachable agents" over a directory of live sockets.
#
# Sharing this one directory — not `.claude`, which holds the credentials and the conversation
# history that must stay per box — is the whole fix, and it costs no skein code: box-to-box messaging
# becomes a feature of the runtime rather than a thing skein carries.
#
# Safe to share by construction, which is worth stating because "shared state" is what the block
# above exists to prevent. The files are keyed by PID; every box's agent runs in the sandbox's single
# PID namespace (that is why one socket directory holds them all), so the names cannot collide and
# liveness checks against a pid mean what they say. A stale entry is a dead pid, which is exactly
# what the runtime already prunes.
mkdir -p "$HOME/.claude/sessions" 2>/dev/null || true
share_paths+=(".claude/sessions")

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
#
# But a credentials file is not the same thing as a login, and that is what "newest" cannot see. A
# logged-out agent leaves the file exactly where it was with its tokens blanked — same shape, same
# keys, empty strings — and that husk is NEWER than the working copy it replaced. Newest-wins then
# propagates the logout: the husk flows up on the next box start, every box created afterwards seeds
# from it, and every box that starts pulls it back down over a login that was fine. One logged-out
# box takes the fleet with it, which is precisely the "log in to each box separately" this exists to
# prevent. Observed in the fleet: boxes holding `"accessToken": ""` with the structure intact.
#
# So a file only competes if it carries a login. One that does not never overwrites one that does,
# in either direction, whatever the mtimes say. A logout therefore stays where it happened, and a
# login anywhere still heals everything started after it.
#
# And between two that do, mtime is still the wrong question. It says when a file was WRITTEN; the
# thing being compared is which CREDENTIAL is better, and the answer is in the file — `expiresAt`.
# The two come apart exactly where it hurts: a box that starts rewrites its copy and so holds the
# newer mtime whether or not its token is the older one, and a valid login is then replaced by a
# stale one. Found in the live fleet as boxes sitting on tokens that expired days ago.
#
# Ordering by expiry can only ever prefer the credential that lives longer, so unlike recency it
# cannot cost you a working login. mtime remains the tiebreak for shapes that record no expiry at
# all (a codex `auth.json`), where there is genuinely nothing better to go on.
#
# Named blocks rather than "a non-empty token anywhere": this same file also carries per-repo MCP
# OAuth, and an MCP token says nothing about whether the agent itself is signed in.
#
# Prints the epoch-ms this login stops working, or 0 when it carries one that records no expiry.
# Prints nothing, and fails, when the file carries no login at all — which is the husk test.
login_life() {
  [ -s "$1" ] || return 1
  python3 - "$1" 2>/dev/null <<'PY'
import json, sys
try:
    data = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
if not isinstance(data, dict):
    sys.exit(1)
KEYS = ("accessToken", "refreshToken", "access_token", "refresh_token", "OPENAI_API_KEY")
found, best = False, 0
# `data` itself for the flat shapes; the named blocks for the nested ones. Never mcpOAuth.
for block in (data.get("claudeAiOauth"), data.get("tokens"), data):
    if not isinstance(block, dict):
        continue
    if not any(str(block.get(k) or "").strip() for k in KEYS):
        continue
    found = True
    for k in ("expiresAt", "expires_at", "expiry"):
        v = block.get(k)
        # `bool` is an int in Python and `True` would read as expiry 1 — a login dated 1970.
        if isinstance(v, (int, float)) and not isinstance(v, bool) and v > 0:
            best = max(best, int(v))
            break
if not found:
    sys.exit(1)
print(best)
PY
}

# Is the first login better than the second? Longer-lived wins; equal expiry (or none recorded on
# either side) falls back to the mtime tiebreak this rule used to apply to everything.
better_login() { # $1 $2 = expiries, $3 $4 = the files they came from
  [ "$1" -gt "$2" ] && return 0
  [ "$1" -lt "$2" ] && return 1
  [ "$3" -nt "$4" ]
}

# Only the login moves, and it is merged rather than copied over.
#
# `.credentials.json` is not just the agent's login: it also holds an `mcpOAuth` block per MCP
# server, keyed by name and a hash of the server's URL. Those are per-repo — a box's work-tracking
# gateway is its repo's, which is the same reason `~/.claude.json` is kept private — so a whole-file
# copy hands one box's MCP grants to another and, worse, DISCARDS whatever the receiving box had.
# Not merged, replaced: a box's own gateway token disappears the first time any other box wins the
# recency race, and the symptom is an MCP server that asks to be authorised again for no reason.
#
# Today every box happens to point at the same gateway, so nothing visibly breaks. The second repo
# with its own gateway is when it would — silently, and a long way from this file.
#
# So: take the login keys, leave everything else in the destination alone. A `null` is skipped for
# the same reason a logout is: an absent credential must never overwrite a present one. The
# destination keeps the source's mtime, as `cp -p` gave it, or the two would take turns being
# "newer" and copy back and forth on every start.
merge_login() {
  python3 - "$1" "$2" 2>/dev/null <<'PY'
import json, os, sys, tempfile
src, dst = sys.argv[1], sys.argv[2]
# The whole of a codex auth.json, and only the agent's block of a claude credentials file.
LOGIN = ("claudeAiOauth", "tokens", "OPENAI_API_KEY", "last_refresh")
try:
    incoming = json.load(open(src))
except Exception:
    sys.exit(1)
if not isinstance(incoming, dict):
    sys.exit(1)
try:
    merged = json.load(open(dst))
    if not isinstance(merged, dict):
        merged = {}
except Exception:
    merged = {}
moved = False
for key in LOGIN:
    if incoming.get(key) is not None:
        merged[key] = incoming[key]
        moved = True
if not moved:
    sys.exit(1)
where = os.path.dirname(dst) or "."
os.makedirs(where, exist_ok=True)
handle, temp = tempfile.mkstemp(dir=where)
try:
    with os.fdopen(handle, "w") as out:
        json.dump(merged, out)
    os.chmod(temp, 0o600)
    os.replace(temp, dst)
except Exception:
    try:
        os.unlink(temp)
    except OSError:
        pass
    sys.exit(1)
stat = os.stat(src)
os.utime(dst, (stat.st_atime, stat.st_mtime))
PY
}

# Said once and out loud, because the alternative is a fleet that quietly stops sharing logins. The
# degradation is deliberate: with no way to tell a login from a husk, nothing propagates at all —
# which costs a login per box, where guessing would cost the fleet its credentials.
command -v python3 >/dev/null 2>&1 \
  || echo "skein: no python3 here, so logins cannot be shared between boxes; each will need its own" >&2

for rel in ".claude/.credentials.json" ".codex/auth.json"; do
  mine="$home/$rel"; canon="$HOME/$rel"
  mine_life="$(login_life "$mine")" || mine_life=""
  canon_life="$(login_life "$canon")" || canon_life=""
  if [ -n "$mine_life" ] && { [ -z "$canon_life" ] || better_login "$mine_life" "$canon_life" "$mine" "$canon"; }; then
    merge_login "$mine" "$canon"
  elif [ -n "$canon_life" ] && { [ -z "$mine_life" ] || better_login "$canon_life" "$mine_life" "$canon" "$mine"; }; then
    merge_login "$canon" "$mine"
  fi
done

# Deliver box-to-box messages instead of holding them for approval — and do it in the BOX's own
# settings, which is user scope.
#
# This was written into the project store first, and that is the wrong scope for it. A repository's
# settings can only ever TIGHTEN this: "your own 'accept' cannot override a repo tightening", in
# Claude Code's own words, so `accept` there grants nothing at all. Measured the expensive way — a
# real message to a fleet box expired unapproved while the store cheerfully said accept.
#
# It is the sibling of sharing `~/.claude/sessions/` above: that makes a box findable, this makes it
# answerable. Without both, a message is delivered to a session that never sees it, and the sender is
# told it arrived — the silent loss the mailbox exists to avoid.
#
# Only when absent, and never over an unparseable file: a box whose user has taken a position on this
# keeps it, including a deliberate `hold`.
if command -v python3 >/dev/null 2>&1; then
  python3 - "$home/.claude/settings.json" 2>/dev/null <<'PY' || true
import json, os, sys, tempfile
p = sys.argv[1]
try:
    with open(p) as f:
        data = json.load(f)
    if not isinstance(data, dict):
        sys.exit(0)
except FileNotFoundError:
    data = {}
except Exception:
    sys.exit(0)
if "crossSessionInbound" in data:
    sys.exit(0)
data["crossSessionInbound"] = "accept"
d = os.path.dirname(p) or "."
os.makedirs(d, exist_ok=True)
fd, tmp = tempfile.mkstemp(dir=d)
try:
    with os.fdopen(fd, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    os.replace(tmp, p)
except Exception:
    try:
        os.unlink(tmp)
    except OSError:
        pass
PY
fi

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

# `sudo` cannot work in here, and the way it fails is worse than it not existing: the real one says
# "/etc/sudo.conf is owned by uid 65534, should be 0", which is true, unfixable, and explains
# nothing. An agent that reads that tries to chown it, retries, and eventually abandons the task.
#
# Unfixable because only this box's uid is mapped into its user namespace, so every root-owned file
# reads as `nobody` — and even with the ownership check satisfied, setuid cannot grant uid 0 inside
# a namespace you created yourself. There is no configuration that makes it work.
#
# It is also the right behaviour, which is the part the message has to convey: the boxes share one
# filesystem, so an `apt-get install` in here would change the toolchain under every other box in
# the fleet. System packages belong to the substrate, installed once for all of them.
#
# Bound only inside this namespace. The sandbox's own sudo is untouched and must be — `ensure_substrate`
# uses it to install the substrate this message tells people to ask for, and `ensure_container_cgroup`
# above runs it on every box start.
#
# Bound over the RESOLVED binary, never over `sudo` as PATH spells it. On Debian that name is a
# symlink chain (/usr/bin/sudo → /etc/alternatives/sudo → /usr/bin/sudo.ws), and bwrap cannot mount
# a file onto a symlink: it tries to create the destination instead, fails on a /usr/bin no
# unprivileged user can write, and takes the whole box start down with
# `bwrap: Can't create file at /usr/bin/sudo`. Found by `a_box_lives_and_dies_inside_the_fleet_sandbox`,
# which is exactly the trade this shim must never make — a worse error message is a nuisance, a box
# that will not start is not.
#
# So every step is a reason to skip rather than to fail: no sudo, a chain that does not resolve, a
# target that is not a regular file. Skipping costs the old cryptic message; failing costs the box.
sudo_real=$(command -v sudo 2>/dev/null || true)
[ -n "$sudo_real" ] && sudo_real=$(readlink -e "$sudo_real" 2>/dev/null || true)
if [ -n "$sudo_real" ] && [ -f "$sudo_real" ]; then
  mkdir -p "$root/bin" || exit 1
  # The box name and the launcher's own path are baked in rather than read from the environment,
  # because this shim runs in whatever shell an agent happens to have: an attach shell enters the
  # namespace fresh, without $SKEIN_BOX, and a request filed under an empty box name is a request
  # nobody can answer. `%q` because a name is not guaranteed to be a bare word.
  skein_launcher="$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")"
  # `$0` is right when this was started as the launcher, and wrong the moment it is not — sourced,
  # piped, or run through a wrapper, where it resolves to `bash` in whatever the current directory
  # happens to be. The failure would be silent: the shim would find no launcher, fall back to the
  # old refusal, and asking would quietly stop working. So an unusable `$0` falls back to where the
  # launcher is actually installed, which is the same path `fleet::box_session_path` builds.
  [ -f "$skein_launcher" ] || skein_launcher="${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh"
  {
    printf '#!/bin/sh\n'
    printf 'skein_box=%q\n' "$box"
    printf 'skein_launcher=%q\n' "$skein_launcher"
    cat <<'SHIM'
# An install here cannot work — see below — but it is also the clearest statement of what someone
# wanted, so it is turned into a request for the fleet instead of only a refusal. The command still
# fails, because nothing has been installed yet; what changes is that the ask now goes somewhere.
out=''
rc=9
if [ -x "$skein_launcher" ]; then
  out=$("$skein_launcher" --request-package "$skein_box" "$@" 2>&1)
  rc=$?
fi
# 0 filed, 3 refused for the name. Both have already said the useful thing; anything else means
# this was not an install command at all, and falls through to the explanation.
case "$rc" in
  0 | 3)
    [ -n "$out" ] && printf '%s\n' "$out" >&2
    exit 1
    ;;
esac
cat >&2 <<'WHY'
skein: sudo does not work inside a box, and cannot be made to.

A box is a user namespace that maps only your own uid, so root-owned files read as `nobody` and
setuid cannot grant uid 0. The real sudo refuses with "owned by uid 65534, should be 0" — that is
the cause, and no chown or config fixes it.

It is also deliberate. Every box in this fleet shares one filesystem, so installing a system
package here would change the toolchain under all of them.

Instead:
  * Install into your own home, which needs no root:
      pip install --user X   ·   cargo install X   ·   npm i -g X (with a user prefix)
      or drop a binary in ~/.local/bin
  * If it genuinely has to be a system package, it belongs in the fleet's substrate — installed
    once for every box. Ask for it by running the install you wanted:
        sudo apt-get install <package>     sudo npm install -g <package>
    That installs nothing. It files a request for this fleet's owner to approve in the cockpit,
    and once approved the package is there for every box.
WHY
exit 1
SHIM
  } > "$root/bin/sudo" || exit 1
  chmod 755 "$root/bin/sudo" || exit 1
  binds+=(--ro-bind "$root/bin/sudo" "$sudo_real")
fi

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
if [ -n "$limits" ]; then
  cgroup_root="/sys/fs/cgroup/skein"
  cg="$cgroup_root/$box"
  # A controller is only available in a child if the PARENT delegates it, so the order is: make the
  # parent, delegate, then make the leaf. Processes live only in the leaf — cgroup v2 forbids a
  # cgroup having both children and processes.
  if sudo mkdir -p "$cgroup_root" 2>/dev/null \
    && sudo sh -c 'echo "+memory +pids" > '"$cgroup_root"'/cgroup.subtree_control' 2>/dev/null \
    && sudo mkdir -p "$cg" 2>/dev/null; then
    # In a subshell, so that no way this can fail becomes a box that will not start. The guard above
    # closes the one that bit; this closes the shape. `set -u` aborts the shell it runs in, and the
    # spec here comes from a *newer* skein than the launcher reading it, so the next token nobody
    # anticipated would do again exactly what `max` did. A subshell makes the blast radius the
    # ceilings rather than the session. Nothing downstream reads what it sets.
    ( apply_fleet_ceilings ) || echo "skein: the shared ceilings could not be applied for $box; it starts under whatever is already on those cgroups" >&2
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

# --- One box cannot see another's files ----------------------------------------------------------
#
# Measured on this fleet, from inside a box, before any of this existed:
#
#   ls /Users/you/.skein/boxes/example-box-7/claude-projects/   → readable
#
# Another box's conversation history, its checkout, and — once git scoping is on — its write tokens.
# Every box is uid 1000 and `--dev-bind / /` shows it the whole sandbox, so the files were simply
# there for the reading.
#
# **Why a mount and not a uid per box.** Separate uids were the obvious answer and are the wrong one
# here, measured rather than assumed. What one box can already reach of another through `/proc` is:
#
#   root/  cwd  environ  maps   → denied   (each box is its own user namespace)
#   cmdline  fd/           → readable (paths, no contents)
#
# So the process boundary already holds; only the filesystem was open. A mount namespace closes
# exactly that gap, needs no root at box start, no ownership migration, and no change to how skein
# attaches — the tmux socket is 0700 uid 1000, and a box running as its own uid would leave the
# cockpit unable to attach to any box in the fleet. A uid split would have cost all of that to close
# a hole a bind closes for free.
#
# **How.** `--tmpfs` over the two directories that hold every box, then bind back this box's own and
# the fleet root. It reads oddly and it is the reliable spelling: bwrap resolves every source against
# the ORIGINAL filesystem, so the binds below still name the real directories even though their
# parents are now empty — the same property `--bind "$home" "$HOME"` already relies on. Covering the
# parents rather than listing siblings also covers boxes created *after* this one starts, which an
# enumeration could not.
#
# A box keeps: its own root, its own host state, and the fleet root's scripts (read-only — the
# launcher, the credential helper and the substrate queue all live there).
if [ "${SKEIN_BOX_PRIVILEGED-}" != "1" ]; then
  fleet_root_dir="${SKEIN_FLEET_ROOT:-/boxes}"
  state_parent="$(dirname "$state")"
  binds+=(--tmpfs "$fleet_root_dir")
  [ -d "$fleet_root_dir/.skein" ] && binds+=(--ro-bind "$fleet_root_dir/.skein" "$fleet_root_dir/.skein")
  binds+=(--bind "$root" "$root")
  # The state parent is a separate mount (the host's `~/.skein/boxes`), so it needs its own cover.
  # Guarded on the two being different directories: if a fleet ever put box state inside the fleet
  # root, a tmpfs over it here would erase the bind just made above.
  if [ -d "$state_parent" ] && [ "$state_parent" != "$fleet_root_dir" ]; then
    binds+=(--tmpfs "$state_parent" --bind "$state" "$state")
  fi

  # --- and every OTHER host path the sandbox mounts ----------------------------------------------
  #
  # The two covers above are written against paths skein chose, so they could be spelled here. The
  # rest cannot: `fleet_mounts()` also binds in every repo's store and every repo's work tree, and
  # for a repo added by path those are wherever the person keeps their code. No rule written over
  # `~/.skein` reaches `/home/you/code/thing`, so the cover has to be told what to cover.
  #
  # Hence an INVERSION rather than a list of things to hide: tmpfs each mount, then bind back the
  # two paths this box is entitled to. Anything skein starts mounting later is covered the day it
  # appears, with nobody remembering to add it — which is the property a hide-list cannot have.
  #
  # What the box loses, and each of these was reachable read-write until now:
  #   * every OTHER repo's store — its memory, its mailbox, its skills, its boot records;
  #   * every other repo's work tree on the host;
  #   * its own repo's work tree read-write. `sandbox-bootstrap.sh` copies out of the source tree
  #     rather than linking at it, exactly so "a box can still never reach the host checkout" — but skein
  #     runs `git -C <repo.work>` on the HOST, so a box that could write `.git/config` there had
  #     `core.fsmonitor` executed as the host user. That was a convention holding a boundary; it is
  #     a mount option now.
  #
  # Absent variable ⇒ no cover, deliberately. A launcher already installed in a running sandbox
  # predates this and passes nothing, and the two wrong guesses are not symmetric: covering with
  # nothing bound back takes the store away from every box in that sandbox and none of them
  # provisions, while covering nothing is exactly where the fleet already was.
  if [ -n "${SKEIN_FLEET_MOUNTS-}" ]; then
    while IFS= read -r fleet_mount; do
      [ -n "$fleet_mount" ] || continue
      [ -d "$fleet_mount" ] || continue
      # The two covers above are already in `binds`, and a tmpfs over one of them — or over
      # anything they sit inside — lands AFTER them in the argument list and throws their binds
      # away. The box would come up with no root of its own and no state, which is a worse failure
      # than the exposure this loop exists to close. Ancestors and not just equality: a store or a
      # work tree at `~/.skein` would be an ancestor of `~/.skein/boxes`.
      skip=
      for owned in "$fleet_root_dir" "$state_parent"; do
        case "$owned" in
          "$fleet_mount"|"$fleet_mount"/*) skip=1 ;;
        esac
      done
      [ -n "$skip" ] && continue
      binds+=(--tmpfs "$fleet_mount")
    done <<SKEIN_MOUNTS
$SKEIN_FLEET_MOUNTS
SKEIN_MOUNTS
    # After every tmpfs, never between them: a bind whose destination is under a mount covered later
    # in the argument list is thrown away by the tmpfs that follows it. Two repos sharing a parent
    # directory is enough to hit that, and the symptom is one box of the pair starting fine.
    [ -n "${SKEIN_BOX_STORE-}" ] && [ -d "$SKEIN_BOX_STORE" ] &&
      binds+=(--bind "$SKEIN_BOX_STORE" "$SKEIN_BOX_STORE")
    [ -n "${SKEIN_BOX_SOURCE-}" ] && [ -d "$SKEIN_BOX_SOURCE" ] &&
      binds+=(--ro-bind "$SKEIN_BOX_SOURCE" "$SKEIN_BOX_SOURCE")
  fi
  unset fleet_root_dir state_parent fleet_mount owned skip
fi
# Not a box's to pass on: the mount set names every repo on the host, which is the shape of the
# fleet, and the box has no use for it after this point.
unset SKEIN_FLEET_MOUNTS SKEIN_BOX_STORE SKEIN_BOX_SOURCE

# --- The fleet agent's token is not a box's to hold ----------------------------------------------
#
# The agent is the HOST's channel into this sandbox: it takes a script on `/exec` and runs it at
# fleet scope, outside any box's namespace, where `sudo` works (see `substrate.rs` — that is how a
# package gets installed for the whole fleet). Nothing inside a box calls it; only the host does.
#
# But its token sat at a fixed path in the fleet root, mode 0600 and owned by uid 1000 — which every
# box also is. Measured from a box: readable. So the shortest path out of a box was not an exploit
# at all, it was `cat`: read the token, POST a script, and be root in the sandbox. That reaches every
# other box's git tokens, its conversation history, and the credential helper itself.
#
# Same instrument as the ssh-agent socket below, and for the same reason: the path is well known and
# unsetting a variable stops nobody, so an empty file goes over it. The host's own copy is untouched
# and the agent — which starts at fleet scope, before any box exists — keeps reading the real one.
#
# Covered for every box except a deliberately privileged one — the workshop box, which exists to
# debug and extend skein itself and is useless without fleet reach. That is a per-box decision its
# owner makes in the cockpit, never a default and never inferred.
: >"$root/no-fleet-token" 2>/dev/null || true
fleet_token="${SKEIN_FLEET_ROOT:-/boxes}/.skein/fleet-agent.token"
if [ "${SKEIN_BOX_PRIVILEGED-}" != "1" ] && [ -f "$fleet_token" ] && [ -f "$root/no-fleet-token" ]; then
  binds+=(--ro-bind "$root/no-fleet-token" "$fleet_token")
fi
unset fleet_token

# A privileged box says so on its own terminal, every start.
#
# The whole risk of this switch is forgetting which box carries it: a box that can read every other
# box's credentials must never be one you have to check a settings pane to identify.
if [ "${SKEIN_BOX_PRIVILEGED-}" = "1" ]; then
  echo "skein: $box is the WORKSHOP box — it sees every box's files and can act at fleet scope. Settings → Boxes turns this off." >&2
fi

# --- GitHub: one repo to write, everything else to read ------------------------------------------
#
# What this replaces, measured rather than assumed: every box held the same `GH_TOKEN` — a user token
# with `repo`, `admin:public_key`, `gist` and `read:org`, reaching 460 repositories read and write —
# plus a forwarded ssh-agent socket signing for anything that key could reach. Ten boxes, one
# identity, and `admin:public_key` meant a box could add a key to the account: access that outlives
# the sandbox and shows up nowhere in skein.
#
# Now the sandbox-wide `GH_TOKEN` is a read-only PAT, and write is a per-repository App token the
# host mints and drops in this box's state directory. `git-credential-skein` picks between them by
# the repository git is asking about. See `src/gitgate.rs`.
#
# Opt-out, not opt-in: $SKEIN_GIT_SCOPE is set to `fleet` by the host when this box's owner has
# turned the switch off, and anything else — including an old host that never sets it — is scoped.
# Wrong in the safe direction: the failure is a box that reads everything and cannot push outside
# its own repo, which is recoverable by flipping one switch — not a box holding the account.
if [ "${SKEIN_GIT_SCOPE-repo}" != "fleet" ]; then
  export SKEIN_GIT_TOKENS="$state/git-tokens"
  mkdir -p "$SKEIN_GIT_TOKENS" 2>/dev/null || true
  chmod 700 "$SKEIN_GIT_TOKENS" 2>/dev/null || true

  # The sandbox-wide GH_TOKEN is dropped FIRST, and this is the line the whole boundary rests on.
  #
  # It holds whatever the sbx `github` secret was set to — for this fleet, a user token reaching 460
  # repositories read and write. Leaving it in place while scoping everything else would mean the
  # isolation held only if its owner had also remembered to swap that secret by hand, and a boundary
  # that depends on a manual step nobody is reminded of is not a boundary. Replaced below by this
  # box's own-repo token when the host has placed one; otherwise `gh` simply has no credential,
  # which is the honest state rather than a borrowed one.
  unset GH_TOKEN
  if [ -n "${SKEIN_BOX_REPO-}" ]; then
    own_token="$SKEIN_GIT_TOKENS/$(printf '%s' "$SKEIN_BOX_REPO" | sed 's#/#%2F#')"
    if [ -r "$own_token" ]; then
      # `gh pr create` and `gh pr comment` against this box's own repo work with this; against any
      # other repo `gh` is unauthenticated, while git keeps reading through the helper.
      GH_TOKEN="$(cat "$own_token" 2>/dev/null)"
      export GH_TOKEN
    fi
    unset own_token
  fi

  # The forwarded agent is account-wide, so leaving it reachable would undo all of the above: any
  # box could sign for any repository the host's key reaches. Unsetting the variable is not enough —
  # the socket path is well known, and anything can export it again — so a regular file goes over it
  # and `connect()` fails on a thing that is not a socket.
  : >"$root/no-ssh-agent" 2>/dev/null || true
  if [ -n "${SSH_AUTH_SOCK-}" ] && [ -S "$SSH_AUTH_SOCK" ] && [ -f "$root/no-ssh-agent" ]; then
    binds+=(--ro-bind "$root/no-ssh-agent" "$SSH_AUTH_SOCK")
  fi
  unset SSH_AUTH_SOCK

  # Wire the helper into the box's own gitconfig. `useHttpPath` is what makes a per-repository answer
  # possible at all: without it git sends only the host, and every GitHub URL looks identical to the
  # helper. `insteadOf` rewrites the SSH remotes this fleet already has — every box's origin is
  # `git@github.com:…` — onto HTTPS, so nothing has to be re-pointed by hand and a clone made before
  # today keeps working.
  helper="${SKEIN_FLEET_ROOT:-/boxes}/.skein/git-credential-skein"
  if [ -x "$helper" ] && command -v git >/dev/null 2>&1; then
    git config --file "$home/.gitconfig" credential.useHttpPath true 2>/dev/null || true
    git config --file "$home/.gitconfig" credential.helper "$helper" 2>/dev/null || true
    # Cleared then re-added, because `insteadOf` is multi-valued: a plain `git config` sets the
    # first value, so the second call would replace the first rather than join it, and `--add` on
    # every box start would instead grow a duplicate a run. Unset-then-add is the only idempotent
    # spelling of "these two, exactly".
    git config --file "$home/.gitconfig" \
      --unset-all url."https://github.com/".insteadOf 2>/dev/null || true
    git config --file "$home/.gitconfig" \
      --add url."https://github.com/".insteadOf "git@github.com:" 2>/dev/null || true
    git config --file "$home/.gitconfig" \
      --add url."https://github.com/".insteadOf "ssh://git@github.com/" 2>/dev/null || true
  fi
  unset helper

  # A push to a repo this box cannot write comes back from GitHub as a bare 403, and an agent that
  # reads one does not conclude "I should ask" — it retries, re-authenticates, tries SSH, edits the
  # remote, and burns a turn on a thing that was never going to work. This turns that dead end into
  # the request it should have been.
  #
  # **The shim is the message, not the boundary.** It never blocks: it files the ask and then runs
  # the real git anyway, so the push still fails exactly as it would have, with GitHub's own answer.
  # Nothing here is load-bearing for isolation — the token is, and an agent calling the real binary
  # directly gets the same 403. That is why every check below falls through rather than refusing.
  #
  # Unlike `sudo`, git is on every code path in every box, so the shape matters more than the logic:
  # anything that is not a push execs the real binary on the first line, and any surprise on the push
  # path execs it too.
  # `skein_launcher` is resolved inside the sudo shim above, and that block is skipped entirely when
  # a box has no sudo to shim. Under `set -u` an unset one would abort the launch — so it is worked
  # out again here rather than assumed, by the same two steps and for the same reason: `$0` is right
  # only when this was started as the launcher, and falls back to where it is actually installed.
  if [ -z "${skein_launcher-}" ]; then
    skein_launcher="$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")"
    [ -f "$skein_launcher" ] || skein_launcher="${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh"
  fi
  git_real=$(command -v git 2>/dev/null || true)
  [ -n "$git_real" ] && git_real=$(readlink -e "$git_real" 2>/dev/null || true)
  if [ -n "$git_real" ] && [ -f "$git_real" ]; then
    mkdir -p "$root/bin" || exit 1
    # Binding the shim over git shadows the very binary the shim has to exec, so the real one is
    # bound to a second path FIRST. bwrap resolves every source against the ORIGINAL filesystem, so
    # both land correctly however they overlap — the same property `--bind "$home" "$HOME"` relies on
    # above. The destination has to be somewhere this box can write: `/run/git.real` was tried and
    # bwrap refused with "Can't create file", exactly as it does for a new file in /usr/bin.
    : >"$root/bin/git.real" 2>/dev/null || true
    {
      printf '#!/bin/sh\n'
      printf 'skein_box=%q\n' "$box"
      printf 'skein_launcher=%q\n' "$skein_launcher"
      printf 'skein_git=%q\n' "$root/bin/git.real"
      cat <<'GITSHIM'
# Not a push, or nothing scoped: be git, immediately and with no further thought.
[ -n "${SKEIN_GIT_TOKENS-}" ] || exec "$skein_git" "$@"

# Find the verb. It is not always $1 — `git -C dir push` and `git -c k=v push` are ordinary, and
# both of those options take a separate argument that must not be mistaken for the verb.
verb=''
skip=0
for arg in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$arg" in
    -C | -c) skip=1 ;;
    -*) ;;
    *) verb="$arg"; break ;;
  esac
done
[ "$verb" = push ] || exec "$skein_git" "$@"

# The remote is the first bare word after the verb; absent, whatever this branch pushes to, and
# `origin` if even that is unset. Every one of these resolutions runs the REAL git.
remote=''
seen_verb=0
skip=0
for arg in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$arg" in
    -C | -c | --repo) skip=1 ;;
    -*) ;;
    *)
      if [ "$seen_verb" = 1 ]; then remote="$arg"; break; fi
      seen_verb=1
      ;;
  esac
done
if [ -z "$remote" ]; then
  branch="$("$skein_git" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
  remote="$("$skein_git" config --get "branch.$branch.remote" 2>/dev/null || true)"
  [ -n "$remote" ] || remote=origin
fi

# A name becomes a URL; a URL is already one. `get-url` also applies the insteadOf rewrite, so an
# `git@github.com:` remote arrives here in the same shape as an HTTPS one.
case "$remote" in
  *://* | *@*:* | *:*/*) url="$remote" ;;
  *) url="$("$skein_git" remote get-url "$remote" 2>/dev/null || true)" ;;
esac
[ -n "$url" ] || exec "$skein_git" "$@"

# owner/name, and only for github.com — anything else is not ours to have an opinion about.
rest="${url#*://}"
rest="${rest##*@}"
case "$rest" in
  github.com[:/]*) ;;
  *) exec "$skein_git" "$@" ;;
esac
path="${rest#github.com}"
path="${path#:}"
path="${path#/}"
owner="${path%%/*}"
name="${path#*/}"
name="${name%%/*}"
name="${name%.git}"
case "$owner" in '' | *[!A-Za-z0-9._-]* | -*) exec "$skein_git" "$@" ;; esac
case "$name" in '' | *[!A-Za-z0-9._-]* | -*) exec "$skein_git" "$@" ;; esac

# A token for it means this box may write it: nothing to say, get out of the way.
[ -r "$SKEIN_GIT_TOKENS/${owner}%2F${name}" ] && exec "$skein_git" "$@"

# No token. File the ask — `--request-write` collapses repeats, so a retrying agent does not grow
# the queue — then run the push anyway so GitHub gives its own answer alongside this one.
if [ -x "$skein_launcher" ]; then
  branch="$("$skein_git" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
  out=$("$skein_launcher" --request-write "$skein_box" "$owner/$name" "pushing $branch" 2>&1) || true
  [ -n "$out" ] && printf '%s\n' "$out" >&2
fi
cat >&2 <<WHY
skein: this box holds a GitHub token for its own repository only, so the push below will be
refused by GitHub. That is deliberate, not a misconfiguration — re-authenticating, switching to
SSH or editing the remote will not change it.
Approve the request above in the cockpit and the access appears within a minute.
WHY
exec "$skein_git" "$@"
GITSHIM
    } > "$root/bin/git" || exit 1
    chmod 755 "$root/bin/git" || exit 1
    binds+=(--ro-bind "$git_real" "$root/bin/git.real")
    binds+=(--ro-bind "$root/bin/git" "$git_real")
  fi
  unset git_real
fi

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
# The binary that reports the anchor, resolved here and by absolute path.
#
# The line that reports it runs in a LOGIN shell inside the box, whose PATH includes
# `~/.local/bin` — which is shared read-write with every box in the fleet (see the share list
# above). So an unqualified `tmux` there is a binary any box can replace, and the pid skein
# addresses this box by would be whatever that binary chose to print. Resolved out here instead,
# against a PATH that names only root-owned directories, before any box's namespace exists.
tmux_bin="$(PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin command -v tmux || true)"
[ -n "$tmux_bin" ] || { echo "skein: no tmux on the sandbox's own PATH, so this box has no session" >&2; exit 1; }

exec bwrap \
  --dev-bind / / \
  --bind "$tmp" /tmp \
  "${binds[@]}" \
  -- \
  bash -lc '
    tmux_bin="$1"; session="$2"; sock="$3"; pidfile="$4"; tree="$5"; shift 5
    # $TMUX is inherited from whatever session started skein, and when it is set tmux takes the
    # socket path from it VERBATIM instead of computing one and creating its parent directory. That
    # path names the OUTER /tmp, which does not exist in this box private one, so the server fails
    # to start — while the client still exits 0. Silent, and it reads exactly like a namespace
    # restriction. Unset it: this box session is not nested inside anything.
    unset TMUX TMUX_TMPDIR
    # cd here rather than relying on bwrap --chdir: this is a LOGIN shell, and sourcing the
    # profile can move it. Observed doing exactly that — the box started at / instead of its tree.
    cd "$tree" || exit 1
    "$tmux_bin" -S "$sock" new-session -d -s "$session" -- "$@" || exit 1
    # The anchor. A namespace lives as long as some process is in it, and the tmux server
    # double-forks away from this shell — so the shell exits while the box keeps running, and its
    # pid would name a corpse. The server is the right anchor on its own terms: it is in the
    # namespace, it lives exactly as long as the box does, and skein already treats it as the box life.
    # Box alive <=> server alive <=> namespace joinable, and kill-server drops the last process in
    # the namespace, which frees it.
    #
    # Reported on STDOUT, over the channel skein opened, and written to the pidfile only for the
    # box to read. skein must never read that file: it lives under the box own root, which is bound
    # read-write, so a box can put a SIBLING tmux server pid there — and the next provisioning,
    # diff, upload or takeover skein runs for this box would run in that sibling namespace instead.
    # A confused deputy walking straight through the file cover above.
    #
    # (No apostrophes and no single quotes in here: the whole block is one single-quoted argument
    # to bash -lc, so either one ends it early. Caught by a box that tried to cd to its pidfile.)
    anchor="$("$tmux_bin" -S "$sock" display -p "#{pid}")" || exit 1
    printf "%s\n" "$anchor" > "$pidfile"
    printf "SKEIN_ANCHOR %s\n" "$anchor"
  ' bash "$tmux_bin" "$session" "$sock" "$pidfile" "$tree" "$@"
