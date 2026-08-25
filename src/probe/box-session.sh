#!/usr/bin/env bash
# skein box-session.sh — write THIS box's narrative signal into the shared store so the cockpit's
# inbox headline / fork-detector / session digest have words to work with.
# SHIPPED AND INSTALLED BY SKEIN (not the repo) — written into <store>/skein/bin/ and wired from
# <store>/settings.json. Modes (the hook event that drives each, in parens):
#   stop  (Stop)          the turn ended → record the last assistant message. Preferred source is
#                         the payload's last_assistant_message; fallback is the tail of the
#                         transcript JSONL the payload points at (same file box-token-usage.sh
#                         already parses — a proven read path).
#   ask   (Notification, matcher permission_prompt|elicitation_dialog|agent_needs_input)
#                         the agent is blocked on you → record the prompt it's blocked on
#                         (the payload's `message` text).
# Writes <store>/sessions/<vmid>.json = {"ts":..,"kind":"stop"|"notification","lastMessage":..,"prompt":..}
# (the schema session_signal() in lib.rs deserializes). Fail-soft, never blocks the turn, and prints
# NOTHING to stdout. Also appends one heartbeat line to <store>/hook-log/<vmid>.jsonl FIRST, so a
# box whose hooks are broken is distinguishable from a box with nothing to say (see hook_health).
set -uo pipefail

mode="${1:-}"
[ -n "$mode" ] || exit 0
payload="$(cat 2>/dev/null || true)"   # the hook's JSON on stdin

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
# Merged layout: when the repo ships its own .claude/, the kit links only skein/ into it — the
# shared store is that link's target parent, NOT the repo dir. Writing here without this hop
# would land signals in the box-local clone where the host can never see them.
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

# The BOX, not the VM. In a shared sandbox every box has the same SANDBOX_VM_ID, so keying a
# signal on it makes every box write one file and the board see none of them report.
#
# SKEIN_BOX names the box wherever it was set: the launcher exports it before it starts the box's
# tmux server (src/box-session.sh), so the agent and every hook it forks inherit it, and every
# placement hop into a shared box exports it too (`wrap` in src/place.rs).
#
# The old chain ran on from there to SANDBOX_VM_ID and then `hostname` unconditionally, and in a
# shared sandbox BOTH of those name the sandbox — one string for every box in it. Whether that
# fallback is sound depends on which world this box is in, and the fact that answers it here is the
# fleet launcher: skein installs it at `fleet::box_session_path()` in the one sandbox that holds
# boxes, and never in a per-VM sandbox, which `sbx create` builds with no fleet machinery at all.
# box-pane.sh answers the same question from SKEIN_TMUX_SOCK and spells the argument out in full; a
# hook is not started by the attach and never sees that variable, but the launcher is a fact about
# the SANDBOX and so is visible to anything running inside it, whatever its lineage. So
#   · SKEIN_BOX set          — that is the box, whatever else is in the environment;
#   · unset, no launcher     — a legacy box, alone in its VM, where the two names are the same
#                              string. Unchanged: this is the path that has always worked;
#   · unset, with a launcher — a shared sandbox and no identity. Writing under SANDBOX_VM_ID here
#                              files this box's signal under a name that is not its own, and
#                              overwrites whichever box does own that name.
#
# Refusing is the conservative half. A box with no signal reads as one that has not reported, which
# is TRUE and which the board already says out loud; a signal under the wrong name is well-formed,
# fresh, and renders as another box's state with nothing to mark it. Measured residue of the
# writing version: five repo stores hold a `status/skein-fleet.json`, one holds a `skein-fleet`
# entry in its registry — `skein-fleet` is `config::default_fleet_sandbox`, the SANDBOX's name, and
# no box has ever been called that.
if [ -n "${SKEIN_BOX:-}" ]; then
  vmid="$SKEIN_BOX"
