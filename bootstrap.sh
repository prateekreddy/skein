#!/usr/bin/env bash
#
# The whole of installing skein, run INSIDE the fleet sandbox.
#
#   curl -fsSL https://raw.githubusercontent.com/prateekreddy/skein/HEAD/bootstrap.sh -o bootstrap.sh
#   sbx create --name skein-fleet shell "$HOME/.skein"
#   sbx exec -i skein-fleet bash < bootstrap.sh
#
# Those three lines are the install, and the middle one is the only privileged thing in it. Nothing
# here runs on the host: the host holds one downloaded file and gains no binary, no service, no
# wrapper script and no toolchain (SKEIN-312). `sbx exec` reads this on stdin, which is why it is a
# script a person can read before they run it rather than a pipe into a shell.
#
# It is also **the one implementation of the build**. Fleet creation runs this same file with
# SKEIN_BOOTSTRAP_STOP_AFTER=build, and the cockpit's Update button runs all of it, so an upgrade
# from the cockpit and a first install cannot come out differently — a second copy in Rust would be
# right on the day it was written.
#
# ## Why the toolchain is not the sandbox's own
#
# `box-session.sh` binds `~/.cargo` and `~/.rustup` read-write into EVERY box (architecture §9.2:
# no shared writable path may contain anything another box executes). Building the server with
# those would put the process that holds the fleet's credentials downstream of a compiler any box
# can overwrite — worse than the host build this replaces, where the sandbox's most privileged
# process was the one thing the sandbox did not build. So CARGO_HOME and RUSTUP_HOME are pointed at
# a private toolchain under the fleet root's `.skein`, which the launcher binds read-ONLY into
# boxes. Doing nothing is the unsafe option here, which is why it is spelled out.

set -eu

# Every path skein uses inside the sandbox, derived exactly as `src/fleet/paths.rs` derives them —
# `fleet_root()` and the `.skein` beneath it. A test asserts these agree with the Rust rather than
# trusting this comment.
fleet_root="${SKEIN_FLEET_ROOT:-/boxes}"
skein_dir="$fleet_root/.skein"
src="$skein_dir/src"
toolchain="$skein_dir/toolchain"
server="$skein_dir/skein-server"
doorway="$skein_dir/server-doorway.py"
stamp="$skein_dir/server.door"
# `private/` is the one directory under `.skein` that no box can see: `src/box-session.sh` puts a
# `--tmpfs` over it in every ordinary box's namespace. The cockpit's tmux socket is in it rather
# than beside it because a read-only bind refuses nothing to a socket, so anywhere else in `.skein`
# is a socket every box may `connect()` to, and a tmux client is a place the server runs a command
# (SKEIN-529). `fleet::server_tmux_sock` in `src/fleet/install.rs` is the other spelling; they move
# together or the fleet gets two tmux servers contending for the cockpit's port.
private="$skein_dir/private"
sock="$private/server.tmux"
port="${SKEIN_SERVER_PORT:-7878}"
# Set below, from the volume the create mounted. Not from `$HOME`, which is the whole bug it
# replaces — see "the volume" further down, after `say` exists to report what was found.
skein_home="${SKEIN_HOME:-}"

# What to build. Public by default because the default install must not need a credential; set
# SKEIN_SOURCE_URL to a private remote (with an `sbx secret` behind it) to build a fork.
url="${SKEIN_SOURCE_URL:-https://github.com/prateekreddy/skein.git}"

# **Deliberately no default branch name.** Empty means "whatever the remote's HEAD is", which is
# what `git clone` with no `--branch` already does. A literal here is a second place that has to be
# right: `main` was one, and there is no `main` — the install 404ed on line one and then failed the
# clone (SKEIN-461). Set SKEIN_SOURCE_REF to build a branch, tag or sha instead.
ref="${SKEIN_SOURCE_REF:-}"

# `build` stops after the binary is installed and the revision printed — how fleet creation
# (`fleet::build_server_in_sandbox`) reuses this file without restarting anything. Empty means go
# all the way to a serving fleet, which is what a hand run and the cockpit's Update button both do.
stop_after="${SKEIN_BOOTSTRAP_STOP_AFTER:-}"

# Who started this, so a message can say how to start it again. `update` is the cockpit's Update
# button (`update::run_script` sets it); anything else is a person with this file.
from="${SKEIN_BOOTSTRAP_FROM:-}"
if [ "$from" = update ]; then
  what=update
  again="press Update skein again"
else
  what=install
  again="run bootstrap.sh again"
fi

say() { printf 'skein: %s\n' "$1" >&2; }

# ---- nothing here may ask a question, because nobody is there to answer it -----------------------
#
# The Update button runs this file in a detached tmux pane that no person can see. On 2026-09-22 the
# fetch below was refused the stored GitHub token, and git did what it does next: it opened the
# pane's terminal and printed `Username for 'https://github.com':`. It waited there for 37 minutes,
# with the button disabled and no line in the log saying why, until someone found the pane and
# pressed C-c (SKEIN-1032). A hand run is no better placed: `sbx exec -i … bash < bootstrap.sh` feeds
# this file on stdin, so there is no one at a prompt there either.
#
# So every way a command here can ask is closed, once, for everything below:
#
#   * `GIT_TERMINAL_PROMPT=0` — git dies with "terminal prompts disabled" instead of reading
#     /dev/tty. The one that matters; `repos::fleet_git` sets it for the same reason (SKEIN-951).
#   * `GIT_ASKPASS=` — empty, not unset. git asks an askpass program BEFORE it looks at the
#     setting above, taking `GIT_ASKPASS`, then `core.askPass`, then `SSH_ASKPASS`; an empty
#     `GIT_ASKPASS` is taken and skips all three, which unsetting it would not.
#   * `GCM_INTERACTIVE=never` — Git Credential Manager, if an image has it, has its own prompts.
#   * ssh, which none of the above reaches, is told `BatchMode=yes` further down, where there is a
#     git to ask what ssh command it would otherwise have used.
#
# A remote that wants a login then fails in seconds, and remote_git below, which sends no
# credential at all, says so.
export GIT_TERMINAL_PROMPT=0
export GIT_ASKPASS=
export GCM_INTERACTIVE=never
unset SSH_ASKPASS

# ---- nor wait for ever on a network that stopped answering ---------------------------------------
#
# Closing the prompts ended one way a run could hang; a remote that accepts the connection and then
# says nothing was the other (SKEIN-1037). git has no low-speed limit unless it is given one, so a
# fetch against such a remote sat at "fetching" with the Update button disabled for as long as the
# connection stayed open. Every network step below now gives up once nothing has arrived for
# `$stall` seconds, and the log says so:
#
#   * git over http(s): under 1000 bytes/s for `$stall` seconds ends the fetch, with git's own
#     "Operation too slow" line. 1000 rather than 1 because a shallow fetch of this repository is
#     megabytes, and a link slower than a kilobyte a second would take hours to finish it anyway.
#     **One case this cannot reach, measured:** an https remote that accepts TCP and never answers
#     the TLS handshake. That is curl's connect phase, which the low-speed limit does not cover and
#     for which git has no setting; libcurl's own 300-second connect timeout ends it instead.
#   * git over ssh: `ConnectTimeout` also bounds the banner exchange (measured: "Connection timed
#     out during banner exchange" against a listener that never speaks), and `ServerAlive` ends a
#     session that goes silent mid-transfer after two unanswered probes, so about `2 x $stall`. A
#     server busy computing a pack still answers the probe, so a slow fetch is not taken for a dead
#     one.
#   * the rustup download: `--connect-timeout` and the same low-speed rule, and no `--max-time` —
#     a total cap would end a slow but working download, which is exactly what this must not do.
#   * the rustup-init binary the installer script downloads, with a curl of ITS own that sets no
#     bound (SKEIN-1120): the same two, handed to it in a config file through CURL_HOME — see the
#     install block below for why that reaches it.
#   * the toolchain that installer then downloads, which is rustup's own downloader and not curl
#     (SKEIN-1090): RUSTUP_DOWNLOAD_TIMEOUT. rustup 1.29.0's `src/download/mod.rs` hands it to the
#     default (reqwest) backend as its read timeout, so it ends a connection that has sent nothing for
#     that long and never a slow one that is still sending. Unset, rustup's own default is 180
#     seconds. RUSTUP_USE_CURL is unset because the same file hands the number to the curl backend
#     as a connect timeout only: measured against a listener that accepts and never answers, that
#     backend was still waiting after 20 seconds with the timeout at 3, where the default one gave
#     up at 3 with "operation timed out".
#   * cargo's registry fetch: CARGO_HTTP_TIMEOUT is cargo's connect timeout and low-speed window
#     at once. cargo already had one (30 seconds, retried 3 times); it is set here so the bound is
#     this file's rather than a default nobody wrote down, and so one number governs every step.
#
# 60 seconds: long enough that a congested link or a remote slow to start a pack is not cut off,
# short enough that a dead one fails in a minute rather than an evening. `SKEIN_NET_STALL_SECS`
# changes it — a whole number of seconds, or it is ignored, because it is spliced into
# `GIT_SSH_COMMAND`, which git hands to a shell.
stall="${SKEIN_NET_STALL_SECS:-60}"
case "$stall" in
  '' | *[!0-9]* | 0*) stall=60 ;;
