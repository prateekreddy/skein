#!/usr/bin/env bash
# skein-startup.sh — durable kit startup hook (runs whenever the sandbox starts).
# Repo-agnostic: finds the skein-managed shared store (mounted at its absolute host path),
# checks out the branch skein recorded for this box, and links the store into the clone so
# Claude loads the project settings/skills/hooks + skein's probe. Idempotent; missing required
# session/probe tools block agent startup with a diagnostic rather than degrading silently.
#
# brace-free reads only: the sbx kit resolver scans initFiles content for dollar-brace
# placeholders and rejects any it doesn't recognise (it supports only WORKDIR). printenv
# reads the env without braces and exits nonzero when unset, safe under `set -u`.
set -uo pipefail

# `sbx create` returns while durable startup is still running. The first Skein attach waits
# on this provider-neutral handshake so it cannot race dependency or hook installation.
#
# The markers carry the START's id when there is one. In the fleet, /tmp is the box's $root/tmp on
# disk and a restart keeps it, so a bare `ready` from the previous start would satisfy the next
# start's wait before this script had done anything — and a leftover `failed` would refuse starts
# it knows nothing about. The launcher writes `/tmp/skein-start-id` fresh on every launch
# (box-session.sh), this suffixes both markers with it, and the setup wait
# (`fleet::initial_setup_wait`) reads the same id — so a stale marker is inert rather than deleted,
# which matters because deleting is not an option: a persistent `failed` is tested FIRST by the
# wait, and a start that cleaned it up could not be told apart from one that never failed. With no
# id — a per-VM sandbox, whose /tmp dies with it — the bare names carry on meaning what they did.
# $SKEIN_STARTUP_MARKERS is the test seam for the directory; a box never sets it.
markers="$(printenv SKEIN_STARTUP_MARKERS 2>/dev/null || true)"
[ -n "$markers" ] || markers=/tmp
start_id="$(cat "$markers/skein-start-id" 2>/dev/null | tr -cd 'A-Za-z0-9._-' || true)"
suffix=""
[ -z "$start_id" ] || suffix=".$start_id"
startup_ready="$markers/skein-startup.ready$suffix"
startup_failed="$markers/skein-startup.failed$suffix"
startup_done="false"
# Every start's markers, not only this one's: earlier starts' are inert now, so they are litter,
# and /tmp here is disk that a box keeps for as long as it lives.
rm -f "$markers"/skein-startup.ready* "$markers"/skein-startup.failed*
trap '[ "$startup_done" = "true" ] || touch "$startup_failed" 2>/dev/null || true' EXIT

# Direct (non-clone) mode already has an in-repo .claude → nothing to provision.
#
# The test asks "did sbx clone this sandbox", but what it MEANS is "is there a skein-managed tree
# to provision". Those came apart with the shared sandbox: a fleet box's tree IS a clone — skein
# made it, from the remote — inside a sandbox sbx never cloned. So skein says so explicitly rather
# than this hook guessing from a mount that only exists in one of the two shapes.
if [ ! -d /run/sandbox/source ] && [ -z "$(printenv SKEIN_PROVISION 2>/dev/null || true)" ]; then
  touch "$startup_ready"
  startup_done="true"
  exit 0
fi

# The clone root. The durable-startup dispatcher runs this hook from /home/agent/workspace,
# not the clone, so prefer sandboxd's WORKSPACE_DIR; fall back to git/pwd.
clone_root="$(printenv WORKSPACE_DIR 2>/dev/null || true)"
[ -n "$clone_root" ] || clone_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# A repo launched through Skein was explicitly selected by the user, so record that one
# workspace as trusted before Codex starts. This bypasses Codex's separate first-run project
# prompt (hook trust is a different gate) without weakening trust for any other directory.
codex_cfg="$HOME/.codex/config.toml"
if [ -f "$codex_cfg" ]; then
  toml_root="$(printf '%s' "$clone_root" | sed 's/\\/\\\\/g; s/"/\\"/g')"
  project_header="[projects.\"$toml_root\"]"
  if ! grep -Fqx "$project_header" "$codex_cfg" 2>/dev/null; then
    printf '\n%s\ntrust_level = "trusted"\n' "$project_header" >> "$codex_cfg"
  fi
fi

# The helpers this runs from here on are skein's read-only copies, not the store's (SKEIN-1149): the
# store is writable by every box of the repo, so a helper run from there is one a sibling box can
# rewrite and this box then runs as it starts. They are installed with this script, under the fleet
# root's `.skein`, in the plugin variant every box loads whichever way the fleet's switch is set, and
# the launcher binds that directory read-only into every box. Found from this script's own path,
# which is where the fleet runs it from (`fleet::box_provision_path`). A per-VM sandbox runs its
# copy from ~/.local/bin, has no `.skein`, and so goes without these.
skein_probe="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/plugin-turn-state/probe"
# Which box this is, and the first two ways of finding its store: box-self.sh, the one answer every
# script skein ships into a box uses (SKEIN-1174). Without it (a per-VM sandbox, above) the store is
# found by the scan below, and the name is the sandbox's, which there is the box's own.
box_self="false"
# shellcheck disable=SC1091
. "$skein_probe/box-self.sh" 2>/dev/null && box_self="true"

