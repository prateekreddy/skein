#!/usr/bin/env bash
# sync-install.sh — wire this box to the `sync` work tracker, and install the discipline for it.
#
# Lives in the store (`<store>/skein/bin/`), which is mounted live into every box for the repo. That
# is deliberately not the kit: a kit only reaches boxes created after it changed, and the whole point
# is that an EXISTING box can be wired up too. Refreshed host-side on every launch, so a box picks up
# a newer version of this script by doing nothing.
#
# Runs from two places, and must behave the same in both:
#   - the kit's startup hook, on every box start (silently does nothing until credentials exist);
#   - `skein::sync_provision_box`, right after it writes those credentials.
#
# The `sync` MCP server comes from upstream's PLUGIN now, not from a registration of skein's — see
# the block below for why the two cannot both be present. Codex is the exception, and not as a
# fallback: plugins are a Claude Code feature, so the TOML block is the only way Codex ever gets
# these tools.
#
# Installs three things, and only AFTER the tools exist:
#   - a "Work tracking" section in CLAUDE.md (and so in AGENTS.md) — always in context, because
#     "claim before you start" has to fire when the agent was not thinking about tools at all;
#   - a memory, so the rules that must fire unprompted survive into a fresh session;
#   - the `work-tracking` skill — but only where the plugin is not already shipping it.
# An instruction to "call capture" in a box with no sync server is a rule the agent cannot follow
# and will learn to read past, which is worse than no instruction at all.
#
# ONCE per box, then hands off. Non-destructive copying is not enough on its own: after setup the box
# owns all of this, and owning it includes deleting the parts it does not want. A script that
# re-asserts on every start makes an edit the box cannot make stick. SKEIN_SYNC_FORCE=1 re-applies —
# that is how a rotated token gets installed.
#
# Fail-soft throughout: this runs during startup, and a work-tracking problem must never stop a box.
set -uo pipefail

# Every `claude` call below reaches the network, and this script is the LAST thing box provisioning
# runs — so anything unbounded here is unbounded in the caller's deadline. It was: the marketplace
# `add`/`install` branch had no limit while the `update` branch beside it had one, and a fleet with no
# GitHub token sat on a credential prompt until provisioning was killed. The box came up anyway, its
# startup marker was never written, and the next agent launch read that as "box setup failed".
#
# **A per-call bound is not a bound.** Four calls of 150s is ten minutes, and the startup that
# invoked this has less than that. So the budget is the SCRIPT's, one deadline, and each call spends
# what is left of it — the same shape `fleet-agent.py` uses for the request it is serving.
#
# Refusing outright when there is no `timeout(1)` rather than falling back to running unbounded: the
# fallback is the bug. A box is a GNU userland and has it; anywhere that does not, a tracker this
# script could not wire is the box we had yesterday, which is what fail-soft means here.
SYNC_BUDGET="${SKEIN_SYNC_BUDGET:-240}"
sync_deadline=$(( $(date +%s) + SYNC_BUDGET ))
# No single call gets the whole budget: one stuck clone would otherwise spend every second the
# later steps need, and the box would end up with a marketplace and no registration.
SYNC_CALL_MAX=120
claude_bounded() {
  local left
  if ! command -v timeout >/dev/null 2>&1; then
    echo "[sync] no timeout(1) here, so a network call cannot be bounded — skipping rather than \
risking an unbounded startup" >&2
    return 1
  fi
  left=$(( sync_deadline - $(date +%s) ))
  if [ "$left" -le 0 ]; then
    echo "[sync] out of time; the rest is left for the next start" >&2
    return 1
  fi
  [ "$left" -le "$SYNC_CALL_MAX" ] || left="$SYNC_CALL_MAX"
  timeout "$left" claude "$@"
}

# Fail rather than ask. The marketplace is a private repo cloned over the box's forwarded ssh-agent,
# and with no key loaded git's default is to PROMPT — on a stdin that is a pipe, which is a wait with
# nothing at the other end of it. These turn that wait into an error, which is a thing this script
# can report and recover from.
export GIT_TERMINAL_PROMPT=0
export GIT_SSH_COMMAND="${GIT_SSH_COMMAND:-ssh} -o BatchMode=yes"

# The store is two levels up from this script — exact, and free of any guess about layout.
store="$(cd "$(dirname "$0")/../.." 2>/dev/null && pwd)"
src="$store/skein/sync"