esac
export GIT_HTTP_LOW_SPEED_LIMIT=1000
export GIT_HTTP_LOW_SPEED_TIME="$stall"
export CARGO_HTTP_TIMEOUT="$stall"
export CARGO_NET_RETRY=3
export RUSTUP_DOWNLOAD_TIMEOUT="$stall"
unset RUSTUP_USE_CURL

# ---- the fleet root, the one line here that needs sudo -------------------------------------------

# `/boxes` sits at the filesystem root, where the sandbox user cannot mkdir. Without this the whole
# install stopped on its first write with two bare `mkdir: Permission denied` lines and nothing
# else — the two arguments of the `mkdir -p` below, and no clue which directory or why.
#
# skein has escalated here since long before this file existed (`fleet::ensure_fleet_root`, and its
# doc comment is the same warning), but that runs from a skein binary and there is no skein binary
# until this script has built one. So the bootstrap has to do this step for itself; it is not a
# second implementation of the build, it is the step that happens before there is anything to build
# with.
#
# `[ -w ]` twice, and sudo only when there is something to escalate for. A fleet root pointed
# somewhere already writable — `$SKEIN_FLEET_ROOT`, which is how the tests exercise this file at
# all — is made without sudo, and a box is a user namespace where sudo cannot work. `mkdir -p` on an
# existing directory succeeds, so the second `-w` is what keeps an unwritable-but-present root
# falling through to sudo rather than being called done.
#
# Every sudo in this file is `sudo -n` (SKEIN-1038): the cockpit's Update button runs this file
# where nobody can type a password, so a sudo that wants one must refuse at once rather than wait
# at a prompt nobody sees. `set -e` then ends the install on sudo's own "a password is required".
if [ ! -w "$fleet_root" ] && ! { mkdir -p "$fleet_root" 2>/dev/null && [ -w "$fleet_root" ]; }; then
  say "creating the fleet root $fleet_root, which needs sudo inside the sandbox"
  sudo -n mkdir -p "$fleet_root"
  sudo -n chown "$(id -u):$(id -g)" "$fleet_root"
  chmod 755 "$fleet_root"
fi

# ---- what the image does not ship, and the build cannot do without --------------------------------

# `cc`. A Rust toolchain is not a build: rustc links through the system C compiler, so the `shell`
# image — which has no compiler at all — got as far as downloading crates and then failed every
# build script it tried to link, `libc`, `proc-macro2` and `quote` first:
#
#     error: linker `cc` not found
#
# That is not skein needing a C library; skein has no `-sys` dependency on Linux. It is rustc
# needing a linker, which is true of every Rust build there has ever been.
#
# `ensure_substrate` installs the sandbox's packages (tmux, jq, the agent runtimes) and would be
# the obvious home for this, but it runs from a skein binary — and a prerequisite of BUILDING that
# binary cannot live behind it. Same shape as the fleet root above: the steps that come before there
# is a skein are the ones this file has to own.
need=''
command -v cc      >/dev/null 2>&1 || need="$need build-essential"
command -v git     >/dev/null 2>&1 || need="$need git"
# The supervisor the cockpit runs under is a tmux session, so this file runs `tmux` itself — which
# the `shell` image does not have either. It got as far as a finished release build and then
# `bash: line 227: tmux: command not found`, with the binary already installed and nothing serving
# it.
command -v tmux    >/dev/null 2>&1 || need="$need tmux"
# Needed a few lines down to fetch rustup, and only then — but apt is one round trip and this is the
# round trip.
command -v curl    >/dev/null 2>&1 || need="$need curl"
# The doorway is a python3 script. Not fatal here, because `SKEIN_BOOTSTRAP_STOP_AFTER=build` is a
# real and complete use of this file that never runs it.
command -v python3 >/dev/null 2>&1 || need="$need python3"
# `jq` is the one package here this script never runs. It belongs to `ensure_substrate`, which
# installs it when a box starts — but that is a round trip to apt in a few minutes' time, and this
# is a round trip to apt now. Named here as a head start and nothing more: substrate keeps the list,
# asks `command -v` the same way, and finding it already installed is the whole point.
command -v jq      >/dev/null 2>&1 || need="$need jq"

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
  _skein_apt_once "$@" >>"$_skein_log" 2>&1 && return 0
  _skein_rc=$?
  # A timeout is not retried: the same mirror gets the same time again and the caller's own
  # deadline runs out first. Anything else, the lock race above most of all, gets one more go.
  [ "$_skein_rc" -eq 124 ] && return 124
  sleep 5
  _skein_apt_once "$@" >>"$_skein_log" 2>&1
}

# One update and one install.
#
# `update` FIRST, every time: a fresh image ships an empty index, where install reports "Package
# 'tmux' has no installation candidate", which reads as a missing package and is a missing index.
# `;` rather than `&&` after it: one unreachable source fails `update` for the whole index, and
# the packages wanted may well be on the sources that answered. Install's status is the verdict.
#
# `sudo -n`: nothing here can answer a password prompt, and a prompt nobody answers waits out
# the whole timeout before failing. 600s for the install: killing dpkg mid-unpack leaves a
# half-configured package that breaks every later install until `dpkg --configure -a`, so the
# bound is set for a slow mirror, not for a quick failure.
_skein_apt_once() {
  _skein_bounded 180 sudo -n apt-get update -qq
  _skein_bounded 600 sudo -n apt-get install -y -qq "$@"
}

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
    _skein_bounded 600 npm install -g "$@" >>"$_skein_log" 2>&1
  else
    _skein_bounded 600 sudo -n npm install -g "$@" >>"$_skein_log" 2>&1
  fi
}
# <<< package-install.sh
if [ -n "$need" ]; then
  say "the image is missing$need — installing, once, into the sandbox"
  # `|| true`, deliberately: apt's exit status is the wrong judge. What decides is whether the
  # commands are on the PATH afterwards, which is what the check below asks. An install that exits
  # non-zero over an unrelated warning must not end an install that in fact worked.
  # Its words are still shown when it fails, since the check below can only say what is missing.
  # Kept, at one fixed name, as the fleet's own substrate log is: no `mktemp` or `rm`, which are
  # more programs the image would have to have, and it is there to read if the check below fails.
  apt_log=/tmp/skein-bootstrap-apt.log
  : >"$apt_log"
  skein_apt_install "$apt_log" $need || {
    say "apt did not finish cleanly; the last of what it said (all of it is in $apt_log):"
    tail -n 15 "$apt_log" | sed 's/^/  | /' >&2
  }
fi

# Asked of the PATH, not of apt. `cc` and `git` only: they are what the lines below run, and the
# other two are wanted later or not at all.
missing=''
for t in cc git; do
  command -v "$t" >/dev/null 2>&1 || missing="$missing $t"
done
if [ -n "$missing" ]; then
  say "this sandbox image is missing$missing and apt could not install it"
  say "cc is rustc's linker, so without it no Rust builds here at all — not skein's dependencies,"
  say "not its build scripts. Try 'sudo apt-get update && sudo apt-get install -y build-essential'"
  say "inside the sandbox to see what apt says."
  exit 1
fi

# ---- the volume, which is NOT the sandbox's $HOME -------------------------------------------------