# The box identity, which is the sandbox's own only in the one-box-per-sandbox shape. In a shared
# sandbox every box would otherwise report the same SANDBOX_VM_ID, so they would read each other's
# launch spec and overwrite each other's boot report — one box's diagnosis for all of them. So a box
# in a shared sandbox that cannot say which box it is has no name here: it reads no launch spec and
# writes no boot report, rather than the sandbox's.
if [ "$box_self" = "true" ]; then
  vmid="$(skein_box_name)" || vmid=""
else
  # No box-self.sh means no `.skein`, so no fleet launcher either: box-self.sh's own rule for that
  # world, which is the only one this branch can be in.
  vmid="$(printenv SKEIN_BOX 2>/dev/null || true)"
  [ -n "$vmid" ] || vmid="$(printenv SANDBOX_VM_ID 2>/dev/null || hostname 2>/dev/null || echo unknown)"
  vmid="$(printf '%s' "$vmid" | tr / -)"
fi

# Find the skein store: skein writes a per-box launch spec at <store>/skein/launch/<vmid>.json,
# so the store is the one mounted dir that contains that marker. Scanning mounts (rather than
# guessing a sibling path) works for any repo layout — URL-cloned or local-in-place.
# An explicit override wins outright: the fleet path knows the store exactly, and there the scan
# below would find nothing to scan — a box's store is not a mount of its own, it is a directory
# inside the one workspace the whole sandbox mounts.
#
# Only the provision passes $SKEIN_STORE, and only a box whose store is bound back to it
# (box-session.sh's `--bind "$SKEIN_BOX_STORE"`) has the store as a mount point. The workshop box
# has neither on a restart: it is exempt from the cover, so its store is a directory inside the
# workspace mount. So after $SKEIN_STORE, in this order:
#   * `skein_box_store` (box-self.sh): what skein recorded in this checkout's git directory, then
#     the `.claude/skein` link an earlier start made — its target's parent is the store — and never
#     the checkout's own `.claude`. The answer every probe and the bootstrap get, so the kit cannot
#     link one store while they report into another;
#   * a mount holding this box's launch spec;
#   * a store under any mount, at `<mount>/*/store/.claude` or `<mount>/repos/*/store/.claude`,
#     holding this box's launch spec. This is what finds it on the first start after git replaced
#     the old `.claude` link with the directory the repo tracks.
# A box never adopts another repository's store. When the clone names a repo's mirror as a remote
# (`fleet::clone_script` clones from `<repo>/mirror`), only that repo's store is taken; when it names
# none, one store with this box's launch spec is taken and several are refused.
store="$(printenv SKEIN_STORE 2>/dev/null || true)"
[ -n "$store" ] && [ -d "$store" ] || store=""
# The host named it, so it is recorded where box-self.sh looks first: the checkout's git directory,
# beside this script's other markers there. The launcher writes the same record at every start.
if [ -n "$store" ]; then
  record_dir="$(git -C "$clone_root" rev-parse --git-common-dir 2>/dev/null || true)"
  case "$record_dir" in "") ;; /*) ;; *) record_dir="$clone_root/$record_dir" ;; esac
  [ -z "$record_dir" ] || printf '%s\n' "$store" > "$record_dir/skein-store" 2>/dev/null || true
fi
store_note=""
if [ -z "$store" ] && [ "$box_self" = "true" ]; then
  store="$(skein_box_store "$clone_root")" || store=""
fi
if [ -z "$store" ] && [ -r /proc/self/mountinfo ]; then
  while read -r mp; do
    [ -n "$mp" ] || continue
    if [ -f "$mp/skein/launch/$vmid.json" ]; then store="$mp"; break; fi
  done < <(awk '{print $5}' /proc/self/mountinfo 2>/dev/null | sort -u)
fi
if [ -z "$store" ] && [ -r /proc/self/mountinfo ]; then
  specs=""
  while read -r mp; do
    [ "$mp" = / ] && mp=""
    for cand in "$mp"/*/store/.claude "$mp"/repos/*/store/.claude; do
      [ -f "$cand/skein/launch/$vmid.json" ] && specs="$specs$cand
"
    done
  done < <(awk '{print $5}' /proc/self/mountinfo 2>/dev/null | sort -u)
  specs="$(printf '%s' "$specs" | sort -u)"
  remotes="$(git -C "$clone_root" remote -v 2>/dev/null | awk '{print $2}' | sort -u)"
  # Every repo, spec or not, whose mirror this clone was made from: the one store it may adopt.
  mine=""
  while read -r mp; do
    [ "$mp" = / ] && mp=""
    for cand in "$mp"/*/store/.claude "$mp"/repos/*/store/.claude; do
      [ -d "$cand" ] || continue
      printf '%s\n' "$remotes" | grep -qxF "$(dirname "$(dirname "$cand")")/mirror" \
        && mine="$cand"
    done
  done < <(awk '{print $5}' /proc/self/mountinfo 2>/dev/null | sort -u)
  others="$(printf '%s\n' "$specs" | grep -vxF "$mine" | grep . | tr '\n' ' ' | sed 's/ $//')"
  if [ -n "$mine" ]; then
    if printf '%s\n' "$specs" | grep -qxF "$mine"; then
      store="$mine"
      [ -z "$others" ] \
        || store_note="found this box's store by its launch spec, beside the mirror this clone was made from; not adopted, because they are another repository's: $others"
    elif [ -n "$others" ]; then
      echo "[skein-kit] not adopting another repository's store: this clone was made from $(dirname "$(dirname "$mine")")/mirror, and $vmid's launch spec is only in $others" >&2
    fi
  elif [ -n "$specs" ] && [ "$(printf '%s\n' "$specs" | grep -c .)" = 1 ]; then
    store="$specs"
  elif [ -n "$specs" ]; then
    echo "[skein-kit] more than one repository's store has a launch spec for $vmid, and this clone names none of their mirrors, so none is adopted: $others" >&2
  fi
fi
# Last resort: the thing-style sibling convention.
[ -n "$store" ] && [ -d "$store" ] || store="$(dirname "$clone_root")/store/.claude"

# Read the launch spec skein recorded for this box.
# jq-free by design: the agent image may not ship jq, and a missed branch here is exactly the
# "box stuck on the base branch" failure — so parse the small skein-written JSON with sed/grep
# when jq is absent rather than silently skipping the checkout.
spec="$store/skein/launch/$vmid.json"
branch=""
selected_agent=""
if [ -r "$spec" ]; then
  if command -v jq >/dev/null 2>&1; then
    branch="$(jq -r '.branch // ""' "$spec" 2>/dev/null)"
    selected_agent="$(jq -r '.agent // ""' "$spec" 2>/dev/null)"
  else
    branch="$(sed -n 's/.*"branch"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$spec" | head -n1)"
    selected_agent="$(sed -n 's/.*"agent"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$spec" | head -n1)"
  fi
else
  echo "[skein-kit] no launch spec at $spec — staying on the clone's default branch" >&2
fi

# Check out the branch skein recorded for this box (creating it if it's new) — but ONLY on
# the box's first-ever startup. This hook re-runs on every `sbx run` including reattaches
# (see the top comment), while the launch spec is written once at creation time. Without a
# once-only guard, an agent that switches branches mid-session (e.g. branch-per-slice work)
# gets silently reverted to the creation branch on its next reconnect — a real incident:
# unattributed `git checkout` in the reflog clobbering an active slice branch. The marker
# lives in $clone_root/.git (persists exactly as long as the clone's branch state does), so
# "first startup" and "branch not yet asserted" are the same condition.
marker="$clone_root/.git/skein-branch-checked-out"
if [ -n "$branch" ] && [ ! -e "$marker" ]; then
  git -C "$clone_root" checkout "$branch" 2>/dev/null \
    || git -C "$clone_root" checkout -b "$branch" 2>/dev/null \
    || echo "[skein-kit] could not checkout $branch" >&2
  touch "$marker" 2>/dev/null || true
fi

# Install the box-side tools the agent image may lack — one apt pass for whatever's missing:
#   jq   — the bootstrap's registry writer (sandboxes.json) and the launch-spec read both use
#          it; the sed/grep fallback above only covers the branch, so without jq a box still
#          never registers. Always ensure it.
#   tmux — the persistent shell/agent sessions (`tmux new-session -A`). Without it, a UI
#          reload would kill or fork in-progress work, so it is part of the box contract.
# Installation is best-effort until the boot report is written, then missing tools fail the
# startup. That keeps the failure diagnosable without ever launching an unsafe direct agent.
# In a fleet box this never fires — `ensure_substrate` installs both into the shared sandbox before
# any box exists, which is just as well: a box runs in an unprivileged user namespace, where a
# setuid `sudo` has nothing to escalate to. The verification below still applies, and is what turns
# that into a diagnostic rather than an agent starting without a session to live in.
need=""
command -v jq >/dev/null 2>&1 || need="$need jq"
command -v tmux >/dev/null 2>&1 || need="$need tmux"
# >>> package-install.sh — a byte-for-byte copy of src/package-install.sh; edit that file, not this
# skein's one package install. Every place skein installs a package runs these two functions:
# the fleet sandbox's substrate at each launch and an approved package request
# (src/fleet/substrate.rs, src/substrate.rs), a runtime update, a takeover's source box
# (src/takeover.rs), a per-VM box's startup kit (src/kit/skein-startup.sh) and bootstrap.sh.
#
# The Rust callers embed this file with include_str! and put their own lines after it. The two
# shell callers run before any skein binary exists, so they carry it byte for byte between the
# `>>> package-install.sh` and `<<< package-install.sh` marker lines instead, and
# `fleet::substrate`'s `every_package_install_runs_the_same_bytes` fails when either copy differs.
# Edit this file, then paste it into both.
#
# They used to be five copies, and they had drifted: two lock waits, three install timeouts,
# a retry in one, a log kept, deleted or thrown away, and npm run as root in all but one.
#
# POSIX sh, and definitions only: sourcing it runs nothing. Each function appends apt's or npm's
# own output to the LOG it is given, because that output is the only thing that says whether the
# mirror, the lock, sudo or the package name was the problem, and returns the tool's own status.
# What a failure MEANS (a failed launch, a refused request, a warning) is the caller's decision.

# skein_apt_install LOG PACKAGE...
skein_apt_install() {
  _skein_log=$1
  shift
  # A fresh sandbox or agent image runs its own first-boot apt, and apt refuses to run twice.
  # Outlast it rather than fail on a race: measured on a real rebuild, where the retry landed on
  # "Could not get lock ... held by process 281 (apt-get)". Two ways of seeing it, because each
  # misses a case: `fuser` is absent from images without psmisc, and between its update and its
  # install an image's own apt holds no lock at all while its process is still running. grep reads
  # all of ps rather than `-q`, which can quit early and fail the pipe under `pipefail`. An image
  # without `fuser`, `ps` or `grep` sees no lock through that one, and does not wait on it.
  _skein_waited=0
  while [ "$_skein_waited" -lt 120 ]; do
    if sudo -n fuser /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock >/dev/null 2>&1 \
      || ps -eo comm= 2>/dev/null | grep -E '^[[:space:]]*(apt|apt-get|dpkg)[[:space:]]*$' >/dev/null 2>&1; then
      sleep 3
      _skein_waited=$((_skein_waited + 3))
    else
      break
    fi
  done
  {
    _skein_bounded 120 sudo -n apt-get update -qq
    _skein_bounded 180 sudo -n apt-get install -y -qq "$@"
  } >>"$_skein_log" 2>&1 && return 0
  _skein_rc=$?
  # A timeout is not retried: the same mirror gets the same time again and the caller's own
  # deadline runs out first. Anything else, the lock race above most of all, gets one more install.
  # The index is already fetched, so it is not fetched again.
  [ "$_skein_rc" -eq 124 ] && return 124
  sleep 5
  _skein_bounded 180 sudo -n apt-get install -y -qq "$@" >>"$_skein_log" 2>&1
}

# Why these bounds, which every caller shares. `update` FIRST, every time: a fresh image ships an
# empty index, where install reports "Package 'tmux' has no installation candidate", which reads as
# a missing package and is a missing index. `;` rather than `&&` after it: one unreachable source
# fails `update` for the whole index, and the packages wanted may well be on the sources that
# answered. Install's status is the verdict.
#
# `sudo -n`: nothing here can answer a password prompt, and a prompt nobody answers waits out the
# whole timeout before failing.
#
# The numbers are set by the tightest deadline around them, not by the slowest mirror: at worst
# 120 waiting + 120 + 180 + 5 + 180 = 605s of apt, and the fleet's launch runs npm after it inside
# one 900s `sbx exec`, so npm has 240 of what is left. Longer bounds here are a deadline that fires
# on the caller's side instead, where it says nothing about apt. `fleet::start`'s
# `the_provisioning_budget_outlasts_the_script` reads them out of the startup kit's copy.

# `timeout SECS COMMAND...` where the image has `timeout`, and the command unbounded where it does
# not. coreutils is essential on Debian, so that is rare, but bootstrap.sh runs on whatever image
# the sandbox was made from and a missing `timeout` would otherwise fail every install on it.
_skein_bounded() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "$@"
  else
    shift
    "$@"
  fi
}

# skein_npm_install LOG PACKAGE...
#
# Into the prefix a box runs from, which is not the one `sudo npm` writes (SKEIN-968): root's
# global prefix is /usr/local, while every box's PATH leads with the npm-global prefix, owned by
# the sandbox's user. Installed as root, a runtime lands where no box looks, and `npm ls -g`,
# asked as that user, says it is missing at every launch. Unprivileged when the configured
# prefix is writable, `sudo -n` when it is not, as on a plain image with a root-owned prefix.
skein_npm_install() {
  _skein_log=$1
  shift
  _skein_prefix="$(npm config get prefix 2>/dev/null)"
  case "$_skein_prefix" in undefined | null) _skein_prefix="" ;; esac
  if [ -n "$_skein_prefix" ] && [ -w "$_skein_prefix" ]; then
    _skein_bounded 240 npm install -g "$@" >>"$_skein_log" 2>&1
  else
    _skein_bounded 240 sudo -n npm install -g "$@" >>"$_skein_log" 2>&1
  fi
}
# <<< package-install.sh
if [ -n "$need" ] && command -v apt-get >/dev/null 2>&1; then
  apt_log="$(mktemp)"
  if skein_apt_install "$apt_log" $need; then
    # Package indexes dwarf jq itself and are useless after the one setup pass.
    sudo -n rm -rf /var/lib/apt/lists/* 2>/dev/null || true
  else
    echo "[skein-kit] could not install required tools:$need — apt said:" >&2
    tail -n 25 "$apt_log" | sed 's/^/  | /' >&2
  fi
  rm -f "$apt_log"
fi
tools_ok="true"
missing=""
for tool in jq tmux; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    tools_ok="false"
    missing="$missing $tool"
  fi
done
[ "$tools_ok" = "true" ] \
  || echo "[skein-kit] required box tools unavailable:$missing" >&2

# A cross-runtime replacement carries an immutable snapshot in the mounted store. Restore it
# before either provider starts: the bundle preserves unpushed commits, separate patches
# preserve index vs worktree state, and the archive preserves untracked files. This runs once
# in the NEW target clone only; the source box is never reset, stopped, or destroyed.
handoff_dir=""
if [ -r "$spec" ] && command -v jq >/dev/null 2>&1; then
  handoff_dir="$(jq -r '.handoff.dir // ""' "$spec" 2>/dev/null)"
fi
handoff_marker="$clone_root/.git/skein-handoff-restored"
if [ -n "$handoff_dir" ] && [ ! -e "$handoff_marker" ]; then
  case "$handoff_dir" in
    skein/handoff-snapshots/*) snapshot="$store/$handoff_dir" ;;
    *) echo "[skein-kit] refusing unsafe handoff path: $handoff_dir" >&2; exit 1 ;;
  esac
  [ -s "$snapshot/repo.bundle" ] || { echo "[skein-kit] handoff bundle missing" >&2; exit 1; }
  # Two bundle shapes reach here. A cross-runtime takeover bundles HEAD alone, because it is moving
  # one branch to a new box. A fleet resize bundles --all, because it is reconstructing a box that
  # may have been carrying several local branches — and restoring only HEAD there would silently
  # drop the rest, which looks like a clean box rather than like lost work. So prefer every branch
  # and fall back, rather than assuming either shape.
  restored=""
  if git -C "$clone_root" fetch "$snapshot/repo.bundle" \
       'refs/heads/*:refs/remotes/snapshot/*' >/dev/null 2>&1 \
     && git -C "$clone_root" rev-parse --verify -q "refs/remotes/snapshot/$branch" >/dev/null; then
    git -C "$clone_root" checkout -B "$branch" "refs/remotes/snapshot/$branch" 2>/dev/null \
      && restored="all-branches"
  fi
  if [ -z "$restored" ]; then
    git -C "$clone_root" fetch "$snapshot/repo.bundle" HEAD >/dev/null 2>&1 \
      && git -C "$clone_root" checkout -B "$branch" FETCH_HEAD 2>/dev/null \
      && restored="head"
  fi
  # A bundle with no ref this clone can use is fatal ONLY if the clone is not already there. The
  # snapshot records the commit it was taken at, so that is answerable rather than a guess: when the
  # fresh clone's HEAD is already that commit, the bundle had nothing to add and the patches below
  # are the whole remaining restore. Skipping the check would risk silently dropping unpushed
  # commits; without it, a box whose branch was fully pushed could not be restored at all — its
  # snapshot was complete and its migration failed anyway, with the old sandbox already stopped.
  if [ -z "$restored" ]; then
    want="$(sed -n 's/.*"head"[[:space:]]*:[[:space:]]*"\([0-9a-f]*\)".*/\1/p' \
              "$snapshot/manifest.json" 2>/dev/null)"
    have="$(git -C "$clone_root" rev-parse HEAD 2>/dev/null)"
    if [ -n "$want" ] && [ "$want" = "$have" ]; then
      git -C "$clone_root" checkout -B "$branch" HEAD >/dev/null 2>&1
      echo "[skein-kit] snapshot commit is already this clone's HEAD; restoring working state only" >&2
    else
      echo "[skein-kit] could not restore handoff commit" >&2
      exit 1
    fi
  fi
  if [ -s "$snapshot/index.patch" ]; then
    git -C "$clone_root" apply --binary --index "$snapshot/index.patch" \
      || { echo "[skein-kit] could not restore staged changes" >&2; exit 1; }
  fi
  if [ -s "$snapshot/worktree.patch" ]; then
    git -C "$clone_root" apply --binary "$snapshot/worktree.patch" \
      || { echo "[skein-kit] could not restore unstaged changes" >&2; exit 1; }
  fi
  if [ -s "$snapshot/untracked.tgz" ]; then
    tar -C "$clone_root" -xzf "$snapshot/untracked.tgz" \
      || { echo "[skein-kit] could not restore untracked files" >&2; exit 1; }
  fi
  # The conversation, where the snapshot carried one. A fleet resize rebuilds a box at the same path
  # it had before, so the transcript's cwd slug still addresses it and the runtime's own --continue
  # finds the session rather than opening a new one against a familiar-looking tree. Extracted OVER
  # the seeded HOME, so the credentials seeded from the sandbox stay as they are — the snapshot
  # deliberately carries no auth of its own (see fleet::agent_state_tar).
  if [ -s "$snapshot/agent-state.tgz" ]; then
    tar -C "$HOME" -xzf "$snapshot/agent-state.tgz" \
      || echo "[skein-kit] could not restore the previous conversation; the tree is intact" >&2
  fi
  cp "$snapshot/manifest.json" "$handoff_marker" 2>/dev/null \
    || printf '%s\n' "$handoff_dir" > "$handoff_marker"
  echo "[skein-kit] restored replacement snapshot from $handoff_dir"