project="${WORKSPACE_DIR:-}"
[ -n "$project" ] || project="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# Credentials: the environment first, then the box-private file skein writes. Never the store — the
# agent token is a bearer credential and the store is mounted live into every box for this repo.
cred="$HOME/.config/sync/env"
# shellcheck disable=SC1090
[ -r "$cred" ] && . "$cred" 2>/dev/null
url="${SYNC_GATEWAY_URL:-}"
token="${SYNC_AGENT_TOKEN:-}"

# The URL is what makes a box wired; the token is not, any more. Claude reaches the gateway through
# the plugin's OAuth, so requiring a minted token before doing anything would gate the whole install
# on a credential only Codex still uses — and a box could not be given a tracker without one being
# minted for it. The token is checked where it is spent, in the Codex block.
#
# The URL can arrive three ways, in order of how specific they are: the box's own credential file,
# an environment override, then the repo-wide gateway the host publishes into the store. The last is
# what makes wiring a repo one action — every box of it reads the same file on start.
#
# Read here and written into this box's OWN `~/.claude/settings.json` further down, rather than left
# for Claude Code to pick up from the store's settings: project-scope `env` does not reach the
# plugin's `.mcp.json`. Measured, not assumed — with the variable set only in project settings, the
# plugin still resolved to the default gateway compiled into it. User scope does work.
[ -n "$url" ] || url="${SYNC_MCP_URL:-}"
[ -n "$url" ] || url="$(sed -n '1s/[[:space:]]*$//p' "$src/gateway" 2>/dev/null)"
if [ -z "$url" ]; then
  # The common case at box startup. Quiet on purpose: a box with no tracker is not a broken box.
  exit 0
fi
if [ ! -d "$src" ]; then
  echo "[sync] $src is missing — restart the host server to refresh this store" >&2
  exit 0
fi

slug="$(printf '%s' "$project" | sed 's#/#-#g')"
state="$HOME/.local/state/skein"
stamp="$state/sync-$slug.done"

# --- the plugin, BEFORE the stamp gate ------------------------------------------------------------
#
# Ahead of the gate on purpose, and it is the only thing that is. The stamp means "this box owns its
# CLAUDE.md, its memory and its skill now" — earned, and re-asserting over it is exactly what that
# gate exists to prevent. The plugin is a different claim: upstream started shipping one after those
# boxes were wired, so it is not a re-assertion of something the box already owns, it is a thing the
# box never had. Behind the gate it would reach only boxes created from here on, which for an
# established fleet means none of them.
#
# It gets its own marker for the same reason the stamp exists: install once, then the box owns that
# too. A box that removes the plugin has made a decision, and a script that reinstalls it every
# start makes that decision unmakeable.
#
# Per box rather than per project, because `plugin install` is user-scoped — one box, one answer,
# whatever repo it is working in.

# Which version this box is serving, read from the marketplace checkout the plugin loads from.
plugin_version() {
  sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$HOME/.claude/plugins/marketplaces/sync/plugin/.claude-plugin/plugin.json" 2>/dev/null | head -1
}

# Keep it CURRENT, not merely present.
#
# The marker answers "does this box have the plugin", and that is the only question it should be
# answering. It was quietly answering "which version" as well: a box that installed once never looked
# again, so the fleet sat on 0.2.0 for months — the version whose lease monitor reads a session id
# Claude Code does not set, polls a file nothing writes, and therefore keeps no claim alive at all.
# Silently, which is the worst way for a guard to fail: everything looks armed. Presence stays a
# decision the box owns; the version is not a decision and must not be frozen by one.
#
# `marketplace update` is the whole mechanism — the marketplace checkout IS what the plugin loads
# from, so refreshing it refreshes the plugin. Time-boxed and fail-soft, because a box that cannot
# reach the marketplace must still start, with whatever it already has.
#
# Announced only when the version actually moves. A coordination plugin changing under you without a
# word is how a confusing morning starts, and silence is the default the rest of this script keeps.
refresh_plugin() {
  local before after
  before="$(plugin_version)"
  claude_bounded plugin marketplace update sync >/dev/null 2>&1 || return 0
  after="$(plugin_version)"
  if [ -n "$after" ] && [ "$before" != "$after" ]; then
    echo "[sync] plugin updated ${before:-none} -> $after" >&2
  fi
  return 0
}