# `$SKEIN_HOME` is where skein keeps `api-token`, `repos.json`, `config.json` and the box state. It
# used to default to `$HOME/.skein`, which is right on a host and wrong in here, and wrong in the
# way that costs the most: it works. The server starts, generates a token, and writes every piece of
# state into the container's own `/home/<user>/.skein` — a directory that is not the mounted volume
# and does not survive the sandbox. What a person sees is a cockpit that says it needs the fleet's
# token while `~/.skein/api-token` on the host holds a different one, or none.
#
# The two are different because sbx bind-mounts a workspace at its HOST absolute path while giving
# the sandbox a home of its own:
#
#     /Users/you/.skein  /Users/you/.skein  rw,... - virtiofs host rw     <- the volume
#     HOME=/home/<user>                                                   <- not the volume
#
# So the volume is discovered rather than guessed, from the one place that records it. `$5` is the
# mount point in every `mountinfo` line — the fields before the `-` are fixed at six, and a path
# with a space in it is escaped as `\040`, so splitting on whitespace is safe here.
#
# **Ambiguous means refuse.** A wrong `$SKEIN_HOME` is invisible until somebody cannot open the
# cockpit; a refusal naming the flag is not. `SKEIN_HOME` set explicitly always wins and skips all
# of this — which is what `fleet::bootstrap_env` passes when the cockpit re-runs this file, and it
# is the escape hatch for a volume this cannot find.
if [ -z "$skein_home" ]; then
  # `$SKEIN_MOUNTINFO` is a test seam, in the same spirit as `$SKEIN_FLEET_ROOT`: without it this
  # branch could only ever be exercised against a real sandbox, which is precisely how a wrong
  # `$SKEIN_HOME` shipped.
  found=$(awk -v skip="$skein_dir" '$5 ~ /\/\.skein$/ && $5 != skip { print $5 }' \
    "${SKEIN_MOUNTINFO:-/proc/self/mountinfo}" 2>/dev/null | sort -u)
  # Counted in the shell rather than with `grep -c`/`wc -l`: one candidate is "non-empty, and no
  # newline in it", which parameter expansion answers without a subprocess. A missing `grep` made
  # this refuse while it was printing the single mount it had just found — a dependency the count
  # never needed, failing in the direction that stops the install.
  if [ -n "$found" ] && [ "$found" = "${found%%
*}" ]; then
    skein_home="$found"
    say "the fleet volume is $skein_home"
  else
    say "cannot tell which directory is the fleet volume, and guessing is the bug this replaces."
    say "The volume is the path named on the create -- \$HOME/.skein unless you moved it -- and it"
    say "is mounted inside the sandbox at that same absolute path. Name it and re-run:"
    say ""
    say "    sbx exec -i <sandbox> env SKEIN_HOME=\"\$HOME/.skein\" bash < bootstrap.sh"
    if [ -n "$found" ]; then
      say ""
      say "(mounts that looked like candidates:)"
      printf '%s\n' "$found" | sed 's/^/skein:     /' >&2
    fi
    exit 1
  fi
fi

# ---- the size, which nothing can change afterwards -----------------------------------------------

# Memory and CPUs are fixed for the life of this sandbox. sbx sets both at create and has no resize,
# so changing either means destroying the sandbox — and with it every box's checkout, which lives on
# its disk. They are the least revisitable decisions in the install and they were the only ones
# nothing asked about: omit the flags and sbx takes half the host's memory and EVERY one of its
# cores, decided by nobody.
#
# So they have to be stated. Stating alone would be a rubber stamp, though — a create that forgot
# `-m` and an exec that claims `26g` are a matched pair of assertions about a sandbox that has
# neither. So the statement is checked against what this sandbox actually got, and it is what the
# sandbox got that is permanent.
#
# **This cannot see the host**, and does not pretend to. It will not tell you that 26g is 70% of your
# machine. What it can see is what this sandbox HAS, and that is the thing being approved.
#
# Before the toolchain, the clone and the build, deliberately: a refusal here costs seconds, and the
# same refusal after the build costs the build.
declared_mem="${SKEIN_FLEET_MEMORY:-}"
declared_cpus="${SKEIN_FLEET_CPUS:-}"

# Three sources, most specific first, because "state it" must not mean "state it again every time".
#
# `fleet-size` is what a previous run of THIS gate recorded after checking it, so it is a decision
# that was already made and verified — which is what makes an upgrade silent. `build_script` re-runs
# this file to upgrade a fleet and passes no size, so without this every upgrade would be refused
# for a question that was answered at install.
#
# It is still checked against the sandbox below, so a recorded number does not become permission to
# skip the check: a fleet rebuilt at a different size is caught on its next run rather than carrying
# the old answer forward.
size_file="$skein_dir/fleet-size"
if [ -f "$size_file" ]; then
  [ -n "$declared_mem" ] || declared_mem=$(sed -n 's/^memory=//p' "$size_file" | head -n1)
  [ -n "$declared_cpus" ] || declared_cpus=$(sed -n 's/^cpus=//p' "$size_file" | head -n1)
fi