elif [ ! -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
  vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
else
  exit 0
fi
vmid="${vmid//\//-}"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"

# Heartbeat before anything can fail: proof the hook fired at all. Rotated crudely at ~256KB so a
# long-lived box can't grow it unbounded (keep the newest half).
beat() {
  local hl="$store/hook-log" f line
  mkdir -p "$hl" 2>/dev/null || return 0
  f="$hl/$vmid.jsonl"
  printf '{"ts":"%s","script":"box-session","event":"%s","ok":%s}\n' "$ts" "$mode" "${1:-true}" >>"$f" 2>/dev/null || true
  if [ "$(wc -c <"$f" 2>/dev/null || echo 0)" -gt 262144 ]; then
    line="$(wc -l <"$f" 2>/dev/null || echo 0)"
    tail -n "$((line / 2))" "$f" >"$f.tmp" 2>/dev/null && mv "$f.tmp" "$f" 2>/dev/null || rm -f "$f.tmp" 2>/dev/null
  fi
}
beat true

have_jq() { command -v jq >/dev/null 2>&1; }

# Pull a string field out of the hook payload (empty if jq/field absent).
field() { have_jq && printf '%s' "$payload" | jq -r --arg k "$1" '.[$k] // ""' 2>/dev/null || true; }

# Last assistant text from the transcript JSONL (the fallback when the payload lacks the message).
# Scans a bounded tail so a huge transcript stays cheap.
last_from_transcript() {
  local tp="$1"
  [ -n "$tp" ] && [ -r "$tp" ] && have_jq || return 0
  tail -n 80 "$tp" 2>/dev/null | jq -rs '
    [ .[] | select(.type? == "assistant")
          | (.message.content // []) | if type == "array" then . else [] end
          | map(select(.type? == "text") | .text) | join("\n") | select(length > 0) ]
    | last // ""' 2>/dev/null || true
}

write_signal() { # $1 = kind, $2 = lastMessage, $3 = prompt
  local dir="$store/sessions" tmp
  mkdir -p "$dir" 2>/dev/null || return 0
  tmp="$(mktemp "$dir/.ss.XXXXXX" 2>/dev/null)" || return 0
  # `box` names whose words these are, inside the file, so the filename is a claim the reader can
  # check rather than one it has to believe (`signal_is_ours`, src/signals.rs). This signal is words
  # the agent already wrote, shown as the box's headline: under the wrong name it reads as this box
  # saying something it never said, which is the one kind of wrong nothing downstream can spot.
  if have_jq; then
    jq -n --arg t "$ts" --arg k "$1" --arg m "$2" --arg p "$3" --arg b "$vmid" \
      '{ts:$t, kind:$k, lastMessage:$m, prompt:$p, box:$b}' >"$tmp" 2>/dev/null
  else
    # jq-less degraded write: strip the characters that would break hand-rolled JSON rather than
    # lose the whole signal (headline text survives; exotic content is truncated by the sanitize).
    s() { printf '%s' "$1" | tr -d '\000-\037"\\' | head -c 2000; }
    printf '{"ts":"%s","kind":"%s","lastMessage":"%s","prompt":"%s","box":"%s"}\n' \
      "$ts" "$1" "$(s "$2")" "$(s "$3")" "$vmid" >"$tmp" 2>/dev/null
  fi
  mv "$tmp" "$dir/$vmid.json" 2>/dev/null || rm -f "$tmp" 2>/dev/null
}

case "$mode" in
  stop)
    msg="$(field last_assistant_message)"
    [ -n "$msg" ] || msg="$(last_from_transcript "$(field transcript_path)")"
    # cap: the headline shows one line; the digest shows a paragraph. 4000 chars is plenty.
    msg="$(printf '%s' "$msg" | head -c 4000)"
    [ -n "$msg" ] || { beat false; exit 0; }
    write_signal stop "$msg" ""
    ;;
  ask)
    q="$(field message)"
    q="$(printf '%s' "$q" | head -c 2000)"
    [ -n "$q" ] || { beat false; exit 0; }
    write_signal notification "" "$q"
    ;;
  *) : ;;
esac
exit 0