plugin="no"
plugin_marker="$state/sync-plugin.done"
if [ -e "$plugin_marker" ] && [ -z "${SKEIN_SYNC_FORCE:-}" ]; then
  # Already handled once, so the presence question is a file test rather than a subprocess. The
  # version question is not answerable from a file test, and is the one that went stale.
  plugin="yes"
  command -v claude >/dev/null 2>&1 && refresh_plugin
elif command -v claude >/dev/null 2>&1; then
  # The plugin carries three things skein has no copy of and cannot write: the lease MONITOR, which
  # keeps a claim alive as a process rather than as an obligation the model must remember; the
  # session HOOKS (hand work back on exit, report what is still held on resume, fence `git push`
  # against a lapsed lease); and the skill, now three files where skein vendored one.
  #
  # The monitor is the one that matters most here. A box compacts constantly, and a lease kept alive
  # by "call heartbeat periodically" is a promise across a context boundary — upstream removed it for
  # exactly the failure it caused: the lease lapsed, another agent took the item, and the two
  # collided. A process cannot be talked out of running.
  #
  # Asked before installed, because a box may already have it by hand — the sync repo's own box does,
  # with its own OAuth grant. Reinstalling over that would be skein taking something that was not
  # its to take.
  if claude_bounded plugin list 2>/dev/null | grep -q 'sync@sync'; then
    plugin="yes"
    # Installed by hand, and still kept current. `marketplace update` pulls from whatever source that
    # marketplace was added from, so a box pointed at a local checkout stays pointed at it — this
    # updates the plugin without taking the decision of where it comes from.
    refresh_plugin
  # The marketplace is a private repo, so this clones over the box's forwarded ssh-agent. Fail-soft
  # like everything else here: a box without the plugin is the box we had yesterday.
  elif claude_bounded plugin marketplace add prateekreddy/sync >/dev/null 2>&1 \
       && claude_bounded plugin install sync@sync >/dev/null 2>&1; then
    plugin="yes"
    echo "[sync] installed the sync plugin — lease monitor, session hooks and the skill" >&2
  else
    echo "[sync] could not install the sync plugin (needs ssh access to prateekreddy/sync); the tracker still works, without the lease monitor or the push fence" >&2
  fi
  # Only on success, so a box that failed on a network blip tries again next start rather than
  # recording a plugin it does not have.
  [ "$plugin" = "yes" ] && mkdir -p "$state" 2>/dev/null && : > "$plugin_marker" 2>/dev/null
fi

if [ -e "$stamp" ] && [ -z "${SKEIN_SYNC_FORCE:-}" ]; then
  echo "[sync] already set up — this box owns its tracker config now (SKEIN_SYNC_FORCE=1 to re-apply)" >&2
  exit 0
fi

# The gateway URL is a base; the MCP endpoint is /mcp under it. Accept either spelling so a pasted
# endpoint URL does not silently become /mcp/mcp.
url="$(printf '%s' "$url" | sed 's#/*$##')"
case "$url" in
  */mcp) ;;
  *) url="$url/mcp" ;;
esac

registered="no"

# Point the plugin's server at THIS fleet's gateway.
#
# The plugin declares its url as `${SYNC_MCP_URL:-<upstream's own>}`, so this variable is the
# supported seam and the only one that survives a plugin update — there is nothing to edit inside the
# plugin, and nothing to redo when it changes. The gateway URL still comes from the same place it
# always did: the connection configured in Settings, written into this box by `sync_provision_box`.
# What changed is where it lands, not where it comes from.
#
# `~/.claude/settings.json` is Claude Code's own and is bound private per box, so this is box-local
# and cannot leak one repo's gateway into another's. Merged rather than written: the file already
# carries marketplaces, enabled plugins and notification settings that are none of our business.
set_sync_url() {
  python3 - "$HOME/.claude/settings.json" "$1" <<'PY' 2>/dev/null
import json, os, sys, tempfile
path, url = sys.argv[1], sys.argv[2]
try:
    with open(path) as f:
        data = json.load(f)
    if not isinstance(data, dict):
        raise ValueError
except FileNotFoundError:
    data = {}
except Exception:
    # A settings file we cannot parse is one we must not rewrite: replacing it would take out
    # whatever Claude Code is keeping there. Failing here leaves the fallback below to register.
    sys.exit(1)
env = data.get("env")
if not isinstance(env, dict):
    env = {}
if env.get("SYNC_MCP_URL") == url:
    sys.exit(0)
env["SYNC_MCP_URL"] = url
data["env"] = env
os.makedirs(os.path.dirname(path), exist_ok=True)
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path))
with os.fdopen(fd, "w") as f:
    json.dump(data, f, indent=2)