# `config.json` last: it is where a person or the cockpit writes an intention, and `fleet_cpus` is
# empty on every fleet made before this gate existed. Same `sed` shape `skein-startup.sh` uses on the
# launch spec — no `jq` on the critical path of an install whose whole job is to run before skein.
conf="$skein_home/config.json"
if [ -f "$conf" ]; then
  [ -n "$declared_mem" ] || declared_mem=$(sed -n 's/.*"fleet_memory"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$conf" | head -n1)
  [ -n "$declared_cpus" ] || declared_cpus=$(sed -n 's/.*"fleet_cpus"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$conf" | head -n1)
fi

# `nproc` when the image has it, `awk` over `/proc/cpuinfo` when it does not — and the fallback is
# `awk` rather than `grep -c` because this file already needs `awk` for the volume discovery above,
# so the fallback adds no dependency the install did not already have. An install that stopped over
# the tool it counts with rather than over the count would be the worse failure.
#
# `$SKEIN_CPUINFO` is the seam, for the same reason `$SKEIN_MEMINFO` is: pinned, these two make the
# gate testable on any machine; unpinned, a test of it passes on the laptop it was written on.
actual_cpus=$(nproc 2>/dev/null \
  || awk '/^processor/ { n++ } END { print n + 0 }' "${SKEIN_CPUINFO:-/proc/cpuinfo}" 2>/dev/null \
  || echo 0)
# `$SKEIN_MEMINFO` is a test seam, exactly as `$SKEIN_MOUNTINFO` is above: without it this gate
# could only be exercised on a machine that happened to have the right amount of memory, which is
# the same as not exercising it.
actual_mem_mib=$(awk '/^MemTotal:/ { print int($2 / 1024); exit }' "${SKEIN_MEMINFO:-/proc/meminfo}" 2>/dev/null || echo 0)

# sbx's own spelling — `26g`, `8G`, `1024m` — into MiB. An unparseable value is 0, which fails the
# comparison below rather than passing it.
to_mib() {
  printf '%s' "$1" | awk '{
    v = tolower($0)
    if (v ~ /^[0-9]+g[b]?$/)      { sub(/g[b]?$/, "", v); print v * 1024 }
    else if (v ~ /^[0-9]+m[b]?$/) { sub(/m[b]?$/, "", v); print v + 0 }
    else                          { print 0 }
  }'
}

size_refusal() {
  say "$1"
  say ""
  say "  this sandbox has   $actual_cpus CPUs, $(awk -v m="$actual_mem_mib" 'BEGIN{printf "%.1f GiB", m/1024}')"
  say "  it was created as  memory=${declared_mem:-(not stated)} cpus=${declared_cpus:-(not stated)}"
  say ""
  say "Nothing is installed yet. If those are the numbers you want, say so and re-run:"
  say ""
  say "    sbx exec -i <sandbox> env SKEIN_FLEET_MEMORY=$(awk -v m="$actual_mem_mib" 'BEGIN{printf "%dg", int((m+1023)/1024)}') SKEIN_FLEET_CPUS=$actual_cpus bash < bootstrap.sh"
  say ""
  say "If they are NOT what you want, destroy this sandbox and create it again with -m and --cpus."
  say "sbx fixes both at creation, so there is no other way to change them:"
  say ""
  say "    sbx rm <sandbox>"
  say ""
  say "What your machine has:  sysctl -n hw.memsize hw.ncpu   (macOS)"
  say "                        free -g; nproc                 (Linux)"
  exit 1
}

if [ -z "$declared_mem" ] || [ -z "$declared_cpus" ]; then
  size_refusal "this fleet's memory and CPUs were never stated, and they cannot be changed later."
fi

if [ "$declared_cpus" != "$actual_cpus" ]; then
  size_refusal "this fleet was asked for $declared_cpus CPUs and has $actual_cpus."
fi

declared_mib=$(to_mib "$declared_mem")
# Never MORE than asked for, and not much less: a VM keeps a little of its own memory back, measured
# at 247 MiB of a 26g fleet, so an exact test would refuse every correct install. Below 95% is not
# overhead — it is a different number, which is the case worth stopping.
if [ "$declared_mib" -le 0 ] \
  || [ "$actual_mem_mib" -gt "$declared_mib" ] \
  || [ "$actual_mem_mib" -lt $(( declared_mib * 95 / 100 )) ]; then
  size_refusal "this fleet was asked for $declared_mem and has $(awk -v m="$actual_mem_mib" 'BEGIN{printf "%.1f GiB", m/1024}')."
fi

say "size: $actual_cpus CPUs, $declared_mem — stated, and matches what this sandbox got"
# Recorded for `skein doctor`, which otherwise has no way to tell a size somebody chose from one
# sbx picked. Beside the binaries and in the same shape as `skein-home`, for the same reason: the
# CLI reads it without needing a JSON parser or the volume to be mounted where it expects.
#
# `mkdir` first: this gate runs BEFORE the toolchain section that used to be the first thing to
# create `$skein_dir`, so without this a correct install aborts on the redirection — which is how it
# was found, by checking the exit code of the passing case rather than the message it printed.
mkdir -p "$skein_dir"
# Made here as well as in `start-door.sh`, because the door is not the only writer under it: skein
# writes the review call's credential here at fleet scope, and a fleet that has never served has
# never run the door.
mkdir -p "$private"
chmod 700 "$private"
printf 'memory=%s\ncpus=%s\n' "$declared_mem" "$declared_cpus" > "$skein_dir/fleet-size"

# ---- the toolchain, kept out of every box's reach ------------------------------------------------

export CARGO_HOME="$toolchain/cargo"
export RUSTUP_HOME="$toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
mkdir -p "$src" "$toolchain"

# The question is "can a cargo here build", and `command -v cargo` stopped being that question
# three lines up. `RUSTUP_HOME` now names the private toolchain, which is empty until this block
# fills it — so a rustup shim from anywhere else (the image's `~/.cargo/bin/cargo`, or the one an
# interrupted earlier run of THIS script left behind) still answers `command -v`, resolves against
# a rustup home with no default toolchain in it, and turns the build into:
#
#     error: rustup could not choose a version of cargo to run, because one wasn't specified
#     explicitly, and no default is configured.
#
# A shim is not a toolchain, and the gate has to ask for the toolchain. Running cargo is the ask:
# a shim with nothing behind it fails it, and every real cargo passes it.
if ! cargo --version >/dev/null 2>&1; then
  say "installing a Rust toolchain in $toolchain (this is not the sandbox's own, on purpose)"
  # Downloaded whole before any of it runs, rather than piped: a pipe's status is the shell's, so
  # a download that timed out handed `sh` half a script (or none) and the next thing the log said
  # was "there is no cargo", with the reason one line up in curl's words and nothing joining the
  # two. `-S` keeps curl's own reason in the log; the bounds are the block at the top of this file.
  if ! rustup_sh=$(curl --proto '=https' --tlsv1.2 -sSf --connect-timeout "$stall" \
    --speed-limit 1000 --speed-time "$stall" https://sh.rustup.rs); then
    say "the Rust installer could not be downloaded from sh.rustup.rs (curl's reason is the line"
    say "above), so the build cannot start. Check that this sandbox can reach sh.rustup.rs, then"
    say "$again."
    exit 1
  fi
  # The installer's own download is a curl this file does not write (SKEIN-1120). rustup-init.sh
  # 1.29.0 fetches the rustup-init BINARY in its `downloader()` with `--retry 3 -C -`, `--proto`,
  # `--tlsv1.2`, `--ciphers`, `--silent --show-error --fail --location` and nothing that bounds a
  # silence — so a mirror that accepts and then says nothing held a first install there, between
  # the bounded curl above and the RUSTUP_DOWNLOAD_TIMEOUT at the top. What it does NOT pass is
  # `-q`, so curl reads a config file first, and CURL_HOME says where: the same two bounds as the
  # curl above, written where only this invocation looks. Each attempt then ends after `$stall`
  # seconds of nothing; curl retries a timeout, so a dead mirror costs four attempts and the
  # script's own backoff, not an evening. It lives in the private toolchain directory, which this
  # file already made, and is rewritten each time rather than removed — no `mktemp` or `rm` here.
  # A `~/.curlrc` of the sandbox's own is not read by that one invocation; a proxy is still honoured
  # through `https_proxy`, which curl reads from the environment and not from the file.
  curl_home="$toolchain/curl-home"
  mkdir -p "$curl_home"
  printf 'connect-timeout = %s\nspeed-limit = 1000\nspeed-time = %s\n' "$stall" "$stall" \
    > "$curl_home/.curlrc"
  # Checked rather than left to `set -e`, which would end the run here with rustup's own line as the
  # last word — "operation timed out" and a URL, with nothing saying that it was the toolchain
  # download, that it was given up on deliberately, or what to do (SKEIN-1090).
  if ! printf '%s\n' "$rustup_sh" \
    | CURL_HOME="$curl_home" sh -s -- -y --no-modify-path --default-toolchain stable >/dev/null; then
    say "rustup could not download the Rust toolchain (its reason is the line above; a download"
    say "that sends nothing for $stall seconds is given up on rather than waited on), so the build"
    say "cannot start. Check that this sandbox can reach static.rust-lang.org, then $again."
    exit 1
  fi
  # bash remembers where it found a command and does not look again while the file is still there.
  # The `cargo` a moment ago was the shim further down the PATH, and rustup has just written a
  # better one into `$CARGO_HOME/bin` — which is *ahead* of it. Without this the shell keeps
  # running the one it hashed, so the install below appears not to have happened.
  hash -r
  # An install that finds a rustup already under `$RUSTUP_HOME` updates rustup and *leaves the
  # toolchains alone* — including when there are none, which is exactly the state a run that died
  # mid-download leaves behind. `--default-toolchain` is not honoured on that path, so the one
  # thing that state is missing has to be asked for separately. Idempotent when it is not.
  cargo --version >/dev/null 2>&1 || "$CARGO_HOME/bin/rustup" default stable
fi

# Before the clone and the build, not after: cargo failing to resolve a toolchain is minutes of
# downloading crates away from where it would otherwise be noticed, with nothing installed at the
# end of them.
if ! cargo --version >/dev/null 2>&1; then
  say "there is no cargo that can run under $toolchain, so the build cannot start"
  say "that toolchain is skein's own and not the sandbox's, deliberately (see the top of this file),"
  say "so a working ~/.cargo does not help. To see what rustup says about it, run"
  say "'RUSTUP_HOME=$RUSTUP_HOME CARGO_HOME=$CARGO_HOME $CARGO_HOME/bin/rustup default stable'"
  exit 1
fi

# ---- the source ---------------------------------------------------------------------------------

# ssh, which `GIT_TERMINAL_PROMPT` does not reach: it asks for a key's passphrase or about an
# unknown host key on /dev/tty itself. `BatchMode=yes` makes it refuse instead, added to whatever
# command git would have run anyway — `$GIT_SSH_COMMAND`, else `core.sshCommand` — so a key chosen
# there is kept. `repos::ssh_that_cannot_ask` is the same rule for the fleet's own mirrors.
ssh_cmd="${GIT_SSH_COMMAND:-$(git config --get core.sshCommand 2>/dev/null || true)}"
# The two bounds from the top of this file ride on the same command, for the same reason.
export GIT_SSH_COMMAND="${ssh_cmd:-ssh} -o BatchMode=yes -o ConnectTimeout=$stall \
-o ServerAliveInterval=$stall -o ServerAliveCountMax=1"

# `owner/name`, and the host, out of a remote URL — `https://host/owner/name.git` and
# `git@host:owner/name.git` alike. Only ever used to word a message.
slug_of() {
  printf '%s' "$1" | sed -E 's#^[a-z+]+://[^/]*/##; s#^[^/@:]+@[^:]+:##; s#\.git/*$##; s#/+$##'
}
host_of() {
  printf '%s' "$1" | sed -E 's#^[a-z+]+://([^/@]*@)?([^/:]*).*#\2#; s#^[^/@:]+@([^:]+):.*#\1#'
}

# `scheme://host[:port]` out of a URL, with any `user@` dropped — the part git names when it asks
# for a login. Only ever used to tell whose login git was asking for.
site_of() {
  printf '%s' "$1" | sed -E 's#^([a-z+]+://)([^/@]*@)?([^/]*).*#\1\3#'
}

# Run `git "$@"` against the remote `$1` with no credential; on failure, say why and stop.
#
# **No credential, ever** (SKEIN-1171). The source is public, so the fetch needs none, and the owner
# decided on 2026-09-23 that the Update fetch gets no credential plumbing (SKEIN-1036). A helper that
# handed git something was the one way left for this to go wrong quietly — a stale token sent and
# refused, or a helper with prompts of its own. `-c credential.helper=` empties git's helper list,
# URL-scoped helpers included (measured on git 2.53: a `credential "http://host/o"` helper is not
# asked once this is on the command line). With no helper and the prompts closed at the top of this
# file, a remote that wants a login gets none, and git ends at once.
#
# git's own words are passed through first, whole — they are the evidence. The one thing added is
# the owner's sentence for a login refused, and only when git says so about THIS remote:
# "could not read Username for '<site>'" is the line git writes when the remote answered 401 and it
# had nothing to send. A failure that is not that keeps git's own error, and is not guessed at.
# What this cannot tell apart: a proxy between here and the remote that demands a login of its own
# on CONNECT — git 2.53 words that 407 exactly as it words the remote's 401, naming the remote.
remote_git() {
  remote="$1"
  shift
  # git's stderr is kept in a variable, not a file, and its stdout passes through on descriptor 4:
  # no `mktemp`, `cat` or `rm`, none of which this file may assume before it has a build to run.
  exec 4>&1
  said=$(git -c credential.helper= "$@" 2>&1 1>&4 4>&-) && rc=0 || rc=$?
  exec 4>&-
  [ -z "$said" ] || printf '%s\n' "$said" >&2
  [ "$rc" = 0 ] && return 0

  slug=$(slug_of "$remote")
  host=$(host_of "$remote")
  if [ "$host" = github.com ]; then who=GitHub; else who="$host"; fi
  case "$said" in
    *"could not read Username for '"* | *"could not read Password for '"*)
      asked=${said#*"could not read "*" for '"}
      asked=${asked%%"'"*}
      if [ "$(site_of "$asked")" = "$(site_of "$remote")" ]; then
        say "GitHub refused to let this sandbox fetch $remote without a login — the repository is private, or the address is wrong"
      fi
      ;;
    *"Permission denied (publickey"* | *"Host key verification failed"*)
      say ""
      say "$host refused this sandbox's ssh key for $slug (or does not know the host yet), so the"
      say "$what stopped here instead of waiting at an ssh prompt nobody can see."
      say "Give the sandbox a key $host accepts for $slug, or point SKEIN_SOURCE_URL at an https"
      say "remote, then $again."
      ;;
    # The bounds at the top of this file, in each program's words: curl's low-speed limit (git over
    # http), curl's connect timeout, ssh's `ConnectTimeout` and its `ServerAlive` probe.
    *"Operation too slow"* | *"timed out"* | *"not responding"*)
      say ""
      say "$who stopped answering while the $what was fetching $slug, so the $what gave up rather"
      say "than wait for ever. Nothing was installed. Check that this sandbox can reach $host, then"
      say "$again."
      ;;
  esac
  exit 1
}

