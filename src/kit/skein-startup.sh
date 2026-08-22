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
startup_ready="/tmp/skein-startup.ready"
startup_failed="/tmp/skein-startup.failed"
startup_done="false"
rm -f "$startup_ready" "$startup_failed"
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

# The box identity, which is the sandbox's own only in the one-box-per-sandbox shape. In a shared
# sandbox every box would otherwise report the same SANDBOX_VM_ID, so they would read each other's
# launch spec and overwrite each other's boot report — one box's diagnosis for all of them. skein
# names the box when it knows better.
vmid="$(printenv SKEIN_BOX 2>/dev/null || true)"
[ -n "$vmid" ] || vmid="$(printenv SANDBOX_VM_ID 2>/dev/null || hostname 2>/dev/null || echo unknown)"
vmid="$(printf '%s' "$vmid" | tr / -)"

# Find the skein store: skein writes a per-box launch spec at <store>/skein/launch/<vmid>.json,
# so the store is the one mounted dir that contains that marker. Scanning mounts (rather than
# guessing a sibling path) works for any repo layout — URL-cloned or local-in-place.
# An explicit override wins outright: the fleet path knows the store exactly, and there the scan
# below would find nothing to scan — a box's store is not a mount of its own, it is a directory
# inside the one workspace the whole sandbox mounts.
store="$(printenv SKEIN_STORE 2>/dev/null || true)"
[ -n "$store" ] && [ -d "$store" ] || store=""
if [ -z "$store" ] && [ -r /proc/self/mountinfo ]; then
  while read -r mp; do
    [ -n "$mp" ] || continue
    if [ -f "$mp/skein/launch/$vmid.json" ]; then store="$mp"; break; fi
  done < <(awk '{print $5}' /proc/self/mountinfo 2>/dev/null | sort -u)
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
if [ -n "$need" ] && command -v apt-get >/dev/null 2>&1; then
  installed=0
  # Agent images start apt update in the background. Serialize with that process rather than
  # racing it with another updater: use its fresh indexes first, and update ourselves only
  # when a direct install says they are insufficient. Every network/package step is bounded.
  apt_busy() {
    ps -eo comm= 2>/dev/null \
      | grep -Eq '^[[:space:]]*(apt|apt-get|dpkg)[[:space:]]*$'
  }
  waited=0
  while apt_busy && [ "$waited" -lt 240 ]; do
    sleep 2
    waited=$((waited + 2))
  done
  if ! apt_busy; then
    if timeout 120 sudo apt-get install -y -qq $need 2>/dev/null \
      || { timeout 120 sudo apt-get update -qq 2>/dev/null \
        && timeout 120 sudo apt-get install -y -qq $need 2>/dev/null; }; then
      installed=1
    fi
  fi
  if [ "$installed" = "1" ]; then
    # Package indexes dwarf jq itself and are useless after the one setup pass.
    sudo rm -rf /var/lib/apt/lists/* 2>/dev/null || true
  else
    echo "[skein-kit] could not install required tools:$need" >&2
  fi
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
# Three cases — and every one leaves a boot report, because a silent miss here is the #1 way
# a box ends up with zero hooks while looking perfectly healthy:
#   1. no .claude in the clone  → symlink the whole store (the original path);
#   2. the repo SHIPS a real .claude/ (committed settings/commands — common!) → the old code
#      silently skipped the link and the box ran hookless forever. Now: link the store's
#      skein/ into the repo's .claude (probe scripts resolve the true store through that
#      link) and MERGE the store's hook wiring into the repo's own settings.json (additive,
#      dedup by whole entry, repo's own hooks preserved — jq, which the apt step above ensures);
#   3. .claude is already the store symlink → nothing to do.
link_state="failed"
if [ ! -d "$store" ]; then
  link_state="no-store"
  echo "[skein-kit] no store found — probes will not report; check the launch spec mount" >&2
elif [ -L "$clone_root/.claude" ]; then
  link_state="linked"
elif [ ! -e "$clone_root/.claude" ]; then
  if ln -s "$store" "$clone_root/.claude" 2>/dev/null; then link_state="linked"; else
    echo "[skein-kit] could not link $store -> $clone_root/.claude" >&2
  fi
else
  # case 2: repo ships its own .claude directory — merge, don't skip
  rc="$clone_root/.claude"
  [ -e "$rc/skein" ] || ln -s "$store/skein" "$rc/skein" 2>/dev/null || true
  if [ -L "$rc/skein" ] && command -v jq >/dev/null 2>&1; then
    [ -f "$rc/settings.json" ] || echo '{}' > "$rc/settings.json"
    merged="$(jq -s '
      .[0] as $c | .[1] as $s | ($c.hooks // {}) as $ch |
      $c
      | .hooks = (($s.hooks // {}) | to_entries
          | reduce .[] as $e ($ch;
              .[$e.key] = ((.[$e.key] // []) + ($e.value | map(. as $x
                | select(((($ch[$e.key]) // []) | index($x)) == null))))))
      | (if .tui == null and $s.tui != null then .tui = $s.tui else . end)
      | (if .statusLine == null and $s.statusLine != null then .statusLine = $s.statusLine else . end)
    ' "$rc/settings.json" "$store/settings.json" 2>/dev/null)"
    if [ -n "$merged" ]; then
      tmp="$(mktemp "$rc/.settings.XXXXXX" 2>/dev/null)" \
        && printf '%s\n' "$merged" > "$tmp" && mv "$tmp" "$rc/settings.json" \
        && link_state="merged"
    fi
  fi
  [ "$link_state" = "merged" ] \
    || echo "[skein-kit] repo ships .claude/ and the settings merge failed — probes will not report" >&2
fi
# The mounted store is infrastructure, never a worktree change. Exclude it immediately;
# waiting for SessionStart is too late when a runtime blocks on first-run trust.
git_dir="$(git -C "$clone_root" rev-parse --git-dir 2>/dev/null || true)"
case "$git_dir" in "") ;; /*) ;; *) git_dir="$clone_root/$git_dir" ;; esac
if [ -n "$git_dir" ]; then
  mkdir -p "$git_dir/info" 2>/dev/null || true
  grep -qxF '/.claude' "$git_dir/info/exclude" 2>/dev/null \
    || printf '/.claude\n' >> "$git_dir/info/exclude"
fi

# Expose the project-scoped durable workspace without sharing literal HOME. The helper is
# provider-neutral, lives on the mounted store, and refuses to overwrite a real HOME/shared
# path. Keep startup gated on its verified result: silent non-sharing would risk data loss.
shared_home_state="failed"
shared_home_helper="$store/skein/bin/shared-home.sh"
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
if [ -n "$instruction_file" ] && [ -r "$store/skein/bin/agent-guide.sh" ] \
  && bash "$store/skein/bin/agent-guide.sh" "$store" "$instruction_file" "$instruction_override"; then
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
codex_installer="$store/skein/bin/install-codex-hooks.sh"
if [ -r "$codex_installer" ] && bash "$codex_installer" "$store"; then
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
# why a box is dark. Written into the store when reachable, best-effort.
if [ -d "$store" ]; then
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
  printf '{"ts":"%s","claude_link":"%s","shared_home":"%s","agent_guide":"%s","codex_hooks":"%s","agents":"%s","jq":%s,"tmux":%s,"probe_revision":"%s","branch":"%s"}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$link_state" "$shared_home_state" "$agent_guide_state" "$codex_hooks_state" "$cap" "$jqp" "$tmuxp" "$revision" "$branch" \
    > "$store/skein/boot/$vmid.json" 2>/dev/null || true
fi
# Work tracking: wire this box to the `sync` gateway if credentials are already present.
# The script lives in the store (refreshed host-side every launch), so it reaches boxes
# created before it existed too — this line only decides whether a box wires itself up at
# START, which is what makes a NEW box come up already tracking once Skein has provisioned
# its token. Silent and quick when there are no credentials, which is the common case; a box
# with no tracker is not a broken box, so this can never gate startup.
#
# **Bounded here as well as inside**, and the belt-and-braces is the point. The comment above is a
# claim this line has to keep, and for a long time nothing made it keep it: the script's own network
# calls were unbounded, provisioning ran out of its deadline waiting on one, and the kill landed
# before the `startup_ready` marker below — so the EXIT trap wrote `startup_failed` and the next
# agent launch read a box that was fully provisioned as one whose setup had failed. A step that
# cannot gate startup has to be unable to, rather than intended not to.
sync_install="$store/skein/bin/sync-install.sh"
if [ -r "$sync_install" ]; then
  # The outer bound is longer than the inner one it hands down, for the reason `via_agent` asks the
  # agent for `timeout + 5s`: a deadline that fires first turns the callee's answer into silence,
  # and "[sync] out of time; the rest is left for the next start" is worth more than a kill.
  sync_budget=240
  if command -v timeout >/dev/null 2>&1; then
    SKEIN_SYNC_BUDGET=$((sync_budget - 30)) timeout "$sync_budget" bash "$sync_install" || true
  else
    echo "[skein-kit] no timeout(1), so tracker wiring cannot be bounded — skipped" >&2
  fi
fi

[ "$tools_ok" = "true" ] || exit 1
[ "$shared_home_state" = "linked" ] || exit 1
touch "$startup_ready"
startup_done="true"
exit 0
