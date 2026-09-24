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
# rather than a missing flag, which is why it is written down here and asserted in
# src/place/argv.rs.
set -uo pipefail

# --- The PATH this script resolves its own commands against ---------------------------------------
#
# Root-owned directories only, and this is the first executable line because everything below it
# runs at FLEET scope — outside any box's mount namespace, where `sudo` works, where `/boxes` is
# whole, and where the fleet agent's token is a readable file.
#
# What it replaces: the PATH inherited from the fleet agent, which is a login shell's. On this
# substrate that begins `~/.local/bin:/usr/local/share/npm-global/bin:…`, and both of those are
# uid 1000 — the same uid every box runs as. `.local` was bound read-WRITE into every one of them
# when this was written, so that eleven boxes shared one toolchain instead of paying for it each,
# and a box that dropped a `sudo`, a `jq`, a `python3` or a `bwrap` into `~/.local/bin` had it run
# here, at fleet scope, with the real one still sitting behind it on the path. Nothing about that
# needed an exploit: it was a file copy into a directory the box already wrote.
#
# `.local` is a copy-on-write overlay per box now and `/usr/local/share/npm-global` is read-only in
# a box (`overlay_paths` below, SKEIN-963/968), so neither half of that sentence is still live —
# and this export stays exactly as it is, because the reason it exists is that a fleet-scope script
# must not resolve its commands through anything a box can reach, and "cannot today" is a weaker
# claim than "does not, by construction".
#
# The narrow fix — resolving one binary against a fixed PATH — was already here, for `tmux`, and
# the twelve `sudo` calls, the two `python3` calls and the `bwrap` below went on resolving through
# the inherited one. A single export is the only spelling that covers the ones nobody thought of,
# including the ones a later edit adds.
#
# It DOES reach inside a box, so the box's own PATH is decided here as well.
#
# This paragraph used to say the opposite: that the `bash -lc` under `exec bwrap` at the end of this
# file "rebuilds PATH from the profile exactly as before", and that a fixed PATH here therefore
# could not reach a box. Every clause of that is false on this substrate, and the measurement is
# here instead of the belief:
#
#   $ ls ~/.profile ~/.bash_profile ~/.bash_login      # in the sandbox, and in every box
#   (nothing: no box's private home under the fleet root has one, and nor does the sandbox's)
#   $ grep -rn PATH /etc/profile /etc/profile.d/
#   (nothing)
#   $ env PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
#       bash -lc 'echo $PATH; command -v claude'
#   /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
#   /usr/local/bin/claude
#
# An `export` is inherited by that child like any other variable, a login shell rebuilds nothing,
# and a box runs on the PATH it was handed. So the belief was not a harmless simplification: left
# alone it hands every agent session the fixed six, with no `~/.local/bin` in it, which is the very
# thing the old paragraph said must not happen. The `claude` a box ran would be the substrate's
# copy rather than the one the fleet installs and shares. It never bit only because the sandbox was
# carrying an older copy of this file with no `export PATH=` line at all, so the next install of the
# launcher is what would have exposed it (SKEIN-851). `Place::wrap` in `src/place/argv.rs` reached
# the same conclusion one file over, for a crossing, by the same route (SKEIN-832).
#
# `box_path` below is what the session gets instead: the box's own `~/.local/bin`, then the shared
# npm prefix, then exactly the six above. Three things about it are deliberate.
#
#   * It is DERIVED rather than spelled. Its tail is `$PATH` — the line right above it — so the
#     fixed six is written once and both PATHs move together. Its head is `$HOME`, which is also
#     what the box's private home is bound over (`binds=(--bind "$home" "$HOME")` further down), so
#     `$HOME` names the same directory inside the namespace and outside it. An empty `$HOME` is
#     already refused before any of this runs: the `case` that rejects a box root under `"$HOME"/*`
#     collapses to `/*` and turns every absolute path away.
#   * It is the same string a CROSSING carries, and that is not a coincidence to be trusted.
#     `Place::wrap` builds `{home}/.local/bin:/usr/local/share/npm-global/bin:{FLEET_PATH}` from the
#     placement record's `home`, which `fleet::sandbox_home` reads out of this same sandbox's
#     `$HOME`. Two producers, one value — so a box would resolve one agent when skein enters it and
#     another when it starts itself if they ever disagreed.
#     `tests/isolation_bwrap/path.rs::a_box_session_and_a_crossing_into_it_agree_on_the_boxs_path`
#     measures both and fails on the difference.
#   * It is exported INSIDE the `bash -lc`, not here and not through `--setenv`. A login shell
#     sources the profile BEFORE it runs the command string, so an export in that block wins over
#     any profile — including one a future substrate does have, which neither of the other two
#     spellings would. Exporting it here would be worse than useless: it would put a box-writable
#     directory in front of `bwrap` itself, which is the whole of ISO-1.
#
# That export stopped SKEIN's own scripts being one of the things a box can decide. `overlay_paths`
# and the `--ro-bind` beside it finish the other half: neither entry of `box_path` is writable from
# inside a box any more, so the agent one box runs is no longer a file another box can replace.
# Boxes remain one trust domain for their STATE — §9.2 says so and it is still true — but the
# domain's membership is no longer writable from inside it (SKEIN-963, SKEIN-968).
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
box_path="$HOME/.local/bin:/usr/local/share/npm-global/bin:$PATH"
# --- Why every one of those twelve is `sudo -n` ---------------------------------------------------
#
# Nobody is watching when this runs. skein spawns it through `util::run_bounded`, which hands every
# launcher call `Stdio::null()` for stdin and captures stdout and stderr (`src/util.rs:384-390`),
# and the calls that start and repair boxes come from `skein-server` — which itself lives in a
# detached tmux session with nothing attached to it.
#
# A `sudo` with no cached timestamp and no `-n` asks for a password anyway, and it asks on
# `/dev/tty` rather than on stdin, so the null stdin above does not save it and the `2>/dev/null`
# every call below already carries hides the prompt without stopping the wait. What that costs is
# not an error somebody can act on: it is a box start that never returns, until skein's own
# deadline kills it. This is the same defect the test side fixed first, in the probe described at
# the ceiling block of `a_box_lives_and_dies_inside_the_fleet_sandbox`.
#
# `-n` makes the same sudo refuse at once, which is the answer every one of these call sites is
# already written for: each ends `|| true` or `|| return 0`, or is the condition of an `if` whose
# other branch records `uncapped no-cgroup-delegation` and starts the box regardless. None of them
# wants a prompt — a call whose failure is written off with `|| true` is not one worth stopping a
# launch to ask a person about, and what it is asking for is a cgroup ceiling, not the box. Where
# sudo is passwordless `-n` costs nothing; a cached timestamp still counts as one.
#
# Being a comment, this block moves no launcher revision — `revision_of` hashes `cover_text`, which
# drops comment lines. The twelve edited lines do move it, which is right: a sandbox still carrying
# the older copy is genuinely running something else.

# Which cover these bytes apply, stamped in by `fleet::install_launcher` before the script is
# written into the sandbox — `launcher_revision()` in fleet/launcher.rs derives it from this file.
#
# It is stamped rather than passed on the command line because the two answer different questions.
# A value skein handed this script at launch would say what SKEIN was running; what anyone needs to
# know is what the SCRIPT does, and those disagree exactly when it matters — a sandbox still
# carrying a launcher older than the binary talking to it. A box then keeps that older namespace
# for as long as it lives, and nothing later can tell, because there is nothing left to read.
#
# Unstamped is left as the marker itself rather than substituted for something plausible: this file
# is also run straight out of the repo by tests, and a made-up revision there would be a claim.
launcher_revision="@SKEIN_LAUNCHER_REVISION@"

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
# `docker` arrives here too, but as `max/max` — see `fleet_limits` in `fleet/limits.rs` for why it
# is named only in order to be left uncapped. Briefly: that cgroup is not just a box's `docker
# build`, it also holds the sandbox's own init, socat and dockerd, so a ceiling there throttles or
# kills the machinery that answers `sbx exec` rather than the workload that overshot.
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
    sudo -n mkdir -p "$c" 2>/dev/null || return 0
    # `+cpu` too, and it has to be delegated for the same reason the others do: `cpu.weight` on a
    # child only exists if the parent handed the controller down. Best-effort like the rest — a
    # kernel or a sandbox without the controller is a fleet with no CPU shares, not a fleet that
    # will not start.
    sudo -n sh -c 'echo "+memory +pids +cpu" > "$1/cgroup.subtree_control"' _ "$c" 2>/dev/null || true
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
  # What the plumbing is GUARANTEED, before the ceilings are narrowed. `memory.min` is a promise
  # that memory under it is never reclaimed, which is the difference between a daemon the kernel
  # leaves alone and one it reaches for first — see `fleet_guarantees()` for why it is half the
  # plumbing share rather than all of it.
  #
  # Its own variable rather than a third field on `docker=max/max`: that spec is `max/high` split on
  # one `/`, and a launcher older than the skein driving it reads whatever it is handed. A ceiling it
  # cannot parse it skips loudly; a *grammar* it cannot parse it would misread as a ceiling. A name
  # an old launcher has never heard of is simply not read, which is the failure mode worth having.
  for pair in $(printf '%s' "${SKEIN_FLEET_GUARANTEES-}" | tr ',' ' '); do
    dir="/sys/fs/cgroup/${pair%%=*}"
    want="${pair#*=}"
    # Same rule as the ceilings below: only ever write over a cgroup that already exists. Creating
    # `docker` would hand dockerd a cgroup it did not make and expects to own. `boxes` is the one
    # name that is not a cgroup; it is handled below.
    [ "${pair%%=*}" = boxes ] || [ -d "$dir" ] || continue
    case "${want%M}" in
      "" | *[!0-9]*)
        echo "skein: ignoring the guarantee on ${dir##*/}: '$want' is not a size this launcher understands" >&2
        continue
        ;;
    esac
    mib="${want%M}"
    [ -n "$scale" ] && mib=$(( mib * actual / planned ))
    # `boxes` is what every box's floor adds up to (`box_floor_budget()` says why it is that size).
    # It is divided here, between the box cgroups that exist now, because only the sandbox can
    # count them. Every box gets the same share and each one is rewritten, so the floors add up to
    # no more than the budget however many boxes there are. A fleet that has grown since the last
    # write has each box's floor shrunk to fit. `containers` is not a box, and its containers have
    # no floor.
    if [ "${pair%%=*}" = boxes ]; then
      boxes_found=0
      for box_cg in /sys/fs/cgroup/skein/*/; do
        [ -d "$box_cg" ] && [ "$box_cg" != /sys/fs/cgroup/skein/containers/ ] && boxes_found=$(( boxes_found + 1 ))
      done
      [ "$boxes_found" -gt 0 ] || continue
      for box_cg in /sys/fs/cgroup/skein/*/; do
        [ -d "$box_cg" ] && [ "$box_cg" != /sys/fs/cgroup/skein/containers/ ] || continue
        sudo -n sh -c 'echo "$1" > "$2"' _ "$(( mib / boxes_found ))M" "${box_cg}memory.min" 2>/dev/null || true
      done
      continue
    fi
    sudo -n sh -c 'echo "$1" > "$2"' _ "${mib}M" "$dir/memory.min" 2>/dev/null || true
  done

  # What a container is worth against a box when both want the machine (architecture §9.5).
  #
  # **A weight, not a cap.** A `cpu.max` would idle cores while a container waits, which is the waste
  # the paragraph below rejects for boxes and rejects here for the same reason: when nothing else
  # wants the machine, a container should have all of it. A weight costs nothing while the machine
  # is quiet and decides who yields when it is not.
  #
  # **Half a box, and that is a judgement rather than a derivation.** Boxes weigh 100 each; a box is
  # somebody waiting at a terminal, and a container is work that box started and can wait a little
  # longer for. It is hard-coded here rather than sent from the host because it is a statement about
  # what things are worth, not a share of a machine's size — the numbers that scale with the sandbox
  # arrive in the spec above.
  if [ -d /sys/fs/cgroup/skein/containers ]; then
    sudo -n sh -c 'echo "$1" > "$2"' _ 50 /sys/fs/cgroup/skein/containers/cpu.weight 2>/dev/null || true
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
      sudo -n sh -c 'echo "$1" > "$2"' _ "$value" "$dir/$file" 2>/dev/null || true
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