if [ -d "$src/.git" ]; then
  say "fetching ${ref:-the default branch}"
  # `HEAD` is a ref the remote always has, and it is the same thing a bare clone would take.
  remote_git "$(git -C "$src" remote get-url origin 2>/dev/null || printf '%s' "$url")" \
    -C "$src" fetch --depth 1 origin "${ref:-HEAD}"
  git -C "$src" checkout -f FETCH_HEAD
else
  say "cloning $url at ${ref:-the default branch}"
  if [ -n "$ref" ]; then
    remote_git "$url" clone --depth 1 --branch "$ref" "$url" "$src"
  else
    remote_git "$url" clone --depth 1 "$url" "$src"
  fi
fi

# ---- the build ----------------------------------------------------------------------------------

# `--locked` because a build that quietly resolved a different dependency tree than the revision
# pins is not "what was published"; it is whatever crates.io looked like this morning.
say "building skein and skein-server — minutes on a cold build, and that cost is the point: what runs is what was published"
# **Both** binaries, and their adjacency is the point. `sandbox::skein_exe` spells `skein` as the
# sibling of the running executable — "the two binaries are built and installed together" — and this
# script built only the server, so the lookup found nothing, fell back to a bare `skein` that is on
# no sandbox's PATH, and every box start died on `sh: skein: command not found`.
cargo build --release --locked --manifest-path "$src/Cargo.toml" --bin skein-server --bin skein

# Written beside and renamed into place, never over: `cat >` onto an ELF a process is executing
# fails ETXTBSY, and the doorway's two-second retry can otherwise exec a half-written binary. A
# rename is atomic — what it can exec is the old file or the new one, never a fragment.
#
# The CLI first, so there is never a moment with a new server beside an older `skein` than the one
# it was built with.
for built in skein skein-server; do
  cp "$src/target/release/$built" "$skein_dir/$built.new"
  chmod 755 "$skein_dir/$built.new"
  mv "$skein_dir/$built.new" "$skein_dir/$built"
done

revision=$(git -C "$src" rev-parse --short HEAD)

if [ "$stop_after" = "build" ]; then
  printf '%s\n' "$revision"
  exit 0
fi

# Recorded beside the binaries, because the CLI has no other way to learn it. The supervisor passes
# `$SKEIN_HOME` to the server; nothing passes it to `skein`, so every CLI invocation fell back to the
# container's own `$HOME` and reported an empty fleet — `skein repos` said "no repos yet" about a
# fleet whose cockpit was showing them. `config::skein_home` reads this file when `$SKEIN_HOME` is
# unset.
#
# Here rather than beside the other paths at the top, because `$skein_home` is not known until the
# discovery above has run: written earlier it recorded an empty line, which is the same bug one
# level quieter.
printf '%s\n' "$skein_home" > "$skein_dir/skein-home.new"
mv "$skein_dir/skein-home.new" "$skein_dir/skein-home"

# ---- the door, which is opened before anything is put behind it ----------------------------------

# Asked here rather than beside `cc` and `git`, because this is where it is first needed and
# `SKEIN_BOOTSTRAP_STOP_AFTER=build` returns above without ever wanting it. Asked at all because the
# `has-session` below swallows its own stderr — a missing tmux reads there as "no session", falls
# through to `new-session`, and reports itself as a bash line number.
if ! command -v tmux >/dev/null 2>&1; then
  say "the build finished and skein-server is installed at $server, but this sandbox has no tmux"
  say "and apt could not install it — so there is nothing to run the cockpit under. Try"
  say "'sudo apt-get update && sudo apt-get install -y tmux' inside the sandbox, then re-run this."
  exit 1
fi

# The doorway comes out of the checkout that was just made, so there is no bootstrapping problem to
# solve: the file skein installs into a sandbox and the file this installs are the same file.
cp "$src/src/server-doorway.py" "$doorway.new"
chmod 755 "$doorway.new"
mv "$doorway.new" "$doorway"

# Starting the cockpit is a **file in the sandbox**, not a passage of this script.
#
# Nothing in this sandbox starts the door at boot. pid 1 is `tini`; there is no systemd, no cron,
# nothing to hook — measured, not assumed. So every sandbox restart leaves the whole install intact
# on disk with nothing serving: no tmux session, no doorway, :7878 unbound, and the host's port
# mapping connecting to nothing. And a restart is not rare, because `sbx exec` arms a ~30s stop as
# it disconnects — measured in the live fleet.
#
# Written down here, the way back was "re-run the installer" — a fetch, a build and a minute, to
# re-run four lines that were already right. Now those four lines are `start-door.sh`, installed
# beside the binaries, so putting the door back costs one second:
#
#     sbx exec -i <sandbox> /boxes/.skein/start-door.sh
#
# It is also the piece any durable answer needs — whatever eventually runs at start has to run
# *something*, and this is the something. One implementation, so the thing a person runs by hand
# and the thing a restart runs cannot come apart.
#
# The heredoc is QUOTED. Nothing here is interpolated into it: the script derives every path from
# the fleet root exactly as this file does, and reads the volume from the marker written above, so
# it is correct when run by itself with no environment at all.
cat > "$skein_dir/start-door.sh.new" <<'DOOR'
#!/usr/bin/env bash
#
# start-door.sh — put the cockpit's door back, and nothing else.
#
# Installed by bootstrap.sh, which also runs it. Safe to run at any time: a door that is already
# open is reloaded across its own socket rather than replaced, so the port is never free.