fi

# Link the shared store into the clone (idempotent) so project .claude + skein's probe resolve.
# `.claude` is a directory of this box's own in both layouts (SKEIN-1053), and every run leaves a
# boot report, because a silent miss here is the #1 way a box ends up with zero hooks while
# looking perfectly healthy:
#   1. the repo tracks nothing under .claude/ → skein makes the directory and links each of the
#      store's entries into it, one by one — memory/, skills/, mailbox/, skein/ and the rest — and
#      leaves out the store's `settings.json` and `settings.local.json`. Every box of the repo can
#      write the store, so a settings file read from there is one where a box plants a hook or a
#      status line that its siblings run (SKEIN-1153);
#   2. the repo tracks files under .claude/ (committed settings, commands, skills) → link only the
#      store's skein/ into it (the probes resolve the true store through that link). A link an
#      earlier start made for layout 1 goes, the link and never what it points at.
# In both, skein's settings go into `.claude/settings.local.json`: untracked, this box's own, and
# never the repo's `settings.json`, which the repo may track and which is left exactly as it is
# (SKEIN-1048). What goes in is skein's `tui` and `statusLine` defaults, read from the plugin's
# read-only `settings-defaults.json` and never from the store. Each default goes in only where
# neither file sets one, except the status line a past merge copied into `settings.json`, which ran
# the store's renderer and which the local file now overrides. The same pass RETIRES every skein
# hook a past merge copied into either file: skein's turn-state hooks load from its read-only
# plugin now (SKEIN-1062), and a copy left here would fire each of them twice. "skein's" is the
# rule `takeover::sanitized_user_hooks` uses: a command that runs something under
# `/.claude/skein/bin/`, skein's own namespace in the store.
#
# A clone whose `.claude` is still the store's own link — every box skein made before SKEIN-1053 —
# is converted: the link is removed, never the store it points at, and the layout above is built in
# its place. Everything the box reached through the link it still reaches, through the entries
# linked one by one, except the store's two settings files. In layout 2, an untracked
# `settings.json` a past merge created is moved aside to `settings.json.skein-old`, once, which
# Claude does not load; nothing is deleted.
link_state="failed"
claude_note=""
rc="$clone_root/.claude"
if [ ! -d "$store" ]; then
  link_state="no-store"
  echo "[skein-kit] no store found — probes will not report; check the launch spec mount" >&2