# Is this a box name, or something that would leave the queue when spelled as a directory?
#
# A request is filed at `<queue>/requests/<box>/<id>.json`, so the box name is a path component
# before it is anything else. Nothing here is a trust check — a box can put any name in this
# variable, and the whole design of the per-box drop-box is that the *mount* answers rather than the
# name. This only stops a traversal turning a refused ask into a write somewhere else, and gives a
# box a sentence about what went wrong instead of an EROFS from a path it did not mean to name.
valid_box() {
  case "$1" in
    "" | -* | *..* | */* | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  return 0
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
  # **Neither of these is evidence, and the queue no longer asks them to be.** This line used to
  # prefer `$SKEIN_BOX` and say it "cannot be argued with"; it is an environment variable of a
  # process the box owns, so `SKEIN_BOX=other-box sudo apt-get install x` filed under `other-box`.
  # What answers "which box is this" is the *directory the file lands in*: the launcher creates
  # `requests/<box>/` outside the namespace and binds that one read-write, so a name that is not
  # this box's names a path this box cannot write, and the ask fails with the "could not be
  # recorded" message rather than arriving under somebody else's name. The host takes the box from
  # the path and ignores this field (`substrate::list`).
  #
  # Argument 1 first, because it is the better *hint* of the two: the sudo shim bakes it in at
  # generation time, outside the namespace, from the launcher's own argv. `$SKEIN_BOX` is the
  # fallback for a call typed by hand inside a box, where the attach shell has it and the caller
  # would otherwise have to know its own name.
  local box="${1:-${SKEIN_BOX:-}}" tool="" verb="" kind="" arg why=""
  shift
  # `--why <text>`: what the box wants it for, from the `skein_request_package` tool (SKEIN-1061).
  # First and only first, so it is never mistaken for one of sudo's own options below. The sudo shim
  # never passes it, and a request without one is filed exactly as before. One line, because the card
  # shows it as one: the tool refuses anything else, and a line break typed here by hand is folded
  # into a space rather than refused, so this path says nothing it was not already saying.
  if [ "${1-}" = "--why" ]; then
    why="${2-}"
    shift 2 2>/dev/null || shift $#
    why="${why//[$'\r\n']/ }"
  fi
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

  # 4 is "could not file", and the caller must not tell anyone to try again — see the shim.
  if ! command -v jq >/dev/null 2>&1; then
    echo "skein: jq is missing here, so the request could not be recorded." >&2
    return 4
  fi

  if ! valid_box "$box"; then
    echo "skein: '$box' is not a box name, so there is no queue to file this in." >&2
    return 4
  fi
  # **This box's own drop-box, not the shared one.** The queue root is read-only inside a box and
  # exactly one directory under it is bound read-write — the one the launcher made for THIS box —
  # so where the file lands is the box's identity, and a request filed under another box's name is
  # a write that fails rather than an impersonation that succeeds.
  local queue dir want
  queue="$(substrate_dir)/requests"
  dir="$queue/$box"
  # Writability is asked as well as creation, and that is not belt-and-braces: `mkdir -p` succeeds
  # on a directory that already exists whatever the mount says, so a box naming a NEIGHBOUR's
  # drop-box — which exists and is read-only here — got past this and failed two lines later with
  # `mktemp: Permission denied`. Refusing here is what turns that into the sentence the shim's
  # "could NOT file" advice is written against.
  if ! mkdir -p "$dir" 2>/dev/null || [ ! -w "$dir" ]; then
    echo "skein: the request could not be recorded — $dir is not writable from inside a box." >&2
    return 4
  fi
  # The identity of a request is what it would install, so the same ask from two boxes — or twice
  # from one — is one decision to make rather than a queue that grows every time a stuck agent
  # retries. Sorted, so argument order is not part of that identity.
  want="$kind $(printf '%s\n' "${packages[@]}" | LC_ALL=C sort -u | tr '\n' ' ')"

  # Every box's drop-box is READ, and only this box's is written. The queue root is bound read-only
  # rather than hidden, so collapsing a repeat across boxes still works — which it must, because
  # "the whole fleet needs `libnss3`" is one decision however many agents trip over it. The flat
  # glob is for a request filed before the queue was split per box; the host shows one of those and
  # refuses to act on it, because nothing can say now which box wrote it.
  #
  # **A neighbour's file gets to say `pending` and nothing else** (SKEIN-931). Every field in it is
  # that box's own words — it owns the directory and picks the ids — so `"state": "approved"` there
  # is a box's assertion about itself, not an answer. Only the host answers a request, and it
  # answers it on the HOST, at `$SKEIN_HOME/substrate/<box>/<id>.json`, which is not reachable from
  # in here; so there is nothing inside a box that can tell a real approval from a made-up one.
  #
  # Believed, one forged file was a silent veto over the whole fleet: every other box asking for
  # that package was told it "is approved", filed nothing, and the person who could have said yes
  # was never asked. A neighbour's *pending* cannot do that, and that is the whole difference — it
  # is a row on the owner's screen with two buttons on it, and whichever gets pressed the state
  # stops being `pending`, so the next ask files.
  #
  # `approved` is still believed in **this box's own** directory, where it is normally the host's
  # write-back and is what stops an agent retrying in a loop filing the same answered ask a hundred
  # times. Forged there it suppresses only this box's own ask, which is not a hole to attack anyone
  # through. `${f%/*}` is the directory the file was globbed out of; the flat glob leaves `$queue`,
  # which is never `$dir`, so a loose pre-split file is pending-only too.
  local f state existing
  for f in "$queue"/*/*.json "$queue"/*.json; do
    [ -f "$f" ] || continue
    state="$(jq -r '.state // ""' "$f" 2>/dev/null)" || continue
    case "$state" in
      pending) ;;
      approved) [ "${f%/*}" = "$dir" ] || continue ;;
      *) continue ;;
    esac
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
  if ! jq -n --arg id "$id" --arg box "$box" --arg kind "$kind" --arg why "$why" \
       --arg asked "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
       '{id:$id, box:$box, kind:$kind, packages:$ARGS.positional, why:$why,
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
# token scoped to its own repo and a read-only one for everything else, so a push elsewhere has no
# credential of this box's to make it with, whatever this queue says. The request exists so the
# refusal has somewhere to go. (A bound on the token and not on the box — see the GitHub block
# below, and SKEIN-548.)
request_write() {
  # Defaulted rather than indexed directly: `set -u` is on, and an agent that types this with an
  # argument missing would abort the shell it ran in rather than be told what it forgot.
  #
  # Argument 1 over `$SKEIN_BOX` — see `request_package` for why neither is trusted and what is.
  # **This is the queue where getting it wrong costs the most**: `gitgate::decide` builds the grant
  # from the request's box as well as its repo, and the refresher writes the minted installation
  # token into the box the grant names. A box that could file in somebody else's name could put a
  # live write token in a box of its choosing, off one approval a person read as somebody else's.
  local box="${1:-${SKEIN_BOX:-}}" repo="${2-}"
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
  if ! command -v jq >/dev/null 2>&1; then
    echo "skein: jq is missing here, so the request could not be recorded." >&2
    return 4
  fi

  if ! valid_box "$box"; then
    echo "skein: '$box' is not a box name, so there is no queue to file this in." >&2
    return 4
  fi
  # This box's own drop-box — see `request_package` for why the directory is the identity.
  local dir
  dir="$(gitgate_dir)/requests/$box"
  # Writability is asked as well as creation, and that is not belt-and-braces: `mkdir -p` succeeds
  # on a directory that already exists whatever the mount says, so a box naming a NEIGHBOUR's
  # drop-box — which exists and is read-only here — got past this and failed two lines later with
  # `mktemp: Permission denied`. Refusing here is what turns that into the sentence the shim's
  # "could NOT file" advice is written against.
  if ! mkdir -p "$dir" 2>/dev/null || [ ! -w "$dir" ]; then
    echo "skein: the request could not be recorded — $dir is not writable from inside a box." >&2
    return 4
  fi

  # One pending ask per box and repo. A stuck agent retrying a push must not grow the queue by one
  # decision per attempt — it is the same decision every time.
  #
  # Only this box's own directory is scanned, and only the repo is compared: the box half of "per
  # box and repo" is now the directory the scan is over, so re-reading it out of a file — which is
  # the field nobody may trust — would be asking a worse source the question the path just answered.
  local f state existing
  for f in "$dir"/*.json; do
    [ -f "$f" ] || continue
    state="$(jq -r '.state // ""' "$f" 2>/dev/null)" || continue
    case "$state" in pending | granted) ;; *) continue ;; esac
    existing="$(jq -r '.repo // ""' "$f" 2>/dev/null)" || continue
    if [ "$existing" = "$repo" ]; then
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

# --- What a box inherits from whoever started it (SKEIN-972) --------------------------------------
#
# This is the ONE list of environment variables a box's session receives from the process that ran
# this launcher, and every name on it carries its reason. Everything else is removed here, before
# the launcher reads anything, so no later line can depend on a variable a box does not get.
#
# Why a list and not the whole environment. The launcher is run by `skein-server`, which the
# fleet's doorway execs, and a box used to inherit that whole environment. So every variable the
# cockpit was started with was part of every box's contract without anybody deciding it was:
# `SKEIN_HOME` (the cockpit's home, a host path), and `SKEIN_LISTEN_INHERITED_ONLY`, which the
# doorway sets for the cockpit alone and which `apiauth` gives a meaning (SKEIN-962). A deny-list
# would name the two that were noticed and pass the next one; this names what a box needs.
#
# Unaffected by this, on purpose (the owner's decision on SKEIN-972): what the launcher EXPORTS
# after this point (SKEIN_BOX, SKEIN_STATE, PATH, SKEIN_GIT_TOKENS, GH_TOKEN, NO_PROXY and the rest),
# and whatever the box's own login profile sets inside the namespace.
#
# Measured, not guessed. The sandbox half is the names in the fleet's own environment on
# 2026-09-23 — `tmux -S <fleet root>/.skein/private/server.tmux show-environment -g | cut -d= -f1`,
# the session the cockpit runs in — kept where something in a box uses them. The skein half is
# every variable this file and the scripts a box runs (`src/probe/`, `src/kit/`,
# `src/git-credential-skein.sh`) read from their environment. Deliberately NOT here, though that
# environment has them: SKEIN_HOME, SKEIN_LISTEN_INHERITED_ONLY and SKEIN_IN_FLEET (the cockpit's;
# nothing in a box reads them); WORKSPACE_DIR (the sandbox's workspace, which is the cockpit's home;
# nothing in a box reads it); WAYLAND_DISPLAY (its socket is under the `/run` cover, so a box cannot
# reach it); PWD, OLDPWD, SHLVL and `_` (the shell's own, remade by the shell the box starts); and
# TMUX and TMUX_TMPDIR (the outer session's, which would point the box's tmux at the wrong server).
#
# `tests/fleet_launch/environment.rs::a_box_session_inherits_only_its_allow_list` starts a real box
# with a canary in the environment and reads the environment back from inside it.
#
# A CROSSING into a box keeps this same list (SKEIN-1085): `place::inherited_env` in
# `src/place/crossing.rs` reads it out of this file's text, so it is one list rather than two. Keep
# the shape the parse expects: the line `inherited_env=(`, names and `#` comments, then a line of
# just `)`. `place::crossing::tests::the_crossing_list_is_the_launchers_list` fails if the two
# readings differ.
inherited_env=(
  # The sandbox user's home and search path. The launcher needs HOME to know what to bind the box's
  # private home over, and replaces PATH with a fixed one before it runs anything (see the top).
  HOME PATH
  # skein -> launcher: set by `fleet::session_script` on this launcher's own command line, in the
  # environment rather than as positionals so an older launcher ignores them. The launcher unsets
  # the ones a box has no use for as it reads them.
  SKEIN_FLEET_LIMITS SKEIN_FLEET_GUARANTEES SKEIN_GIT_SCOPE SKEIN_BOX_REPO SKEIN_BOX_PRIVILEGED
  SKEIN_MODEL_SCRATCH SKEIN_FLEET_MOUNTS SKEIN_BOX_STORE SKEIN_BOX_PEERS SKEIN_FLEET_NAME
  # Where the fleet's own files are. Unset in production (it defaults to /boxes); a test fixture
  # sets it, and then the probes and the request helpers inside its boxes must resolve the
  # fixture's launcher and queues rather than the live fleet's.
  SKEIN_FLEET_ROOT
  # The runtime directory under a name a test can point elsewhere. Unset in production.
  SKEIN_RUNTIME_DIR
  # The sandbox's egress: every request a box makes goes out through the sandbox proxy, and these
  # are how each toolchain is told where it is. Without them a box has no network. The scoped block
  # further down adds the GitHub hosts to NO_PROXY.
  HTTP_PROXY HTTPS_PROXY http_proxy https_proxy NO_PROXY no_proxy NODE_USE_ENV_PROXY JAVA_TOOL_OPTIONS
  # The CA the proxy signs with, so TLS through it verifies: OpenSSL/curl, Python requests, Node.
  SSL_CERT_FILE REQUESTS_CA_BUNDLE NODE_EXTRA_CA_CERTS PROXY_CA_CERT_B64
  # The sandbox's credential proxy: placeholder keys the proxy swaps for the real one, and the mode
  # it runs each provider in. The runtimes a box starts authenticate through these; the launcher
  # drops ANTHROPIC_API_KEY/OPENAI_API_KEY further down when the box has a login of its own.
  ANTHROPIC_API_KEY OPENAI_API_KEY GOOGLE_API_KEY MISTRAL_API_KEY NEBIUS_API_KEY OPENROUTER_API_KEY
  XAI_API_KEY SBX_CRED_ANTHROPIC_MODE SBX_CRED_OPENAI_MODE SBX_CRED_GOOGLE_MODE SBX_CRED_MISTRAL_MODE
  SBX_CRED_NEBIUS_MODE SBX_CRED_OPENROUTER_MODE SBX_CRED_XAI_MODE
  # The sandbox's `github` secret. A `fleet`-scoped box keeps it on purpose; a scoped box has it
  # replaced by its own-repo token or removed, further down.
  GH_TOKEN
  # The sandbox's MCP gateway, which the runtimes' MCP servers are reached through.
  MCP_GATEWAY_URL MCP_SENTINEL_TOKEN_NAME
  # The forwarded ssh-agent and its gateway. A `fleet`-scoped box keeps both; a scoped box has a
  # file bound over the socket further down, and the gateway is reachable by owner decision
  # (SKEIN-929, docs/threat-model.md).
  SSH_AUTH_SOCK SSH_AUTH_SOCK_GATEWAY
  # Which sandbox this is. The probes fall back to SANDBOX_VM_ID for an identity outside a fleet.
  SANDBOX_ID SANDBOX_NAME SANDBOX_VM_ID
  # The sandbox's layout: npm's global prefix (where the agent CLI is installed, second on every
  # box's PATH), the file every non-interactive bash sources, and the per-user runtime directory
  # the peer network's sockets live in.
  NPM_CONFIG_PREFIX BASH_ENV XDG_RUNTIME_DIR
)
# First the names bash cannot hold as variables, because `unset` cannot reach them: an entry whose
# name is not an identifier — `CARGO_BIN_EXE_skein-server`, or an exported function's
# `BASH_FUNC_<name>%%` — is passed through to every child untouched. None of them can be on the list,
# so this launcher starts itself again without them; `env -u` takes a name, never a value, so no
# value reaches an argv. The second run finds none and goes on.
odd_env=()
while IFS= read -r -d '' entry; do
  name="${entry%%=*}"
  [[ "$name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || odd_env+=(-u "$name")
done </proc/$$/environ
[ "${#odd_env[@]}" -eq 0 ] || exec env "${odd_env[@]}" "$BASH" "$0" "$@"
for name in $(compgen -e); do
  keep=0
  for want in "${inherited_env[@]}"; do
    [ "$name" = "$want" ] && { keep=1; break; }
  done
  [ "$keep" = 1 ] || unset -v "$name" 2>/dev/null || true
done
unset entry name want keep odd_env inherited_env


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

# Which START this is, for the provisioning handshake. The box's /tmp is $tmp on disk, and a
# restart keeps it — so the `skein-startup.ready` / `skein-startup.failed` markers the kit writes
# there outlive the start that wrote them. Read bare, the previous start's `ready` satisfies the
# next start's setup wait before its provisioning has done anything (a box whose provisioning timed
# out came up "working"), and a leftover `failed` would refuse every later launch until someone
# deleted a file by hand — which is also why the fix is NOT to delete markers here: the id makes a
# stale marker inert without making a persistent one fatal. The kit suffixes its markers with this
# id and the agent launch waits on the suffixed names, so "ready" can only mean ready for THIS
# start. Written fresh on every launch, before anything that could read it: provisioning and the
# setup wait both run inside the namespace this launcher is about to build.
start_id="$(date -u +%Y%m%d%H%M%S)-$$"
printf '%s\n' "$start_id" >"$tmp/skein-start-id" || exit 1

# Private by DEFAULT, with a short list of deliberate escapes.
#
# The earlier shape was the other way round — share $HOME, bind over the paths known to matter — and
# that is unsafe for a reason no list can fix: an agent harness keeps state wherever it likes, and
# anything unanticipated was silently SHARED, so two boxes corrupt each other quietly. This way
# round, something unanticipated is merely private: it costs a re-download, not an identity two
# boxes both claim work under.
#
# (A copy-on-write overlay is the honest version of this, and half of it is available here. The
# paragraph that used to sit on this line said it was unavailable outright — "the sandbox root is
# ITSELF overlayfs, and overlayfs refuses an overlayfs upperdir" — which is true of the UPPER layer
# and was read as true of the mechanism. Re-measured on this substrate, bwrap 0.11.1:
#
#     $ bwrap --dev-bind / / --overlay-src LOWER --tmp-overlay DEST -- \
#         sh -c 'echo x > DEST/bin/probe && echo wrote'
#     wrote                                        # and LOWER/bin/probe does not exist outside
#     $ bwrap --dev-bind / / --overlay-src LOWER --overlay /boxes/u/upper /boxes/u/work DEST -- true
#     bwrap: Can't make overlay mount on ... upperdir=/oldroot/boxes/u/upper ...: Invalid argument
#
# The lower layer may live anywhere; the upper layer may not live on overlayfs, and every writable
# filesystem here except tmpfs IS overlayfs — `stat -f -c %T` answers `overlayfs` for /, /boxes,
# /var/tmp and /tmp alike. The one virtiofs mount accepts the upperdir, mounts, and then refuses
# every write with EROFS, which is worse than refusing to mount. So `--tmp-overlay` is what is
# left: bwrap makes the upper layer as a tmpfs INSIDE the namespace, which makes it private to the
# box by construction and gives it exactly the box's lifetime.)
#
# Seeded into the box on first start and diverging from there: credentials, per-box conversation
# history, the MCP registration that points each box at its own repo's work-tracking gateway, and
# the work-tracker install stamps under `.local/state` — which are the one thing under `.local`
# that must outlive a restart, and the reason is below `overlay_paths`.
seed_paths=(".claude" ".claude.json" ".codex" ".gitconfig" ".bashrc" ".profile" ".local/state")
# Bound back through, genuinely shared. These are package caches no box needs its own copy of.
#
# **`.local` is no longer one of them, and that is SKEIN-963.** It carries the agent CLIs and about
# 1.1 GB of `pip --user` libraries — software every box wants identical, which is why it was shared
# — and `box_path` above puts `$HOME/.local/bin` FIRST on every box's PATH. Shared read-write plus
# first on PATH is architecture §9.2 path 1 word for word: `~/.local/bin/claude` written by one box
# is what every other box executes on its next start, persistent cross-box code execution with no
# live target needed. It is in `overlay_paths` below instead, which keeps the reading and drops the
# writing.
#
# NOT ~/shared, though it is the most obviously shared thing here. It is scoped to a REPO, not to a
# sandbox — the two were the same object when a box was a sandbox, and this is where they come apart:
# one fleet sandbox hosts boxes from many repos, so binding its copy through would hand all of them
# one `shared` and quietly cross project boundaries. Each box gets its own instead, created during
# provisioning by shared-home.sh as a symlink into that box's repo store — which is host-mounted, so
# it stays live across boxes of the SAME repo, which is what `shared` has always meant. Binding it
# also broke provisioning outright: shared-home.sh refuses to replace a real path, and it gates
# startup, so every fleet box would have failed to come up.
share_paths=(".cargo" ".rustup" ".npm")

# Shared to READ, private to WRITE. Every box sees the sandbox's copy whole; every box's own writes
# land in a tmpfs upper layer that no other box has a name for.
#
# **What this buys, and it is the only thing it buys**: a box can no longer decide what another box
# executes. It does not make a box's writes safe FROM itself, and it is not a sandbox — a box still
# runs whatever the sandbox's `.local/bin` holds, which is what "boxes are one trust domain" has
# always meant and still does. What changes is that the domain's membership stops being writable
# from inside it.
#
# **The cost, stated rather than discovered**: a box's own `pip install --user` lives in RAM and is
# gone at the next `skein restart`. That is the right way round — installs that every box should
# have are made in the SANDBOX, outside any box, where they land in the lower layer and reach every
# box at once. It is also unbounded: `--size` applies only to `--tmpfs`, so nothing here caps what
# a box can push into its own upper layer. A box that installs torch inside itself spends 1 GB of
# the sandbox's memory until it restarts.
#
# **`.local/state` is the exception, and it is in `seed_paths` for a reason worth writing down.**
# `sync-install.sh` gates itself on `$HOME/.local/state/skein/sync-<slug>.done`, and that stamp
# means "this box owns its CLAUDE.md, its memory and its skill now" — the whole point of the gate
# is that a later start must not re-assert over them. An ephemeral upper layer loses the stamp at
# every restart, so the gate would open every time and rewrite a box's own memory on every start.
# Seeded and then bound back private below: the stamps a box has today come with it, and the ones
# it writes from now on are its own. That also closes a smaller defect nobody had filed — the
# stamp directory was SHARED, so one box's stamp answered for a box that had never run the install.
overlay_paths=(".local")

# `.claude/sessions` was a fifth entry here, and the reason it is not is the whole of SKEIN-572.
#
# It was added to make a box FINDABLE — Claude Code registers each session in
# `~/.claude/sessions/<pid>.json` and `ListAgents` reads that directory — over a comment arguing that
# the other half needed nothing, because "in this fleet the TRANSPORT is already shared and always
# was — every session's inbox socket lands in `/run/user/1000/cc-socks/`, which belongs to the
# sandbox, and nine of them were visible from inside one box when this was measured."
#
# That was true when it was written and false by the time it was read. The `/run` cover further down
# tmpfs'd the whole per-user runtime directory, and a mount beats a comment: measured from a live box
# on 2026-09-07, the shared registry held six sessions, every one advertising a socket, and exactly
# one resolved — the box's own. `ListAgents` still answered, with twenty peers, labelling every one
# `Remote Control`: routed out through Anthropic servers on each box's claude.ai login, which is a
# round trip this fleet should not need to talk to itself and is simply offline for any box whose
# connection has dropped.
#
# Neither comment was careless; they were written months apart, each correct on its own, and never
# read against each other. So the two halves are no longer decided in two places: see "the peer
# network" beside the `/run` cover, which either binds BOTH the registry and the socket directory or
# neither. Discovery without transport is the worst of the three states, because the registry says a
# box is live, peers address it on that word, and nothing on either end reports the loss.

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
# And a REFRESH token that has already expired is not a source, however good the access token beside
# it looks. That is the candidacy question, and it is the one the host's heal asks too (SKEIN-488):
# `refreshTokenExpiresAt` decides whether a copy may be seeded FROM, `expiresAt` decides which of
# the copies that may is best. The two used to be asked in opposite orders on the two sides of this
# boundary, and on the owner's live fleet they elected opposite winners — the launcher choosing the
# working credential and the heal choosing one two boxes were already logged out of.
#
# Prints the epoch-ms this login was last renewed to, or 0 when it carries one that records no
# expiry. Prints nothing, and fails, when the file carries no login that could seed anything — a
# husk, or a credential whose refresh token is spent. Both mean the same thing to every caller:
# there is nothing here to give away, so the vacuum clause below is free to fire.
login_life() {
  [ -s "$1" ] || return 1
  python3 - "$1" 2>/dev/null <<'PY'
import json, sys, time
try:
    data = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
if not isinstance(data, dict):
    sys.exit(1)
NOW = time.time() * 1000
KEYS = ("accessToken", "refreshToken", "access_token", "refresh_token", "OPENAI_API_KEY")


def number(value):
    # `bool` is an int in Python and `True` would read as expiry 1 — a login dated 1970.
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value


found, best = False, 0
# `data` itself for the flat shapes; the named blocks for the nested ones. Never mcpOAuth.
for block in (data.get("claudeAiOauth"), data.get("tokens"), data):
    if not isinstance(block, dict):
        continue
    if not any(str(block.get(k) or "").strip() for k in KEYS):
        continue
    dies = number(block.get("refreshTokenExpiresAt") or block.get("refresh_token_expires_at"))
    if dies is not None and dies <= NOW:
        continue
    found = True
    for k in ("expiresAt", "expires_at", "expiry"):
        v = number(block.get(k))
        if v is not None and v > 0:
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

# WHICH WAY A LOGIN MOVES, and why it is not symmetric.
#
# It used to be: whichever file claims the later expiry wins, in either direction. The expiry is a
# number inside the file, and the box side of that file is a box's to write. So a box writes a
# credentials file with a far-future expiry and a token of its choosing, wins the comparison, is
# copied UP into the fleet's canonical copy, and every box started afterwards seeds from it. No
# signature, no second opinion, fleet-wide propagation, from one line of JSON. Theft of this
# credential is obvious; poisoning it is not, and poisoning is the worse of the two.
#
# The comparison cannot be fixed by comparing something else. A box legitimately holds the refresh
# token, so anything it can produce honestly it can also produce dishonestly, and no field in a file
# a box writes is evidence about that file.
#
# So the DIRECTION carries the rule instead:
#
#   * the fleet already holds a login  ->  it flows DOWN only. A box takes it when it is better, and
#     keeps its own when its own is better. Nothing a box holds can replace the fleet's copy.
#   * the fleet holds no login at all  ->  a box's login heals it. There is nothing to poison: the
#     alternative is every box logged out, and any login is better than none.
#
# What this costs, stated rather than discovered: a token refreshed inside a box no longer improves
# the fleet's copy, so the canonical login ages until somebody runs `skein login`. That is a
# recoverable inconvenience, announced below. A poisoned fleet-wide credential is neither.
for rel in ".claude/.credentials.json" ".codex/auth.json"; do
  mine="$home/$rel"; canon="$HOME/$rel"
  mine_life="$(login_life "$mine")" || mine_life=""
  canon_life="$(login_life "$canon")" || canon_life=""
  if [ -n "$canon_life" ] && [ -n "$mine_life" ]; then
    if better_login "$canon_life" "$mine_life" "$canon" "$mine"; then
      merge_login "$canon" "$mine"
    else
      # Its own is the better one and it keeps it — but it does not write the fleet's copy from
      # here, and that rule is not softening: nothing in a file a box writes is evidence about that
      # file.
      #
      # And the host's tick does not soften it either, which is what this comment used to say.
      # `heal_logins` spreads the best copy DOWN and sideways on the file's own claim, because
      # boxes are one trust domain; it replaces the FLEET's copy only when that copy carries no
      # usable login at all, or when a model call skein itself made with that copy came back
      # refused (`login_evidence.json` beside it). So a token refreshed in a box reaches its
      # siblings within the minute and reaches the fleet's canonical copy when the fleet's copy
      # has been shown not to work — the same rule as this one, applied by the host on its own
      # evidence instead of by the box on its own say-so. Until then the answer is `skein login`.
      echo "skein: ${SKEIN_BOX:-this box} has a longer-lived $rel than the fleet's; a box cannot write the fleet's copy, so it reaches the other boxes within the minute and the fleet's own only if the fleet's stops working — run \`skein login\` to replace it now" >&2
    fi
  elif [ -n "$canon_life" ]; then
    merge_login "$canon" "$mine"
  elif [ -n "$mine_life" ]; then
    # The vacuum case, and the only way up. Nothing is displaced, because there is nothing there.
    merge_login "$mine" "$canon"
  fi
done

# A BOX SKEIN HAS HANDED A CREDENTIAL TO IS NEVER ASKED TO LOG IN.
#
# Seeding a login and seeding "you have logged in before" are one act, and they came apart. Claude
# Code gates its onboarding screen on `hasCompletedOnboarding` in `~/.claude.json`, not on whether a
# credential exists — so a box that inherited a perfectly good `.credentials.json` a few lines above
# still opened on the login screen, on every box the owner has ever created. Measured across his
# 13-box fleet on 2026-09-19: one refresh-token hash everywhere, `oauthAccount` everywhere, and the
# only key that differed between a box that asks and a box that does not was this one. Nothing in
# skein had ever written it (`grep -rn hasCompletedOnboarding src/ tools/` answered nothing at all),
# so it appeared only where a person had completed onboarding BY HAND — into that box's own private
# `.claude.json`, which never flows back — and every box created afterwards seeded from the same
# un-onboarded copy. That is why it was every new box and only its first launch.
#
# Written HERE, beside the credential, rather than into the copy a box is seeded from. This is the
# only place that knows whether the box ACTUALLY ended up with a login: it may have arrived in the
# seed above, or in the merge above from a login made elsewhere, and either way the answer is the
# file this reads and not what the seed happened to contain. It therefore also holds for every fleet
# whose seed carries no flag at all, which is every fleet that exists today.
#
# `lastOnboardingVersion` is deliberately NOT written. It gates re-onboarding after a Claude Code
# upgrade, which is plausibly wanted, and no onboarded box in this fleet carries it at all — the
# installed Claude Code writes `hasCompletedOnboarding` and no such key — so writing it would be
# suppressing a prompt nothing here has been seen to produce.
#
# READ, MODIFY, WRITE, and never a file from a template. `~/.claude.json` belongs to Claude Code,
# not to skein: it holds that box's project history, its MCP servers and its tips state — 60K of it
# on a box that has been used for a week. So one key is added to whatever is already there, through
# a temporary file and a rename so a crash cannot leave a half-written one. A file that is present
# and unparseable is LEFT ALONE and said out loud: the person is then exactly where they are today,
# one onboarding prompt, rather than one conversation record worse off. An absent or empty file has
# no record to lose and is written.
if command -v python3 >/dev/null 2>&1 && login_life "$home/.claude/.credentials.json" >/dev/null 2>&1; then
  onboarding_flag=0
  python3 - "$home/.claude.json" <<'PY' || onboarding_flag=$?
import json, os, sys, tempfile

p = sys.argv[1]
try:
    with open(p) as f:
        raw = f.read()
except FileNotFoundError:
    raw = ""
except OSError:
    sys.exit(1)
if raw.strip():
    try:
        data = json.loads(raw)
    except ValueError:
        sys.exit(1)
    if not isinstance(data, dict):
        sys.exit(1)
else:
    data = {}
if data.get("hasCompletedOnboarding") is True:
    sys.exit(0)
data["hasCompletedOnboarding"] = True
try:
    mode = os.stat(p).st_mode & 0o777
except OSError:
    mode = 0o600
where = os.path.dirname(p) or "."
os.makedirs(where, exist_ok=True)
handle, temp = tempfile.mkstemp(dir=where)
try:
    with os.fdopen(handle, "w") as out:
        json.dump(data, out)
    os.chmod(temp, mode)
    os.replace(temp, p)
except Exception:
    try:
        os.unlink(temp)
    except OSError:
        pass
    sys.exit(1)
PY
  [ "$onboarding_flag" -eq 0 ] || echo "skein: ${SKEIN_BOX:-this box} has a login, but its ~/.claude.json could not be read or is not JSON this can extend — it is left exactly as it is, so the agent may ask you to onboard once" >&2
  unset onboarding_flag
fi

# THE TREE SKEIN CLONED FOR A BOX IS TRUSTED IN THAT BOX — that tree, and nothing else.
#
# With the login screen gone (above), a new box stopped next on Claude Code's workspace-trust dialog
# for its own tree. Measured across the owner's 14 boxes on 2026-09-19: every box the owner had
# clicked through recorded `projects["/boxes/<box>/tree"].hasTrustDialogAccepted = true` in its own
# private `~/.claude.json` — the key is the absolute path, no trailing slash, which is `$tree` exactly
# (the `cd "$tree"` the session starts with, further down) — and nothing in skein wrote it. The
# question that dialog asks is "do you trust this folder's own `.claude/settings.json` hooks and MCP
# servers to run". For this one folder the person has already answered it: it is the repository they
# registered, cloned by skein into this box. Asking again tells them nothing.
#
# That is the whole of the claim, so it is the whole of the write. Exactly `$tree`: never `/`, never
# a parent, never any other key under `projects` — trust in Claude Code is inherited by every folder
# beneath the one trusted, so a broader key would vouch for things skein did not clone. A new entry is
# `{"hasTrustDialogAccepted": true}` and nothing more: Claude Code fills the rest of an entry from its
# own defaults, and its own runner writes this same one-key shape. An entry already there gains that
# one key and loses nothing.
#
# NOT behind the login guard above, deliberately. The two are different facts: whether this box holds
# a credential, and whether this tree is the one skein cloned. Sharing the guard would mean a box with
# no login shows the login screen and then, once the person has logged in, the trust dialog as well —
# a second prompt for a question whose answer never depended on the first. Standing alone, a box with
# no login shows the login screen and nothing after it.
#
# Same discipline as the onboarding flag: read, modify, write through a temp file and a rename; absent
# or empty is written; present and unreadable — or a `projects`, or an entry for this tree, that is
# not an object — is LEFT ALONE and said out loud, so the person meets one trust dialog rather than
# losing the record.
if command -v python3 >/dev/null 2>&1 && [ -d "$tree" ]; then
  workspace_trust=0
  python3 - "$home/.claude.json" "$tree" <<'PY' || workspace_trust=$?
import json, os, sys, tempfile

p, tree = sys.argv[1], sys.argv[2]
try:
    with open(p) as f:
        raw = f.read()
except FileNotFoundError:
    raw = ""
except OSError:
    sys.exit(1)
if raw.strip():
    try:
        data = json.loads(raw)
    except ValueError:
        sys.exit(1)
    if not isinstance(data, dict):
        sys.exit(1)
else:
    data = {}
projects = data.get("projects")
if projects is None:
    projects = {}
elif not isinstance(projects, dict):
    sys.exit(1)
entry = projects.get(tree)
if entry is None:
    entry = {}
elif not isinstance(entry, dict):
    sys.exit(1)
if entry.get("hasTrustDialogAccepted") is True:
    sys.exit(0)
entry["hasTrustDialogAccepted"] = True
projects[tree] = entry
data["projects"] = projects
try:
    mode = os.stat(p).st_mode & 0o777
except OSError:
    mode = 0o600
where = os.path.dirname(p) or "."
os.makedirs(where, exist_ok=True)
handle, temp = tempfile.mkstemp(dir=where)
try:
    with os.fdopen(handle, "w") as out:
        json.dump(data, out)
    os.chmod(temp, mode)
    os.replace(temp, p)
except Exception:
    try:
        os.unlink(temp)
    except OSError:
        pass
    sys.exit(1)
PY
  [ "$workspace_trust" -eq 0 ] || echo "skein: ${SKEIN_BOX:-this box}'s ~/.claude.json could not be read or is not JSON this can extend — it is left exactly as it is, so the agent may ask you once to trust $tree" >&2
  unset workspace_trust
fi

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
# ...and the copy-on-write ones on top of those, lower layer first.
#
# `--overlay-src` names the layer to READ and applies to the `--tmp-overlay` that follows it, which
# is why the two are emitted as one pair per entry and never separated: a stray `--overlay-src`
# with no overlay after it attaches itself to the next one, and the box would then see a directory
# it was never meant to.
#
# The destination is the same path as the source, exactly as the share loop above does it, and for
# the same reason — `$HOME` is already the box's private home by the time this is read inside the
# namespace, while the SOURCE resolves against the original filesystem. So this reads the sandbox's
# real `~/.local` and shows it at the box's own `~/.local`.
#
# **Asked by running it, because a box that cannot start is worse than a box that cannot write.**
# `--tmp-overlay` arrived in bubblewrap 0.9; this substrate has 0.11.1. An unconditional overlay on
# a sandbox whose image ships an older bwrap is `bwrap: Unknown option`, which is every box on that
# fleet refusing to start with no way in to fix it — the one failure this file must never have. So
# the capability is probed rather than assumed, and a bwrap without it gets the SAFE half: a
# read-only bind, which keeps the property that matters (no box decides what another box executes)
# and loses only a box's ability to write its own `~/.local`. Said out loud, because losing
# `pip install --user` inside a box with no explanation is the kind of thing people debug for an
# hour.
#
# **The probe is the mount itself, not a stand-in for it**, and the first spelling here was a
# stand-in: it asked bwrap to overlay `/usr`, which fails on this very substrate — `/usr/bin/docker`,
# `/usr/bin/git` and `/usr/bin/sudo.ws` are separate mounts, and overlayfs refuses a lowerdir with
# submounts under it. So the capability probe answered "no overlay here" on a machine whose
# `~/.local` overlays perfectly, and every box would have quietly taken the read-only fallback. A
# proxy for the real question is a third thing that can be wrong. This runs the exact source and
# destination the bind below will use, so whatever it answers is about the mount that will be made.
#
# One extra `bwrap` per overlay entry per box start, ~8ms, against a launch that already spends
# seconds in provisioning.
for rel in "${overlay_paths[@]}"; do
  [ -e "$HOME/$rel" ] || continue
  if bwrap --dev-bind / / --overlay-src "$HOME/$rel" --tmp-overlay "$HOME/$rel" -- /bin/true >/dev/null 2>&1; then
    binds+=(--overlay-src "$HOME/$rel" --tmp-overlay "$HOME/$rel")
  else
    binds+=(--ro-bind "$HOME/$rel" "$HOME/$rel")
    echo "skein: bwrap here cannot overlay ~/$rel, so it is READ-ONLY in $box rather than \
copy-on-write — every tool is still there, but nothing inside the box can install into it. \
bubblewrap 0.9 or newer, and a ~/$rel with no mount points under it, is what gets the writable \
upper layer back." >&2
  fi
done
# ...and the box's own state back through the overlay it just went under (see `overlay_paths`).
#
# AFTER the loop, because it has to win: the overlay covers all of `.local`, and this is the one
# subtree under it that a restart must find again. Created here rather than assumed, because the
# seed only copies when the sandbox has something to copy — a fleet whose sandbox has never run
# `sync-install.sh` has no `~/.local/state` at all, and `--bind` of a missing source is a hard
# bwrap failure, which would mean no box on that fleet could start.
mkdir -p "$home/.local/state" || exit 1
binds+=(--bind "$home/.local/state" "$HOME/.local/state")

# The npm prefix the agent CLIs actually live in — READ-ONLY, and this is the other half of
# SKEIN-963 (filed separately as SKEIN-968 because a private `~/.local` does not touch it).
#
# `box_path` above is `$HOME/.local/bin:/usr/local/share/npm-global/bin:$PATH`. The first entry is
# handled by the overlay; the second is the one `which -a claude` answers with inside a real box,
# and it is not under `$HOME`, so nothing above reaches it. Measured on this fleet, from inside a
# box, 2026-09-19: `/usr/local/share/npm-global` is owned by uid 1000 — the uid every box runs as —
# `test -w` on its `bin` says writable, and the `claude` symlinked there was 2.1.278 while the
# root-owned `/usr/local/bin/claude` that skein's own updater installs was 2.1.272. That gap is the
# proof rather than the theory: Claude Code's background auto-updater had already written, from
# inside whichever box ran it, the binary every other box then executed.
#
# **A read-only bind rather than an overlay, and the difference is what happens to a box that
# updates itself.** Under an overlay the in-box updater SUCCEEDS — into a tmpfs, ~440 MB of it, lost
# at the next restart, and until then that box runs an agent build skein neither installed nor can
# see. Two boxes could run two different agents while the cockpit reports one version. Read-only
# makes the attempt fail instead, which costs nothing and leaves exactly one answer to "which agent
# is this fleet running". The attempt is then stood down explicitly rather than left to fail — see
# the `DISABLE_` exports in the session block at the end of this file.
#
# Conditional because it is an absolute path on a substrate that need not have it: a `--ro-bind` of
# a missing source fails the whole launch.
[ -d /usr/local/share/npm-global ] \
  && binds+=(--ro-bind /usr/local/share/npm-global /usr/local/share/npm-global)
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
WHY
# 4 is "this WAS an install command and the request could not be filed". The advice below is the
# whole of what changes, and it is not a detail: the general text tells you to ask by running the
# install you wanted — which is the command that just failed to file anything. Repeating it would
# send someone round the same loop for as long as they were willing to try.
if [ "$rc" = 4 ]; then
  [ -n "$out" ] && printf '%s\n' "$out" >&2
  cat >&2 <<'WHYNOFILE'
  * A system package belongs in the fleet's substrate, installed once for every box — but skein
    could NOT file the request from here, so running this again will not file one either. Ask
    whoever runs this fleet to add it from the cockpit (Substrate).
WHYNOFILE
else
  cat >&2 <<'WHYASK'
  * If it genuinely has to be a system package, it belongs in the fleet's substrate — installed
    once for every box. Ask for it by running the install you wanted:
        sudo apt-get install <package>     sudo npm install -g <package>
    That installs nothing. It files a request for this fleet's owner to approve in the cockpit,
    and once approved the package is there for every box.
WHYASK
fi
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
#
# **Containers are the one exception, and it is a weight rather than a cap.** Every word above is
# right about boxes and none of it transfers: nobody waits at a terminal for a container the way
# somebody waits for an agent, and the daemon a container depends on is not in its weight class — a
# container saturating every core makes dockerd miss its own deadlines, which reads as a crash with
# nothing having died. So `skein/containers` weighs half a box (see `apply_fleet_ceilings`), which
# costs nothing while the machine is quiet and decides who yields when it is not.
#
# **Made whether or not there is a ceiling to write into it.** The cgroup does two jobs and only one
# of them is the ceiling: it is also the box's identity as a *set of processes*, which is what
# `cgroup.kill` needs at stop and what the fleet's accounting is built on. Gating the whole block on
# `$limits` gave a box with no computed limit neither — so the box that most needed containing was
# the one with none, and `2>/dev/null || true` on the kill made that silent.
cgroup_root="/sys/fs/cgroup/skein"
cg="$cgroup_root/$box"
limits_state="uncapped no-cgroup-delegation"
# A controller is only available in a child if the PARENT delegates it, so the order is: make the
# parent, delegate, then make the leaf. Processes live only in the leaf — cgroup v2 forbids a
# cgroup having both children and processes.
if sudo -n mkdir -p "$cgroup_root" 2>/dev/null \
  && sudo -n sh -c 'echo "+memory +pids" > '"$cgroup_root"'/cgroup.subtree_control' 2>/dev/null \
  && sudo -n mkdir -p "$cg" 2>/dev/null; then
  # In a subshell, so that no way this can fail becomes a box that will not start. The guard above
  # closes the one that bit; this closes the shape. `set -u` aborts the shell it runs in, and the
  # spec here comes from a *newer* skein than the launcher reading it, so the next token nobody
  # anticipated would do again exactly what `max` did. A subshell makes the blast radius the
  # ceilings rather than the session. Nothing downstream reads what it sets.
  #
  # Applied whether or not THIS box got a ceiling of its own: these are the fleet's — the boxes'
  # shared parent, docker, and the containers' — and they are the ones that keep the sandbox
  # answering. Making them conditional on one box's per-box limit was never the intent, only where
  # the code happened to sit.
  ( apply_fleet_ceilings ) || echo "skein: the shared ceilings could not be applied for $box; it starts under whatever is already on those cgroups" >&2
  for kv in $(printf '%s' "$limits" | tr ',' ' '); do
    case "$kv" in
      max=*)  sudo -n sh -c 'echo "$1" > "$2"' _ "${kv#max=}"  "$cg/memory.max"  2>/dev/null || true ;;
      high=*) sudo -n sh -c 'echo "$1" > "$2"' _ "${kv#high=}" "$cg/memory.high" 2>/dev/null || true ;;
      pids=*) sudo -n sh -c 'echo "$1" > "$2"' _ "${kv#pids=}" "$cg/pids.max"    2>/dev/null || true ;;
      *) echo "skein: ignoring unknown limit $kv for $box" >&2 ;;
    esac
  done
  if sudo -n sh -c 'echo $1 > "$2"' _ "$$" "$cg/cgroup.procs" 2>/dev/null; then
    # Two different states, and the difference is what somebody can do about it. In the cgroup
    # WITH a ceiling is the intended one. In the cgroup with NO ceiling means the box is contained
    # — a stop reaches it, the fleet accounts for it — and nothing bounds what it can take, which
    # is a skein-side question about the memory plan. Not in a cgroup at all is the sandbox's
    # answer and needs a different fleet.
    if [ -n "$limits" ]; then
      limits_state="capped $limits"
    else
      limits_state="uncapped no-limit-computed"
      echo "skein: no memory ceiling computed for $box; it runs uncapped" >&2
    fi
  else
    limits_state="uncapped could-not-join-cgroup"
    echo "skein: $box could not join its cgroup; it runs without a memory ceiling" >&2
  fi
else
  # Not fatal: an uncapped box still works, and refusing to start one because the image lacks
  # cgroup delegation would be a worse trade. Loud, though — this is the guard that keeps one
  # box's runaway build from killing every other box in the sandbox.
  echo "skein: no cgroup delegation in this sandbox; $box runs WITHOUT a memory ceiling, so a runaway build in it can take the whole fleet down" >&2
fi
# Recorded in the box, and reported to skein. The file is the box's own copy and nothing on the host
# can read it — `$root` is inside the sandbox — which is why "nothing read it" was true for as long
# as it existed. The report goes out the way the anchor and the launcher revision do: over the
# channel skein already opened, into the placement record, where the board reads it for free.
printf '%s\n' "$limits_state" > "$root/limits.state"
printf "SKEIN_LIMITS %s\n" "$limits_state"

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
#   ls /Users/you/.skein/boxes/example-master/claude-projects/   → readable
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
  # Whether anything told this box which mounts are its own — decided HERE, well before the banner
  # that reports it, because `unset SKEIN_FLEET_MOUNTS` runs in between: a banner reading the
  # variable directly would find it gone and never fire (SKEIN-836). The flag is what carries the
  # condition across that unset, and it is set inside this block so a privileged box never sets it
  # at all — that box is uncovered on purpose and says so in its own words.
  #
  # Two causes, one consequence, and the launcher cannot tell them apart: `fleet::mount_manifest`
  # hands an empty manifest to a box it cannot match to a repository, and a launcher already
  # installed in a running sandbox predates the variable and passes nothing. Either way the two
  # loops below iterate nothing and every host mount the sandbox gave this box stays where it is.
  # So the banner names the consequence, which is observable here, rather than the cause, which is
  # not.
  [ -n "${SKEIN_FLEET_MOUNTS-}" ] || uncovered=1
  fleet_root_dir="${SKEIN_FLEET_ROOT:-/boxes}"
  state_parent="$(dirname "$state")"

  # --- first, any mount this box's own directories live INSIDE (SKEIN-219) ------------------------
  #
  # A mount that CONTAINS the fleet root or the state parent cannot be covered by the loop further
  # down: bwrap applies its arguments in order, so a tmpfs over `~/.skein` written after the binds
  # of `~/.skein/boxes/<box>` throws them away, and the box comes up with no state at all. That is
  # why the loop skips ancestors — and skipping is not covering. The fleet skein-server runs inside
  # is mounted at exactly such an ancestor (delivery §3 4c: the volume holding `credentials/`,
  # `api-token` and `github-pats/`), so from every box on that fleet the fleet's own credentials
  # were one `cat` away. Measured, not reasoned: `tests/isolation_bwrap/` runs bwrap and reads
  # them back.
  #
  # Ordering rather than enumeration, so nothing has to be listed: cover the ancestor HERE, before
  # the two entitlements are bound back below. bwrap resolves every `--bind` source against the
  # original filesystem, so those binds still name the real directories through the tmpfs — the
  # same property the covers below already rely on. Equality is left to the loop: a mount that IS
  # the fleet root or the state parent is already covered by the two covers that follow.
  if [ -n "${SKEIN_FLEET_MOUNTS-}" ]; then
    while IFS= read -r fleet_mount; do
      [ -n "$fleet_mount" ] || continue
      [ -d "$fleet_mount" ] || continue
      for owned in "$fleet_root_dir" "$state_parent"; do
        case "$owned" in
          "$fleet_mount"/*) binds+=(--tmpfs "$fleet_mount") ; break ;;
        esac
      done
    done <<SKEIN_ANCESTOR_MOUNTS
$SKEIN_FLEET_MOUNTS
SKEIN_ANCESTOR_MOUNTS
  fi

  binds+=(--tmpfs "$fleet_root_dir")
  [ -d "$fleet_root_dir/.skein" ] && binds+=(--ro-bind "$fleet_root_dir/.skein" "$fleet_root_dir/.skein")

  # --- the one directory under `.skein` that is skein's alone (SKEIN-516 Rule 1) -----------------
  #
  # `.skein` is bound back READABLE just above, which is right for the launcher, the credential
  # helper and the toolchain and wrong for everything skein authenticates with. What used to stand
  # between a box and those bytes was one empty file bound over one name — so the fleet agent's
  # token was covered and the review call's GitHub credential, written to the same directory by the
  # same argument, was not. An enumeration protects what somebody remembered.
  #
  # So it is a DIRECTORY that is covered, not a list of files: what skein puts under `private/` —
  # the fleet agent's token, the socket the fleet agent listens on, and the credential a model call
  # acts with while the call is in flight — goes away with one `--tmpfs`. Anything skein puts there
  # later is covered the day it is written, with nobody remembering to add it, which is the property
  # the per-file cover could not have. That argument is the point of this block and it stands.
  #
  # **`server.tmux` is under the cover too now, and it is the reason to say what this block is for**
  # (SKEIN-529). It is the socket the cockpit's own tmux server is supervised on, and for most of a
  # year it sat BESIDE `private/` at the top of `.skein`, in the half every box can read — so every
  # box could `connect()` to it, tmux admits a client whose peer uid matches its own, which every
  # box's does, and a tmux client is a place the server runs a command (`MSG_SHELL`, `MSG_EXEC`).
  # `0600` separates nothing under one fleet-wide uid. What had kept it out there was not a judgement
  # about the risk: the path is spelled again in `bootstrap.sh`, and moving one spelling without the
  # other gives a fleet two tmux servers contending for the cockpit's port. They moved together.
  #
  # Nothing in a box wants it. Every caller reaches that socket at *sandbox* scope, outside every
  # box's namespace, where this tmpfs is not applied at all.
  #
  # A `--tmpfs` and not a `--ro-bind` of an empty directory, because a socket is not stopped by a
  # read-only mount: `connect()` on a unix socket asks nothing of the filesystem's write
  # permission, so the fleet agent's socket would still be reachable from every box (kernel
  # `sb_permission` returns EROFS for regular files, directories and symlinks — not for sockets).
  # A tmpfs replaces the directory rather than restricting it, and a name that is not there cannot
  # be connected to. That is the whole of what moving `server.tmux` in here bought: the same
  # reasoning, now applied where the mount actually is rather than beside it.
  #
  # Created here, outside the namespace, for the same two reasons the request drop-boxes below are:
  # bwrap needs a source that exists, and it cannot make one under the read-only mount it just
  # applied. Ordering, as everywhere in this block — the later, narrower mount is the one that wins.
  private="$fleet_root_dir/.skein/private"
  mkdir -p "$private" 2>/dev/null || true
  chmod 700 "$private" 2>/dev/null || true
  [ -d "$private" ] && binds+=(--tmpfs "$private")
  unset private

  # --- the three drop-boxes a box may WRITE into, inside the read-only `.skein` -----------------
  #
  # Read-only `.skein` is right for everything in it except the one thing a box is supposed to put
  # there: a request for its owner to approve. `--request-package` (which the sudo shim calls on
  # every `sudo apt-get install`) and `--request-write` (which the git shim calls on a refused push,
  # and which the handoff brief tells the agent to run by hand) both begin with
  # `mkdir -p .../requests` — under a read-only mount. Both therefore failed, always, and said so
  # politely: "skein could not file the request from here". Measured on a fleet of eleven boxes
  # running for weeks: neither request directory existed, because the only thing that creates them
  # is the half that cannot write.
  #
  # So the *directories* are bound read-write and nothing else is. They are created HERE, before
  # `exec bwrap`, because this runs outside the namespace where `.skein` is still writable — and
  # because bwrap needs a source that exists. Applied after the `--ro-bind` above, since bwrap takes
  # its arguments in order and the later, narrower mount is the one that wins.
  #
  # **`requests/<box>/`, not `requests/`, and the difference is who a request is from.** Unmasking
  # the queue as one shared directory is the second of the three steps architecture §8.4 puts in
  # order — "bind the artifact, make the request path per box, *then* unmask" — and it was the step
  # that got skipped. With one directory every box could delete, rewrite or flip the state of every
  # other box's pending request, and could file one in another box's name; the only thing standing
  # against the last of those was `$SKEIN_BOX`, which is an environment variable of a process the
  # box owns. On the gitgate queue that is not an attribution nicety: the grant is built from the
  # request's box, and the refresher writes the minted GitHub token into the box the grant names.
  #
  # Per box, the identity of a request is the directory it is in — a fact about the mount namespace
  # rather than a value the box supplied — and no box has a writable path to any other's. Boxes can
  # still READ each other's asks through the read-only queue root, which is deliberate: they share a
  # uid, `substrate.rs` says at length that this gate is a chokepoint and not a wall, and collapsing
  # a repeated ask across boxes needs the read.
  #
  # The third queue, `asks`, is a box's questions for its owner (the `skein_ask_person` tool in the
  # box plugin, SKEIN-1061), bound the same way for the same reason: the directory is who asked.
  #
  # What a box gains is exactly the ability to ask, which is what the cockpit's approval panels were
  # built for. It gains no ability to answer: the grants, the decisions and the package manifest all
  # live elsewhere under `.skein` and stay read-only.
  for asking in substrate gitgate asks; do
    drop="$fleet_root_dir/.skein/$asking/requests/$box"
    mkdir -p "$drop" 2>/dev/null || true
    [ -d "$drop" ] && binds+=(--bind "$drop" "$drop")
  done
  unset asking drop

  binds+=(--bind "$root" "$root")
  # The state parent is a separate mount (the host's `~/.skein/boxes`), so it needs its own cover.
  # Guarded on the two being different directories: if a fleet ever put box state inside the fleet
  # root, a tmpfs over it here would erase the bind just made above.
  #
  # READ-ONLY, and the box loses nothing by it. Architecture §5 divides a box's durable state by who
  # writes it, and this directory holds two kinds:
  #
  #   * **recorded** — `claude-projects/` and `codex-sessions/`, the box's own conversation. Bound
  #     read-WRITE a hundred lines above, at `$HOME/.claude/projects` and `$HOME/.codex/sessions`,
  #     which is where the agent actually writes them. Separate mounts of the same directories, so
  #     read-only here costs nothing there.
  #   * **artifact** — `git-tokens/`, which the HOST mints and places and this box only ever reads.
  #     A box that could write it could write itself a token for a repository it was not given.
  #
  # Nothing in a box writes here through this path. The launcher's own `mkdir`/`chmod` on the token
  # directory run before `exec bwrap`, outside this namespace, and the resize archives are written
  # by the sandbox at fleet scope. Read-only is also what keeps `declared/` gone rather than merely
  # moved: SKEIN-7 took the four security-deciding files out of this directory, and this makes
  # putting one back impossible rather than unfashionable.
  if [ -d "$state_parent" ] && [ "$state_parent" != "$fleet_root_dir" ]; then
    binds+=(--tmpfs "$state_parent" --ro-bind "$state" "$state")
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
  #   * its own repo's work tree, at all — not read-write, not read-only. It was bound read-only
  #     for exactly one thing: the gitignored files `shared-paths.txt` names, which
  #     `sandbox-bootstrap.sh` now surfaces out of the store's own `shared-rw/` instead — nothing on
  #     the host copies them into the store any more. Nothing else a box does needs the tree its
  #     user works in, and a box that cannot see it cannot have `core.fsmonitor` in its
  #     `.git/config` executed as the host user either.
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
      #
      # Skipped HERE because it was already covered THERE: the block above tmpfs'd every ancestor
      # before the entitlements were bound back, which is the only order in which both hold
      # (SKEIN-219). Skipping without that block is what left a volume-mounted fleet's credentials
      # readable from every box.
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
  fi
  unset fleet_root_dir state_parent fleet_mount owned skip
fi
# Not a box's to pass on: the mount set names every repo on the host, which is the shape of the
# fleet, and the box has no use for it after this point.
unset SKEIN_FLEET_MOUNTS SKEIN_BOX_STORE

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
# It is the `private/` cover above that closes this now, and the empty file that used to be bound
# over this one name is gone with it. The reason for the swap is the second credential: the review
# call's GitHub token was written into the same directory by the same argument — "beside the fleet
# agent's token and for the same reason" — and got the path without the cover, because a cover
# spelled as a file name protects only the file somebody remembered. `rm -f` below takes the empty
# marker away rather than leaving a file whose only meaning was a mechanism that no longer exists.
rm -f "$root/no-fleet-token" 2>/dev/null || true

# --- /run: what the cover never reached (architecture §9.5 R11) ---------------------------------
#
# Every cover above is about paths skein chose. `/run` is not one of them, and three things live
# there that every box shares because every box is the same uid:
#
#   * `/run/user/<uid>` — the per-user runtime directory, one for the whole sandbox. A private
#     tmpfs, with exactly ONE named hole in it: `cc-socks`, bound back by the peer-network block
#     below. The original argument for the cover was "empty today, which is exactly when to close
#     it — a private tmpfs costs nothing now and stops it becoming a channel the first time
#     something puts a socket in it". Something did: the agent runtime's inbox sockets. That
#     retires the premise, not the requirement — the cover stays, and what changes is that the one
#     channel through it is declared, reasoned about and tested rather than discovered later.
#   * `/run/secrets` — world-writable and sticky. Same treatment, and no hole.
#   * `/run/docker.sock` — **deliberately left reachable**, and §9.5 R11 says why: skein points the
#     sandbox's dockerd at the workload cgroup precisely so containers a box starts are accounted
#     for. Covering it would remove a capability the design supports. What it grants — a container
#     in this sandbox, as root, with any bind mount — is written down there rather than here.
#
# `$SKEIN_RUNTIME_DIR` names the same directory under another name, and nothing in production sets
# it. It is here because the cover and its one hole are only worth as much as the test that runs
# them, and the honest test plants a file in the runtime directory and asks a real bwrap namespace
# whether it is there — which against `/run/user/1000` means writing into the live fleet's own
# runtime directory, beside the inbox sockets of running agents. Named the way `$SKEIN_FLEET_ROOT`
# is, set by the host or by nobody, never by a box: a box cannot reach the launcher's environment
# any more than it can reach `$SKEIN_BOX_PRIVILEGED`. A test in `cockpit.rs` asserts the default is
# still the real path, so the seam cannot quietly become the production value.
runtime_dir="${SKEIN_RUNTIME_DIR:-/run/user/$(id -u 2>/dev/null || echo 0)}"
if [ "${SKEIN_BOX_PRIVILEGED-}" != "1" ]; then
  run_user="$runtime_dir"
  [ -d "$run_user" ] && binds+=(--tmpfs "$run_user")
  [ -d /run/secrets ] && binds+=(--tmpfs /run/secrets)
  unset run_user
fi

# --- The peer network: discovery and transport, together or not at all (architecture §9.5 R11) ---
#
# Claude Code's session-to-session messaging is TWO shared paths, and skein controls them
# separately: `ListAgents` finds a session by reading `~/.claude/sessions/<pid>.json` (DISCOVERY),
# and `SendMessage` reaches it over the inbox socket that session opened in the runtime directory's
# `cc-socks/` (TRANSPORT). Both are sandbox-wide, so both are shadowed — discovery by the private
# `$HOME` this box gets, transport by the `/run` cover directly above — unless something binds them
# back. This block is that something, and it is one block on purpose.
#
# **They are never independently switchable.** Skein ran the half-open configuration for months:
# registry shared, sockets tmpfs'd away. Every box advertised an inbox nothing could reach, peers
# addressed it because the registry said it was there, and neither end was told — the sender's
# message went out over Anthropic's servers instead, needing a claude.ai login this fleet should not
# need to talk to itself, unavailable on Bedrock, Vertex and Foundry, and offline for any box whose
# connection had dropped. Half-open is worse than either whole state, because the cost lands on a
# sender who did nothing wrong and has no way to find out why. Hence: both binds or neither, and a
# failure to prepare either half turns the other one OFF rather than leaving it advertising.
#
# **What an open peer network costs, stated here rather than discovered later.** Every box in a
# fleet is the same uid, so the socket's own protection — the runtime restricts it to the operating
# system user — separates nothing here. The mount is the only boundary and it is now deliberately
# open, and with `crossSessionInbound: accept` seeded above there is no approval gate left between
# boxes: any box can put text of its choosing in front of any other box's agent, and that agent may
# hold credentials the sender does not. What the receiving side still does is real but partial — it
# says the message came from another session rather than from you, grants it no approval and no
# configuration change, and never runs commands out of its text. That is why a relayed human review
# travels as a NOTICE whose authoritative copy stays in the read-only owner inbox no box can write
# (SKEIN-382), and why anything else delivered over this socket owes the same split.
#
# **Off is full isolation, never a quiet half.** `SKEIN_BOX_PEERS=0` is set by the host from the
# repo's own switch, which ships ON; anything else, an old host that never sets it included, is that
# default. Off takes the discovery share AND covers the socket directory, so the repo's boxes
# neither see peers nor are seen and `ListAgents` in them answers as it did before any of this
# existed. Enforced by the MOUNT and not by a setting, because a box can edit its own
# `settings.json`: `permissions.deny` and `crossSessionInbound: refuse` are advisory where this is
# meant to be a boundary, and `refuse` would drop what skein sends the box too, which is not what
# turning off box-to-box means.
#
# Deliberately NOT under the privileged check, unlike every cover above it. The workshop box is the
# escape hatch for the FILE cover; making it the one box whose repo switch silently does nothing
# would be the same class of bug this block exists to close.
peer_socks="$runtime_dir/cc-socks"
peers=0
case "${SKEIN_BOX_PEERS-1}" in
  0|off|no|false) ;;
  *)
    # Both sources created before either is bound: bwrap resolves a source against the original
    # filesystem and fails the whole namespace on one that is missing, so the first box to start in
    # a fresh sandbox would otherwise get discovery and no transport — the exact half-state.
    if mkdir -p "$HOME/.claude/sessions" 2>/dev/null && mkdir -p "$peer_socks" 2>/dev/null; then
      binds+=(--bind "$HOME/.claude/sessions" "$HOME/.claude/sessions")
      binds+=(--bind "$peer_socks" "$peer_socks")
      peers=1
    fi
    ;;
esac
# A tmpfs rather than an omission, because omitting it only closes the transport for a box that is
# already under the `/run` cover — the workshop box is not, and would keep a reachable socket
# directory while its registry was private.
[ "$peers" = "1" ] || binds+=(--tmpfs "$peer_socks")

# What this box was actually BORN with, reported the way the ceiling and the launcher revision are.
#
# The switch lives in `repos.json`, and flipping it changes not one byte of this script — so
# `fleet::launcher_revision`, which hashes this file, stays identical and `fleet::cover_is_current`
# keeps answering yes over a box still running the mount it started with. A flag whose effect is a
# mount must therefore travel WITH the box rather than be re-read from config, or it is a switch
# that silently does nothing until somebody happens to restart.
printf 'SKEIN_PEERS %s\n' "$peers"
unset runtime_dir peer_socks peers

# A privileged box says so at every start.
#
# The whole risk of this switch is forgetting which box carries it: a box that can read every other
# box's credentials must never be one you have to check a settings pane to identify.
#
# **This line used to say "on its own terminal", and that was never true** (SKEIN-846). Both
# production callers run this script through `Place::exec`, which pipes stderr and reads it only
# when the launcher EXITS NON-ZERO (`src/place/run.rs`, the `!out.status.success()` branch of
# `bytes`); on the success path every word written here is dropped. Nor is it the box's terminal:
# that is the tmux pane made under `exec bwrap` at the end of this file, a pty this `echo` happens
# long before. So what follows is correct in its wording and not yet delivered, and the delivery is
# SKEIN-846's — it needs the placement and the board, which carry the launcher's STDOUT facts
# already. Left here rather than deleted, because the words are the part that is hard to get right,
# and because a failed start does carry them.
#
# **Three grants, named** (architecture §9.5 R9). The first two were always said; the third and the
# line after it were not, and both are things somebody turning this on cannot discover by using it.
# It holds every secret under `.skein/private/` — the fleet agent's token, the credential a review
# call acts with, the two sockets — because the tmpfs over that directory is skipped here. And
# it is exempt from the mount cover — which is not only its own business: the guards on the git
# token directory and the resize archive both hold *because an ordinary box cannot plant a link
# where the host writes*, and this switch is what turns that off.
#
# Short on purpose. A warning long enough to be skipped is a warning nobody reads, and the reasoning
# belongs in the architecture rather than on a terminal at every start.
#
# **And the box beside it that is uncovered by ACCIDENT says so here too** (SKEIN-836). Two boxes
# can be running without the mount cover and only one of them was chosen: the workshop box, and a
# box no manifest reached (see the flag set at the top of the isolation block). They were
# indistinguishable from inside — the only thing that mentioned the second was one `eprintln!` in
# `fleet::mount_manifest`, on the server's own stderr, which SKEIN-799 established nobody reads.
#
# Deliberately NOT the same sentence. The workshop banner describes a switch somebody threw and
# names where to throw it back; this one describes something that happened TO the box, so it says
# what is reachable and what skein would have to know for it not to be. Two states that read alike
# are two states nobody checks.
#
# It is also narrower than "the whole fleet", and saying so is the point: the covers over the fleet
# root and the state parent are spelled from paths skein chose and still apply, so the other BOXES
# are still gone. What no manifest means is that the *host* mounts stay — every other repo's store
# and work tree, and, on a fleet whose state sits on a mounted volume, the volume holding
# `credentials/`, `api-token` and `github-pats/` (SKEIN-219).
# **Decided here, DELIVERED three ways below** (SKEIN-846). What follows used to be two `echo … >&2`
# and nothing else, and stderr is the one channel on this path that reaches nobody: both production
# callers run this script through `Place::exec`, which pipes stderr and reads it only when the
# launcher exits NON-ZERO. So the words were right and no one had ever seen them. The banner is a
# string first and a delivery second now, because there are three places it has to arrive and only
# one of them is a stream.
#
# ONE LINE, no newlines in it: the stdout marker below is a line-oriented channel skein greps with
# `strip_prefix`, the same shape as `SKEIN_ANCHOR` and `SKEIN_PEERS`, and a second line would be
# read as a line the launcher did not write.
announce=""
if [ "${SKEIN_BOX_PRIVILEGED-}" = "1" ]; then
  announce="$box is the WORKSHOP box. It sees every box's files, acts at fleet scope, and holds the fleet agent token; the mount cover is off for it. Settings → Boxes turns this off."
elif [ "${uncovered-}" = "1" ]; then
  announce="$box came up UNCOVERED, and nobody chose that. Nothing told it which mounts are its own, so every other repo's store and work tree are readable from here, and so is whatever the fleet's own state sits on. Its own checkout and the other boxes' are still separate. skein can only name a box's mounts when the box's name matches a repository it knows."
fi
if [ -n "$announce" ]; then
  # SURFACE 2 — the skein command that started this box. STDOUT is the channel skein actually reads:
  # `Place::bytes` returns it on success, and `fleet::notices_from_launch` lifts these lines back out
  # for `start_box` and `ensure_box_session` to print on the person's own stderr. The anchor pid, the
  # launcher revision and the ceiling all travel this way; this is the same road, not a new one.
  printf 'SKEIN_NOTICE %s\n' "$announce"
  # And stderr as well, unchanged, because the FAILURE path does deliver it: a launcher that exits
  # non-zero has its stderr turned into the error text, which is the one case that always worked.
  echo "skein: $announce" >&2
fi
unset uncovered

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
# the repository git is asking about. See `src/gitgate/`.
#
# **Why the credential is not enough on its own, and what the block below does about it (SKEIN-548).**
# The sandbox routes HTTP through a credential-injecting proxy at `$HTTPS_PROXY` that TERMINATES TLS
# for the GitHub hosts (issuer `O=Docker Sandboxes` through it, a real CA direct). Left to the proxy,
# a request with no Authorization header, a deliberately invalid one, or this box's own per-repo
# token all come back authenticated as the fleet ACCOUNT — so the credential this block places would
# narrow what a box's token can DO while the proxy quietly handed the box the whole account's reach.
# Measured from inside a box on 2026-09-06/07/11 and again 2026-09-15: garbage `Bearer` is 200
# through the proxy and 401 direct, and the git wire protocol lists a private repo's refs through the
# proxy with no credential.
#
# So the `SKEIN_GIT_TOKENS` block above puts the GitHub hosts in `NO_PROXY`. git and gh then reach
# GitHub DIRECT, the proxy never sees them, and each presents THIS box's own token and is bounded by
# it server-side — which is the boundary that was missing. That closes the reach for the tools an
# agent actually uses: a push to a repo this box was not granted is refused by GitHub against this
# box's own token; `gh` is authenticated only for this box's own repo and unauthenticated elsewhere;
# and the ssh-agent that would have signed for the whole account has a regular file bound over its
# socket.
#
# **What it is still not.** It is not a hard isolation boundary. A process that deliberately routes
# back through the proxy — re-exporting `$HTTPS_PROXY` and clearing `$NO_PROXY` — can still be injected
# as the account, exactly as one can re-export the ssh-agent socket path the block binds a file over:
# a variable anything can set again is not a containment. This narrows the NORMAL path to skein's own
# credential; escaping it takes intent, and the substrate (a deny-by-default egress policy, SKEIN-926)
# is what turns intent away. `docs/architecture.md` §9.5/§9.6 and `docs/parity.md` carry the mechanism
# and the reproduction. The `fleet` opt-out keeps the proxy and the account-wide token ON PURPOSE —
# that is the honest third mode, not a hole.
#
# Opt-out, not opt-in: $SKEIN_GIT_SCOPE is set to `fleet` by the host when this box's owner has
# turned the switch off, and anything else — including an old host that never sets it — is scoped.
# Wrong in the safe direction: the failure is a box that reads everything and cannot push outside
# its own repo, which is recoverable by flipping one switch — not a box holding the account.
if [ "${SKEIN_GIT_SCOPE-repo}" != "fleet" ]; then
  export SKEIN_GIT_TOKENS="$state/git-tokens"
  mkdir -p "$SKEIN_GIT_TOKENS" 2>/dev/null || true
  chmod 700 "$SKEIN_GIT_TOKENS" 2>/dev/null || true

  # Route GitHub DIRECT for this scoped box — the line that makes the per-repo boundary REAL
  # (SKEIN-548). Whether `$HTTPS_PROXY` terminates TLS for the GitHub hosts is the substrate's
  # choice, not skein's, and it has already gone both ways: measured from inside a box on
  # 2026-09-06 and again on 2026-09-15, it did, and had the final say on the credential — no token,
  # a garbage token, and this box's own per-repo token were all answered as the fleet ACCOUNT.
  # Measured again on 2026-09-21 it did not: every host tried presented its own real certificate
  # (a plain CONNECT tunnel, MITMing nothing) and a garbage `Bearer` came back 401 where the same
  # request came back 200 six days earlier. Nothing in this tree changed between those dates — the
  # substrate did — and `/usr/local/share/ca-certificates/proxy-ca.crt`
  # (`CN=Docker Sandboxes Proxy CA`) is still installed, so the 2026-09-06/15 behaviour is dormant
  # machinery, not removed machinery, and can return without anything here changing. Do not read
  # this comment for today's answer: `docs/threat-model.md`'s "GitHub through the sandbox proxy"
  # row and the hourly `proxy_injection` health check (`src/health/reach.rs`, SKEIN-927) carry it,
  # because they are re-measured rather than remembered. Everything below (the helper, the own-repo
  # token, the shim) narrows what a box's token can DO regardless of which answer that is. With sbx
  # v0.43.0 a client credential the proxy did not issue is no longer forwarded either, so through
  # the proxy this box's own token is dropped anyway — DIRECT is the only path on which this box's
  # own token is ever seen.
  #
  # So NO_PROXY names exactly the GitHub hosts the proxy has MITMed before and may again — see
  # `docs/threat-model.md` for which is true today rather than a certificate issuer asserted here —
  # and git and gh honour NO_PROXY through libcurl and Go respectively. `localhost`/`127.0.0.1`
  # stay direct too, so the `$SKEIN_GITHUB_API` stub the tests point at loopback keeps working. A
  # `fleet`-scoped box does NOT reach this block: it keeps the proxy and the account-wide token,
  # which is the honest third mode ("one PAT for the entire app, provided by the sbx secret")
  # rather than a broken boundary.
  gh_direct="api.github.com,github.com,raw.githubusercontent.com,gist.github.com,copilot.github.com"
  case ",${no_proxy-}," in
    *",github.com,"*) ;;
    *) no_proxy="${no_proxy:+$no_proxy,}$gh_direct" ;;
  esac
  case ",${NO_PROXY-}," in
    *",github.com,"*) ;;
    *) NO_PROXY="${NO_PROXY:+$NO_PROXY,}$gh_direct" ;;
  esac
  export no_proxy NO_PROXY
  unset gh_direct

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
      # The sandbox's name, carried from skein (`fleet::session_script`) so the blocked-egress hint
      # below prints a command a person pastes unedited. Empty when an OLDER launcher started this
      # box — baked in anyway, because the hint tells "not told the name" apart from having one.
      printf 'skein_fleet=%q\n' "${SKEIN_FLEET_NAME-}"
      cat <<'GITSHIM'
# Not a push, or nothing scoped: be git, immediately and with no further thought.
[ -n "${SKEIN_GIT_TOKENS-}" ] || exec "$skein_git" "$@"

# Blocked egress vs an auth answer (SKEIN-548, SKEIN-926). A scoped box reaches GitHub DIRECT (the
# launcher put the GitHub hosts in NO_PROXY), so if the host's deny-by-default network policy blocks
# GitHub the connection never lands and git fails with a bare transport error — which an agent reads
# as "retry", not "ask the person to change a policy". `skein_reach_hint` says the one thing that
# fixes it, and ONLY on a real block: a DIRECT probe that comes back with any HTTP status means
# GitHub answered (a 401/403 is an auth answer, not a block), so `curl` without `-f` exits 0 and no
# hint prints; it exits non-zero only when the transport itself failed to connect. The probe target
# is overridable for tests via `$SKEIN_GITHUB_REACH_URL`, the same seam shape as `$SKEIN_GITHUB_API`.
skein_reach_hint() {
  command -v curl >/dev/null 2>&1 || return 0
  if curl -sS --noproxy '*' -o /dev/null -m 5 --connect-timeout 3 \
      "${SKEIN_GITHUB_REACH_URL:-https://api.github.com/}" >/dev/null 2>&1; then
    return 0
  fi
  # The name is baked in at box start (`skein_fleet`, from $SKEIN_FLEET_NAME) so this command can be
  # pasted unedited. A box started by an OLDER launcher carries no name — then say that plainly. An
  # empty `--sandbox ` would be worse than the placeholder this replaced: it reads as a finished
  # command and is not one.
  if [ -n "${skein_fleet-}" ]; then
    printf '%s\n' "skein: GitHub is blocked by the sandbox's network policy. On your host run:  sbx policy allow network --sandbox $skein_fleet github.com,api.github.com" >&2
  else
    printf '%s\n' "skein: GitHub is blocked by the sandbox's network policy, and this box was never told its sandbox's name, so skein cannot finish the command. On your host, run 'sbx ls' for the name, then: sbx policy allow network --sandbox THAT-NAME github.com,api.github.com" >&2
  fi
}

# Run the real git, then — only if it failed — add the blocked-egress hint. Used on the paths that
# actually reach GitHub, so a passthrough (`status`, `log`) still execs immediately below and pays
# nothing. Exits with git's own code, so nothing downstream sees a difference on success.
skein_git_run() {
  "$skein_git" "$@"
  rc=$?
  [ "$rc" -ne 0 ] && skein_reach_hint
  exit "$rc"
}

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
# Push falls through to the write-request logic below. The other GitHub-reaching verbs run through
# `skein_git_run`, so a box blocked from GitHub gets the policy hint on a fetch or clone too, not
# only on a push. Everything else is git, immediately.
case "$verb" in
  push) ;;
  fetch | pull | clone | ls-remote | fetch-pack) skein_git_run "$@" ;;
  *) exec "$skein_git" "$@" ;;
esac

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

# A token for it means this box may write it: nothing to say, get out of the way (but still add the
# blocked-egress hint if the direct push cannot reach GitHub at all).
[ -r "$SKEIN_GIT_TOKENS/${owner}%2F${name}" ] && skein_git_run "$@"

# **One push files one ask, however many shims it passes through** (SKEIN-956). `git_real` is
# whatever `command -v git` found when this box started, and inside a box that is the box's OWN
# shim — so a box started from a box, or skein's tests run in one, wrap this shim around another,
# and each filed an ask under its own box name: two cockpit rows for one push. The shim further out
# has already asked for this repo and named it below, so the one further in is only git.
[ "${SKEIN_GIT_ASKED-}" = "$owner/$name" ] && exec "$skein_git" "$@"

# No token. File the ask — `--request-write` collapses repeats, so a retrying agent does not grow
# the queue — then run the push anyway so GitHub gives its own answer alongside this one. A scoped
# box now reaches GitHub DIRECT (SKEIN-548 closed for the token's own path), so this push is bounded
# by the token: with none for this repo GitHub answers 403/404 against whatever credential the box
# holds, rather than the proxy answering as the account. The message still must NOT promise a
# particular outcome — a repo the box's read token can see still fetches — only that no WRITE grant
# for it exists here.
filed=1
if [ -x "$skein_launcher" ]; then
  branch="$("$skein_git" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
  out=$("$skein_launcher" --request-write "$skein_box" "$owner/$name" "pushing $branch" 2>&1)
  filed=$?
  [ -n "$out" ] && printf '%s\n' "$out" >&2
fi
cat >&2 <<WHY
skein: this box holds a GitHub token for its own repository only, so nothing here grants you
$owner/$name. That is deliberate, not a misconfiguration — re-authenticating, switching to
SSH or editing the remote will not change it.
WHY
# "Approve the request above" is only true if there IS a request above. Filing can fail — the queue
# is read-only from inside a box — and pointing at a request that was never written sends someone
# to look for it in a cockpit that has nothing to show them.
if [ "$filed" = 0 ]; then
  echo "Approve the request above in the cockpit and the access appears within a minute." >&2
else
  echo "skein could not file the request from here, so there is nothing in the cockpit to approve — ask whoever runs this fleet for write access to $owner/$name." >&2
fi
SKEIN_GIT_ASKED="$owner/$name"
export SKEIN_GIT_ASKED
skein_git_run "$@"
GITSHIM
    } > "$root/bin/git" || exit 1
    chmod 755 "$root/bin/git" || exit 1
    binds+=(--ro-bind "$git_real" "$root/bin/git.real")
    binds+=(--ro-bind "$root/bin/git" "$git_real")
  fi
  unset git_real
fi

# The docker shim: whose container is this?
#
# Docker records nothing about which box asked. Every box reaches the same daemon over the same
# socket at the same uid, and the daemon puts every container under one shared cgroup parent —
# which is deliberate, and is what makes a box's containers count against the fleet's ceilings.
# Shared accounting and per-box ownership are different questions, and the second had no answer at
# all: stopping a box left its containers running, with nothing able to say which they were.
#
# So the shim stamps two things onto `run` and `create`, and each reaches a case the other cannot:
#
#   * a **label**, which is how the box's stop finds them — `docker rm -f` rather than a signal,
#     because killing a container's processes leaves the daemon believing it still runs, so the name
#     stays taken and the volumes stay attached;
#   * a **cgroup under the box's own name**, still inside the shared parent so the fleet ceiling
#     above is untouched, which is what the stop can reach when dockerd is the thing that has
#     stopped answering — and a container that wedged dockerd is one of the ways a box gets here.
#
# **The shim is a convention, not a boundary**, exactly as the git shim above is. A box holds the
# daemon: it can curl the socket directly and ask for anything, `docker compose` composes its own
# create calls, and neither carries what this stamps. That is stated where it is decided
# (architecture §9.5 R11) rather than pretended away, and skein's stop names every running container
# it cannot attribute rather than reporting success over the ones it missed.
#
# Never blocks and never refuses: anything that is not `run` or `create`, and any surprise on the
# path that is, execs the real binary unchanged. An explicit `--cgroup-parent` from the caller wins,
# because a person who asked for one meant it.
docker_real=$(command -v docker 2>/dev/null || true)
[ -n "$docker_real" ] && docker_real=$(readlink -e "$docker_real" 2>/dev/null || true)
if [ -n "$docker_real" ] && [ -f "$docker_real" ]; then
  mkdir -p "$root/bin" || exit 1
  # Same overlap trick as git's: the real binary is bound to a second path FIRST, because binding
  # the shim over it shadows the very thing the shim has to exec.
  : >"$root/bin/docker.real" 2>/dev/null || true
  {
    printf '#!/bin/sh\n'
    printf 'skein_box=%q\n' "$box"
    printf 'skein_docker=%q\n' "$root/bin/docker.real"
    cat <<'DOCKERSHIM'
# Find the verb. It is not always $1: `docker -H unix:///…  run` and `docker --context foo run` are
# ordinary, and both of those options take a separate argument that must not be read as the verb.
verb=''
skip=0
for arg in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$arg" in
    -H | --host | -c | --context | --config | -l | --log-level) skip=1 ;;
    -*) ;;
    *) verb="$arg"; break ;;
  esac
done
case "$verb" in
  run | create) ;;
  *) exec "$skein_docker" "$@" ;;
esac

# The caller's own --cgroup-parent wins. Anything else would be skein quietly overruling a request
# somebody made on purpose, on a flag whose whole point is placement.
for arg in "$@"; do
  case "$arg" in --cgroup-parent | --cgroup-parent=*) exec "$skein_docker" "$@" ;; esac
done

# Rebuilt rather than prepended to: the stamps have to land AFTER the verb, since docker refuses
# them as global options, and the verb is not always $1.
#
# Rotated rather than accumulated in a variable, because an argument list is not a string: a
# container command with spaces, quotes or newlines in it is ordinary — `docker run img sh -c "a b"`
# — and rebuilding through `$(...)` would split it. Take the first off the front, put it on the
# back, exactly $# times: the order comes back unchanged, with the stamps inserted where the verb
# passed by.
placed=0
seen=0
count=$#
while [ "$seen" -lt "$count" ]; do
  arg="$1"
  shift
  set -- "$@" "$arg"
  if [ "$placed" = 0 ] && [ "$arg" = "$verb" ]; then
    set -- "$@" --label "skein.box=$skein_box" \
      --cgroup-parent "/skein/containers/$skein_box"
    placed=1
  fi
  seen=$((seen + 1))
done
exec "$skein_docker" "$@"
DOCKERSHIM
  } > "$root/bin/docker" || exit 1
  chmod 755 "$root/bin/docker" || exit 1
  binds+=(--ro-bind "$docker_real" "$root/bin/docker.real")
  binds+=(--ro-bind "$root/bin/docker" "$docker_real")
fi
unset docker_real

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

# Where the host writes what only the host may write. The state directory is bound READ-ONLY into
# this namespace, which is what makes it useful here: a message skein puts in `$SKEIN_STATE/inbox`
# is one no box can have written, and that is the whole of how "from you" is told apart from "from
# another box" (architecture §9.5 R10). The shared store's `mailbox/` stays writable and stays where
# box-to-box messages go.
export SKEIN_STATE="$state"

# That this is a box at all, for the one reader that needs to know (SKEIN-1086).
#
# `apiauth::off_switch_refused` refuses `$SKEIN_NO_API_AUTH` for a `skein-server` started in here:
# a box shares the fleet's network namespace, so a server it starts with auth off answers every
# other box exactly as the cockpit would (architecture §9.4). That used to be decided by the
# doorway's `SKEIN_LISTEN_INHERITED_ONLY` leaking in with the cockpit's environment, which SKEIN-972
# stopped. This says it on purpose instead. Not `SKEIN_BOX`: that names which box, is set by the
# crossings as well, and a test or a tool may reasonably set it outside one.
export SKEIN_IN_BOX=1

# Where this box's agent keeps its model scratch, instead of letting the CLI derive one from /tmp.
#
# Claude Code puts its temp directory at `${os.tmpdir()}/claude-<uid>` and REFUSES to start when
# that path exists and is not owned by the calling uid — a deliberate guard against a directory
# somebody else planted. Skein already argued this for the model calls it makes itself
# (`fleet::MODEL_SCRATCH`); a box is the other half, and it is the half a person watches. The box's
# `/tmp` is private (bound from `$tmp` below), so what this buys is not privacy from the other
# boxes: it is that a box no longer depends on nothing having got to a shared path first, which is
# the same reasoning at a different entry point rather than a second mechanism.
#
# The VALUE comes from skein — `fleet::MODEL_SCRATCH`, passed as `SKEIN_MODEL_SCRATCH` — so the path
# is defined once and this script only joins it to a HOME. That join has to happen HERE: `$HOME` is
# the box's own private home, bound over the sandbox's a few lines down, and no shell outside this
# namespace can name it. Exported beside `SKEIN_BOX` and for the same reason — the agent, its hooks
# and everything they fork are children of the tmux server started below.
#
# Absent means an older skein started this box, and then nothing is exported and the box behaves
# exactly as it did before. An empty export would be worse than none: `CLAUDE_CODE_TMPDIR=$HOME/`
# is a directory skein never meant to name.
if [ -n "${SKEIN_MODEL_SCRATCH-}" ] && [ -n "${HOME:-}" ]; then
  export CLAUDE_CODE_TMPDIR="$HOME/$SKEIN_MODEL_SCRATCH"
fi
unset SKEIN_MODEL_SCRATCH

# --dev-bind / / keeps the sandbox's own filesystem visible (the repo, the toolchains, the store
# mount) and then binds the box's private directories over the two paths that must not be shared.
# No --unshare-pid: the pid recorded below has to be the pid skein sees from outside, or nsenter has
# nothing to address.
# The binary that reports the anchor, resolved here and by absolute path.
#
# The line that reports it runs in the shell inside the box, whose PATH begins with the box's own
# `~/.local/bin`. That was shared read-write with every box in the fleet when this was written and
# is a per-box overlay now (see `overlay_paths` above), so an unqualified `tmux` there is no longer
# a binary ANY box can replace — but it is still one THIS box can, into its own upper layer, and
# the pid skein addresses this box by would be whatever that binary chose to print. The overlay
# narrows the blast radius and does not remove the reason. Resolved out here instead,
# before any box's namespace exists, against this script's own PATH — which is fixed and
# root-owned at the top of the file, so the one-line override that used to sit on this command is
# now what every command here gets.
#
# **The `~/.local/bin` in the sentence above is `box_path`, not a profile.** It said "a LOGIN shell
# … whose PATH includes `~/.local/bin`" and meant the profile, which puts nothing there on this
# substrate (see the measurement at the top of the file) — so for as long as that was the reason,
# the reason was false and the conclusion was right by accident. It is `export PATH="$box_path"` in
# that block that makes it true, and this resolution is what stops a box choosing the tmux whose pid
# skein would then address it by.
tmux_bin="$(command -v tmux || true)"
[ -n "$tmux_bin" ] || { echo "skein: no tmux on the sandbox's own PATH, so this box has no session" >&2; exit 1; }

# Reported over the channel skein opened, beside the anchor below and for the same reason: this is
# the only moment the answer exists. The namespace is about to be made, and afterwards nothing on
# the host can ask which script made it — so it is said here and recorded in the placement.
printf "SKEIN_LAUNCHER %s\n" "$launcher_revision"

# SURFACE 1 — the box's own terminal (SKEIN-846).
#
# The banner decided ~400 lines up has not been anywhere a person looks yet, and *this* is the pty
# that belongs to the box rather than to the command that started it: the tmux pane below is what
# `skein attach` attaches to and what anyone opening a shell in this box lands in. The `echo` up
# there happens in the launcher, long before this namespace or this pty exist, so it could never
# have reached here however it was redirected.
#
# Spelled as a wrapper around the pane's own command rather than a second tmux call, because tmux
# has no way to write into a pane it is not running: `send-keys` would TYPE the sentence at the
# shell, which executes it, and `display-message` is a status flash that is gone before anybody
# attaches. Printing it as the pane's first act puts it at the top of that pane's scrollback, where
# it stays for the life of the box.
#
# `sh -c SCRIPT NAME ARGS…` makes NAME `$0` and ARGS `$@`, so the sentence rides as `$0` and needs no
# quoting into the single-quoted bwrap block below — which is why the array is built out here, where
# apostrophes are still allowed, and passed through as ordinary positionals.
pane_cmd=("$@")
if [ -n "$announce" ]; then
  pane_cmd=(sh -c 'printf "\n%s\n\n" "$0"; exec "$@"' "$announce" "$@")
fi

exec bwrap \
  --dev-bind / / \
  --bind "$tmp" /tmp \
  "${binds[@]}" \
  -- \
  bash -lc '
    tmux_bin="$1"; session="$2"; sock="$3"; pidfile="$4"; tree="$5"; box_path="$6"; shift 6
    # The box own PATH, decided at the top of this file and handed across as a positional.
    #
    # HERE and not before the exec, and not with bwrap --setenv, for the two reasons written up
    # there: this is a LOGIN shell, so the profile has already been sourced by the time this line
    # runs and an export here is the only one of the three spellings a profile cannot undo — and
    # setting it before the exec would resolve bwrap itself through a box-writable directory.
    #
    # What it reaches is everything, which is the point. tmux inherits it, the server the next line
    # starts inherits it from tmux, and the pane command -- the agent -- inherits it from the
    # server. That chain is the box agent session, and before this line it ran on the fixed six
    # with no ~/.local/bin in it (SKEIN-851).
    export PATH="$box_path"
    # The agent CLI does not update ITSELF in a box; skein updates it for every box at once.
    #
    # This is the half of SKEIN-968 that faces the person rather than the attacker. The `--ro-bind`
    # of /usr/local/share/npm-global up in the bind list is what makes a box unable to replace the
    # agent every other box runs; on its own it turns a silent background write into a silent
    # background FAILURE, and Claude Code surfaces those — so every box would carry a red update
    # line about a directory it is never again allowed to write. A guard that makes the tool
    # complain is a guard people learn to read past.
    #
    # Both names, and both are read out of the binary rather than remembered. `strings` on
    # `/usr/local/share/npm-global/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe`
    # (2.1.278) describes them in its own words: DISABLE_AUTOUPDATER "turns off BACKGROUND
    # auto-updates only", and under DISABLE_UPDATES it will "report the stale version but propose
    # nothing -- that is an admin decision (`claude update` refuses under DISABLE_UPDATES)". The
    # first stops the unasked-for write; the second turns a hand-typed `claude update` into one
    # clear refusal instead of an npm permission error nobody can act on. Neither hides the
    # version: a box is still told when it is behind, which is what must stay true while the
    # installing moves to skein.
    #
    # Exported HERE for the reason the paragraph above gives about PATH -- a login shell sources
    # the profile before it runs this command string, so an export in this block is the one
    # spelling a profile cannot undo -- and it reaches the agent down the same tmux chain.
    export DISABLE_AUTOUPDATER=1 DISABLE_UPDATES=1
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
  ' bash "$tmux_bin" "$session" "$sock" "$pidfile" "$tree" "$box_path" "${pane_cmd[@]}"