set -eu

fleet_root="${SKEIN_FLEET_ROOT:-/boxes}"
skein_dir="$fleet_root/.skein"
doorway="$skein_dir/server-doorway.py"
server="$skein_dir/skein-server"
stamp="$skein_dir/server.door"
# The same three lines as bootstrap.sh's own prelude, and for the reason written there: no box can
# see under `private/`, and a socket anywhere else in `.skein` is one every box may connect to.
private="$skein_dir/private"
sock="$private/server.tmux"
port="${SKEIN_SERVER_PORT:-7878}"

say() { printf 'skein: %s\n' "$1" >&2; }

if [ ! -f "$doorway" ]; then
  say "there is no doorway at $doorway — this fleet has not been installed, or its .skein was"
  say "deleted. Run bootstrap.sh in this sandbox."
  exit 1
fi

# The volume, from the marker the install wrote. NOT `$HOME`, which is the container's own and not
# the mount — see "the volume" in bootstrap.sh. `$SKEIN_HOME` from the environment wins, so this is
# still overridable and still testable, but it is not needed for the file to be right.
skein_home="${SKEIN_HOME:-}"
if [ -z "$skein_home" ]; then
  skein_home="$(cat "$skein_dir/skein-home" 2>/dev/null || true)"
fi
if [ -z "$skein_home" ]; then
  say "no volume is recorded at $skein_dir/skein-home, so the server would write its token and"
  say "its box state into this container's own home and lose them at the next restart."
  exit 1
fi

# The supervisor loop, and its condition. `while [ -f "$doorway" ]` rather than `while true`: a
# fleet whose `.skein` has been deleted leaves a bash restarting a missing script at 0.5 Hz for
# ever, and 105 of those were measured before the condition was added. The `sleep` is conditional so
# a crash-loop backs off while a doorway that had been up is replaced in the time python takes to
# start — that gap is the port standing empty.
supervise="while [ -f '$doorway' ]; do \
began=\$(date +%s); \
SKEIN_HOME='$skein_home' python3 '$doorway' '$port' '$server' '$stamp'; \
[ \$((\$(date +%s) - began)) -lt 5 ] && sleep 2; \
done"

# Before the `has-session`, not just before the `new-session`: tmux does not create a socket's
# parent directory, and this script is the one that runs at every sandbox start — including the
# first one after an upgrade, on a fleet whose `.skein` predates `private/` existing at all.
mkdir -p "$private"
chmod 700 "$private"

# ---- a supervisor still on the socket every fleet had before SKEIN-529 --------------------------
#
# The session used to live at `.skein/server.tmux`, beside `private/` instead of in it. A fleet
# serving since then still has it there, and asking only the new path whether the cockpit runs
# answered "no" about a cockpit that was running: this started a SECOND supervisor, whose doorway
# looped on "cannot bind" while the first kept the port and kept serving the build it was started
# with (SKEIN-1020). Both are asked now.
#
# The old session is MOVED, not restarted: its socket file is renamed into `private/`. The tmux
# server keeps its listening socket — a rename moves the name, not the socket — so it goes on
# answering at the new path and nowhere else, and the doorway it supervises is reloaded in place
# below like any other. Restarting it on the new socket instead would mean letting go of the port
# for a moment, which is architecture §9.4's squat window; leaving it where it is would keep a tmux
# socket every box can connect to, which is the fleet-scope command channel SKEIN-529 closed. The
# rename costs neither.
legacy_sock="$skein_dir/server.tmux"

supervising() { tmux -S "$1" has-session -t skein-server 2>/dev/null; }

# The doorway the stamp names, and the reload: the same bytes `fleet::door_pid` and
# `fleet::reload_server` run, so the two ways into a cockpit cannot pick different processes.
# >>> door.sh — a byte-for-byte copy of src/door.sh; edit that file, not this
# Finding the cockpit's doorway, and reloading it. One implementation for both of skein's ways in:
# `fleet::door_pid` and `fleet::reload_server` (src/fleet/server.rs) embed this file with
# include_str!, and bootstrap.sh's start-door.sh, which runs before any skein binary exists,
# carries it byte for byte between the `>>> door.sh` and `<<< door.sh` marker lines.
# `fleet::server`'s `the_doorway_is_found_one_way` fails when that copy differs. Edit this file,
# then paste it there.
#
# Reload used to be two mechanisms: Rust sent `pkill -USR1 -f <pattern>`, a command-line match
# the repository's own rules ban, and start-door.sh sent `kill -USR1` to the pid in the stamp.
#
# POSIX sh, and definitions only: sourcing it runs nothing.

# skein_stamped_door STAMP PORT DOORWAY
#
# Print the pid of the doorway holding PORT by its stamp, or fail. A pid is a number that gets
# reused, so it counts only when the process is alive AND is this doorway, which its command line
# says exactly: one argument equal to DOORWAY. A stamp left by a doorway that was killed names a
# dead pid and reads as no doorway, which is the honest answer.
skein_stamped_door() {
  read -r _door_pid _door_held <"$1" 2>/dev/null || return 1
  [ "$_door_held" = "$2" ] || return 1
  kill -0 "$_door_pid" 2>/dev/null || return 1
  tr '\0' '\n' <"/proc/$_door_pid/cmdline" 2>/dev/null | grep -qxF -- "$3" || return 1
  printf '%s\n' "$_door_pid"
}

