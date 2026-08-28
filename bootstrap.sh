#!/usr/bin/env bash
#
# The whole of installing skein, run INSIDE the fleet sandbox.
#
#   curl -fsSL https://raw.githubusercontent.com/prateekreddy/skein/main/bootstrap.sh -o bootstrap.sh
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
# `${HOME:-}` and not `$HOME`: `set -u` turns an unset HOME into a fatal error on line one, and a
# shell reached through `sbx exec` is not guaranteed to have one. The fleet root is the fallback
# because it is the one directory this script already knows exists.
skein_home="${SKEIN_HOME:-${HOME:-$fleet_root}/.skein}"

# What to build. Public by default because the default install must not need a credential; set
# SKEIN_SOURCE_URL to a private remote (with an `sbx secret` behind it) to build a fork.
url="${SKEIN_SOURCE_URL:-https://github.com/prateekreddy/skein.git}"
ref="${SKEIN_SOURCE_REF:-main}"

# `build` stops after the binary is installed and the revision printed — how the cockpit's upgrade
# path reuses this file without restarting anything. Empty means go all the way to a serving fleet.
stop_after="${SKEIN_BOOTSTRAP_STOP_AFTER:-}"

say() { printf 'skein: %s\n' "$1" >&2; }

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
  say "fetching $ref"
  git -C "$src" fetch --depth 1 origin "$ref"
  git -C "$src" checkout -f FETCH_HEAD
else
  say "cloning $url at $ref"
  git clone --depth 1 --branch "$ref" "$url" "$src"
fi

# ---- the build ----------------------------------------------------------------------------------

# `--locked` because a build that quietly resolved a different dependency tree than the revision
# pins is not "what was published"; it is whatever crates.io looked like this morning.
say "building skein-server — minutes on a cold build, and that cost is the point: what runs is what was published"
cargo build --release --locked --manifest-path "$src/Cargo.toml" --bin skein-server

# Written beside and renamed into place, never over: `cat >` onto an ELF a process is executing
# fails ETXTBSY, and the doorway's two-second retry can otherwise exec a half-written binary. A
# rename is atomic — what it can exec is the old file or the new one, never a fragment.
cp "$src/target/release/skein-server" "$server.new"
chmod 755 "$server.new"
mv "$server.new" "$server"

revision=$(git -C "$src" rev-parse --short HEAD)

if [ "$stop_after" = "build" ]; then
  printf '%s\n' "$revision"
  exit 0
fi

# ---- the door, which is opened before anything is put behind it ----------------------------------

# The doorway comes out of the checkout that was just made, so there is no bootstrapping problem to
# solve: the file skein installs into a sandbox and the file this installs are the same file.
cp "$src/src/server-doorway.py" "$doorway.new"
chmod 755 "$doorway.new"
mv "$doorway.new" "$doorway"

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

skein: one thing left, and it is on the host — the sandbox cannot publish its own port:

    sbx ports <sandbox> --publish $port:$port

Then open what the cockpit prints for its token. If the port is already published, that is done.
EOF
