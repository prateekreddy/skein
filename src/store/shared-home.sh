#!/usr/bin/env bash
# shared-home.sh — expose one project's durable shared workspace at $HOME/shared.
#
# The canonical data lives inside the already-mounted Skein store. Real $HOME stays private: agent
# auth/runtime state, credentials, caches, sockets, and toolchains are never shared between boxes.
# This helper is provider-neutral and is called both by kit startup and the live SessionStart probe,
# so new and existing Claude/Codex boxes converge on the same layout.
set -uo pipefail

store="${1:-}"
[ -n "$store" ] && [ -d "$store" ] || {
  echo "[skein-shared-home] shared store unavailable — refusing to claim $HOME/shared is live" >&2
  exit 1
}

canonical="$store/shared-home"
link="$HOME/shared"
mkdir -p "$canonical" 2>/dev/null || {
  echo "[skein-shared-home] cannot create canonical directory: $canonical" >&2
  exit 1
}
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
