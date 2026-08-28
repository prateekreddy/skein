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
# It is also **the one implementation of the build**. `fleet::build_script` runs this same file with
# SKEIN_BOOTSTRAP_STOP_AFTER=build, so an upgrade from the cockpit and a first install cannot come
# out differently — a second copy in Rust would be right on the day it was written.
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

# Every path skein uses inside the sandbox, derived exactly as `src/fleet.rs` derives them —
# `fleet_root()` and the `.skein` beneath it. A test asserts these agree with the Rust rather than
# trusting this comment.
fleet_root="${SKEIN_FLEET_ROOT:-/boxes}"
skein_dir="$fleet_root/.skein"
src="$skein_dir/src"
toolchain="$skein_dir/toolchain"
server="$skein_dir/skein-server"
doorway="$skein_dir/server-doorway.py"
stamp="$skein_dir/server.door"
sock="$skein_dir/server.tmux"
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

# `build` stops after the binary is installed and the revision printed — how the cockpit's upgrade
# path reuses this file without restarting anything. Empty means go all the way to a serving fleet.
stop_after="${SKEIN_BOOTSTRAP_STOP_AFTER:-}"

say() { printf 'skein: %s\n' "$1" >&2; }

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
if [ ! -w "$fleet_root" ] && ! { mkdir -p "$fleet_root" 2>/dev/null && [ -w "$fleet_root" ]; }; then
  say "creating the fleet root $fleet_root, which needs sudo inside the sandbox"
  sudo mkdir -p "$fleet_root"
  sudo chown "$(id -u):$(id -g)" "$fleet_root"
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

if [ -n "$need" ]; then
  say "the image is missing$need — installing, once, into the sandbox"
  # A freshly created sandbox is still running its own first-boot apt, and apt refuses to run twice.
  # Outlast it rather than failing the install on a race — the same wait, and for the same measured
  # reason, as `SUBSTRATE_SCRIPT` in src/fleet.rs. A `fuser` the image does not have simply fails,
  # which ends the wait, which is the right answer when there is no lock to see.
  waited=0
  while [ "$waited" -lt 120 ] \
    && sudo fuser /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock >/dev/null 2>&1; do
    sleep 3
    waited=$((waited + 3))
  done
  # `update` FIRST: a fresh image ships an empty index, where install reports "Package
  # 'build-essential' has no installation candidate" — which reads as a missing package and is a
  # missing index.
  #
  # And `|| true` on all of it, deliberately: apt's exit status is the wrong judge. What decides is
  # whether the commands are on the PATH afterwards, which is what the check below asks. An install
  # that exits non-zero over an unrelated warning must not end an install that in fact worked.
  { sudo apt-get update -qq && sudo apt-get install -y -qq $need; } \
    || { sleep 5; sudo apt-get update -qq && sudo apt-get install -y -qq $need; } \
    || true
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

# ---- the toolchain, kept out of every box's reach ------------------------------------------------

export CARGO_HOME="$toolchain/cargo"
export RUSTUP_HOME="$toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
mkdir -p "$src" "$toolchain"

if ! command -v cargo >/dev/null 2>&1; then
  say "installing a Rust toolchain in $toolchain (this is not the sandbox's own, on purpose)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --default-toolchain stable >/dev/null
fi

# ---- the source ---------------------------------------------------------------------------------

if [ -d "$src/.git" ]; then
  say "fetching ${ref:-the default branch}"
  # `HEAD` is a ref the remote always has, and it is the same thing a bare clone would take.
  git -C "$src" fetch --depth 1 origin "${ref:-HEAD}"
  git -C "$src" checkout -f FETCH_HEAD
else
  say "cloning $url at ${ref:-the default branch}"
  if [ -n "$ref" ]; then
    git clone --depth 1 --branch "$ref" "$url" "$src"
  else
    git clone --depth 1 "$url" "$src"
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

# `SKEIN_IN_FLEET=1` is how skein learns where it is running, and this script is the only thing
# that can tell it. `deployment.rs` says out loud that the deployment is **declared and never
# detected** — every sniff (is `/run/sandbox` there, is `sbx` on `$PATH`) is a guess about somebody
# else's machine — so a server nobody declares believes it is on the host and reaches for an `sbx`
# that is not in here. Nothing else in the tree sets it: a `grep` for the variable finds
# `deployment.rs`, one test, and this line.
#
# Unconditional, because it is not a judgement. A server installed by this file runs inside the
# sandbox by construction; there is no arrangement in which the binary it starts is on a host.
#
# The supervisor loop, and its condition. `while [ -f "$doorway" ]` rather than `while true`: a
# fleet whose `.skein` has been deleted leaves a bash restarting a missing script at 0.5 Hz for
# ever, and 105 of those were measured before the condition was added. The `sleep` is conditional so
# a crash-loop backs off while a doorway that had been up is replaced in the time python takes to
# start — that gap is the port standing empty.
supervise="while [ -f '$doorway' ]; do \
began=\$(date +%s); \
SKEIN_HOME='$skein_home' SKEIN_IN_FLEET=1 python3 '$doorway' '$port' '$server' '$stamp'; \
[ \$((\$(date +%s) - began)) -lt 5 ] && sleep 2; \
done"

if tmux -S "$sock" has-session -t skein-server 2>/dev/null; then
  say "the cockpit is already running; sending it the reload it uses to swap binaries"
  # SIGUSR1 re-execs the doorway across the SAME descriptor, so the socket is never closed and the
  # port is never free. Killing and restarting here would reopen exactly the window the doorway
  # exists to close (architecture §9.4).
  pid=$(cut -d' ' -f1 <"$stamp" 2>/dev/null || true)
  [ -n "${pid:-}" ] && kill -USR1 "$pid" 2>/dev/null || true
else
  tmux -S "$sock" new-session -d -s skein-server "$supervise"
fi

say "built $revision"
say "the cockpit is listening on :$port inside the sandbox"
cat >&2 <<EOF

skein: open what the cockpit prints for its token, and the install is done.

If the browser cannot reach :$port, the create did not publish it. Publishing is the one thing a
sandbox cannot do for itself, which is why it belongs on the create -- "sbx create ... -p
$port:$port ..." -- and a sandbox already made without it is repaired from the host with:

    sbx ports <sandbox> --publish $port:$port
EOF