os.replace(tmp, path)
PY
}

# Claude Code gets the `sync` server from the PLUGIN now, not from a registration of skein's.
#
# The two cannot coexist by design: a hand-added `sync` entry WINS and the plugin's is skipped with
# a note. skein added one on every box it wired, so leaving it in place would shadow the plugin we
# just installed — the tools would still work, over a long-lived token, and the monitor and hooks
# would be dead weight beside them.
#
# The trade is real and worth naming: the plugin authenticates over OAuth, so a box signs in once in
# a browser instead of carrying a minted token. That is upstream's primary path, and the sync repo's
# own box already runs this way.
if command -v claude >/dev/null 2>&1; then
  # Removed, not merely no longer written: every box skein wired before today carries one, and a
  # hand-added entry WINS over a plugin's — so leaving it would shadow the plugin's own server and
  # the monitor and hooks would sit dead beside a set of tools that still worked. There is no
  # fallback registration here on purpose. The plugin is the only source of this server now.
  claude_bounded mcp remove sync -s user >/dev/null 2>&1 \
    || claude_bounded mcp remove sync >/dev/null 2>&1 || true
  if set_sync_url "$url"; then
    [ "$plugin" = "yes" ] && registered="yes"
  else
    echo "[sync] could not write SYNC_MCP_URL into ~/.claude/settings.json — the plugin would reach its built-in default gateway, not yours" >&2
  fi
fi

# Codex. Its config is TOML that may carry hand-written entries, so append only when the section is
# absent and never rewrite it — except on a forced re-apply, which is how a new token arrives.
#
# Still a registration, and it has to be: plugins are a Claude Code feature. Codex cannot load one,
# so the token route is not a fallback here, it is the only route — which is also why the minted
# token is still worth writing into a box even though Claude no longer uses it.
# Skipped without a token rather than written with an empty one: a bearer header of `Bearer ` is a
# registration that looks complete and 401s on the first call, which is a worse place to discover
# the credential is missing than here.
codex_registered="no"
codex_cfg="$HOME/.codex/config.toml"
if [ -z "$token" ]; then
  :
elif command -v codex >/dev/null 2>&1 || [ -f "$codex_cfg" ]; then
  mkdir -p "$HOME/.codex" 2>/dev/null || true
  if [ -n "${SKEIN_SYNC_FORCE:-}" ] && [ -f "$codex_cfg" ]; then
    tmpcfg="$codex_cfg.skein.tmp"
    if awk '/^\[mcp_servers\.sync\]/ { skip=1; next } /^\[/ { skip=0 } skip != 1 { print }' \
         "$codex_cfg" > "$tmpcfg" 2>/dev/null; then
      mv "$tmpcfg" "$codex_cfg" 2>/dev/null || rm -f "$tmpcfg" 2>/dev/null
    else
      rm -f "$tmpcfg" 2>/dev/null
    fi
  fi
  if grep -Fq '[mcp_servers.sync]' "$codex_cfg" 2>/dev/null; then
    registered="yes"; codex_registered="yes"
  elif printf '\n[mcp_servers.sync]\nurl = "%s"\nhttp_headers = { Authorization = "Bearer %s" }\n' \
         "$url" "$token" >> "$codex_cfg" 2>/dev/null; then
    registered="yes"; codex_registered="yes"
  else
    echo "[sync] could not write $codex_cfg" >&2
  fi
fi

if [ "$registered" != "yes" ]; then
  echo "[sync] no runtime registered — work-tracking docs not installed" >&2
  exit 0
fi

# --- the discipline, now that the tools behind it exist -------------------------------------------

# What we installed, by content hash, so a later correction can tell "skein put this here and
# upstream has moved" from "the box rewrote it". Without this record the two are identical on disk,
# and sync-refresh.sh would have to either touch nothing or overwrite the box's own edits.
manifest="$state/sync-$slug.manifest"
note() {
  [ -r "$2" ] || return 0
  local h
  h="$(sha256sum < "$2" 2>/dev/null | cut -d' ' -f1)" || return 0
  [ -n "$h" ] || return 0
  mkdir -p "$state" 2>/dev/null || return 0
  local tmp="$manifest.tmp.$$"
  { [ -r "$manifest" ] && awk -F'\t' -v k="$1" '$1!=k' "$manifest"; printf '%s\t%s\n' "$1" "$h"; } \
    > "$tmp" 2>/dev/null && mv "$tmp" "$manifest" 2>/dev/null || rm -f "$tmp" 2>/dev/null
}

