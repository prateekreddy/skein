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
# Which store this box reports into, and under which name: both decided in box-self.sh, installed
# beside this script, which says why neither is guessed (SKEIN-1174). Either one unknown is a quiet
# exit — a hook's stdout is read by the agent, and a signal nobody files is one the board already
# calls missing, while one filed in the checkout or under the sandbox's name is one it believes.
here="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
. "$here/box-self.sh" 2>/dev/null || exit 0
store="$(skein_box_store "$root")" || exit 0
vmid="$(skein_box_name)" || exit 0
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