else
  store_real="$(cd "$store" 2>/dev/null && pwd -P)"
  converted="false"
  if [ -L "$rc" ] && [ "$(cd "$rc" 2>/dev/null && pwd -P)" = "$store_real" ]; then
    rm -f "$rc" && converted="true"
  fi
  if [ -L "$rc" ]; then
    # A link to somewhere other than this repo's store is somebody's own choice, and stays.
    link_state="linked"
  else
    tracked="$(git -C "$clone_root" ls-files -- .claude 2>/dev/null | head -n 1)"
    mkdir -p "$rc" 2>/dev/null || true
    if [ "$converted" = "true" ]; then
      claude_note="converted .claude from the store's link to a directory of this box's own; the store's entries are linked into it, except settings.json and settings.local.json, which are now this box's own"
      # The files the repo tracks there, which the link stood in the way of.
      git -C "$clone_root" ls-files -z --deleted -- .claude 2>/dev/null \
        | xargs -0 -r git -C "$clone_root" checkout -- 2>/dev/null || true
    fi
    if [ -n "$tracked" ]; then
      for entry in "$rc"/*; do
        name="$(basename "$entry")"
        [ "$name" != "skein" ] && [ -L "$entry" ] && [ "$(readlink "$entry")" = "$store/$name" ] \
          && rm -f "$entry"
      done
      if [ -f "$rc/settings.json" ] && [ ! -L "$rc/settings.json" ] \
        && [ ! -e "$rc/settings.json.skein-old" ] \
        && ! git -C "$clone_root" ls-files --error-unmatch -- .claude/settings.json >/dev/null 2>&1; then
        if mv "$rc/settings.json" "$rc/settings.json.skein-old" 2>/dev/null; then
          moved="moved an untracked .claude/settings.json aside to .claude/settings.json.skein-old, which Claude does not load; nothing was deleted"
          if [ -n "$claude_note" ]; then claude_note="$claude_note; $moved"; else claude_note="$moved"; fi
        fi
      fi
      # A real `.claude/skein` directory holding nothing but boot reports is what the bootstrap left
      # when it took the checkout's `.claude` for the store (SKEIN-1174), and it would block the
      # link below on every start. Moved aside once, as the settings file above is; nothing is
      # deleted, and a directory holding anything else, or one the repo tracks, is left alone.
      if [ -d "$rc/skein" ] && [ ! -L "$rc/skein" ] && [ ! -e "$rc/skein.skein-old" ] \
        && ! git -C "$clone_root" ls-files --error-unmatch -- .claude/skein >/dev/null 2>&1 \
        && [ -z "$(find "$rc/skein" -mindepth 1 ! -path "$rc/skein/boot" ! -path "$rc/skein/boot/*" -print -quit 2>/dev/null)" ]; then
        if mv "$rc/skein" "$rc/skein.skein-old" 2>/dev/null; then
          moved="moved .claude/skein, a directory of boot reports an earlier start wrote into this checkout, aside to .claude/skein.skein-old so the store could be linked; nothing was deleted"
          if [ -n "$claude_note" ]; then claude_note="$claude_note; $moved"; else claude_note="$moved"; fi
        fi
      fi
      [ -e "$rc/skein" ] || [ -L "$rc/skein" ] || ln -s "$store/skein" "$rc/skein" 2>/dev/null || true
    else
      for entry in "$store"/*; do
        [ -e "$entry" ] || continue
        name="$(basename "$entry")"
        case "$name" in settings.json|settings.local.json) continue ;; esac
        [ -e "$rc/$name" ] || [ -L "$rc/$name" ] || ln -s "$store/$name" "$rc/$name" 2>/dev/null || true
      done
    fi
    defaults="$skein_probe/settings-defaults.json"
    if [ -L "$rc/skein" ] && [ -r "$defaults" ] && command -v jq >/dev/null 2>&1; then
      retire='def unskein: with_entries(.value |= (if type == "array" then
            map(if (.hooks | type) == "array"
              then .hooks |= map(select((.command // "" | tostring | contains("/.claude/skein/bin/")) | not))
              else . end)
            | map(select((.hooks | type) != "array" or (.hooks | length) > 0))
          else . end))
          | with_entries(select((.value | type) != "array" or (.value | length) > 0));
        def retire: if (.hooks | type) == "object" then .hooks |= unskein else . end;'
      shared="/dev/null"
      [ -f "$rc/settings.json" ] && shared="$rc/settings.json"
      [ -f "$rc/settings.local.json" ] || echo '{}' > "$rc/settings.local.json"
      merged="$(jq -s "$retire"'
        (.[0] | if type == "object" then . else {} end | retire) as $l
        | .[1] as $d | (.[2] // {}) as $s
        | $d.settings.statusLine as $line
        | $l
        | (if .tui == null and $s.tui == null then .tui = $d.settings.tui else . end)
        | (if .statusLine.command == $d.storeEraStatusLine then .statusLine.command = $line.command
           elif .statusLine == null and ($s.statusLine == null or $s.statusLine.command == $d.storeEraStatusLine)
           then .statusLine = (($s.statusLine // $line) + {command: $line.command})
           else . end)
      ' "$rc/settings.local.json" "$defaults" "$shared" 2>/dev/null)"
      if [ -n "$merged" ]; then
        if [ "$(printf '%s' "$merged" | jq -S -c .)" = "$(jq -S -c . "$rc/settings.local.json" 2>/dev/null)" ]; then
          link_state="merged"
        else
          tmp="$(mktemp "$rc/.settings.XXXXXX" 2>/dev/null)" \
            && printf '%s\n' "$merged" > "$tmp" && mv "$tmp" "$rc/settings.local.json" \
            && link_state="merged"
        fi
      fi
      # The repo's own settings file is rewritten only to take out a hook a past merge copied in, and
      # never merely reformatted: a file the repo tracks is otherwise left byte for byte.
      if [ "$shared" != "/dev/null" ]; then
        retired="$(jq "$retire"' retire' "$shared" 2>/dev/null)"
        if [ -n "$retired" ] \
          && [ "$(printf '%s' "$retired" | jq -S -c .)" != "$(jq -S -c . "$shared")" ]; then
          tmp="$(mktemp "$rc/.settings.XXXXXX" 2>/dev/null)" \
            && printf '%s\n' "$retired" > "$tmp" && mv "$tmp" "$shared"
        fi
      fi
    fi
    if [ -n "$tracked" ]; then
      [ "$link_state" = "merged" ] \
        || echo "[skein-kit] repo ships .claude/ and the settings merge failed — probes will not report" >&2
    elif [ -L "$rc/skein" ]; then
      link_state="linked"
    else
      link_state="failed"
      echo "[skein-kit] could not link $store -> $clone_root/.claude" >&2
    fi
  fi
fi
# The mounted store is infrastructure, never a worktree change. Exclude it immediately;
# waiting for SessionStart is too late when a runtime blocks on first-run trust.
#
# What is excluded is what skein put there, and only that (SKEIN-1049). Where the repo tracks
# nothing under `.claude`, that is the whole of it. Where it does, it is the `skein` link, this box's
# `settings.local.json` and a `settings.json.skein-old` moved aside: excluding the directory would
# hide a file a contributor adds there, such as a new skill, from `git status` and `git add`. A clone
# that was the first shape and is now the second loses the old `/.claude` line.
git_dir="$(git -C "$clone_root" rev-parse --git-dir 2>/dev/null || true)"
case "$git_dir" in "") ;; /*) ;; *) git_dir="$clone_root/$git_dir" ;; esac
if [ -n "$git_dir" ]; then
  mkdir -p "$git_dir/info" 2>/dev/null || true
  exclude="$git_dir/info/exclude"
  if [ -n "$(git -C "$clone_root" ls-files -- .claude 2>/dev/null | head -n 1)" ]; then
    if grep -qxF '/.claude' "$exclude" 2>/dev/null; then
      { grep -vxF '/.claude' "$exclude" || true; } > "$exclude.skein" \
        && mv "$exclude.skein" "$exclude"
    fi
    for own in /.claude/skein /.claude/settings.local.json /.claude/settings.json.skein-old \
      /.claude/skein.skein-old; do
      grep -qxF "$own" "$exclude" 2>/dev/null || printf '%s\n' "$own" >> "$exclude"
    done
  else
    grep -qxF '/.claude' "$exclude" 2>/dev/null || printf '/.claude\n' >> "$exclude"
  fi
fi

# Expose the project-scoped durable workspace without sharing literal HOME. The helper is
# provider-neutral and refuses to overwrite a real HOME/shared path. Keep startup gated on its
# verified result: silent non-sharing would risk data loss.
shared_home_state="failed"
shared_home_helper="$skein_probe/shared-home.sh"
if [ -r "$shared_home_helper" ] && bash "$shared_home_helper" "$store"; then
  shared_home_state="linked"
else
  echo "[skein-kit] shared home unavailable — agent startup is blocked" >&2
fi

# Put persistent product guidance in the runtime's native instruction file, not a turn hook.
# The adapter manifest owns the paths, so a future runtime adds one registry row rather than
# another provider conditional here. Existing user instructions are preserved around one
# replaceable Skein-managed block.
agent_guide_state="failed"
instruction_file=""
instruction_override=""
if [ -r "$store/skein/runtimes.tsv" ]; then
  while IFS="$(printf '\t')" read -r runtime_id runtime_label runtime_exe runtime_instruction runtime_override; do
    if [ "$runtime_id" = "$selected_agent" ]; then
      instruction_file="$runtime_instruction"
      instruction_override="$runtime_override"
      break
    fi
  done < "$store/skein/runtimes.tsv"
fi
if [ -n "$instruction_file" ] && [ -r "$skein_probe/agent-guide.sh" ] \
  && bash "$skein_probe/agent-guide.sh" "$store" "$instruction_file" "$instruction_override"; then
  agent_guide_state="installed"
else
  echo "[skein-kit] durable agent guidance unavailable — check runtime manifest" >&2
fi

# Codex adapter: install Skein's generated hooks at the USER layer. Project-local .codex
# hooks are suppressed until the clone is trusted; the user layer is independent of project
# trust, and Skein launches Codex with --dangerously-bypass-hook-trust for these vetted
# generated commands. Preserve unrelated user hooks, while replacing older Skein entries so
# upgrades don't double-fire. jq is preferred for the additive merge; a fresh file can be
# installed without it.
codex_hooks_state="absent"
codex_home="$HOME/.codex"
codex_installer="$skein_probe/install-codex-hooks.sh"
if [ -r "$codex_installer" ] && bash "$codex_installer"; then
  codex_hooks_state="installed"
fi

# The same project skills should be available to both runtimes. Claude reads them through
# .claude/skills; Codex discovers user skills under ~/.codex/skills. Link each project skill
# without replacing Codex's own/system skills.
if [ -d "$store/skills" ]; then
  mkdir -p "$codex_home/skills" 2>/dev/null || true
  for skill in "$store/skills"/*; do
    [ -e "$skill" ] || continue
    dst="$codex_home/skills/$(basename "$skill")"
    [ -e "$dst" ] || [ -L "$dst" ] || ln -s "$skill" "$dst" 2>/dev/null || true
  done
fi

# Boot report: one small JSON the cockpit (and `skein doctor`) can read instead of guessing
# why a box is dark. Written into the store when reachable, best-effort, and under the box's own
# name only: a box that cannot say which it is writes none rather than the sandbox's.
if [ -d "$store" ] && [ -n "$vmid" ]; then
  mkdir -p "$store/skein/boot" 2>/dev/null || true
  jqp="false"; command -v jq >/dev/null 2>&1 && jqp="true"
  tmuxp="false"; command -v tmux >/dev/null 2>&1 && tmuxp="true"
  cap=""
  if [ -r "$store/skein/runtimes.tsv" ]; then
    while IFS="$(printf '\t')" read -r runtime_id runtime_label runtime_exe runtime_instruction runtime_override; do
      [ -n "$runtime_id" ] && [ -n "$runtime_exe" ] || continue
      if [ "$runtime_id" = "$selected_agent" ] \
        || command -v "$runtime_exe" >/dev/null 2>&1 \
        || bash -lc 'command -v "$1" >/dev/null 2>&1' bash "$runtime_exe"; then
        [ -n "$cap" ] && cap="$cap,"
        cap="$cap$runtime_id"
      fi
    done < "$store/skein/runtimes.tsv"
  fi
  revision="$(sed -n '1p' "$store/skein/probe-revision" 2>/dev/null || true)"
  if [ -n "$store_note" ]; then
    if [ -n "$claude_note" ]; then claude_note="$store_note; $claude_note"; else claude_note="$store_note"; fi
  fi
  printf '{"ts":"%s","claude_link":"%s","claude_note":"%s","shared_home":"%s","agent_guide":"%s","codex_hooks":"%s","agents":"%s","jq":%s,"tmux":%s,"probe_revision":"%s","branch":"%s"}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$link_state" "$claude_note" "$shared_home_state" "$agent_guide_state" "$codex_hooks_state" "$cap" "$jqp" "$tmuxp" "$revision" "$branch" \
    > "$store/skein/boot/$vmid.json" 2>/dev/null || true
fi
[ "$tools_ok" = "true" ] || exit 1
[ "$shared_home_state" = "linked" ] || exit 1
touch "$startup_ready"
startup_done="true"

# Work tracking: wire this box to the `sync` gateway if credentials are already present.
# The script is skein's read-only copy (reinstalled with this one on every start and heal), so it
# reaches boxes created before it existed too — this line only decides whether a box wires itself up at
# START, which is what makes a NEW box come up already tracking once Skein has provisioned
# its token. A box with no tracker is not a broken box, so this can never gate startup.
#
# **After the marker, and detached — because "cannot gate startup" was still false.** Bounding it
# stopped it FAILING a start; it went on COSTING one. Measured on the owner's fleet, 2026-09-03:
# nine boxes out of nine spent 240s here, every one of them within a second of the others, on trees
# from 130 MB to 1.5 GB. A number that ignores the size of the work is a wall, not work — and it was
# this budget, spent in full and then killed. Not one box on that fleet had a single artifact to
# show for it: no `~/.local/state/skein/sync-*.done`, no marketplace checkout, anywhere. Four
# minutes of every box creation, for nothing.
#
# So the marker is touched first and this runs behind it. `setsid` and the redirections are what
# make backgrounding real: the fleet agent reads the script's output to EOF, so a child still
# holding the pipe would keep the create waiting exactly as before — `&` alone is not detaching.
# `-k` because SIGTERM is a request. The inner `claude` calls bound themselves against
# `$SKEIN_SYNC_BUDGET`, and this outer one only ever fires when one of them declined to die.
sync_install="$skein_probe/sync-install.sh"
if [ -r "$sync_install" ]; then
  sync_budget=240
  if command -v timeout >/dev/null 2>&1; then
    detach=""
    command -v setsid >/dev/null 2>&1 && detach="setsid"
    SKEIN_SYNC_BUDGET=$((sync_budget - 30)) \
      $detach timeout -k 10 "$sync_budget" bash "$sync_install" \
      >"$markers/skein-sync.log" 2>&1 </dev/null &
  else
    echo "[skein-kit] no timeout(1), so tracker wiring cannot be bounded — skipped" >&2
  fi
fi

exit 0