# skein_supervised_door DOORWAY SOCKET...
#
# Print the pid of the doorway a cockpit supervisor runs, or fail: the child of the `skein-server`
# session's pane, on any SOCKET given, whose command line is DOORWAY. Asked by the process tree
# rather than by a pattern, so it cannot find anything a supervisor did not start.
skein_supervised_door() {
  _door_path=$1
  shift
  for _door_sock in "$@"; do
    _door_pane=$(tmux -S "$_door_sock" list-panes -t skein-server -F '#{pane_pid}' 2>/dev/null | head -n 1)
    [ -n "$_door_pane" ] || continue
    for _door_status in $(grep -l "^PPid:[[:space:]]*$_door_pane\$" /proc/[0-9]*/status 2>/dev/null); do
      _door_pid=${_door_status#/proc/}
      _door_pid=${_door_pid%/status}
      if tr '\0' '\n' <"/proc/$_door_pid/cmdline" 2>/dev/null | grep -qxF -- "$_door_path"; then
        printf '%s\n' "$_door_pid"
        return 0
      fi
    done
  done
  return 1
}

# skein_door_reload STAMP PORT DOORWAY SOCKET...
#
# Send the doorway SIGUSR1, which ends the server behind it and re-execs the doorway across the
# SAME listening socket, so the port is never free (architecture §9.4). Print the pid signalled,
# or fail when there was no doorway to signal, which tells the caller to start one.
#
# The stamp's doorway first. When the stamp says nothing, the supervisor's: a doorway that is alive
# but unstamped (the stamp deleted, or naming a pid that is not the doorway) is exactly the one a
# re-exec repairs, because the fresh doorway stamps again across the same descriptor (SKEIN-226).
skein_door_reload() {
  _door_stamp=$1
  _door_port=$2
  _door_way=$3
  shift 3
  _door_target=$(skein_stamped_door "$_door_stamp" "$_door_port" "$_door_way") \
    || _door_target=$(skein_supervised_door "$_door_way" "$@") \
    || return 1
  kill -USR1 "$_door_target" 2>/dev/null || return 1
  printf '%s\n' "$_door_target"
}
# <<< door.sh

# Is the doorway $2 the one the session on socket $1 runs? The supervisor loop is the pane's shell
# and the doorway is its child.
runs_door() {
  pane=$(tmux -S "$1" list-panes -t skein-server -F '#{pane_pid}' 2>/dev/null | head -n1)
  parent=$(awk '/^PPid:/ { print $2 }' "/proc/$2/status" 2>/dev/null)
  [ -n "$pane" ] && [ "$pane" = "$parent" ]
}

# End the tmux server on socket $1 and wait for it to be gone. The wait is not politeness: tmux
# unlinks its socket's path as it exits, and a rename onto that path made before it has finished
# would be deleted by it.
retire() {
  t=$(tmux -S "$1" display-message -p '#{pid}' 2>/dev/null || true)
  tmux -S "$1" kill-server 2>/dev/null || true
  n=0
  while [ -n "$t" ] && kill -0 "$t" 2>/dev/null && [ "$n" -lt 50 ]; do
    sleep 0.1
    n=$((n + 1))
  done
}

door=$(skein_stamped_door "$stamp" "$port" "$doorway" || true)
on_new=''
on_old=''
supervising "$sock" && on_new=yes
supervising "$legacy_sock" && on_old=yes

# Both at once is what the bug above leaves behind. The one to keep is the one whose doorway holds
# the port, so ending the other frees nothing; with no stamp to say which, the new socket's.
if [ -n "$on_new" ] && [ -n "$on_old" ]; then
  if [ -n "$door" ] && runs_door "$legacy_sock" "$door"; then
    say "two cockpit supervisors are running; ending the one on $sock, which does not hold :$port"
    retire "$sock"
    on_new=''
  else
    say "two cockpit supervisors are running; ending the one on $legacy_sock"
    retire "$legacy_sock"
    on_old=''
  fi
fi

if [ -n "$on_old" ]; then
  if mv -f "$legacy_sock" "$sock"; then
    say "moved the running cockpit's tmux socket from $legacy_sock to $sock, where no box can reach it"
    on_new=yes
  else
    say "could not move the cockpit's tmux socket from $legacy_sock into $private, so it keeps"
    say "running there — a socket every box can connect to (SKEIN-529). Reloading it in place."
  fi
fi

if [ -n "$on_new" ] || [ -n "$on_old" ]; then
  say "the cockpit is already running; sending it the reload it uses to swap binaries"
  # SIGUSR1 re-execs the doorway across the SAME descriptor, so the socket is never closed and the
  # port is never free. Killing and restarting here would reopen exactly the window the doorway
  # exists to close (architecture §9.4).
  if ! skein_door_reload "$stamp" "$port" "$doorway" "$sock" "$legacy_sock" >/dev/null; then
    say "no doorway holds :$port by its stamp at $stamp, and the supervisor is not running one, so"
    say "there is nothing to reload; the supervisor starts the doorway on disk the next time round"
    say "its loop"
  fi
else
  # A socket file left by a sandbox that stopped is a file with no server behind it. tmux clears
  # its own stale socket on the way to starting a new one, so this is a plain `new-session` and
  # not a `rm` — and a `rm` here would be a race against a door that IS running.
  tmux -S "$sock" new-session -d -s skein-server "$supervise"
  say "the cockpit's door is open on :$port"
fi
DOOR
chmod 755 "$skein_dir/start-door.sh.new"
mv "$skein_dir/start-door.sh.new" "$skein_dir/start-door.sh"

# ---------------------------------------------------------------------------
# The kit that runs start-door.sh at every sandbox start.
#
# The script above is the CURE; this is what applies it without a person. sbx's `commands.startup`
# runs at every sandbox start and is the only hook this sandbox has.
#
# **Written here as well as by skein, and the two must be byte-identical.** `fleet::ensure_fleet_kit`
# writes it from `src/fleet-kit-spec.yaml` on every server start — no use on a FIRST install, where
# nothing has ever run against this volume and the next line a person types is the `sbx run -d` that
# would attach the kit. `the_two_writers_of_the_fleet_kit_agree` compares these bytes against that
# file, so this copy cannot rot into a kit that does nothing.
#
# On the VOLUME rather than in `.skein`, because `sbx` reads a kit from the host and is never inside
# the fleet. The heredoc is QUOTED for start-door.sh's reason: nothing here is interpolated, and the
# fleet root is written in literally because that is what the running sandbox will be.
fleet_kit="$skein_home/fleet-kit"
mkdir -p "$fleet_kit"
cat > "$fleet_kit/spec.yaml.new" <<'KITEOF'
schemaVersion: "1"
kind: mixin
name: skein-fleet
displayName: skein fleet sandbox
description: >-
  The kit for the FLEET sandbox itself, not for a box. Its whole job is one command
  at every sandbox start: put the cockpit's door back. The fleet sandbox has pid 1
  `tini` and no init — no systemd, no cron, no systemctl, measured in a live fleet —
  so nothing else in it survives a stop and start, and every restart left the whole
  install intact on disk with nothing serving. sbx's own `commands.startup` is the
  only thing in reach that runs at every start, and skein already trusts it for
  boxes. Installed by skein (fleet::ensure_fleet_kit) and by bootstrap.sh, which must
  write the same bytes; `the_two_writers_of_the_fleet_kit_agree` is what holds them to it.

commands:
  startup:
    # Guarded, and never `exec`: a sandbox created but not yet bootstrapped has no
    # `.skein` at all, and a startup command that fails there would make a fresh
    # create look broken at the one moment a person cannot tell a missing feature
    # from a missing install. Absent is a normal state on exactly one path — the
    # window between `sbx create` and bootstrap.sh — so it exits 0 and says nothing.
    #
    # `start-door.sh` is the same file the install runs and the same file a person
    # runs by hand. It is safe to run when the door is already open: it reloads
    # across the existing socket rather than rebinding, so the port is never free
    # (architecture §9.4). That is what makes it correct to run unconditionally at
    # every start rather than only when something looks wrong.
    - command:
        - bash
        - -c
        - 'd=/boxes/.skein/start-door.sh; if [ -x "$d" ]; then "$d"; fi'
      user: "1000"
      description: Put the cockpit's door back after a sandbox start
KITEOF
mv "$fleet_kit/spec.yaml.new" "$fleet_kit/spec.yaml"

"$skein_dir/start-door.sh"

# ---- the build that answers, which is the only one that counts ----------------------------------

# "The binary on disk is new" is not the question. It was true for the whole of an upgrade whose
# cockpit went on serving the old build from a process started before the install — the file had
# been replaced, the process never was, and the page answered 200 throughout (SKEIN-1020). So the
# install is not called done until the server holding the port SAYS it is this build.
#
# The server knows: `build.rs` stamps `git describe --always --dirty`, run in this checkout, and
# `/api/health` reports it as `build`. The same command here gives the same string for the same
# tree, so this compares like with like rather than a short sha with a describe.
#
# python3 rather than curl: the doorway is python3, so it is present wherever there is a door to
# ask, and it lets the request refuse a proxy — a sandbox with `$http_proxy` set would otherwise
# send a question about 127.0.0.1 to the proxy and report what the proxy said.
expected=$(git -C "$src" describe --always --dirty 2>/dev/null || true)
answering_build() {
  SKEIN_ASK_PORT="$port" SKEIN_ASK_TOKEN="$skein_home/api-token" python3 - 2>/dev/null <<'ASK' || true
import json, os, urllib.request
req = urllib.request.Request("http://127.0.0.1:%s/api/health" % os.environ["SKEIN_ASK_PORT"])
try:
    with open(os.environ["SKEIN_ASK_TOKEN"]) as f:
        req.add_header("Authorization", "Bearer " + f.read().strip())
except OSError:
    pass
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
print(json.load(opener.open(req, timeout=10)).get("build", ""))
ASK
}

# Every process holding a listening socket on the cockpit's port: the kernel's own table, then the
# descriptors that point at it. Only asked on the way to a failure, so its cost does not matter.
port_holders() {
  hex=$(printf ':%04X' "$port")
  for inode in $(awk -v p="$hex" '$4 == "0A" && substr($2, length($2) - 4) == p { print $10 }' \
    /proc/net/tcp /proc/net/tcp6 2>/dev/null); do
    find /proc/[0-9]*/fd -lname "socket:\[$inode\]" 2>/dev/null | cut -d/ -f3
  done | sort -un
}

# A reload is a re-exec and a server start, so the answer is seconds away rather than immediate.
# `$SKEIN_BOOTSTRAP_ANSWER_WAIT` is a seam for the tests of the failure below, which would
# otherwise each spend the whole wait finding out what they already know.
answer_wait="${SKEIN_BOOTSTRAP_ANSWER_WAIT:-60}"
await_expected() {
  waited=0
  answering=''
  while :; do
    answering=$(answering_build)
    if [ -n "$expected" ] && [ "$answering" = "$expected" ]; then
      return 0
    fi
    [ "$waited" -ge "$answer_wait" ] && return 1
    sleep 1
    waited=$((waited + 1))
  done
}

# ---- and when it is not: fix what is provably ours, and hand over what is not --------------------
#
# A cockpit left serving the old build is not an error to report and walk away from; the person
# who ran this wanted the new build answering, and when what stands in the way is a piece of THIS
# install — a doorway or server started before the upgrade and never replaced — this file is the
# one thing that can say so for certain, so it stops it and asks again. A short gap on the port is
# the price, and a stale cockpit is worse.
#
# "Provably ours" is narrow on purpose: the process's command line names this install's doorway or
# server path as an argument, or its environment names this install's volume or fleet root. A
# fixture's doorway on the same port (SKEIN-1019), another user's process, or one whose `/proc` this
# user cannot read, is none of those, and is left alone — named, with the one command that frees
# the port, because stopping a stranger's process on a guess is the worse failure.
is_ours() {
  tr '\0' '\n' <"/proc/$1/cmdline" 2>/dev/null | grep -qxF -e "$doorway" -e "$server" && return 0
  tr '\0' '\n' <"/proc/$1/environ" 2>/dev/null \
    | grep -qxF -e "SKEIN_HOME=$skein_home" -e "SKEIN_FLEET_ROOT=$fleet_root"
}

# **Never this run's own ancestors, however much they look like ours.** The Update button runs this
# file in a tmux pane, and when the cockpit's own server was what started that tmux server, the tmux
# server inherited the listening socket — skein-server's descriptor 3 was not close-on-exec before
# SKEIN-1035; a tmux server an older build started still holds it for as long as it lives, including
# under the very first Update from that build to a fixed one — and has the server's environment, so
# it holds :$port and names this install's volume, which is both halves of "provably ours" above.
# Stopping it ends this run mid-sentence, with no exit status recorded, which is the stranded pane
# of SKEIN-1032 by another road. Measured on tmux 3.x: a tmux server started by a process holding a
# listener holds it too; the pane under it does not.
#
# `$$` rather than the subshell's own pid because that is what POSIX sh has; the subshell the Update
# button runs this in is a child of `$$`, holds nothing, and so needs no entry of its own.
ancestors=''
a=$$
while [ -n "$a" ] && [ "$a" -gt 1 ] 2>/dev/null; do
  ancestors="$ancestors $a "
  a=$(awk '/^PPid:/ { print $2 }' "/proc/$a/status" 2>/dev/null)
done
is_ancestor() { case "$ancestors" in *" $1 "*) return 0 ;; esac; return 1; }

