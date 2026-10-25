#!/usr/bin/env bash
# shared-home.sh — expose one project's durable shared workspace at $HOME/shared.
#
# The canonical data lives in the repo's Skein store, which is mounted into the box. Real $HOME
# stays private: agent auth/runtime state, credentials, caches, sockets, and toolchains are never
# shared between boxes. This helper is provider-neutral and is called both by kit startup and the
# live SessionStart probe, so new and existing Claude/Codex boxes converge on the same layout.
#
# WHERE in the store is box-self.sh's `skein_shared_home`: for a store skein manages it is
# `store/shared-home`, beside the store's `.claude` and not inside it (SKEIN-1324,
# docs/decisions/shared-home-outside-claude.md). That directory is the host's to make and the
# launcher's to bind into the box, so it is never made here: inside a covered box, a directory made
# where the bind should have been is a tmpfs, and files written there vanish when the box stops.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)"
. "$here/box-self.sh" 2>/dev/null || {
  echo "[skein-shared-home] box-self.sh is missing beside $0 — $HOME/shared is not linked" >&2
  exit 1
}

store="${1:-}"
[ -n "$store" ] && [ -d "$store" ] || {
  echo "[skein-shared-home] shared store unavailable — refusing to claim $HOME/shared is live" >&2
  exit 1
}

canonical="$(skein_shared_home "$store")"
link="$HOME/shared"
if [ "$canonical" = "$store/shared-home" ]; then
  # A store kept elsewhere by `skein add --store`: its shared home is inside it, as it always was.
  mkdir -p "$canonical" 2>/dev/null || {
    echo "[skein-shared-home] cannot create canonical directory: $canonical" >&2
    exit 1
  }
else
  old="$store/shared-home"
  # Files still at the old place mean the host has not moved them, or refused to because both
  # places hold files. Linking to the new place would hide them, so say so and stop.
  if [ -L "$old" ] || { [ -e "$old" ] && ! { [ -d "$old" ] && [ -z "$(ls -A "$old" 2>/dev/null)" ]; }; }; then
    echo "[skein-shared-home] $old still holds shared files, so $HOME/shared is not linked." >&2
    echo "[skein-shared-home] skein moves them to $canonical when it next starts a box of this repo, and stops if both hold files. Move them there yourself, remove $old, and start the box again." >&2
    exit 1
  fi
  if [ -L "$canonical" ] || [ ! -d "$canonical" ]; then
    echo "[skein-shared-home] $canonical is not there, so $HOME/shared is not linked." >&2
    echo "[skein-shared-home] skein makes it and gives it to the box as the box starts. Restart this box." >&2
    exit 1
  fi
fi
[ -d "$canonical" ] && [ -w "$canonical" ] || {
  echo "[skein-shared-home] canonical directory is not writable: $canonical" >&2
  exit 1
}

if [ -L "$link" ]; then
  if [ "$(readlink "$link")" != "$canonical" ]; then
    rm -f "$link" 2>/dev/null && ln -s "$canonical" "$link" 2>/dev/null || {
      echo "[skein-shared-home] cannot repair symlink: $link -> $canonical" >&2
      exit 1
    }
  fi
elif [ -e "$link" ]; then
  echo "[skein-shared-home] refusing to replace real path: $link" >&2
  echo "[skein-shared-home] move it aside explicitly, then restart the agent" >&2
  exit 1
else
  ln -s "$canonical" "$link" 2>/dev/null || {
    echo "[skein-shared-home] cannot create symlink: $link -> $canonical" >&2
    exit 1
  }
fi

[ -L "$link" ] && [ "$(readlink "$link")" = "$canonical" ] || {
  echo "[skein-shared-home] verification failed: $link is not linked to $canonical" >&2
  exit 1
}