# 1. The always-in-context rules. Appended once to CLAUDE.md; AGENTS.md is normally the same file
# through a symlink and is skipped as such. If they are two real files, both get it.
for doc in CLAUDE.md AGENTS.md; do
  path="$project/$doc"
  [ -f "$path" ] || continue
  [ -L "$path" ] && continue
  grep -Fq '## Work tracking' "$path" 2>/dev/null && continue
  printf '\n---\n\n' >> "$path" 2>/dev/null || continue
  if cat "$src/work-tracking.block.md" >> "$path" 2>/dev/null; then
    echo "[sync] added the Work tracking section to $doc" >&2
    # Recorded only on the branch that actually wrote it. Noting a hash for a section we found
    # already there would claim the box's own words as ours, and a later refresh would overwrite them.
    note block "$src/work-tracking.block.md"
  fi
done

# 2. The memory, plus its line in the index loaded every session. A memory file with no index entry
# is never recalled, so the two are one step. Both land in the STORE, which is this repo's `.claude`
# — so every box of the repo sees them, not just this one.
mkdir -p "$store/memory" 2>/dev/null || true
if [ ! -e "$store/memory/work-tracking.md" ] \
   && cp "$src/work-tracking.memory.md" "$store/memory/work-tracking.md" 2>/dev/null; then
  note memory "$src/work-tracking.memory.md"
fi
idx="$store/memory/MEMORY.md"
[ -f "$idx" ] || printf '# Memory index\n' > "$idx" 2>/dev/null
if [ -f "$idx" ] && ! grep -Fq '(work-tracking.md)' "$idx" 2>/dev/null; then
  printf '\n## 🗂 Work tracking\n- [Work tracking](work-tracking.md) — `capture` on notice, `claim` before you work, `held` after a restart; the `work-tracking` skill has the rest\n' \
    >> "$idx" 2>/dev/null || true
fi

# 3. The skill. Loaded only when the model judges it relevant, which is why it can afford to be the
# long one — Plane's whole surface and what each tool answers.
#
# Skipped when the plugin is here and Codex is not registered, because the plugin ships this skill
# itself and keeps it current. Two copies of one skill is not a redundancy, it is a fork: the
# vendored one is pinned to whatever upstream commit was last pulled into skein, so the moment an
# argument name changes the box is being taught two contradictory versions of the same tool and
# nothing says which is older.
#
# The test is whether CODEX GOT THE TOOLS, not whether codex exists. Every box has the binary — the
# kit installs it — so `command -v codex` is true in all ten of them and a condition written on it
# never fires, which is a skip that reads like a feature and behaves like nothing. Whether the sync
# server was actually written into Codex's config is the fact that matters: if Codex has the tools,
# something has to teach Codex the rules, and the plugin cannot.
want_skill="yes"
if [ "$plugin" = "yes" ] && [ "$codex_registered" = "no" ]; then
  want_skill="no"
fi
if [ "$want_skill" = "yes" ] \
   && mkdir -p "$store/skills/work-tracking" 2>/dev/null \
   && [ ! -e "$store/skills/work-tracking/SKILL.md" ] \
   && cp "$src/work-tracking.skill.md" "$store/skills/work-tracking/SKILL.md" 2>/dev/null; then
  # The two pages SKILL.md links to. Copied without guarding on their own absence, because they are
  # only ever written together with it — and a SKILL.md whose links go nowhere is the one outcome
  # worth avoiding here.
  cp "$src/work-tracking.organising.md" "$store/skills/work-tracking/organising.md" 2>/dev/null || true
  cp "$src/work-tracking.troubleshooting.md" "$store/skills/work-tracking/troubleshooting.md" 2>/dev/null || true
  note skill "$src/work-tracking.skill.md"
fi

# Stamped last, and only on a run that got this far: a box left half-wired by a failure should be
# finished on the next start, not frozen in that state.
mkdir -p "$state" 2>/dev/null && : > "$stamp" 2>/dev/null || true
echo "[sync] work tracking ready — tracker at $url" >&2
exit 0