# What the fix below may stop: provably ours, and not something this run is running inside.
stoppable() { is_ours "$1" && ! is_ancestor "$1"; }

# Field 22 of `/proc/<pid>/stat`, the process's start time: with the pid, what makes "the process I
# read a moment ago" a fact rather than a number that may since have been reused. Empty for a
# process that is gone or a zombie — neither of which is anything to signal.
started_at() {
  awk '{ sub(/^.*\) /, ""); if ($1 != "Z") print $20 }' "/proc/$1/stat" 2>/dev/null
}

# Stop pid $1, known to have started at $2: TERM, a few seconds, then KILL — rechecking before
# each signal that the pid is still that process, so a pid reused in between is never touched.
stop_recorded() {
  [ -n "$2" ] && [ "$(started_at "$1")" = "$2" ] || return 0
  kill -TERM "$1" 2>/dev/null || return 0
  n=0
  while [ "$n" -lt 30 ]; do
    [ "$(started_at "$1")" = "$2" ] || return 0
    sleep 0.1
    n=$((n + 1))
  done
  [ "$(started_at "$1")" = "$2" ] && kill -KILL "$1" 2>/dev/null
  return 0
}

port_free() {
  hex=$(printf ':%04X' "$port")
  ! awk -v p="$hex" '$4 == "0A" && substr($2, length($2) - 4) == p { found = 1 } END { exit !found }' \
    /proc/net/tcp /proc/net/tcp6 2>/dev/null
}

if ! await_expected; then
  holders=$(port_holders)
  mine=''
  strangers=''
  for p in $holders; do
    if stoppable "$p"; then mine="$mine $p"; else strangers="$strangers $p"; fi
  done

  if [ -n "$mine" ] && [ -z "$strangers" ] && [ -n "$expected" ]; then
    say "the cockpit on :$port answers as ${answering:-nothing}, not $expected; stopping pid${mine}, this"
    say "install's own doorway/server from before the upgrade, which still holds the port"
    # Recorded before anything is signalled: the supervisor loop that would restart a stopped
    # doorway (its parent, when that parent is this install's `while [ -f '$doorway' ]`), then the
    # holders themselves. The loop goes first, or it puts back what was just stopped.
    loops=''
    for p in $mine; do
      parent=$(awk '/^PPid:/ { print $2 }' "/proc/$p/status" 2>/dev/null)
      if [ -n "$parent" ] && tr '\0' ' ' <"/proc/$parent/cmdline" 2>/dev/null \
        | grep -qF -- "while [ -f '$doorway' ]"; then
        loops="$loops $parent:$(started_at "$parent")"
      fi
    done
    recorded=''
    for p in $mine; do recorded="$recorded $p:$(started_at "$p")"; done
    for e in $loops $recorded; do stop_recorded "${e%%:*}" "${e#*:}"; done

    n=0
    while ! port_free && [ "$n" -lt 100 ]; do sleep 0.1; n=$((n + 1)); done
    # Stopping the loop above ends that session. If it was the one on `$sock`, nothing supervises
    # the port any more, and the door has to be put back; if another is there, its doorway is
    # already retrying the bind and needs nothing.
    tmux -S "$sock" has-session -t skein-server 2>/dev/null || "$skein_dir/start-door.sh"
    await_expected || true
  fi
fi

if [ -z "$expected" ] || [ "$answering" != "$expected" ]; then
  say "built $revision and installed it at $server, but the cockpit on :$port is not serving it."
  if [ -n "$answering" ]; then
    say "What answers on :$port says it is build $answering — a process started before this"
    say "install still holds the port, so the cockpit is the old build however new the file is."
  else
    say "Nothing answered /api/health on :$port with a build within ${answer_wait}s."
  fi
  holders=$(port_holders)
  strangers=''
  if [ -n "$holders" ]; then
    say "Holding :$port:"
    for p in $holders; do
      say "    pid $p  $(tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null)"
      stoppable "$p" || strangers="$strangers $p"
    done
  fi
  for p in $strangers; do
    if is_ancestor "$p"; then
      say "pid $p is the process this $what is running under, which inherited :$port from the cockpit"
      say "that started it; this $what does not stop it, because that would end the $what itself."
    fi
  done
  if [ -n "$strangers" ]; then
    say "This install did not stop pid${strangers} itself: it stops only what it can prove is its"
    say "own — a process naming $doorway or $server, or this fleet's volume or"
    say "root in its environment — and that is not. If it should not be holding :$port, this frees it:"
    say ""
    say "    kill${strangers}"
  elif [ -z "$holders" ] && port_free; then
    say "Nothing is listening on :$port at all, so the doorway this install started has not come up."
    say "What it says is here:"
    say ""
    say "    tmux -S $sock capture-pane -p -t skein-server"
  elif [ -z "$holders" ]; then
    say "Nothing this user can see is holding :$port, so whatever holds it belongs to another user and"
    say "this install cannot tell what it is. If it should not be holding :$port, this frees it:"
    say ""
    say "    sudo fuser -k $port/tcp"
  else
    say "It is this install's own, and stopping it did not bring the new build up. What the doorway"
    say "says is here:"
    say ""
    say "    tmux -S $sock capture-pane -p -t skein-server"
  fi
  exit 1
fi

say "built $revision"
say "the cockpit is listening on :$port inside the sandbox, and answers as $answering — the build just installed"
# The rest is for a first install, and the Update button's reader already has the cockpit open.
[ "$from" = update ] && exit 0
cat >&2 <<EOF

skein: open what the cockpit prints for its token, and the install is done.

If the browser cannot reach :$port, the create did not publish it. Publishing is the one thing a
sandbox cannot do for itself, which is why it belongs on the create -- "sbx create ... -p
$port:$port ..." -- and a sandbox already made without it is repaired from the host with:

    sbx ports <sandbox> --publish $port:$port
EOF
